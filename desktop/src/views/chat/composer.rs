mod alerts;
mod attachments;
mod controls;
mod picker;
mod targets;

use std::cell::Cell;
use std::collections::HashMap;
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
use crate::scale::px;
use crate::strings::Lang;
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
    sending: bool,
    error: Option<String>,
}

struct Quote {
    name: String,
    text: String,
}

impl Quote {
    fn label(&self) -> String {
        let line = self
            .text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("");
        format!("{}의 말 · {line}", self.name)
    }
}

impl Draft {
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
        let mut body = format!("{}의 말:", quote.name);
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
        if plan.queue_only {
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
    trigger: FocusHandle,
    cancel: FocusHandle,
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
            trigger: cx.focus_handle().tab_stop(true),
            cancel: cx.focus_handle().tab_stop(true),
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
            self.picker.open = false;
            self.review = None;
            self.active = id;
        }
        let Some(id) = self.active.clone() else {
            return;
        };
        self.drafts.entry(id.clone()).or_insert_with(|| {
            let changed_room = id.clone();
            let pictures = cx.new(|_| Pictures::draft());
            let field = field::boxed("", false, true, cx);
            field.update(cx, |field, cx| {
                field.set_inset(controls::ATTACH_INSET, controls::MODES_INSET, cx);
                let pictures = pictures.clone();
                field.on_files(move |entries, _, cx| {
                    pictures.update(cx, |pictures, cx| pictures.take_clipboard(entries, cx));
                });
            });
            let observe = cx.observe(&pictures, move |this, _, cx| {
                if this.active.as_ref() == Some(&changed_room) {
                    this.review = None;
                    cx.notify();
                }
            });
            let errors = cx.subscribe(&pictures, move |this, _, event, cx| {
                if let PicturesEvent::Refused(error) = event
                    && let Some(draft) = this.drafts.get_mut(&id)
                {
                    draft.error = Some(error.clone());
                    cx.notify();
                }
            });
            cx.subscribe_in(&field, window, |this, field, event, window, cx| {
                if this.draft().is_some_and(|draft| draft.field == *field) {
                    match event {
                        FieldEvent::Enter => this.submit(SendIntent::Request, window, cx),
                        FieldEvent::Changed => {
                            this.review = None;
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
                _pictures: [observe, errors],
                selected: None,
                mode: Kind::Queue,
                follow_up: None,
                sending: false,
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
        if focus_moved && data.room().is_some_and(|room| !room.archived) {
            self.focus_input(window, cx);
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
            && self.draft().is_some_and(|draft| !draft.sending)
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
        self.picker.open = false;
        self.replace_targets(vec![Target::Node(node)], cx);
        if let Some(draft) = self.draft_mut() {
            draft.follow_up = Some((node, wait));
            draft.mode = Kind::Queue;
        }
        self.focus_input(window, cx);
    }

    #[expect(dead_code)]
    pub(super) fn quote(&mut self, item: &ChatItem, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_room(window, cx);
        if !self.writable(cx) {
            return;
        }
        let data = self.data.read(cx);
        let room = data.room().expect("writable room");
        let (name, text) = match &item.entry {
            ChatEntry::Message { message } => {
                (message.from.name.as_str(), super::message_text(message))
            }
            ChatEntry::Transcript {
                item: ChatTranscriptItem::User { text },
            } => ("나", text.as_str()),
            ChatEntry::Transcript {
                item: ChatTranscriptItem::Assistant { text },
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
        self.replace_targets(Vec::new(), cx);
        self.open_picker(window, cx);
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
            draft.error = Some(text);
            cx.notify();
        }
    }

    fn submit(&mut self, intent: SendIntent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.writable(cx) || self.picker.open {
            return;
        }
        let data = self.data.read(cx);
        let Some((room, recipients)) = data.room().zip(data.recipients()) else {
            return;
        };
        let Some(draft) = self.draft() else { return };
        let field = draft.field.read(cx);
        if field.composing() || !draft.has_content(cx) || draft.pictures.read(cx).busy() {
            return;
        }
        let catalog = Catalog::new(recipients);
        let plan = catalog.plan(draft.selected());
        if plan.unavailable {
            self.set_error("보낼 수 없는 받는 이를 목록에서 빼 주세요.".into(), cx);
            return;
        }
        if plan.count() == 0 {
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
            self.set_error("첨부 파일을 받을 에이전트를 고릅니다.".into(), cx);
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
                self.set_error("첨부 파일을 받을 사람을 다시 확인해 주세요.".into(), cx);
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
        let Some(draft) = self.draft_mut() else {
            return;
        };
        draft.sending = true;
        draft.error = None;
        draft
            .field
            .update(cx, |field, cx| field.set_locked(true, cx));
        draft
            .pictures
            .update(cx, |pictures, cx| pictures.set_locked(true, cx));
        self.review = None;
        self.focus_input(window, cx);
        let data = self.data.clone();
        let graph = data.read(cx).graph().clone();
        let upload = attachments.upload(&graph, cx);
        cx.spawn(async move |this, cx| {
            let result = match upload.await {
                Ok(suffix) => {
                    req.text.push_str(&suffix);
                    let send = data.update(cx, |data, cx| data.send(req, cx));
                    send.await
                }
                Err(error) => Err(error),
            };
            let sent = result.is_ok();
            let _ = this.update(cx, |this, cx| {
                if let Some(draft) = this.drafts.get_mut(&room) {
                    draft.sending = false;
                    draft.error = result.err();
                    draft.field.update(cx, |field, cx| {
                        field.set_locked(false, cx);
                        if sent {
                            field.clear(cx);
                        }
                    });
                    draft.pictures.update(cx, |pictures, cx| {
                        pictures.set_locked(false, cx);
                        if sent {
                            pictures.clear(cx);
                        }
                    });
                    if sent {
                        draft.quote = None;
                        draft.follow_up = None;
                    }
                    cx.notify();
                }
            });
            if !sent {
                data.update(cx, |data, cx| data.refresh(cx));
                return;
            }
            if let Some(wait) = wait {
                let dismissed = graph
                    .update(cx, |store, cx| store.dismiss_wait(wait, cx))
                    .await;
                if let Err(error) = dismissed {
                    graph.update(cx, |store, cx| {
                        store.tell(
                            format!("말을 보냈지만 완료를 확인하지 못했습니다. {error}"),
                            cx,
                        );
                    });
                }
            }
            data.update(cx, |data, cx| data.refresh(cx));
        })
        .detach();
        cx.notify();
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_room(window, cx);
        let alert = self.alert_view(cx);
        let bounds = self.bounds.clone();
        let colors = ui::colors(cx);
        let panel = div()
            .on_key_down(tab)
            .capture_key_down(cx.listener(Self::alert_key))
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
            return div();
        };
        if room.archived {
            return panel.child(
                div()
                    .text_color(colors.ink_2)
                    .child("보관된 방입니다. 새 말을 보낼 수 없습니다."),
            );
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
    status::chip(state, *cx.global::<Lang>())
        .map_or_else(|| "내 답을 기다림".to_owned(), |chip| chip.text)
}

fn mode_label(mode: Kind) -> &'static str {
    match mode {
        Kind::Queue => "일반",
        Kind::Steer => "조정",
        Kind::Interrupt => "중단",
    }
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
