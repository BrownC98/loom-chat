use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::hmac;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params};

use api::graph::Place;
use api::graph::chat::*;
use api::graph::docs::PostOutside;
use api::graph::errors::{BRIEF_LIMIT_BYTES, Refusal};
use api::graph::events::Who;
use api::graph::messages::{Kind, MessageKind, Question};
use api::graph::waits::{Closed, HumanWait};
use api::ids::{AccountId, AgentId, NodeId, RepoId};

use super::{
    Graph, REACHABLE, doc_talk, folder_name, from_json, hosts, mailbox, optional_json, status,
    to_json, waits,
};
use crate::process_tree::Identity;
use crate::protocol::{Delivery, HUMAN, LOOM, MessageId, Session};

const PINNED: &str = "all";
const OUTSIDE_SUFFIX: &str = " (outside agent)";
const NOTICE_KINDS: &str = "('batch', 'accountSwitched', 'noAccountLeft', 'relaunched', 'endpointFailed', 'created', 'removed', 'tidied')";
const EVENT_NODE: &str = "(CASE WHEN json_valid(entry) THEN entry ->> '$.node.id' END)";
const ROOM_COLUMNS: &str = "SELECT id, name, archived FROM chat_rooms";

struct Room {
    id: String,
    name: String,
    kind: ChatRoomKind,
    archived: bool,
    members: Vec<NodeId>,
}

impl Room {
    fn writable(&self) -> Result<(), Refusal> {
        if self.archived {
            return Err(Refusal::Ended(self.name.clone()));
        }
        Ok(())
    }
}

pub(crate) struct ChatLaunch {
    pub node: NodeId,
    pub agent: Option<AgentId>,
    pub account: Option<AccountId>,
    pub worktree: String,
    pub session: Option<Session>,
    token: Option<String>,
}

pub(crate) struct ChatInterrupt {
    pub node: NodeId,
    pub worktree: String,
    pub shells: Vec<Identity>,
}

impl Graph {
    pub fn chat_rooms(&self) -> Result<Vec<ChatRoom>, Refusal> {
        let db = self.lock();
        let key = key(&db)?;
        rooms(&db)?
            .into_values()
            .map(|room| room_view(&db, &key, room))
            .collect::<Result<Vec<_>, _>>()
            .map(ordered_rooms)
    }

    pub fn chat_recipients(
        &self,
        tool: impl Fn(NodeId) -> Option<String>,
    ) -> Result<ChatRecipients, Refusal> {
        let db = self.lock();
        let key = key(&db)?;
        let mut rooms = rooms(&db)?
            .into_values()
            .filter(|room| !room.archived)
            .map(|room| room_view(&db, &key, room))
            .collect::<Result<Vec<_>, _>>()
            .map(ordered_rooms)?;
        let mut agents = Vec::new();
        let mut metadata = db.prepare_cached(&format!(
            "SELECT n.heard, (SELECT max(at) FROM (
                SELECT max(at) AS at FROM events WHERE {EVENT_NODE} = n.id
                UNION ALL SELECT max(created_at) FROM messages WHERE recipient_node_id = n.id
                UNION ALL SELECT max(created_at) FROM messages WHERE sender_node_id = n.id
             )) FROM nodes n WHERE n.id = ?1 AND {REACHABLE}"
        ))?;
        for node in status::views(&db, self.linked, &tool)? {
            let Some((assignment, activity)) = metadata
                .query_row([node.id.0], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                    ))
                })
                .optional()?
            else {
                continue;
            };
            let modes = if node.agent.as_ref().is_some_and(self.linked) {
                Kind::ALL.to_vec()
            } else {
                vec![Kind::Queue]
            };
            agents.push(ChatRecipient {
                node: node.id,
                name: node.name,
                work: work_id(&db, &node.place)?,
                harness: node.agent,
                status: node.status,
                task: tool(node.id).or(assignment),
                modes,
                last_activity: activity
                    .into_iter()
                    .chain(Some(node.since).filter(|at| *at > 0))
                    .max(),
            });
        }
        let selectable: BTreeSet<_> = agents.iter().map(|agent| agent.node.0).collect();
        for room in &mut rooms {
            room.members.retain(|node| selectable.contains(&node.0));
        }
        agents.sort_by(|a, b| (&a.work, &a.name, a.node.0).cmp(&(&b.work, &b.name, b.node.0)));
        Ok(ChatRecipients { rooms, agents })
    }

    pub fn create_chat_room(&self, req: SaveChatRoom) -> Result<ChatRoom, Refusal> {
        self.save_chat_room(None, req)
    }

    pub fn update_chat_room(&self, room: &str, req: SaveChatRoom) -> Result<ChatRoom, Refusal> {
        self.save_chat_room(Some(custom_id(room)?), req)
    }

    fn save_chat_room(&self, id: Option<i64>, req: SaveChatRoom) -> Result<ChatRoom, Refusal> {
        let name = req.name.trim();
        if name.is_empty() {
            return Err(Refusal::EmptyName);
        }
        checked_text(name)?;
        let mut db = self.lock();
        let tx = db.transaction()?;
        let id = if let Some(id) = id {
            let changed = tx.execute(
                "UPDATE chat_rooms SET name = ?2 WHERE id = ?1 AND archived = 0",
                params![id, name],
            )?;
            if changed == 0 {
                return Err(room_missing(&format!("custom-{id}")));
            }
            tx.execute("DELETE FROM chat_members WHERE room = ?1", [id])?;
            id
        } else {
            tx.execute("INSERT INTO chat_rooms (name) VALUES (?1)", [name])?;
            tx.last_insert_rowid()
        };
        for node in req.members {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM chat_node_places WHERE id = ?1)",
                [node.0],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(node_missing(node));
            }
            tx.execute(
                "INSERT OR IGNORE INTO chat_members (room, node) VALUES (?1, ?2)",
                params![id, node.0],
            )?;
        }
        let id = format!("custom-{id}");
        let room = room_view(&tx, &key(&tx)?, take_room(&mut rooms(&tx)?, &id)?)?;
        tx.commit()?;
        self.publish_nodes();
        Ok(room)
    }

    pub fn archive_chat_room(&self, room: &str) -> Result<(), Refusal> {
        let id = custom_id(room)?;
        let db = self.lock();
        if db.execute("UPDATE chat_rooms SET archived = 1 WHERE id = ?1", [id])? == 0 {
            return Err(room_missing(room));
        }
        self.publish_nodes();
        Ok(())
    }

    pub fn chat_stream(
        &self,
        room: &str,
        before: Option<ChatCursor>,
        limit: usize,
    ) -> Result<ChatPage, Refusal> {
        let db = self.lock();
        let room = take_room(&mut rooms(&db)?, room)?;
        stream(&db, &room, Audience::Human, before, limit)
    }

    pub fn outside_chat_stream(
        &self,
        room: &str,
        token: &str,
        before: Option<ChatCursor>,
        limit: usize,
    ) -> Result<ChatPage, Refusal> {
        let mut db = self.lock();
        verify(&db, room, token)?;
        let tx = db.transaction()?;
        let room = take_room(&mut rooms(&tx)?, room)?;
        let mut page = stream(&tx, &room, Audience::Outside, before, limit)?;
        let mut picked = 0;
        for item in &mut page.items {
            if let ChatEntry::Message { message } = &mut item.entry {
                message.editable = false;
                message.room = None;
                if !room.archived
                    && message.origin == ChatOrigin::Human
                    && let Some(group) = message.group
                {
                    picked += tx.execute(
                        "UPDATE chat_room_messages SET outside_read = 1
                         WHERE room = ?1 AND outside_read = 0
                         AND message IN (SELECT id FROM messages WHERE chat_group = ?2)",
                        params![room.id, group],
                    )?;
                    message.outside_read = true;
                }
            }
        }
        tx.commit()?;
        if picked > 0 {
            self.publish_nodes();
        }
        Ok(page)
    }

    pub fn send_chat(
        &self,
        req: SendChat,
        stop: impl FnMut(ChatInterrupt),
    ) -> Result<ChatMessage, Refusal> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let catalog = rooms(&tx)?;
        let targets = Targets::from_request(&tx, &catalog, &req, self.linked)?;
        if req.mode == Kind::Interrupt
            && let Some(confirmed) = &req.confirmed_nodes
        {
            let confirmed: BTreeSet<_> = confirmed.iter().map(|node| node.0).collect();
            if !targets.nodes.keys().copied().eq(confirmed) {
                return Err(Refusal::BadSetting(
                    "받는 이가 바뀌었습니다. 중단할 일을 다시 확인해 주세요.".into(),
                ));
            }
        }
        if let Some(nodes) = &req.attachment_nodes {
            let uploaded: BTreeSet<_> = nodes.iter().map(|node| node.0).collect();
            if !targets.outside.is_empty() || !targets.nodes.keys().copied().eq(uploaded) {
                return Err(Refusal::BadSetting(
                    "받는 이가 바뀌었습니다. 첨부 파일을 받을 사람을 다시 확인해 주세요.".into(),
                ));
            }
        }
        let text = checked_text(&req.text)?;
        let group = enqueue(&tx, targets, HUMAN, req.mode, text)?;
        let message = group_message(&tx, PINNED, group)?;
        let interrupts = if req.mode == Kind::Interrupt {
            interrupts(&tx, &message.receipts)?
        } else {
            Vec::new()
        };
        tx.commit()?;
        interrupts.into_iter().for_each(stop);
        drop(db);
        self.publish_nodes();
        self.wake_mail();
        Ok(message)
    }

    pub fn post_outside_chat(
        &self,
        room: &str,
        token: &str,
        req: PostOutside,
    ) -> Result<ChatMessage, Refusal> {
        let mut db = self.lock();
        verify(&db, room, token)?;
        let tx = db.transaction()?;
        let room = take_room(&mut rooms(&tx)?, room)?;
        room.writable()?;
        let from = doc_talk::outside_name(&req.from)?;
        let text = checked_text(&req.text)?;
        let body =
            doc_talk::outside_body(&from, text, &format!("/api/chat/rooms/{}/stream", room.id));
        let targets = Targets::in_room(&tx, &room)?;
        let group = enqueue(
            &tx,
            targets,
            &format!("{from}{OUTSIDE_SUFFIX}"),
            Kind::Queue,
            &body,
        )?;
        let mut message = group_message(&tx, &room.id, group)?;
        message.room = None;
        tx.commit()?;
        drop(db);
        self.publish_nodes();
        self.wake_mail();
        Ok(message)
    }

    pub fn edit_chat(&self, group: u32, text: &str) -> Result<ChatMessage, Refusal> {
        let text = checked_text(text)?;
        let mut db = self.lock();
        let tx = db.transaction()?;
        check_editable(&tx, group)?;
        tx.execute(
            "UPDATE messages SET body = ?2 WHERE chat_group = ?1",
            params![group, text],
        )?;
        let message = group_message(&tx, PINNED, group)?;
        tx.commit()?;
        self.publish_nodes();
        Ok(message)
    }

    pub fn withdraw_chat(&self, group: u32) -> Result<(), Refusal> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        check_editable(&tx, group)?;
        tx.execute("DELETE FROM messages WHERE chat_group = ?1", [group])?;
        tx.commit()?;
        self.publish_nodes();
        Ok(())
    }

    pub(crate) fn chat_launch(&self, room: &str) -> Result<Option<ChatLaunch>, Refusal> {
        let db = self.lock();
        let room = take_room(&mut rooms(&db)?, room)?;
        let ChatRoomKind::Agent { node } = room.kind else {
            return Err(room_missing(&room.id));
        };
        Ok(chat_launch(&db, node)?)
    }

    pub(crate) fn chat_delivery_texts(
        &self,
        node: NodeId,
        ids: &BTreeSet<u32>,
    ) -> rusqlite::Result<Vec<String>> {
        let db = self.lock();
        db.prepare_cached(
            "SELECT id, sender_name, kind, body, what FROM messages
             WHERE recipient_node_id = ?1 AND (sent_at IS NOT NULL OR delivered = 1 OR chat_exposed = 1)
               AND id IN (SELECT value FROM json_each(?2))",
        )?
        .query_map(params![node.0, to_json(ids)], |row| {
            Ok(Delivery {
                id: MessageId(row.get(0)?),
                from: row.get(1)?,
                kind: from_json(row, 2)?,
                body: row.get(3)?,
                what: optional_json(row, 4)?,
            }
            .text())
        })?
        .collect()
    }

    pub(crate) fn chat_session_page(
        &self,
        room: &str,
        before: Option<ChatCursor>,
        limit: usize,
        launch: &ChatLaunch,
    ) -> Result<(ChatPage, bool), Refusal> {
        let db = self.lock();
        let room = take_room(&mut rooms(&db)?, room)?;
        let same = chat_launch(&db, launch.node)?.is_some_and(|now| {
            now.token == launch.token
                && now.agent == launch.agent
                && now.account == launch.account
                && now.worktree == launch.worktree
                && now.session.as_ref().map(|s| &s.0) == launch.session.as_ref().map(|s| &s.0)
        });
        Ok((stream(&db, &room, Audience::Human, before, limit)?, same))
    }
}

fn chat_launch(db: &Connection, node: NodeId) -> rusqlite::Result<Option<ChatLaunch>> {
    db.query_row(
        "SELECT agent, account, worktree, session, token FROM nodes WHERE id = ?1",
        [node.0],
        |row| {
            Ok(ChatLaunch {
                node,
                agent: from_json(row, 0)?,
                account: optional_json(row, 1)?,
                worktree: row.get(2)?,
                session: row.get::<_, Option<String>>(3)?.map(Session),
                token: row.get(4)?,
            })
        },
    )
    .optional()
}

pub(super) fn ensure_key(db: &Connection) -> rusqlite::Result<()> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_key WHERE id = 1)",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        let mut secret = [0; 32];
        getrandom::fill(&mut secret).map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                error.to_string(),
            )))
        })?;
        db.execute(
            "INSERT OR IGNORE INTO chat_key (id, key) VALUES (1, ?1)",
            [secret.as_slice()],
        )?;
    }
    Ok(())
}

fn key(db: &Connection) -> rusqlite::Result<hmac::Key> {
    let key: Vec<u8> = db.query_row("SELECT key FROM chat_key WHERE id = 1", [], |row| {
        row.get(0)
    })?;
    Ok(hmac::Key::new(hmac::HMAC_SHA256, &key))
}

fn verify(db: &Connection, room: &str, token: &str) -> Result<(), Refusal> {
    let key = key(db)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| room_missing(room))?;
    hmac::verify(&key, room.as_bytes(), &bytes).map_err(|_| room_missing(room))
}

fn ordered_rooms(mut rooms: Vec<ChatRoom>) -> Vec<ChatRoom> {
    rooms.sort_by(|a, b| {
        let rank = |room: &ChatRoom| match &room.kind {
            ChatRoomKind::Pinned => 0,
            ChatRoomKind::Custom => 1,
            ChatRoomKind::Work { .. } => 2,
            ChatRoomKind::Agent { .. } => 3,
        };
        (rank(a), a.archived, &a.name, &a.id).cmp(&(rank(b), b.archived, &b.name, &b.id))
    });
    rooms
}

fn work_id(db: &Connection, place: &Place) -> rusqlite::Result<String> {
    db.query_row(
        "SELECT format('work-%d-%s', ?1, hex(?2))",
        params![place.repo.0, place.worktree],
        |row| row.get(0),
    )
}

fn agent_room(node: NodeId) -> String {
    format!("agent-{node}")
}

fn rooms(db: &Connection) -> rusqlite::Result<BTreeMap<String, Room>> {
    let mut rooms = BTreeMap::new();
    let mut pinned = Room {
        id: PINNED.into(),
        name: "종합 채팅방".into(),
        kind: ChatRoomKind::Pinned,
        archived: false,
        members: Vec::new(),
    };
    let mut stmt = db.prepare_cached(&format!(
        "SELECT id, name, repo, worktree, NOT {REACHABLE} FROM nodes
         UNION ALL SELECT id, name, repo, worktree, 1 FROM chat_archived_nodes ORDER BY id"
    ))?;
    let mut nodes = stmt.query([])?;
    while let Some(row) = nodes.next()? {
        let node = NodeId(row.get(0)?);
        let name = row.get(1)?;
        let archived = row.get(4)?;
        let id = agent_room(node);
        rooms.insert(
            id.clone(),
            Room {
                id,
                name,
                kind: ChatRoomKind::Agent { node },
                archived,
                members: vec![node],
            },
        );
        pinned.members.push(node);
        if let Some((repo, worktree)) = row
            .get::<_, Option<u32>>(2)?
            .zip(row.get::<_, Option<String>>(3)?)
        {
            let place = Place {
                repo: RepoId(repo),
                worktree,
            };
            let id = work_id(db, &place)?;
            let work = rooms.entry(id.clone()).or_insert_with(|| Room {
                id,
                name: folder_name(&place.worktree).to_owned(),
                kind: ChatRoomKind::Work { place },
                archived: true,
                members: Vec::new(),
            });
            work.members.push(node);
            work.archived &= archived;
        }
    }
    rooms.insert(pinned.id.clone(), pinned);
    let mut custom = db.prepare_cached(&format!("{ROOM_COLUMNS} ORDER BY id"))?;
    let mut members =
        db.prepare_cached("SELECT node FROM chat_members WHERE room = ?1 ORDER BY node")?;
    let mut found = custom.query([])?;
    while let Some(row) = found.next()? {
        let number: i64 = row.get(0)?;
        let id = format!("custom-{number}");
        rooms.insert(
            id.clone(),
            Room {
                id,
                name: row.get(1)?,
                kind: ChatRoomKind::Custom,
                archived: row.get(2)?,
                members: members
                    .query_map([number], |r| Ok(NodeId(r.get(0)?)))?
                    .collect::<rusqlite::Result<_>>()?,
            },
        );
    }
    Ok(rooms)
}

fn custom_id(room: &str) -> Result<i64, Refusal> {
    room.strip_prefix("custom-")
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id > 0 && format!("custom-{id}") == room)
        .ok_or_else(|| room_missing(room))
}

fn take_room(rooms: &mut BTreeMap<String, Room>, id: &str) -> Result<Room, Refusal> {
    rooms.remove(id).ok_or_else(|| room_missing(id))
}

fn room_missing(id: &str) -> Refusal {
    Refusal::NotFound(format!("채팅방 {id}"))
}

fn node_missing(node: NodeId) -> Refusal {
    Refusal::NotFound(format!("노드 #{node}"))
}

fn not_steerable(name: String) -> Refusal {
    Refusal::NotSteerable {
        name,
        linked: Vec::new(),
    }
}

fn checked_text(text: &str) -> Result<&str, Refusal> {
    if text.trim().is_empty() {
        return Err(Refusal::EmptyAnswer);
    }
    if text.len() > BRIEF_LIMIT_BYTES {
        return Err(Refusal::BriefTooLong(text.len()));
    }
    Ok(text)
}

#[derive(Default)]
struct Targets {
    nodes: BTreeMap<u32, BTreeSet<String>>,
    outside: BTreeSet<String>,
}

impl Targets {
    fn in_room(db: &Connection, room: &Room) -> Result<Self, Refusal> {
        let mut targets = Self::default();
        targets.add_room(db, room)?;
        Ok(targets)
    }

    fn from_request(
        db: &Connection,
        catalog: &BTreeMap<String, Room>,
        req: &SendChat,
        linked: fn(&AgentId) -> bool,
    ) -> Result<Self, Refusal> {
        let mut targets = Self::default();
        for id in req.rooms.as_deref().unwrap_or_default() {
            let room = catalog.get(id).ok_or_else(|| room_missing(id))?;
            targets.add_room(db, room)?;
        }
        for node in req.nodes.as_deref().unwrap_or_default() {
            let id = agent_room(*node);
            catalog
                .get(&id)
                .ok_or_else(|| node_missing(*node))?
                .writable()?;
            targets.nodes.entry(node.0).or_default();
        }
        if targets.nodes.is_empty() && targets.outside.is_empty() {
            return Err(Refusal::NotFound("받는 이".into()));
        }
        if let Some(id) = &req.room {
            let room = catalog.get(id).ok_or_else(|| room_missing(id))?;
            room.writable()?;
            for member in &room.members {
                if let Some(links) = targets.nodes.get_mut(&member.0) {
                    links.insert(id.clone());
                }
            }
        }
        if req.mode != Kind::Queue {
            if !targets.outside.is_empty() {
                return Err(not_steerable("외부 에이전트".into()));
            }
            for id in targets.nodes.keys() {
                let (name, agent): (String, Option<AgentId>) =
                    db.query_row("SELECT name, agent FROM nodes WHERE id = ?1", [id], |row| {
                        Ok((row.get(0)?, from_json(row, 1)?))
                    })?;
                if !agent.as_ref().is_some_and(linked) {
                    return Err(not_steerable(name));
                }
            }
        }
        Ok(targets)
    }

    fn add_room(&mut self, db: &Connection, room: &Room) -> Result<(), Refusal> {
        room.writable()?;
        let mut live = false;
        let mut stmt = db.prepare_cached(&format!(
            "SELECT EXISTS (SELECT 1 FROM nodes WHERE id = ?1 AND {REACHABLE})"
        ))?;
        for node in &room.members {
            if stmt.query_row([node.0], |row| row.get::<_, bool>(0))? {
                self.nodes
                    .entry(node.0)
                    .or_default()
                    .insert(room.id.clone());
                live = true;
            }
        }
        if !live {
            self.outside.insert(room.id.clone());
        }
        Ok(())
    }
}

fn enqueue(
    db: &Connection,
    targets: Targets,
    sender: &str,
    mode: Kind,
    body: &str,
) -> Result<u32, Refusal> {
    let at: i64 = db.query_row("SELECT unixepoch()", [], |row| row.get(0))?;
    let mut group = None;
    let recipients = targets
        .nodes
        .into_iter()
        .map(|(id, rooms)| (Some(NodeId(id)), rooms))
        .chain((!targets.outside.is_empty()).then_some((None, targets.outside)));
    for (node, mut rooms) in recipients {
        let message =
            mailbox::enqueue_unsigned_in(db, node, sender, mode, &MessageKind::Note, body)?;
        let group = *group.get_or_insert(message.0);
        db.execute(
            "UPDATE messages SET chat_group = ?2, created_at = ?3 WHERE id = ?1",
            params![message.0, group, at],
        )?;
        if let Some(node) = node {
            rooms.insert(agent_room(node));
        }
        if sender == HUMAN {
            rooms.insert(PINNED.into());
        }
        for room in rooms {
            db.execute(
                "INSERT INTO chat_room_messages (room, message) VALUES (?1, ?2)",
                params![room, message.0],
            )?;
        }
    }
    group.ok_or_else(|| Refusal::NotFound("받는 이".into()))
}

fn interrupts(db: &Connection, receipts: &[ChatReceipt]) -> rusqlite::Result<Vec<ChatInterrupt>> {
    receipts
        .iter()
        .filter_map(|receipt| receipt.to.id)
        .map(|node| {
            Ok(ChatInterrupt {
                node,
                worktree: super::place_of(db, node)?.worktree,
                shells: hosts::guarded(db, node)?,
            })
        })
        .collect()
}

fn check_editable(db: &Connection, group: u32) -> Result<(), Refusal> {
    let Some(allowed) = editable(db, group)? else {
        return Err(Refusal::NotFound(format!("메시지 #{group}")));
    };
    if !allowed {
        return Err(Refusal::Ended(format!("메시지 #{group}")));
    }
    Ok(())
}

fn editable(db: &Connection, group: u32) -> rusqlite::Result<Option<bool>> {
    db.query_row(
        &format!(
            "SELECT min(m.sender_name = ?2 AND m.chat_exposed = 0 AND m.kind != ?3
             AND (m.recipient_node_id IS NULL OR EXISTS (SELECT 1 FROM nodes n
                 WHERE n.id = m.recipient AND {REACHABLE}))
             AND NOT EXISTS (SELECT 1 FROM chat_room_messages rm LEFT JOIN chat_rooms r
                 ON rm.room = format('custom-%d', r.id)
                 WHERE rm.message = m.id AND (rm.outside_read = 1 OR r.archived = 1)))
         FROM messages m WHERE m.chat_group = ?1"
        ),
        params![group, HUMAN, to_json(&Kind::Interrupt)],
        |row| row.get(0),
    )
}

fn room_view(db: &Connection, key: &hmac::Key, room: Room) -> Result<ChatRoom, Refusal> {
    let sql = timeline_sql(&room, Audience::Human);
    let (at, unread, questions, done) = db.query_row(
        &format!(
            "{sql} SELECT max(at), coalesce(sum(unread), 0),
            coalesce(sum(question), 0), coalesce(sum(done), 0) FROM timeline"
        ),
        params![room.id, timeline_scope(&room)],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let token = URL_SAFE_NO_PAD.encode(hmac::sign(key, room.id.as_bytes()).as_ref());
    Ok(ChatRoom {
        outside: format!("/api/chat/rooms/{}/outside/{token}", room.id),
        id: room.id,
        name: room.name,
        kind: room.kind,
        archived: room.archived,
        members: room.members,
        unread,
        questions,
        done,
        last_activity: at,
    })
}

fn timeline_scope(room: &Room) -> Value {
    match room.kind {
        ChatRoomKind::Agent { node } => Value::Integer(i64::from(node.0)),
        ChatRoomKind::Work { .. } => Value::Text(to_json(&room.members)),
        _ => Value::Null,
    }
}

enum Audience {
    Human,
    Outside,
}

fn timeline_sql(room: &Room, audience: Audience) -> String {
    let in_work = |node| format!("{node} IN (SELECT node FROM scope)");
    let linked = "m.id IN (SELECT message FROM chat_room_messages WHERE room = ?1)";
    let linked_reply =
        "EXISTS (SELECT 1 FROM chat_room_messages rm WHERE rm.room = ?1 AND rm.message = p.id)";
    let signed = "m.sender_node_id IS NULL AND m.sender_name IN ('human', 'loom')";
    let (messages, talk, card, notices) = match &room.kind {
        ChatRoomKind::Pinned => {
            let direct_talk = "(p.sender_node_id IS NULL AND p.sender_name = 'human')
                OR (p.id IS NULL AND t.said = 'human')";
            let messages = match audience {
                Audience::Human => "1".to_owned(),
                Audience::Outside => format!("{signed} OR ({linked} AND m.chat_group IS NOT NULL
                    AND m.sender_node_id IS NULL AND m.sender_name LIKE '%{OUTSIDE_SUFFIX}')"),
            };
            let talk = match audience {
                Audience::Human => format!("{direct_talk} OR (t.chat_reply = p.id
                    AND p.chat_group IS NOT NULL AND p.sender_node_id IS NULL
                    AND p.sender_name LIKE '%{OUTSIDE_SUFFIX}')"),
                Audience::Outside => format!("{direct_talk} OR ({linked_reply} AND t.chat_reply = p.id
                    AND p.chat_group IS NOT NULL AND p.sender_node_id IS NULL
                    AND p.sender_name LIKE '%{OUTSIDE_SUFFIX}')"),
            };
            (messages, talk, "1".to_owned(), "1".to_owned())
        }
        ChatRoomKind::Agent { .. } => (
            "(m.recipient_node_id = ?2 OR m.sender_node_id = ?2)".to_owned(),
            "p.recipient_node_id = ?2 OR t.doc = printf('room/%d', ?2)
                OR (p.id IS NULL AND EXISTS (SELECT 1 FROM doc_authors a WHERE a.doc = t.doc AND a.node = ?2))".to_owned(),
            "node = ?2".to_owned(),
            format!("{EVENT_NODE} = ?2"),
        ),
        ChatRoomKind::Work { .. } => (
            format!("{linked} OR (m.chat_group IS NULL AND {} AND (
                ({signed}) OR {}))", in_work("m.recipient_node_id"), in_work("m.sender_node_id")),
            format!("({linked_reply} AND p.sender_node_id IS NULL
                AND (p.sender_name = 'human' OR p.sender_name LIKE '% (outside agent)')) OR (p.chat_group IS NULL
                AND p.sender_node_id IS NULL AND p.sender_name = 'human' AND {})", in_work("p.recipient_node_id")),
            in_work("node"),
            in_work(EVENT_NODE),
        ),
        ChatRoomKind::Custom => (
            linked.to_owned(),
            format!("({linked_reply} AND (p.sender_name = 'human' OR p.sender_name LIKE '% (outside agent)'))"),
            "EXISTS (SELECT 1 FROM chat_room_waits cw WHERE cw.room = ?1 AND cw.wait = w.id)".to_owned(),
            "0".to_owned(),
        ),
    };
    format!(
        "WITH scope AS (SELECT value AS node FROM json_each(?2) WHERE ?1 IS NOT NULL), mail AS (
            SELECT coalesce(m.chat_group, m.id) AS group_id, min(m.id) AS id, min(m.created_at) AS at,
                sum(m.recipient IS NOT NULL AND m.delivered = 0 AND m.sent_at IS NULL AND m.printed = 0
                    AND EXISTS (SELECT 1 FROM nodes n WHERE n.id = m.recipient AND
                        {REACHABLE})) AS unread
            FROM messages m WHERE {messages} GROUP BY group_id
         ), history_waits AS (
            SELECT w.id, w.node, w.wait, n.name, NULL AS closed FROM chat_open_waits w JOIN nodes n ON n.id = w.node
            UNION ALL SELECT c.wait ->> '$.id', c.node, c.wait, c.name, c.how FROM closed_waits c
         ), timeline AS (
            SELECT at, 0 AS source, id, unread, 0 AS question, 0 AS done FROM mail
            UNION ALL
            SELECT t.at, 1, t.id, 0, 0, 0 FROM doc_talk t
                LEFT JOIN doc_talk parent ON parent.id = t.reply_to
                LEFT JOIN messages p ON p.id = coalesce(t.chat_reply, parent.message, t.message)
                WHERE (t.message IS NULL OR p.id IS NULL) AND ({talk})
            UNION ALL
            SELECT w.wait ->> '$.since', 2, w.id, 0,
                w.closed IS NULL AND w.wait ->> '$.kind' NOT IN ('done', 'reply'),
                w.closed IS NULL AND w.wait ->> '$.kind' IN ('done', 'reply')
                FROM history_waits w WHERE {card}
            UNION ALL
            SELECT e.at, 3, e.id, 0, 0, 0 FROM events e
                WHERE ({notices}) AND (CASE WHEN json_valid(e.entry) THEN e.entry ->> 'kind' END) IN {NOTICE_KINDS}
         )"
    )
}

fn stream(
    db: &Connection,
    room: &Room,
    audience: Audience,
    before: Option<ChatCursor>,
    limit: usize,
) -> Result<ChatPage, Refusal> {
    let at = before.map_or(i64::MAX, |c| c.at_ms);
    let source = before.map_or(i64::MAX, |c| c.source as i64);
    let id = before.map_or(i64::MAX, |c| c.id);
    let sql = timeline_sql(room, audience);
    let mut stmt = db.prepare_cached(&format!(
        "{sql} SELECT at * 1000 AS ms, source, id FROM timeline
            WHERE (at * 1000, source, id) < (?3, ?4, ?5)
            ORDER BY ms DESC, source DESC, id DESC LIMIT ?6"
    ))?;
    let keys = stmt
        .query_map(
            params![
                room.id,
                timeline_scope(room),
                at,
                source,
                id,
                (limit + 1) as u32
            ],
            |row| {
                Ok(ChatCursor {
                    at_ms: row.get(0)?,
                    source: source_of(row.get(1)?)?,
                    id: row.get(2)?,
                    part: 0,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = keys.len() > limit;
    let items = keys[..keys.len().min(limit)]
        .iter()
        .rev()
        .map(|&cursor| {
            let entry = match cursor.source {
                ChatSource::Message => ChatEntry::Message {
                    message: read_message(db, &room.id, cursor.id)?,
                },
                ChatSource::Talk => ChatEntry::Message {
                    message: read_talk(db, cursor.id)?,
                },
                ChatSource::Wait => read_wait(db, cursor.id)?,
                ChatSource::Notice => ChatEntry::Notice {
                    event: db.query_row(
                        "SELECT entry FROM events WHERE id = ?1",
                        [cursor.id],
                        |row| from_json(row, 0),
                    )?,
                },
                ChatSource::Transcript => unreachable!(),
            };
            Ok(ChatItem { cursor, entry })
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let before = items.first().filter(|_| more).map(|item| item.cursor);
    Ok(ChatPage { items, before })
}

fn source_of(value: i64) -> rusqlite::Result<ChatSource> {
    match value {
        0 => Ok(ChatSource::Message),
        1 => Ok(ChatSource::Talk),
        2 => Ok(ChatSource::Wait),
        3 => Ok(ChatSource::Notice),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(1, value)),
    }
}

fn group_message(db: &Connection, room: &str, group: u32) -> rusqlite::Result<ChatMessage> {
    let anchor: u32 = db.query_row(
        "SELECT min(m.id) FROM chat_room_messages rm JOIN messages m ON m.id = rm.message
         WHERE rm.room = ?1 AND m.chat_group = ?2",
        params![room, group],
        |row| row.get(0),
    )?;
    read_message(db, room, i64::from(anchor))
}

fn read_message(db: &Connection, room: &str, id: i64) -> rusqlite::Result<ChatMessage> {
    let mut message = db.query_row(
        "SELECT m.id, m.chat_group, m.sender_node_id, m.sender_name, m.body, m.kind, m.what,
            coalesce(
                (SELECT rm.room FROM chat_room_messages rm JOIN messages related ON related.id = rm.message
                 WHERE (related.id = m.id OR (?2 = 'all' AND related.chat_group = m.chat_group))
                 AND rm.room NOT IN ('all', ?2)
                 ORDER BY CASE WHEN rm.room LIKE 'agent-%' THEN 1 ELSE 0 END, rm.room LIMIT 1),
                (SELECT nr.room FROM chat_node_rooms nr WHERE nr.node = coalesce(m.sender_node_id, m.recipient_node_id)
                 AND nr.room LIKE 'work-%')
            )
         FROM messages m WHERE m.id = ?1",
        params![id, room],
        |row| {
            let mut from = who(row, 2, 3)?;
            let origin = origin(&from);
            if origin == ChatOrigin::Outside {
                from.name = display_outside(&from.name[..from.name.len() - OUTSIDE_SUFFIX.len()]);
            }
            let what: Option<MessageKind> = optional_json(row, 6)?;
            let reply_to = match what {
                Some(MessageKind::Answer { question: Question::Message { id } }) => Some(id),
                _ => None,
            };
            Ok(ChatMessage {
                id: row.get(0)?,
                group: row.get(1)?,
                origin,
                from,
                text: row.get(4)?,
                mode: Some(from_json(row, 5)?),
                what,
                reply_to,
                room: row.get(7)?,
                receipts: Vec::new(),
                outside_read: false,
                editable: false,
            })
        },
    )?;
    let sql = if message.group.is_some() {
        format!(
            "SELECT m.id, m.recipient_node_id, m.recipient_name, m.delivered, m.printed, m.sent_at,
                  m.chat_exposed, m.recipient, rm.outside_read,
                  EXISTS (SELECT 1 FROM nodes n WHERE n.id = m.recipient AND
                     {REACHABLE})
           FROM chat_room_messages rm JOIN messages m ON m.id = rm.message
           WHERE rm.room = ?1 AND m.chat_group = ?2 ORDER BY m.id"
        )
    } else {
        format!(
            "SELECT m.id, m.recipient_node_id, m.recipient_name, m.delivered, m.printed, m.sent_at,
                  m.chat_exposed, m.recipient, 0,
                  EXISTS (SELECT 1 FROM nodes n WHERE n.id = m.recipient AND
                     {REACHABLE})
           FROM messages m WHERE m.id = ?2 AND ?1 IS NOT NULL"
        )
    };
    let mut stmt = db.prepare_cached(&sql)?;
    let mut rows = stmt.query(params![room, message.group.unwrap_or(message.id)])?;
    while let Some(row) = rows.next()? {
        message.outside_read |= row.get::<_, bool>(8)?;
        if let Some(name) = row.get::<_, Option<String>>(2)? {
            let to = Who {
                id: row.get::<_, Option<u32>>(1)?.map(NodeId),
                name,
            };
            let state = if row.get::<_, bool>(3)? {
                ChatDelivery::Read
            } else if row.get::<_, bool>(4)? {
                ChatDelivery::Printed
            } else if row.get::<_, Option<i64>>(5)?.is_some() {
                ChatDelivery::Handed
            } else if row.get::<_, Option<u32>>(7)?.is_none() || !row.get::<_, bool>(9)? {
                ChatDelivery::Ended
            } else if row.get::<_, bool>(6)? {
                ChatDelivery::Retrying
            } else {
                ChatDelivery::Queued
            };
            message.receipts.push(ChatReceipt {
                message: row.get(0)?,
                to,
                state,
            });
        }
    }
    if let Some(group) = message.group {
        message.editable = editable(db, group)?.unwrap_or_default();
    }
    Ok(message)
}

fn display_outside(name: &str) -> String {
    doc_talk::outside_name(name).unwrap_or_else(|_| "외부 에이전트".to_owned())
}

fn origin(from: &Who) -> ChatOrigin {
    match (from.id, from.name.as_str()) {
        (Some(_), _) => ChatOrigin::Agent,
        (None, HUMAN) => ChatOrigin::Human,
        (None, LOOM) => ChatOrigin::Loom,
        (None, name) if name.ends_with(OUTSIDE_SUFFIX) => ChatOrigin::Outside,
        _ => ChatOrigin::Agent,
    }
}

fn who(row: &Row<'_>, id: usize, name: usize) -> rusqlite::Result<Who> {
    Ok(Who {
        id: row.get::<_, Option<u32>>(id)?.map(NodeId),
        name: row.get(name)?,
    })
}

fn read_talk(db: &Connection, id: i64) -> rusqlite::Result<ChatMessage> {
    db.query_row(
        "SELECT t.id, t.said, t.name, t.sender, t.text, coalesce(t.chat_reply, p.message),
            m.recipient_node_id,
            CASE WHEN t.said = 'agent' THEN
                coalesce((SELECT id FROM chat_node_places WHERE t.doc = printf('room/%d', id)),
                (SELECT node FROM doc_authors WHERE doc = t.doc)) END
         FROM doc_talk t LEFT JOIN doc_talk p ON p.id = t.reply_to
         LEFT JOIN messages m ON m.id = coalesce(t.chat_reply, p.message)
         WHERE t.id = ?1",
        [id],
        |row| {
            let said: String = row.get(1)?;
            let (origin, name) = match said.as_str() {
                "human" => (ChatOrigin::Human, HUMAN.to_owned()),
                "outside" => (
                    ChatOrigin::Outside,
                    display_outside(&row.get::<_, Option<String>>(3)?.unwrap_or_default()),
                ),
                _ => (ChatOrigin::Agent, row.get(2)?),
            };
            Ok(ChatMessage {
                id: row.get(0)?,
                group: None,
                from: Who {
                    id: row.get::<_, Option<u32>>(6)?.or(row.get(7)?).map(NodeId),
                    name,
                },
                origin,
                text: row.get(4)?,
                mode: None,
                what: None,
                reply_to: row.get(5)?,
                room: None,
                receipts: Vec::new(),
                outside_read: false,
                editable: false,
            })
        },
    )
}

fn read_wait(db: &Connection, id: i64) -> rusqlite::Result<ChatEntry> {
    db.query_row(
        "SELECT w.node, n.name, w.wait, NULL FROM chat_open_waits w JOIN nodes n ON n.id = w.node WHERE w.id = ?1
         UNION ALL SELECT c.node, c.name, c.wait, c.how FROM closed_waits c
         WHERE c.wait ->> '$.id' = ?1 LIMIT 1",
        [id],
        |row| {
            let node = who(row, 0, 1)?;
            let wait: HumanWait = from_json(row, 2)?;
            let closed: Option<Closed> = optional_json(row, 3)?;
            let mut wait = waits::for_screen(wait);
            if closed.is_some() {
                wait.wait.answerable = false;
                wait.wait.prompt = None;
            }
            Ok(ChatEntry::Wait { node, wait, closed })
        },
    )
}
