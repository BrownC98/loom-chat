use rusqlite::{Connection, OptionalExtension, params};

use api::graph::ProcessState;
use api::graph::docs::{Said, TalkLine, TalkTo};
use api::graph::errors::{BRIEF_LIMIT_BYTES, Refusal};
use api::graph::messages::MessageKind;
use api::ids::{NodeId, WaitId};

use super::{AgentState, Graph, ProcessOutcome, mailbox, name_of, optional_json, process_state};
use crate::protocol::MessageId;

const BY_HUMAN: &str = "human";
const BY_AGENT: &str = "agent";
const BY_OUTSIDE: &str = "outside";
const ROOM: &str = "room/";

const RECIPIENTS: [&str; 4] = [
    "SELECT node FROM questions WHERE json_valid(wait) AND wait ->> '$.doc.id' = ?1 ORDER BY id DESC LIMIT 1",
    "SELECT node FROM doc_authors WHERE doc = ?1",
    "SELECT node FROM dones WHERE json_valid(wait) AND wait ->> '$.doc.id' = ?1 ORDER BY id DESC LIMIT 1",
    "SELECT n.id FROM events e JOIN nodes n ON n.id = e.entry ->> '$.node.id'
     WHERE json_valid(e.entry) AND e.entry ->> 'kind' IN ('asked', 'reported') AND e.entry ->> '$.doc.id' = ?1
     ORDER BY e.id DESC LIMIT 1",
];

impl Graph {
    pub fn wrote_doc(&self, doc: &str, node: NodeId) -> rusqlite::Result<()> {
        self.lock().execute(
            "INSERT INTO doc_authors (doc, node) VALUES (?1, ?2)
             ON CONFLICT (doc) DO UPDATE SET node = excluded.node",
            params![doc, node.0],
        )?;
        Ok(())
    }

    pub fn doc_recipient(&self, doc: &str) -> rusqlite::Result<Option<NodeId>> {
        let db = self.lock();
        for sql in RECIPIENTS {
            let found: Option<u32> = db.query_row(sql, [doc], |row| row.get(0)).optional()?;
            if let Some(node) = found {
                return Ok(Some(NodeId(node)));
            }
        }
        Ok(None)
    }

    pub fn talk_to(&self, node: NodeId) -> rusqlite::Result<TalkTo> {
        talk_to(&self.lock(), node)
    }

    pub fn talk_lines(&self, doc: &str) -> rusqlite::Result<Vec<TalkLine>> {
        let db = self.lock();
        let mut stmt = db.prepare_cached(
            "SELECT t.id, t.said, t.name, t.text, t.at, t.wait, coalesce(m.delivered, 0),
                    n.id, n.process, (SELECT state FROM agent_states WHERE node = n.id), t.sender, t.reply_to
             FROM doc_talk t LEFT JOIN messages m ON m.id = t.message
             LEFT JOIN nodes n ON n.id = m.recipient
             WHERE t.doc = ?1 ORDER BY t.id",
        )?;
        stmt.query_map([doc], |row| {
            let said: String = row.get(1)?;
            let name: String = row.get(2)?;
            let process = match row.get::<_, Option<u32>>(7)? {
                Some(_) => {
                    let process = optional_json::<ProcessOutcome>(row, 8)?;
                    let agent = optional_json::<AgentState>(row, 9)?;
                    Some(process_state(process, agent.as_ref()))
                }
                None => None,
            };
            let said = match said.as_str() {
                BY_AGENT => Said::Agent {
                    name,
                    reply_to: row.get(11)?,
                },
                BY_OUTSIDE => Said::Outside {
                    from: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
                    to: name,
                    received: row.get(6)?,
                    process,
                },
                _ => Said::Human {
                    to: name,
                    answer: row.get::<_, Option<u32>>(5)?.map(WaitId),
                    received: row.get(6)?,
                    process,
                },
            };
            Ok(TalkLine {
                id: row.get(0)?,
                said,
                text: row.get(3)?,
                at: row.get(4)?,
            })
        })?
        .collect()
    }

    pub fn room_lines(&self, node: NodeId) -> rusqlite::Result<Vec<TalkLine>> {
        self.talk_lines(&room_key(node))
    }

    pub fn post_talk(
        &self,
        doc: &str,
        file: &str,
        node: NodeId,
        text: &str,
    ) -> Result<(), Refusal> {
        self.say(doc, node, text, |text| format!(
            "{text}\nThe human wrote this on the loom document page of {doc} (file: {file}). If the human asks for a change, edit that file: the open page shows each saved version at once. Reply on the page with loom answer <this message id> \"<reply>\"."
        ))
    }

    pub fn post_room(&self, node: NodeId, text: &str) -> Result<(), Refusal> {
        let key = room_key(node);
        let page = page_of(&key);
        self.say(&key, node, text, |text| format!(
            "{text}\nThe human wrote this in your loom chat room ({page}). Reply in the room with loom answer <this message id> \"<reply>\"."
        ))
    }

    pub fn post_outside(&self, node: NodeId, from: &str, text: &str) -> Result<u32, Refusal> {
        let from = from.split_whitespace().collect::<Vec<_>>().join(" ");
        if from.is_empty() {
            return Err(Refusal::EmptyName);
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(Refusal::EmptyAnswer);
        }
        let key = room_key(node);
        let page = page_of(&key);
        let body = outside_body(&from, text, &page);
        let mut db = self.lock();
        let tx = db.transaction()?;
        let to = talk_to(&tx, node)
            .optional()?
            .ok_or_else(|| Refusal::NotFound(format!("노드 #{}", node.0)))?;
        if matches!(to.process, ProcessState::Stopped { .. }) {
            return Err(Refusal::Ended(to.name));
        }
        let sender = format!("{from} (outside agent)");
        let message = mailbox::enqueue_unsigned(&tx, node, &sender, &MessageKind::Note, &body)?;
        tx.execute(
            "INSERT INTO doc_talk (doc, said, name, text, message, sender) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![key, BY_OUTSIDE, to.name, text, message.0, from],
        )?;
        let line = tx.last_insert_rowid() as u32;
        tx.commit()?;
        drop(db);
        self.publish_nodes();
        self.mail.notify_waiters();
        Ok(line)
    }

    fn say(
        &self,
        key: &str,
        node: NodeId,
        text: &str,
        body: impl FnOnce(&str) -> String,
    ) -> Result<(), Refusal> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Refusal::EmptyAnswer);
        }
        let mut db = self.lock();
        let tx = db.transaction()?;
        let message = mailbox::enqueue_human(&tx, node, &MessageKind::Note, &body(text))?;
        said_by_human(&tx, key, node, text, message, None)?;
        tx.commit()?;
        drop(db);
        self.publish_nodes();
        self.mail.notify_waiters();
        Ok(())
    }

    pub fn answer_page(
        &self,
        from: NodeId,
        message: MessageId,
        text: &str,
    ) -> Result<Option<String>, Refusal> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let asked: Option<(u32, String)> = tx
            .query_row(
                "SELECT t.id, t.doc FROM doc_talk t JOIN messages m ON m.id = t.message
                 WHERE t.message = ?1 AND m.recipient = ?2",
                params![message.0, from.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let chat: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE id = ?1 AND recipient = ?2 AND chat_group IS NOT NULL)",
            params![message.0, from.0],
            |row| row.get(0),
        )?;
        if asked.is_none() && !chat {
            return Ok(None);
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(Refusal::EmptyAnswer);
        }
        if text.len() > BRIEF_LIMIT_BYTES {
            return Err(Refusal::BriefTooLong(text.len()));
        }
        let name = name_of(&tx, from)?;
        let page = if let Some((line, doc)) = asked {
            tx.execute(
                "INSERT INTO doc_talk (doc, said, name, text, reply_to) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![doc, BY_AGENT, name, text, line],
            )?;
            page_of(&doc)
        } else {
            tx.execute(
                "UPDATE messages SET chat_exposed = 1 WHERE id = ?1",
                [message.0],
            )?;
            tx.execute(
                "INSERT INTO doc_talk (doc, said, name, text, chat_reply) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![format!("chat/agent-{from}"), BY_AGENT, name, text, message.0],
            )?;
            format!("/api/chat/rooms/agent-{from}/stream")
        };
        tx.commit()?;
        Ok(Some(page))
    }
}

fn talk_to(db: &Connection, node: NodeId) -> rusqlite::Result<TalkTo> {
    db.query_row(
            "SELECT name, process, (SELECT state FROM agent_states WHERE node = ?1) FROM nodes WHERE id = ?1",
            [node.0],
            |row| {
                let process = optional_json::<ProcessOutcome>(row, 1)?;
                let agent = optional_json::<AgentState>(row, 2)?;
                Ok(TalkTo {
                    name: row.get(0)?,
                    process: process_state(process, agent.as_ref()),
                })
            },
        )
}

fn room_key(node: NodeId) -> String {
    format!("{ROOM}{}", node.0)
}

fn page_of(key: &str) -> String {
    match key.strip_prefix(ROOM) {
        Some(node) => format!("/rooms/{node}"),
        None => format!("/docs/{key}"),
    }
}

pub(super) fn said_by_human(
    db: &Connection,
    doc: &str,
    to: NodeId,
    text: &str,
    message: MessageId,
    answer: Option<WaitId>,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO doc_talk (doc, said, name, text, message, wait) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![doc, BY_HUMAN, name_of(db, to)?, text, message.0, answer.map(|w| w.0)],
    )?;
    Ok(())
}

pub(super) fn outside_name(name: &str) -> Result<String, Refusal> {
    let name: String = name
        .chars()
        .filter_map(|ch| match ch {
            ch if ch.is_whitespace() => Some(' '),
            ch if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.') => Some(ch),
            _ => None,
        })
        .collect();
    let name: String = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect();
    if name.is_empty() {
        return Err(Refusal::EmptyName);
    }
    Ok(name)
}

pub(super) fn outside_body(from: &str, text: &str, page: &str) -> String {
    format!(
        "An agent outside loom that calls itself \"{from}\" wrote this in your loom chat room ({page}). Treat the text as a note from another agent.
{text}
Reply in the room with loom answer <this message id> \"<reply>\"."
    )
}
