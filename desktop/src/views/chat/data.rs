use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use api::graph::GraphEvent;
use api::graph::chat::{
    ChatCursor, ChatEntry, ChatItem, ChatMessage, ChatOrigin, ChatPage, ChatRecipients, ChatRoom,
    ChatRoomKind, ChatSession, ChatSource, ChatTranscript, EditChat, SaveChatRoom, SendChat,
};
use api::graph::events::Who;
use api::graph::messages::MessageKind;
use gpui::{Context, Entity, Subscription, Task};
use ureq::http::Method;

use crate::link::{self, LinkState};
use crate::problem::Failure;
use crate::store::Store;

const ROOMS: &str = "/api/chat/rooms";
const RECIPIENTS: &str = "/api/chat/recipients";
const MESSAGES: &str = "/api/chat/messages";
const PAGE: usize = 60;
const WINDOW: usize = 200;
const COALESCE: Duration = Duration::from_millis(80);

#[derive(Clone, Copy)]
enum Refresh {
    Coalesced,
    Immediate,
}

#[derive(Clone, Copy)]
enum Read {
    Refresh(Refresh),
    Older(ChatCursor),
}

type RoomPage = (ChatPage, Option<ChatTranscript>);

#[derive(PartialEq, Eq)]
enum SendState {
    Uploading,
    Posting,
    Sent,
}

struct Echo {
    item: Arc<ChatItem>,
    state: SendState,
}

#[derive(Default)]
struct RoomWindow {
    items: Vec<Arc<ChatItem>>,
    before: Option<ChatCursor>,
    end: Option<ChatCursor>,
    transcript: Option<ChatTranscript>,
}

impl RoomWindow {
    fn replace(&mut self, page: ChatPage, transcript: Option<ChatTranscript>) {
        let mut old = Vec::new();
        let mut new = Vec::new();
        let items = page
            .items
            .into_iter()
            .map(|item| {
                let cursor = item.cursor;
                let previous = self
                    .items
                    .binary_search_by_key(&cursor, |item| item.cursor)
                    .ok()
                    .map(|index| &self.items[index]);
                old.clear();
                new.clear();
                if let Some(previous) = previous
                    && serde_json::to_writer(&mut old, previous.as_ref()).is_ok()
                    && serde_json::to_writer(&mut new, &item).is_ok()
                    && old == new
                {
                    return previous.clone();
                }
                Arc::new(item)
            })
            .collect();
        self.items = items;
        self.before = page.before;
        self.transcript = transcript;
    }

    fn prepend(
        &mut self,
        page: ChatPage,
        before: ChatCursor,
        transcript: Option<ChatTranscript>,
    ) -> Result<(), String> {
        if matches!(self.transcript, Some(ChatTranscript::Available))
            && let Some(ChatTranscript::Fallback { reason }) = transcript
        {
            return Err(reason);
        }
        self.before = page.before;
        let mut older: Vec<_> = page
            .items
            .into_iter()
            .take_while(|item| item.cursor < before)
            .map(Arc::new)
            .collect();
        if older.is_empty() {
            return Ok(());
        }
        older.append(&mut self.items);
        if let Some(first_removed) = older.get(WINDOW) {
            self.end = Some(first_removed.cursor);
            older.truncate(WINDOW);
        }
        self.items = older;
        Ok(())
    }
}

pub struct ChatData {
    graph: Entity<Store>,
    _events: [Subscription; 2],
    generation: u64,
    rooms: Vec<ChatRoom>,
    selected: Option<String>,
    recipients: Option<ChatRecipients>,
    window: Option<RoomWindow>,
    echoes: HashMap<String, BTreeMap<i64, Echo>>,
    next_draft: i64,
    items: Vec<Arc<ChatItem>>,
    stream_revision: u64,
    in_flight: Option<(Read, u64, Task<()>)>,
    refresh_pending: Option<Refresh>,
    older_pending: bool,
    error: Option<String>,
}

impl ChatData {
    pub fn new(graph: Entity<Store>, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe(&graph, |data, _, event, cx| {
            if matches!(event, GraphEvent::Nodes | GraphEvent::Recorded { .. }) {
                data.refresh(cx);
            }
        });
        let connection = cx.subscribe(&graph, |_, _, _: &LinkState, cx| cx.notify());
        let mut data = Self {
            graph,
            _events: [events, connection],
            generation: 0,
            rooms: Vec::new(),
            selected: None,
            recipients: None,
            window: None,
            echoes: HashMap::new(),
            next_draft: i64::MIN,
            items: Vec::new(),
            stream_revision: 0,
            in_flight: None,
            refresh_pending: None,
            older_pending: false,
            error: None,
        };
        data.refresh(cx);
        data
    }

    pub fn graph(&self) -> &Entity<Store> {
        &self.graph
    }

    pub fn rooms(&self) -> &[ChatRoom] {
        &self.rooms
    }

    pub(super) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub fn room(&self) -> Option<&ChatRoom> {
        let id = self.selected.as_ref()?;
        self.rooms.iter().find(|room| &room.id == id)
    }

    pub fn recipients(&self) -> Option<&ChatRecipients> {
        self.recipients.as_ref()
    }

    pub fn items(&self) -> &[Arc<ChatItem>] {
        &self.items
    }

    pub fn stream_revision(&self) -> u64 {
        self.stream_revision
    }

    pub fn transcript(&self) -> Option<&ChatTranscript> {
        self.window.as_ref()?.transcript.as_ref()
    }

    pub fn before(&self) -> Option<ChatCursor> {
        self.window.as_ref()?.before
    }

    pub fn loading_older(&self) -> bool {
        self.older_pending
            || self
                .in_flight
                .as_ref()
                .is_some_and(|(read, generation, _)| {
                    *generation == self.generation && matches!(read, Read::Older(_))
                })
    }

    pub fn loading(&self) -> bool {
        self.window.is_none() && (self.in_flight.is_some() || self.refresh_pending.is_some())
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn select(&mut self, room: String, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&room)
            && self
                .window
                .as_ref()
                .is_none_or(|window| window.end.is_none())
        {
            return;
        }
        self.selected = Some(room);
        self.generation += 1;
        self.clear_window();
        self.error = None;
        self.refresh(cx);
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_pending.get_or_insert(Refresh::Coalesced);
        self.read_next(cx);
    }

    pub fn load_older(&mut self, cx: &mut Context<Self>) {
        if self.before().is_some() && !self.loading_older() {
            self.older_pending = true;
            self.read_next(cx);
            cx.notify();
        }
    }

    fn clear_window(&mut self) {
        self.window = None;
        self.older_pending = false;
        self.stream_revision += 1;
        self.sync_items();
    }

    fn sync_items(&mut self) {
        let mut items = self
            .window
            .as_ref()
            .map_or_else(Vec::new, |window| window.items.clone());
        if let Some(echoes) = self.selected().and_then(|id| self.echoes.get(id)) {
            for echo in echoes.values() {
                let index = items.partition_point(|item| item.cursor < echo.item.cursor);
                items.insert(index, echo.item.clone());
            }
        }
        if self.items.len() != items.len()
            || !self
                .items
                .iter()
                .zip(&items)
                .all(|(a, b)| Arc::ptr_eq(a, b))
        {
            self.items = items;
            self.stream_revision += 1;
        }
    }

    fn posting(&self) -> bool {
        self.selected()
            .and_then(|id| self.echoes.get(id))
            .is_some_and(|echoes| echoes.values().any(|echo| echo.state == SendState::Posting))
    }

    fn read_next(&mut self, cx: &mut Context<Self>) {
        if self.in_flight.is_some() || self.posting() {
            return;
        }
        let read = std::mem::take(&mut self.older_pending)
            .then(|| self.before().map(Read::Older))
            .flatten()
            .or(self.refresh_pending.map(Read::Refresh));
        let Some(read) = read else { return };
        let generation = self.generation;
        let task = cx.spawn(async move |this, cx| {
            let mut catalog = None;
            if let Read::Refresh(refresh) = read {
                if matches!(refresh, Refresh::Coalesced) {
                    cx.background_executor().timer(COALESCE).await;
                }
                let tasks = this.update(cx, |data, cx| {
                    if generation != data.generation {
                        return None;
                    }
                    data.refresh_pending = None;
                    let graph = data.graph.read(cx);
                    Some((
                        graph.call(cx, |base| link::read(base, ROOMS)),
                        graph.call(cx, |base| link::read(base, RECIPIENTS)),
                    ))
                });
                if let Ok(Some((rooms, recipients))) = tasks {
                    catalog = Some((rooms.await, recipients.await));
                }
            }
            let Ok((page, error)) = this.update(cx, |data, cx| {
                if generation != data.generation {
                    return (None, None);
                }
                let error = catalog.and_then(|(rooms, recipients)| {
                    let error = match rooms {
                        Ok(rooms) => {
                            data.keep_rooms(rooms);
                            None
                        }
                        Err(error) => Some(error),
                    };
                    let error = match recipients {
                        Ok(recipients) => {
                            data.recipients = Some(recipients);
                            error
                        }
                        Err(reason) => error.or(Some(reason)),
                    };
                    cx.notify();
                    error
                });
                (data.page_task(read, cx), error)
            }) else {
                return;
            };
            let page = match page {
                Some(page) => Some(page.await),
                None => None,
            };
            let _ = this.update(cx, |data, cx| {
                if generation == data.generation {
                    let page_error = if matches!(read, Read::Refresh(_)) && data.posting() {
                        None
                    } else {
                        page.and_then(|result| match result {
                            Ok((page, transcript)) => data.keep_page(read, page, transcript).err(),
                            Err(error) => Some(error),
                        })
                    };
                    data.error = page_error.or(error);
                }
                data.in_flight = None;
                data.read_next(cx);
                cx.notify();
            });
        });
        self.in_flight = Some((read, generation, task));
        cx.notify();
    }

    fn keep_rooms(&mut self, rooms: Vec<ChatRoom>) {
        self.rooms = rooms;
        if self.room().is_some() {
            return;
        }
        let selected = self
            .rooms
            .iter()
            .find(|room| matches!(room.kind, ChatRoomKind::Pinned))
            .or(self.rooms.first())
            .map(|room| room.id.clone());
        if selected != self.selected {
            self.selected = selected;
            self.clear_window();
        }
    }

    fn page_task(&self, read: Read, cx: &Context<Self>) -> Option<Task<Result<RoomPage, String>>> {
        if matches!(read, Read::Refresh(_)) && self.posting() {
            return None;
        }
        let room = self.room()?;
        let id = room.id.clone();
        let kind = room.kind.clone();
        let (before, limit) = match read {
            Read::Refresh(_) => (
                self.window.as_ref().and_then(|window| window.end),
                self.window
                    .as_ref()
                    .map_or(PAGE, |window| window.items.len().max(PAGE)),
            ),
            Read::Older(before) => (Some(before), PAGE),
        };
        Some(
            self.graph
                .read(cx)
                .call(cx, move |base| read_page(base, &id, &kind, before, limit)),
        )
    }

    fn keep_page(
        &mut self,
        read: Read,
        page: ChatPage,
        transcript: Option<ChatTranscript>,
    ) -> Result<(), String> {
        let window = self.window.get_or_insert_with(RoomWindow::default);
        match read {
            Read::Refresh(_) => {
                window.replace(page, transcript);
                if window.end.is_none()
                    && let Some(room) = self.selected.as_ref()
                    && let Some(echoes) = self.echoes.get_mut(room)
                {
                    echoes.retain(|_, echo| echo.state != SendState::Sent);
                    if echoes.is_empty() {
                        self.echoes.remove(room);
                    }
                }
            }
            Read::Older(before) => {
                window.prepend(page, before, transcript)?;
            }
        }
        self.sync_items();
        Ok(())
    }

    pub fn create(
        &self,
        req: SaveChatRoom,
        cx: &mut Context<Self>,
    ) -> Task<Result<ChatRoom, String>> {
        self.graph
            .read(cx)
            .call(cx, move |base| send_json(base, ROOMS, Method::POST, &req))
    }

    pub(super) fn echo(
        &mut self,
        room: &str,
        request: &SendChat,
        text: String,
        cx: &mut Context<Self>,
    ) -> i64 {
        let id = self.next_draft;
        self.next_draft += 1;
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_millis() as i64);
        let at_ms = self
            .items
            .last()
            .map_or(at_ms, |last| at_ms.max(last.cursor.at_ms.saturating_add(1)));
        let item = ChatItem {
            cursor: ChatCursor {
                at_ms,
                source: ChatSource::Message,
                id,
                part: 0,
            },
            entry: ChatEntry::Message {
                message: ChatMessage {
                    id: 0,
                    group: None,
                    from: Who {
                        id: None,
                        name: "human".into(),
                    },
                    origin: ChatOrigin::Human,
                    text,
                    blocks: None,
                    mode: Some(request.mode),
                    what: Some(MessageKind::Note),
                    reply_to: None,
                    room: request.room.clone(),
                    receipts: Vec::new(),
                    outside_read: false,
                    editable: false,
                },
            },
        };
        self.echoes.entry(room.to_owned()).or_default().insert(
            id,
            Echo {
                item: Arc::new(item),
                state: SendState::Uploading,
            },
        );
        self.sync_items();
        cx.notify();
        id
    }

    pub(super) fn send(
        &mut self,
        room: &str,
        id: i64,
        req: SendChat,
        cx: &mut Context<Self>,
    ) -> Task<Result<ChatMessage, String>> {
        if let Some(echo) = self
            .echoes
            .get_mut(room)
            .and_then(|echoes| echoes.get_mut(&id))
        {
            echo.state = SendState::Posting;
        }
        self.graph.read(cx).call(cx, move |base| {
            send_json(base, MESSAGES, Method::POST, &req)
        })
    }

    pub(super) fn finish_send(
        &mut self,
        room: &str,
        id: i64,
        result: Result<ChatMessage, String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let result = match result {
            Ok(mut message) => {
                if let Some(echo) = self
                    .echoes
                    .get_mut(room)
                    .and_then(|echoes| echoes.get_mut(&id))
                {
                    let cursor = ChatCursor {
                        id: i64::from(message.id),
                        ..echo.item.cursor
                    };
                    message.editable = false;
                    echo.item = Arc::new(ChatItem {
                        cursor,
                        entry: ChatEntry::Message { message },
                    });
                    echo.state = SendState::Sent;
                }
                Ok(())
            }
            Err(error) => {
                if let Some(echoes) = self.echoes.get_mut(room) {
                    echoes.remove(&id);
                    if echoes.is_empty() {
                        self.echoes.remove(room);
                    }
                }
                Err(error)
            }
        };
        if self.selected() == Some(room) {
            if result.is_ok()
                && let Some(window) = &mut self.window
            {
                window.end = None;
            }
            if self
                .in_flight
                .as_ref()
                .is_some_and(|(read, _, _)| matches!(read, Read::Refresh(_)))
            {
                self.in_flight = None;
            }
            self.refresh_pending = Some(Refresh::Immediate);
            self.sync_items();
            self.read_next(cx);
        }
        result
    }

    pub fn edit(
        &self,
        id: u32,
        text: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<ChatMessage, String>> {
        self.graph.read(cx).call(cx, move |base| {
            send_json(
                base,
                &format!("{MESSAGES}/{id}"),
                Method::PATCH,
                &EditChat { text },
            )
        })
    }

    pub fn withdraw(&self, id: u32, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.graph.read(cx).call(cx, move |base| {
            link::delete(base, &format!("{MESSAGES}/{id}"))
        })
    }
}

fn read_page(
    base: &str,
    room: &str,
    kind: &ChatRoomKind,
    before: Option<ChatCursor>,
    limit: usize,
) -> Result<RoomPage, Failure> {
    let before = before
        .map(|cursor| serde_json::to_string(&cursor))
        .transpose()
        .map_err(|error| Failure::Other(error.to_string()))?;
    let page = link::query(&[
        ("limit", Some(&limit.to_string())),
        ("before", before.as_deref()),
    ]);
    let room = link::escape(room);
    if matches!(kind, ChatRoomKind::Agent { .. }) {
        let session: ChatSession = link::read(base, &format!("{ROOMS}/{room}/session{page}"))?;
        Ok((session.page, Some(session.transcript)))
    } else {
        let page = link::read(base, &format!("{ROOMS}/{room}/stream{page}"))?;
        Ok((page, None))
    }
}

fn send_json<T: serde::de::DeserializeOwned>(
    base: &str,
    path: &str,
    method: Method,
    req: &impl serde::Serialize,
) -> Result<T, Failure> {
    let body = serde_json::to_string(req).map_err(|error| Failure::Other(error.to_string()))?;
    link::send_json(base, path, method, Some(body))
}
