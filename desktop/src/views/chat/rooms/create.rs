use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use api::graph::chat::{ChatRoom, ChatRoomKind, SaveChatRoom};
use api::ids::NodeId;
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, App, Context, Div, Entity, EventEmitter, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, anchored, deferred, div, point,
    uniform_list,
};

use crate::link;
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::field::{self, Field, FieldEvent};
use crate::views::icons::{Lucide, agent_mark, icon};
use crate::views::ui::{colors, say};
use crate::views::widgets::btn;

use super::{ChatData, note, words};

const PANEL_WIDTH: f32 = 380.0;
const PICKER_ROW: f32 = 31.0;
const PICKER_MAX_ROWS: usize = 6;

pub(super) struct Created(pub(super) String);

pub(super) struct CreateRoom {
    data: Entity<ChatData>,
    name: Entity<Field>,
    search: Entity<Field>,
    return_focus: Option<FocusHandle>,
    editing: Option<String>,
    field_keys: Vec<Subscription>,
    open: bool,
    saving: bool,
    selected: HashSet<NodeId>,
    error: Option<String>,
}

impl EventEmitter<Created> for CreateRoom {}

struct Choice {
    node: NodeId,
    name: SharedString,
    harness: Option<SharedString>,
}

enum Row {
    Work(SharedString),
    Choice(Choice),
}

impl CreateRoom {
    pub(super) fn new(data: Entity<ChatData>, cx: &mut Context<Self>) -> Self {
        let name = field::plain_label(Str::RoomName, cx);
        let search = field::plain_label(Str::RoomFindAgents, cx);
        cx.observe(&data, |this, _, cx| {
            if this.open {
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(&search, |_, _, event: &FieldEvent, cx| {
            if matches!(event, FieldEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        Self {
            data,
            name,
            search,
            return_focus: None,
            editing: None,
            field_keys: Vec::new(),
            open: false,
            saving: false,
            selected: HashSet::new(),
            error: None,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show(None, window, cx);
    }

    pub(super) fn edit_members(
        &mut self,
        room: &ChatRoom,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show(Some(room), window, cx);
    }

    fn show(&mut self, room: Option<&ChatRoom>, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            return;
        }
        self.open = true;
        self.return_focus = window.focused(cx);
        self.editing = room.map(|room| room.id.clone());
        self.name.update(cx, |field, cx| {
            field.set_text(room.map_or("", |room| room.name.as_str()), cx);
        });
        self.search.update(cx, |field, cx| field.set_text("", cx));
        self.selected = room
            .into_iter()
            .flat_map(|room| room.members.iter().copied())
            .collect();
        self.error = None;
        self.field_keys = vec![
            cx.subscribe_in(
                &self.name,
                window,
                |this, _, event: &FieldEvent, window, cx| match event {
                    FieldEvent::Enter => this.save(window, cx),
                    FieldEvent::Escape => this.close(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &self.search,
                window,
                |this, _, event: &FieldEvent, window, cx| {
                    if matches!(event, FieldEvent::Escape) {
                        this.close(window, cx);
                    }
                },
            ),
        ];
        let focus = if room.is_some() {
            &self.search
        } else {
            &self.name
        };
        window.focus(&focus.read(cx).handle(), cx);
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.saving {
            return;
        }
        self.open = false;
        if let Some(focus) = self.return_focus.take() {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.saving {
            return;
        }
        let editing = self.editing.clone();
        let current = editing.as_ref().and_then(|id| {
            self.data
                .read(cx)
                .rooms()
                .iter()
                .find(|room| room.id == *id)
        });
        let name = current.map_or_else(
            || self.name.read(cx).text().trim().to_owned(),
            |room| room.name.clone(),
        );
        if name.is_empty() {
            self.error = Some(words(cx, "방 이름을 입력해 주세요.", "Enter a room name.").into());
            window.focus(&self.name.read(cx).handle(), cx);
            cx.notify();
            return;
        }
        if self.selected.is_empty() {
            self.error = Some(
                words(
                    cx,
                    "구성원을 한 명 이상 골라 주세요.",
                    "Select at least one member.",
                )
                .into(),
            );
            window.focus(&self.search.read(cx).handle(), cx);
            cx.notify();
            return;
        }
        let mut members: Vec<_> = self.selected.iter().copied().collect();
        members.sort_unstable_by_key(|node| node.0);
        let request = SaveChatRoom { name, members };
        self.error = None;
        self.saving = true;
        for field in [&self.name, &self.search] {
            field.update(cx, |field, cx| field.set_locked(true, cx));
        }
        let task = if let Some(id) = &editing {
            let path = format!("/api/chat/rooms/{}", link::escape(id));
            self.data.read(cx).graph().read(cx).call(cx, move |base| {
                link::send_json::<ChatRoom>(
                    base,
                    &path,
                    ureq::http::Method::PUT,
                    Some(
                        serde_json::to_string(&request)
                            .map_err(|error| crate::problem::Failure::Other(error.to_string()))?,
                    ),
                )
            })
        } else {
            self.data.update(cx, |data, cx| data.create(request, cx))
        };
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                for field in [&this.name, &this.search] {
                    field.update(cx, |field, cx| field.set_locked(false, cx));
                }
                match result {
                    Ok(room) => {
                        this.close(window, cx);
                        if editing.is_some() {
                            this.data.update(cx, |data, cx| data.refresh(cx));
                        } else {
                            cx.emit(Created(room.id));
                        }
                    }
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn choices(&self, cx: &App) -> Vec<Row> {
        let data = self.data.read(cx);
        let query = self.search.read(cx).text().trim().to_lowercase();
        let works: HashMap<_, _> = data
            .rooms()
            .iter()
            .filter(|room| matches!(room.kind, ChatRoomKind::Work { .. }))
            .map(|room| (room.id.as_str(), room.name.as_str()))
            .collect();
        let mut groups = BTreeMap::new();
        let agents = data
            .recipients()
            .into_iter()
            .flat_map(|catalog| &catalog.agents);
        let available: HashSet<_> = agents.clone().map(|agent| agent.node).collect();
        for agent in agents {
            let work = works.get(agent.work.as_str()).copied().unwrap_or(words(
                cx,
                "작업 없음",
                "No work",
            ));
            if !agent.name.to_lowercase().contains(&query) && !work.to_lowercase().contains(&query)
            {
                continue;
            }
            groups
                .entry((work, agent.work.as_str()))
                .or_insert_with(Vec::new)
                .push(Row::Choice(Choice {
                    node: agent.node,
                    name: agent.name.clone().into(),
                    harness: agent.harness.as_ref().map(|id| id.0.clone().into()),
                }));
        }
        let former = words(cx, "이전 구성원", "Previous members");
        let original = self
            .editing
            .as_ref()
            .and_then(|id| data.rooms().iter().find(|room| room.id == *id));
        for room in data.rooms() {
            if let ChatRoomKind::Agent { node } = room.kind
                && original.is_some_and(|room| room.members.contains(&node))
                && !available.contains(&node)
                && room.name.to_lowercase().contains(&query)
            {
                groups
                    .entry((former, ""))
                    .or_default()
                    .push(Row::Choice(Choice {
                        node,
                        name: room.name.clone().into(),
                        harness: None,
                    }));
            }
        }
        groups
            .into_iter()
            .flat_map(|((work, _), agents)| {
                std::iter::once(Row::Work(work.to_owned().into())).chain(agents)
            })
            .collect()
    }

    fn picker(&self, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        let rows = self.choices(cx);
        let count = rows.len();
        let picker = div().border_1().border_color(c.line).rounded(px(6.0));
        if count == 0 {
            let data = self.data.read(cx);
            let label = if data.recipients().is_none() {
                if data.loading() {
                    words(cx, "구성원을 불러옵니다.", "Loading members…")
                } else {
                    words(
                        cx,
                        "구성원을 불러오지 못했습니다.",
                        "Could not load members.",
                    )
                }
            } else if self.search.read(cx).is_blank() {
                words(
                    cx,
                    "고를 수 있는 구성원이 없습니다.",
                    "There are no members to select.",
                )
            } else {
                words(
                    cx,
                    "검색에 맞는 구성원이 없습니다.",
                    "No members match your search.",
                )
            };
            return picker.child(note(label, cx)).when(
                data.recipients().is_none() && !data.loading(),
                |picker| {
                    picker.child(
                        div().p(px(6.0)).child(
                            btn::button("chat-create-retry-members", cx)
                                .child(say(cx, Str::StatusBarRefresh))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.data.update(cx, |data, cx| data.refresh(cx));
                                })),
                        ),
                    )
                },
            );
        }
        let list = uniform_list(
            "chat-create-members",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                rows[range]
                    .iter()
                    .map(|row| match row {
                        Row::Work(work) => div()
                            .flex()
                            .items_center()
                            .h(px(PICKER_ROW))
                            .px(px(9.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c.ink_2)
                            .truncate()
                            .child(work.clone()),
                        Row::Choice(choice) => this.member(choice, cx),
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .h(px(PICKER_ROW * count.min(PICKER_MAX_ROWS) as f32));
        picker.child(list)
    }

    fn member(&self, choice: &Choice, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        let node = choice.node;
        let selected = self.selected.contains(&node);
        let check = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(16.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(if selected { c.accent } else { c.line_strong })
            .when(selected, |check| {
                check
                    .bg(c.accent)
                    .child(icon(Lucide::Check, px(12.0), c.accent_ink))
            });
        let mark = agent_mark(choice.harness.as_deref(), &choice.name, px(16.0), c.ink_2);
        let row = div()
            .id(("chat-create-member", node.0))
            .flex()
            .items_center()
            .h(px(PICKER_ROW))
            .px(px(9.0))
            .gap(px(7.0))
            .when(selected, |row| row.bg(c.accent_wash))
            .child(check)
            .child(mark)
            .child(div().min_w_0().truncate().child(choice.name.clone()))
            .child(
                div()
                    .ml_auto()
                    .min_w_0()
                    .truncate()
                    .text_color(c.ink_2)
                    .child(
                        choice
                            .harness
                            .clone()
                            .unwrap_or_else(|| say(cx, Str::NodeTerminal).into()),
                    ),
            );
        let row = if self.saving {
            row
        } else {
            btn::tabbable(row, cx)
                .cursor_pointer()
                .hover(move |style| style.bg(c.surface_hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.selected.remove(&node) {
                        this.selected.insert(node);
                    }
                    cx.notify();
                }))
        };
        div().child(row)
    }
}

impl Render for CreateRoom {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div();
        }
        let c = colors(cx);
        let view = window.viewport_size();
        let cancel = btn::button("chat-create-cancel", cx)
            .child(say(cx, Str::CommonCancel))
            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)));
        let can_save = !self.saving && !self.selected.is_empty();
        let save = btn::tabbable(div().id("chat-create-save"), cx)
            .flex()
            .items_center()
            .min_h(px(26.0))
            .px(px(10.0))
            .rounded(px(6.0))
            .bg(if can_save { c.accent } else { c.panel })
            .text_color(if can_save { c.accent_ink } else { c.ink_2 })
            .child(if self.saving {
                words(cx, "저장 중…", "Saving…")
            } else if self.editing.is_some() {
                words(cx, "저장", "Save")
            } else {
                words(cx, "방 만들기", "Create room")
            })
            .when(can_save, |save| {
                save.cursor_pointer()
                    .hover(move |style| style.bg(c.accent_ink).text_color(c.accent))
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)))
            });
        let selected = self.selected.len();
        let editing = self.editing.is_some();
        let panel = div()
            .id("chat-create-panel")
            .flex()
            .flex_col()
            .w(px(PANEL_WIDTH).min((view.width - px(16.0)).max(px(0.0))))
            .max_h((view.height - px(16.0)).max(px(0.0)))
            .overflow_y_scroll()
            .p(px(14.0))
            .gap(px(9.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(c.line_strong)
            .bg(c.surface)
            .shadow_lg()
            .occlude()
            .tab_group()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .text_color(c.ink)
            .text_size(px(12.0))
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if editing {
                        words(cx, "구성원 편집", "Edit members")
                    } else {
                        words(cx, "새 방", "New room")
                    }),
            )
            .when(!editing, |panel| {
                panel.child(field_label(say(cx, Str::RoomName), &self.name))
            })
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_color(c.err).child(error)),
            )
            .child(field_label(words(cx, "구성원", "Members"), &self.search))
            .child(self.picker(cx))
            .child(div().text_color(c.ink_2).child(words(
                cx,
                "이 방에서 고른 구성원과 대화합니다. 구성원은 하던 일을 계속합니다.",
                "Chat with the members you select. Members continue their current work.",
            )))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(div().mr_auto().child(match cx.global::<Lang>() {
                        Lang::Ko => format!("구성원 {selected}명"),
                        Lang::En => format!("Members: {selected}"),
                    }))
                    .child(if self.saving {
                        btn::inert(cancel)
                    } else {
                        cancel
                    })
                    .child(if can_save { save } else { btn::inert(save) }),
            );
        div().child(
            deferred(
                anchored()
                    .anchor(Anchor::TopLeft)
                    .position(point(px(0.0), px(0.0)))
                    .child(
                        div()
                            .w(view.width)
                            .h(view.height)
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(c.scrim)
                            .occlude()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.close(window, cx);
                                }),
                            )
                            .child(panel),
                    ),
            )
            .with_priority(2),
        )
    }
}

fn field_label(label: &'static str, field: &Entity<Field>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(div().font_weight(FontWeight::SEMIBOLD).child(label))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(field.clone()),
        )
}
