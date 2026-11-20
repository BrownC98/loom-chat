mod alerts;
mod attachments;
mod controls;
mod picker;
mod targets;

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use api::graph::chat::{ChatEntry, ChatItem, ChatRoomKind, ChatTranscriptItem, SendChat};
use api::graph::messages::Kind;
use api::ids::{NodeId, WaitId};
use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Pixels, Render, Styled, Subscription, Window, canvas, div,
};

use self::alerts::Alerts;
use self::picker::Picker;
use self::targets::{Catalog, Plan, Target};
use super::data::ChatData;
use super::words as local;
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::field::{self, Field, FieldEvent};
use crate::views::pictures::{Pictures, PicturesEvent};
use crate::views::viewer::Viewer;
use crate::views::{status, ui};

struct Draft {
    field: Entity<Field>,
    pictures: Entity<Pictures>,
    quote: Option<Quote>,
    _pictures: [Subscription; 2],
    selected: Option<Vec<Target>>,
    mode: Kind,
    follow_up: Option<(NodeId, WaitId)>,
    last_send: Option<i64>,
    recoverable: VecDeque<SentDraft>,
    error: Option<DraftError>,
}

enum DraftError {
    Input(String),
    Send(String),
}

impl DraftError {
    fn text(&self) -> &str {
        match self {
            Self::Input(text) | Self::Send(text) => text,
        }
    }
}

struct SentDraft {
    text: String,
    pictures: Entity<Pictures>,
    quote: Option<Quote>,
    selected: Option<Vec<Target>>,
    mode: Kind,
    follow_up: Option<(NodeId, WaitId)>,
}

struct Quote {
    name: String,
    text: String,
}

impl Quote {
    fn label(&self, cx: &App) -> String {
        let line = self
            .text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("");
        match cx.global::<Lang>() {
            Lang::Ko => format!("{}의 말 · {line}", self.name),
            Lang::En => format!("From {} · {line}", self.name),
        }
    }
}

impl Draft {
    fn replace(
        &mut self,
        next: SentDraft,
        subscriptions: [Subscription; 2],
        cx: &mut App,
    ) -> SentDraft {
        let previous = SentDraft {
            text: self.field.read(cx).text().to_owned(),
            pictures: std::mem::replace(&mut self.pictures, next.pictures),
            quote: std::mem::replace(&mut self.quote, next.quote),
            selected: std::mem::replace(&mut self.selected, next.selected),
            mode: std::mem::replace(&mut self.mode, next.mode),
            follow_up: std::mem::replace(&mut self.follow_up, next.follow_up),
        };
        self._pictures = subscriptions;
        self.pictures
            .update(cx, |pictures, cx| pictures.set_locked(false, cx));
        self.field.update(cx, |field, cx| {
            let end = next.text.len();
            field.reset(&next.text, end..end, cx);
        });
        previous
    }

    fn selected(&self) -> &[Target] {
        self.selected.as_deref().unwrap_or_default()
    }

    fn has_content(&self, cx: &App) -> bool {
        self.quote.is_some()
            || !self.field.read(cx).is_blank()
            || !self.pictures.read(cx).is_empty()
    }

    fn text(&self, cx: &App) -> String {
        let text = self.field.read(cx).text();
        let Some(quote) = &self.quote else {
            return text.to_owned();
        };
        let mut body = match cx.global::<Lang>() {
            Lang::Ko => format!("{}의 말:", quote.name),
            Lang::En => format!("From {}:", quote.name),
        };
        for line in quote.text.lines() {
            body.push_str("\n> ");
            body.push_str(line);
        }
        if !text.trim().is_empty() {
            body.push_str("\n\n");
            body.push_str(text);
        }
        body
    }

    fn mode(&self, plan: &Plan) -> Kind {
        if !plan.working || plan.queue_only {
            Kind::Queue
        } else {
            self.mode
        }
    }
}

#[derive(PartialEq, Eq)]
struct Impact {
    node: NodeId,
    name: String,
    status: String,
    task: Option<String>,
    since: i64,
}

#[derive(PartialEq, Eq)]
enum SendIntent {
    Request,
    ConfirmInterrupt,
}

pub struct Composer {
    data: Entity<ChatData>,
    drafts: HashMap<String, Draft>,
    active: Option<String>,
    picker: Picker,
    alerts: Alerts,
    viewer: Entity<Viewer>,
    room_waits: Vec<WaitId>,
    trigger: FocusHandle,
    cancel: FocusHandle,
    mode_trigger: FocusHandle,
    mode_options: [FocusHandle; 3],
    mode_menu: bool,
    mode_bounds: Rc<Cell<Bounds<Pixels>>>,
    review: Option<Vec<Impact>>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    _subscriptions: [Subscription; 2],
}

impl Composer {
    pub fn new(data: Entity<ChatData>, cx: &mut Context<Self>) -> Self {
        let graph = data.read(cx).graph().clone();
        let alerts = Alerts::new(graph, cx);
        let viewer = cx.new(Viewer::new);
        let subscriptions = [
            cx.observe(&data, |_, _, cx| cx.notify()),
            cx.observe(&viewer, |_, _, cx| cx.notify()),
        ];
        Self {
            data,
            drafts: HashMap::new(),
            active: None,
            picker: Picker::new(cx),
            alerts,
            viewer,
            room_waits: Vec::new(),
            trigger: cx.focus_handle().tab_stop(true),
            cancel: cx.focus_handle().tab_stop(true),
            mode_trigger: cx.focus_handle().tab_stop(true),
            mode_options: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            mode_menu: false,
            mode_bounds: Rc::new(Cell::new(Bounds::default())),
            review: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            _subscriptions: subscriptions,
        }
    }

    fn sync_room(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let data = self.data.read(cx);
        let id = data.room().map(|room| room.id.clone());
        let changed = id != self.active;
        let focus_moved = changed
            && self
                .draft()
                .is_some_and(|draft| draft.field.read(cx).focused(window));
        if changed {
            self.picker.dismiss();
            self.mode_menu = false;
            self.review = None;
            self.active = id;
        }
        let Some(id) = self.active.clone() else {
            return;
        };
        let composer = cx.weak_entity();
        self.drafts.entry(id.clone()).or_insert_with(|| {
            let pictures = cx.new(|_| Pictures::draft());
            let field = field::boxed("", false, true, cx);
            let subscriptions = Self::observe_pictures(&id, &pictures, cx);
            field.update(cx, |field, _| {
                field.on_files(move |entries, _, cx| {
                    let pictures = composer
                        .upgrade()
                        .and_then(|this| Some(this.read(cx).drafts.get(&id)?.pictures.clone()));
                    if let Some(pictures) = pictures {
                        pictures.update(cx, |pictures, cx| pictures.take_clipboard(entries, cx));
                    }
                });
            });
            cx.subscribe_in(&field, window, |this, field, event, window, cx| {
                if this.draft().is_some_and(|draft| draft.field == *field) {
                    match event {
                        FieldEvent::Enter => this.submit(SendIntent::Request, window, cx),
                        FieldEvent::Changed => {
                            if let Some(draft) = this.draft_mut() {
                                draft.error = None;
                            }
                            this.review = None;
                            if !this.picker.open
                                && field.read(cx).focused(window)
                                && let Some(at) = field.read(cx).mention_start()
                            {
                                this.open_mention(at, window, cx);
                            }
                            cx.notify();
                        }
                        FieldEvent::Escape => this.cancel_review(window, cx),
                        FieldEvent::Step(_) => {}
                    }
                }
            })
            .detach();
            Draft {
                field,
                pictures,
                quote: None,
                _pictures: subscriptions,
                selected: None,
                mode: Kind::Queue,
                follow_up: None,
                last_send: None,
                recoverable: VecDeque::new(),
                error: None,
            }
        });
        let data = self.data.read(cx);
        let draft = self
            .drafts
            .get_mut(self.active.as_ref().expect("active room"))
            .expect("active draft");
        if draft.selected.is_none()
            && let Some((room, recipients)) = data.room().zip(data.recipients())
            && !room.archived
            && (matches!(room.kind, ChatRoomKind::Pinned)
                || recipients.rooms.iter().any(|entry| entry.id == room.id))
        {
            draft.selected = Some(
                Catalog::new(recipients)
                    .default_target(room)
                    .into_iter()
                    .collect(),
            );
        }
        let should_focus = focus_moved && data.room().is_some_and(|room| !room.archived);
        let working = data
            .recipients()
            .is_some_and(|recipients| Catalog::new(recipients).plan(draft.selected()).working);
        if !working {
            self.mode_menu = false;
            self.review = None;
            draft.mode = Kind::Queue;
        }
        let inset = if working {
            controls::mode_inset(cx)
        } else {
            0.0
        };
        draft.field.update(cx, |field, cx| {
            field.set_inset(controls::ATTACH_INSET, inset, cx);
        });
        if should_focus {
            self.focus_input(window, cx);
        }
    }

    fn observe_pictures(
        room: &str,
        pictures: &Entity<Pictures>,
        cx: &mut Context<Self>,
    ) -> [Subscription; 2] {
        let edited_room = room.to_owned();
        let error_room = room.to_owned();
        let observe = cx.observe(pictures, move |this, source, cx| {
            if this.active.as_ref() == Some(&edited_room)
                && this.draft().is_some_and(|draft| *draft.pictures == *source)
            {
                this.review = None;
                cx.notify();
            }
        });
        let errors = cx.subscribe(pictures, move |this, source, event, cx| {
            if let PicturesEvent::Refused(error) = event
                && let Some(draft) = this.drafts.get_mut(&error_room)
                && *draft.pictures == *source
            {
                draft.error = Some(DraftError::Input(error.clone()));
                cx.notify();
            }
        });
        [observe, errors]
    }

    pub(super) fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_room(window, cx);
        if self.writable(cx) {
            self.focus_input(window, cx);
        }
    }

    pub(super) fn set_visible_waits(&mut self, ids: &[WaitId], cx: &mut Context<Self>) {
        if self.room_waits != ids {
            self.room_waits.clear();
            self.room_waits.extend_from_slice(ids);
            cx.notify();
        }
    }

    fn draft(&self) -> Option<&Draft> {
        self.drafts.get(self.active.as_ref()?)
    }

    fn draft_mut(&mut self) -> Option<&mut Draft> {
        self.drafts.get_mut(self.active.as_ref()?)
    }

    fn writable(&self, cx: &App) -> bool {
        self.data
            .read(cx)
            .room()
            .is_some_and(|room| !room.archived && self.active.as_ref() == Some(&room.id))
            && self.draft().is_some()
    }

    fn focus_input(&self, window: &mut Window, cx: &mut App) {
        if let Some(draft) = self.draft() {
            let focus = draft.field.read(cx).handle();
            window.focus(&focus, cx);
        }
    }

    pub fn follow_up(
        &mut self,
        node: NodeId,
        wait: WaitId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_room(window, cx);
        if !self.writable(cx) {
            return;
        }
        self.picker.dismiss();
        self.replace_targets(vec![Target::Node(node)], cx);
        if let Some(draft) = self.draft_mut() {
            draft.follow_up = Some((node, wait));
            draft.mode = Kind::Queue;
        }
        self.focus_input(window, cx);
    }

    pub(super) fn quote(&mut self, item: &ChatItem, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_room(window, cx);
        if !self.writable(cx) {
            return;
        }
        let data = self.data.read(cx);
        let room = data.room().expect("writable room");
        let (name, text) = match &item.entry {
            ChatEntry::Message { message } => (
                message.from.name.as_str(),
                message.origin.visible_text(&message.text),
            ),
            ChatEntry::Transcript {
                item: ChatTranscriptItem::User { text, .. },
                ..
            } => (local(cx, "나", "Me"), text.as_str()),
            ChatEntry::Transcript {
                item: ChatTranscriptItem::Assistant { text, .. },
                ..
            } => (room.name.as_str(), text.as_str()),
            _ => return,
        };
        let quote = Quote {
            name: name.to_owned(),
            text: text.to_owned(),
        };
        if quote.text.trim().is_empty() {
            return;
        }
        if let Some(draft) = self.draft_mut() {
            draft.quote = Some(quote);
            draft.follow_up = None;
        }
        self.focus_input(window, cx);
        cx.notify();
    }

    fn replace_targets(&mut self, selected: Vec<Target>, cx: &mut Context<Self>) {
        if !self.writable(cx) {
            return;
        }
        let queue_only = self
            .data
            .read(cx)
            .recipients()
            .is_some_and(|data| Catalog::new(data).plan(&selected).queue_only);
        if let Some(draft) = self.draft_mut() {
            draft.selected = Some(selected);
            draft.error = None;
            if queue_only {
                draft.mode = Kind::Queue;
            }
        }
        self.review = None;
        cx.notify();
    }

    fn toggle(&mut self, target: Target, cx: &mut Context<Self>) {
        let Some(draft) = self.draft() else { return };
        let mut selected = draft.selected().to_vec();
        if let Some(index) = selected.iter().position(|value| value == &target) {
            selected.remove(index);
        } else {
            selected.push(target);
        }
        self.replace_targets(selected, cx);
    }

    fn impacts(&self, catalog: &Catalog, plan: &Plan, cx: &App) -> Vec<Impact> {
        let store = self.data.read(cx).graph().read(cx);
        let nodes: HashMap<_, _> = store
            .snapshot()
            .into_iter()
            .flat_map(|snapshot| &snapshot.nodes)
            .map(|node| (node.id, node))
            .collect();
        plan.nodes
            .iter()
            .filter_map(|&id| {
                let agent = catalog.node(NodeId(id))?;
                let node = nodes.get(&agent.node).copied();
                let since = node
                    .filter(|node| node.status == agent.status)
                    .map_or(0, |node| node.since);
                Some(Impact {
                    node: agent.node,
                    name: agent.name.clone(),
                    status: state_label(&agent.status, cx),
                    task: agent.task.clone(),
                    since,
                })
            })
            .collect()
    }

    fn cancel_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review.take().is_some() {
            self.focus_input(window, cx);
            cx.notify();
        }
    }

    fn set_error(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft_mut() {
            draft.error = Some(DraftError::Input(text));
            cx.notify();
        }
    }

    fn submit(&mut self, intent: SendIntent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.writable(cx) || self.picker.open {
            return;
        }
        let Some(draft) = self.draft() else { return };
        let field = draft.field.read(cx);
        if field.composing() || !draft.has_content(cx) || draft.pictures.read(cx).busy() {
            return;
        }
        let data = self.data.read(cx);
        let Some((room, recipients)) = data.room().zip(data.recipients()) else {
            self.open_picker(window, cx);
            return;
        };
        let catalog = Catalog::new(recipients);
        let plan = catalog.plan(draft.selected());
        if plan.count() == 0 {
            self.open_picker(window, cx);
            return;
        }
        if plan.unavailable {
            self.set_error(unavailable_recipient(cx).into(), cx);
            return;
        }
        let mode = draft.mode(&plan);
        if intent == SendIntent::ConfirmInterrupt && mode != Kind::Interrupt {
            self.cancel_review(window, cx);
            return;
        }
        if mode == Kind::Interrupt {
            let impacts = self.impacts(&catalog, &plan, cx);
            if intent == SendIntent::Request || self.review.as_ref() != Some(&impacts) {
                self.review = Some(impacts);
                window.focus(&self.cancel, cx);
                cx.notify();
                return;
            }
        }
        let files = draft.pictures.read(cx).attachments();
        if !files.is_empty() && plan.outside_rooms > 0 {
            self.set_error(
                local(
                    cx,
                    "첨부 파일을 받을 에이전트를 고릅니다.",
                    "Select agents to receive the attachments.",
                )
                .into(),
                cx,
            );
            return;
        }
        let mut request = plan.request(room, draft.text(cx), mode);
        let recipients = if files.is_empty() {
            Vec::new()
        } else {
            plan.nodes
                .iter()
                .filter_map(|&id| {
                    let agent = catalog.node(NodeId(id))?;
                    Some((agent.node, agent.name.clone()))
                })
                .collect()
        };
        if !files.is_empty() {
            if recipients.len() != plan.nodes.len() {
                self.set_error(
                    local(
                        cx,
                        "첨부 파일을 받을 사람을 다시 확인해 주세요.",
                        "Check who will receive the attachments.",
                    )
                    .into(),
                    cx,
                );
                return;
            }
            request.attachment_nodes = Some(recipients.iter().map(|(id, _)| *id).collect());
        }
        let follow_up = draft
            .follow_up
            .filter(|(node, _)| plan.nodes.contains(&node.0));
        let room = room.id.clone();
        self.send(
            room,
            request,
            follow_up.map(|(_, wait)| wait),
            attachments::Delivery { files, recipients },
            window,
            cx,
        );
    }

    fn send(
        &mut self,
        room: String,
        mut req: SendChat,
        wait: Option<WaitId>,
        attachments: attachments::Delivery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let data = self.data.clone();
        let graph = data.read(cx).graph().clone();
        let echo_text = attachments.preview(&req.text, cx);
        let pictures = cx.new(|_| Pictures::draft());
        let subscriptions = Self::observe_pictures(&room, &pictures, cx);
        let id = data.update(cx, |data, cx| data.echo(&room, &req, echo_text, cx));
        let draft = self.drafts.get_mut(&room).expect("sending draft");
        let empty = SentDraft {
            text: String::new(),
            pictures,
            quote: None,
            selected: draft.selected.clone(),
            mode: Kind::Queue,
            follow_up: None,
        };
        let saved = draft.replace(empty, subscriptions, cx);
        saved
            .pictures
            .update(cx, |pictures, cx| pictures.set_locked(true, cx));
        draft.last_send = Some(id);
        draft.error = None;
        self.review = None;
        self.mode_menu = false;
        self.focus_input(window, cx);
        let upload = attachments.upload(&graph, cx);
        cx.spawn(async move |this, cx| {
            let result = match upload.await {
                Ok(suffix) => {
                    req.text.push_str(&suffix);
                    let send = data.update(cx, |data, cx| data.send(&room, id, req, cx));
                    send.await
                }
                Err(error) => Err(error),
            };
            let result = data.update(cx, |data, cx| data.finish_send(&room, id, result, cx));
            if let Err(error) = result {
                let _ = this.update(cx, |this, cx| {
                    this.restore_draft(&room, id, saved, error, cx)
                });
                return;
            }
            saved.pictures.update(cx, |pictures, cx| pictures.clear(cx));
            if let Some(wait) = wait {
                let dismissed = graph
                    .update(cx, |store, cx| store.dismiss_wait(wait, cx))
                    .await;
                if let Err(error) = dismissed {
                    graph.update(cx, |store, cx| {
                        let message = local(
                            cx,
                            "말을 보냈지만 완료를 확인하지 못했습니다.",
                            "Message sent. Could not confirm the completed work.",
                        );
                        store.tell(format!("{message} {error}"), cx);
                    });
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn restore_draft(
        &mut self,
        room: &str,
        id: i64,
        saved: SentDraft,
        error: String,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.drafts.get_mut(room) else {
            return;
        };
        if draft.last_send == Some(id)
            && draft.field.read(cx).text().is_empty()
            && !draft.field.read(cx).composing()
            && draft.quote.is_none()
            && draft.pictures.read(cx).is_empty()
            && !draft.pictures.read(cx).busy()
            && draft.selected == saved.selected
            && draft.mode == Kind::Queue
            && draft.follow_up.is_none()
        {
            let subscriptions = Self::observe_pictures(room, &saved.pictures, cx);
            draft.replace(saved, subscriptions, cx);
            draft.last_send = None;
            draft.error = Some(DraftError::Send(error));
        } else {
            draft.recoverable.push_back(saved);
            draft.error = Some(DraftError::Input(error));
        }
        cx.notify();
    }

    fn recover_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(room) = self.active.clone() else {
            return;
        };
        let Some(draft) = self.drafts.get_mut(&room) else {
            return;
        };
        if draft.field.read(cx).composing() {
            return;
        }
        let Some(saved) = draft.recoverable.pop_front() else {
            return;
        };
        let keep = !draft.field.read(cx).text().is_empty()
            || draft.has_content(cx)
            || draft.pictures.read(cx).busy()
            || draft.follow_up.is_some();
        let subscriptions = Self::observe_pictures(&room, &saved.pictures, cx);
        let previous = draft.replace(saved, subscriptions, cx);
        if keep {
            draft.recoverable.push_back(previous);
        }
        draft.last_send = None;
        draft.error = None;
        self.focus_input(window, cx);
        cx.notify();
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_room(window, cx);
        let alert = self.alert_view(window, cx);
        let bounds = self.bounds.clone();
        let colors = ui::colors(cx);
        let panel = div()
            .capture_key_down(cx.listener(Self::alert_key))
            .on_key_down(tab)
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(12.0))
            .border_t_1()
            .border_color(colors.line)
            .bg(colors.panel)
            .text_size(px(ui::TEXT))
            .text_color(colors.ink)
            .child(
                canvas(
                    move |bounds_value, _, _| bounds.set(bounds_value),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(alert);
        let data = self.data.read(cx);
        let Some((room, draft)) = data.room().zip(self.draft()) else {
            return panel;
        };
        if room.archived {
            return panel.child(div().text_color(colors.ink_2).child(local(
                cx,
                "보관된 방입니다. 새 말을 보낼 수 없습니다.",
                "This room is archived. You cannot send new messages.",
            )));
        }
        let catalog = data.recipients().map(Catalog::new);
        let plan = catalog
            .as_ref()
            .map_or_else(Plan::default, |catalog| catalog.plan(draft.selected()));
        let body = self.controls(room, draft, catalog.as_ref(), &plan, cx);
        let picker = self
            .picker
            .open
            .then(|| self.picker_view(catalog.as_ref(), window, cx));
        panel
            .child(body)
            .children(picker)
            .children(self.viewer.read(cx).is_open().then(|| self.viewer.clone()))
    }
}

fn state_label(state: &api::graph::Status, cx: &App) -> String {
    status::chip(state, *cx.global::<Lang>()).map_or_else(
        || ui::say(cx, Str::NodeStatusChipAnswer).to_owned(),
        |chip| chip.text,
    )
}

fn mode_label(mode: Kind, cx: &App) -> &'static str {
    match mode {
        Kind::Queue => local(cx, "작업 후 전달", "After work"),
        Kind::Steer => local(cx, "진행 중 반영", "During work"),
        Kind::Interrupt => local(cx, "멈추고 전달", "Stop and send"),
    }
}

fn unavailable_recipient(cx: &App) -> &'static str {
    local(
        cx,
        "보낼 수 없는 받는 이를 목록에서 빼 주세요.",
        "Remove recipients who cannot receive this message.",
    )
}

fn tab(event: &KeyDownEvent, window: &mut Window, cx: &mut App) {
    let stroke = &event.keystroke;
    let modifiers = stroke.modifiers;
    if stroke.key != "tab" || modifiers.alt || modifiers.control || modifiers.platform {
        return;
    }
    if modifiers.shift {
        window.focus_prev(cx);
    } else {
        window.focus_next(cx);
    }
    window.prevent_default();
    cx.stop_propagation();
}
