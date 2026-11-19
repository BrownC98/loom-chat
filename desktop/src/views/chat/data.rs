use std::sync::Arc;
use std::time::Duration;

use api::graph::GraphEvent;
use api::graph::chat::{
    ChatCursor, ChatItem, ChatMessage, ChatPage, ChatRecipients, ChatRoom, ChatRoomKind,
    ChatSession, ChatTranscript, EditChat, SaveChatRoom, SendChat,
};
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
enum Read {
    Refresh,
    Older(ChatCursor),
}

type RoomPage = (ChatPage, Option<ChatTranscript>);

#[derive(Default)]
struct RoomWindow {
    items: Vec<Arc<ChatItem>>,
    before: Option<ChatCursor>,
    end: Option<ChatCursor>,
    transcript: Option<ChatTranscript>,
}

impl RoomWindow {
    fn replace(&mut self, page: ChatPage, transcript: Option<ChatTranscript>) -> bool {
        let mut changed = self.items.len() != page.items.len();
        let mut old = Vec::new();
        let mut new = Vec::new();
        let items = page
            .items
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
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
                    changed |= self
                        .items
                        .get(index)
                        .is_none_or(|item| item.cursor != cursor);
                    return previous.clone();
                }
                changed = true;
                Arc::new(item)
            })
            .collect();
        self.items = items;
        self.before = page.before;
        self.transcript = transcript;
        changed
    }

    fn prepend(
        &mut self,
        page: ChatPage,
        before: ChatCursor,
        transcript: Option<ChatTranscript>,
    ) -> Result<bool, String> {
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
            return Ok(false);
        }
        older.append(&mut self.items);
        if let Some(first_removed) = older.get(WINDOW) {
            self.end = Some(first_removed.cursor);
            older.truncate(WINDOW);
        }
        self.items = older;
        Ok(true)
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
    stream_revision: u64,
    in_flight: Option<(Read, u64, Task<()>)>,
    refresh_pending: bool,
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
            stream_revision: 0,
            in_flight: None,
            refresh_pending: false,
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
        self.window
            .as_ref()
            .map_or(&[], |window| window.items.as_slice())
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
        self.window.is_none() && (self.in_flight.is_some() || self.refresh_pending)
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
        self.refresh_pending = true;
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
    }

    fn read_next(&mut self, cx: &mut Context<Self>) {
        if self.in_flight.is_some() {
            return;
        }
        let read = std::mem::take(&mut self.older_pending)
            .then(|| self.before().map(Read::Older))
            .flatten()
            .or(self.refresh_pending.then_some(Read::Refresh));
        let Some(read) = read else { return };
        let generation = self.generation;
        let task = cx.spawn(async move |this, cx| {
            let mut catalog = None;
            if matches!(read, Read::Refresh) {
                cx.background_executor().timer(COALESCE).await;
                let tasks = this.update(cx, |data, cx| {
                    if generation != data.generation {
                        return None;
                    }
                    data.refresh_pending = false;
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
                    let page_error = page.and_then(|result| match result {
                        Ok((page, transcript)) => data.keep_page(read, page, transcript).err(),
                        Err(error) => Some(error),
                    });
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
        let room = self.room()?;
        let id = room.id.clone();
        let kind = room.kind.clone();
        let (before, limit) = match read {
            Read::Refresh => (
                self.window.as_ref().and_then(|window| window.end),
                self.items().len().max(PAGE),
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
        let changed = match read {
            Read::Refresh => window.replace(page, transcript),
            Read::Older(before) => window.prepend(page, before, transcript)?,
        };
        if changed {
            self.stream_revision += 1;
        }
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

    pub fn send(&self, req: SendChat, cx: &mut Context<Self>) -> Task<Result<ChatMessage, String>> {
        self.graph.read(cx).call(cx, move |base| {
            send_json(base, MESSAGES, Method::POST, &req)
        })
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
