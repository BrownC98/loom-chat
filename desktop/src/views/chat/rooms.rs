mod create;
mod list;

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use api::graph::chat::{ChatEntry, ChatRoom, ChatRoomKind};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Div, ElementId, Entity, FocusHandle,
    FontWeight, InteractiveElement, IntoElement, ParentElement, Render, Rgba,
    StatefulInteractiveElement, Styled, Window, div,
};

use crate::scale::px;
use crate::views::field::{self, Field, FieldEvent};
use crate::views::icons::{Lucide, icon};
use crate::views::ui::{clock, colors};
use crate::views::widgets::btn;

use super::data::ChatData;
use create::CreateRoom;
use list::{Filter, Index};

const WIDTH: f32 = 280.0;
const SMALL: f32 = 12.0;

#[derive(Clone, Default, PartialEq, Eq, Hash)]
enum Group {
    Custom,
    Work(String),
    #[default]
    Unassigned,
}

pub struct Rooms {
    data: Entity<ChatData>,
    search: Entity<Field>,
    create: Entity<CreateRoom>,
    new_room: FocusHandle,
    filter: Filter,
    flat: bool,
    folded: HashSet<Group>,
    sessions: HashMap<Group, bool>,
    shown_room: Option<String>,
}

impl Rooms {
    pub fn new(data: Entity<ChatData>, cx: &mut Context<Self>) -> Self {
        let search = field::boxed("방 이름·구성원 찾기", false, false, cx);
        search.update(cx, |field, cx| field.set_inset(20.0, 0.0, cx));
        cx.subscribe(&search, |_, _, event: &FieldEvent, cx| {
            if matches!(event, FieldEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        cx.observe(&data, |_, _, cx| cx.notify()).detach();
        let new_room = cx.focus_handle().tab_stop(true);
        let create = cx.new(|cx| CreateRoom::new(data.clone(), new_room.clone(), cx));
        cx.subscribe(&create, |this, _, created: &create::Created, cx| {
            this.restore(cx);
            this.data.update(cx, |data, cx| {
                data.select(created.0.clone(), cx);
            });
        })
        .detach();
        Self {
            data,
            search,
            create,
            new_room,
            filter: Filter::All,
            flat: false,
            folded: HashSet::new(),
            sessions: HashMap::new(),
            shown_room: None,
        }
    }

    fn restore(&mut self, cx: &mut Context<Self>) {
        self.filter = Filter::All;
        self.search.update(cx, |field, cx| field.set_text("", cx));
        cx.notify();
    }

    fn reveal_selection(&mut self, selected: Option<&ChatRoom>, index: &Index<'_>) {
        if self.shown_room.as_deref() == selected.map(|room| room.id.as_str()) {
            return;
        }
        self.shown_room = selected.map(|room| room.id.clone());
        if let Some(room) = selected {
            let group = match &room.kind {
                ChatRoomKind::Pinned => return,
                ChatRoomKind::Custom => Group::Custom,
                ChatRoomKind::Work { .. } => Group::Work(room.id.clone()),
                ChatRoomKind::Agent { node } => {
                    let group = group_for(index.work(*node));
                    self.sessions.insert(group.clone(), true);
                    group
                }
            };
            self.folded.remove(&group);
        }
    }

    fn open_create(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.create.update(cx, |create, cx| create.open(window, cx));
    }

    fn controls(&self, index: &Index<'_>, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        let filters = Filter::ALL.into_iter().enumerate().map(|(i, filter)| {
            let count = if filter == Filter::MyTurn {
                index
                    .rooms
                    .iter()
                    .find(|room| matches!(room.kind, ChatRoomKind::Pinned))
                    .map_or(0, |room| room.questions as usize)
            } else {
                index
                    .rooms
                    .iter()
                    .filter(|room| !matches!(room.kind, ChatRoomKind::Pinned))
                    .filter(|room| filter.shows(room, index))
                    .count()
            };
            let current = filter == self.filter;
            btn::tabbable(div().id(("room-filter", i)), cx)
                .flex()
                .items_center()
                .gap(px(3.0))
                .h(px(24.0))
                .px(px(5.0))
                .rounded_full()
                .border_1()
                .border_color(if current { c.accent } else { c.line_strong })
                .bg(if current { c.accent_wash } else { c.panel })
                .text_size(px(SMALL))
                .text_color(if current { c.ink } else { c.ink_2 })
                .whitespace_nowrap()
                .cursor_pointer()
                .hover(move |style| style.bg(c.surface_hover))
                .child(filter.label())
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.filter = filter;
                    cx.notify();
                }))
        });
        div()
            .flex()
            .flex_col()
            .flex_none()
            .px(px(12.0))
            .pt(px(8.0))
            .child(div().flex().flex_wrap().gap(px(3.0)).children(filters))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .mt(px(5.0))
                    .text_size(px(SMALL))
                    .text_color(c.ink_2)
                    .child("묶기")
                    .child(
                        btn::ghost("room-grouping", cx)
                            .child(if self.flat {
                                "묶지 않음"
                            } else {
                                "작업별"
                            })
                            .child(icon(Lucide::ChevronsUpDown, px(12.0), c.ink_2))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.flat = !this.flat;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn groups(
        &self,
        rooms: &[&ChatRoom],
        index: &Index<'_>,
        query: &str,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let mut work_rooms = HashMap::new();
        let mut custom = Vec::new();
        let mut sessions = HashMap::<Option<&str>, Vec<&ChatRoom>>::new();
        for &room in rooms {
            match &room.kind {
                ChatRoomKind::Pinned => {}
                ChatRoomKind::Custom => custom.push(room),
                ChatRoomKind::Work { .. } => {
                    work_rooms.insert(room.id.as_str(), room);
                }
                ChatRoomKind::Agent { node } => {
                    sessions.entry(index.work(*node)).or_default().push(room);
                }
            }
        }
        let auto_expand = self.filter != Filter::All || !query.is_empty();
        let mut groups = Vec::new();
        if !custom.is_empty() {
            let body = custom
                .into_iter()
                .map(|room| self.row(room, index, cx).into_any_element())
                .collect::<Vec<_>>();
            groups.push(self.group(Group::Custom, "내가 만든 방", body, cx));
        }
        for work in index
            .rooms
            .iter()
            .filter(|room| matches!(room.kind, ChatRoomKind::Work { .. }))
        {
            let matching_work = work_rooms.get(work.id.as_str()).copied();
            let group = Group::Work(work.id.clone());
            let agents = sessions.remove(&Some(work.id.as_str())).unwrap_or_default();
            if matching_work.is_none() && agents.is_empty() {
                continue;
            }
            let mut body = Vec::new();
            if let Some(room) = matching_work {
                body.push(self.row(room, index, cx).into_any_element());
            }
            body.extend(self.session_list(&group, &agents, index, auto_expand, cx));
            groups.push(self.group(group, &work.name, body, cx));
        }
        if let Some(agents) = sessions.remove(&None) {
            let group = Group::Unassigned;
            let body = self.session_list(&group, &agents, index, auto_expand, cx);
            groups.push(self.group(group, "작업 없는 세션", body.into_iter().collect(), cx));
        }
        groups
    }

    fn group(
        &self,
        group: Group,
        title: &str,
        body: Vec<AnyElement>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = !self.folded.contains(&group);
        let suffix = group_id(&group);
        let c = colors(cx);
        let button = btn::tabbable(div().id((ElementId::from("room-group"), suffix)), cx)
            .flex()
            .items_center()
            .gap(px(5.0))
            .min_h(px(26.0))
            .px(px(6.0))
            .rounded(px(6.0))
            .text_size(px(SMALL))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c.ink_2)
            .child(chevron(open, c.ink_2))
            .child(div().truncate().child(title.to_owned()))
            .cursor_pointer()
            .hover(move |style| style.bg(c.surface_hover))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.folded.remove(&group) {
                    this.folded.insert(group.clone());
                }
                cx.notify();
            }));
        div()
            .pt(px(4.0))
            .child(button)
            .when(open, |group| group.children(body))
            .into_any_element()
    }

    fn session_list(
        &self,
        group: &Group,
        rooms: &[&ChatRoom],
        index: &Index<'_>,
        auto_expand: bool,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if rooms.is_empty() {
            return None;
        }
        let open = self.sessions.get(group).copied().unwrap_or(auto_expand);
        let group = group.clone();
        let c = colors(cx);
        let button = btn::tabbable(
            div().id((ElementId::from("room-sessions"), group_id(&group))),
            cx,
        )
        .flex()
        .items_center()
        .gap(px(5.0))
        .min_h(px(24.0))
        .px(px(6.0))
        .rounded(px(6.0))
        .text_size(px(SMALL))
        .text_color(c.ink_2)
        .child(chevron(open, c.ink_2))
        .child(format!("1:1 세션 {}개", rooms.len()))
        .cursor_pointer()
        .hover(move |style| style.bg(c.surface_hover))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.sessions.insert(group.clone(), !open);
            cx.notify();
        }));
        Some(
            div()
                .pl(px(14.0))
                .child(button)
                .when(open, |body| {
                    body.children(rooms.iter().map(|room| self.row(room, index, cx)))
                })
                .into_any_element(),
        )
    }

    fn row(&self, room: &ChatRoom, index: &Index<'_>, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        let data = self.data.read(cx);
        let selected = index.selected.is_some_and(|active| active.id == room.id);
        let pinned = matches!(room.kind, ChatRoomKind::Pinned);
        let working = index.working(room);
        let (glyph, color) = match room.kind {
            ChatRoomKind::Pinned => (Lucide::Mail, c.accent),
            ChatRoomKind::Agent { .. } => (Lucide::Terminal, c.ink_2),
            ChatRoomKind::Custom | ChatRoomKind::Work { .. } => (Lucide::MessageCircle, c.ink_2),
        };
        let dot = if room.archived {
            c.ink_3
        } else if room.questions > 0 {
            c.warn
        } else if working {
            c.accent
        } else if room.done > 0 {
            c.ok
        } else {
            c.ink_3
        };
        let current_text = if pinned {
            Some("모든 에이전트의 질문·완료·알림".to_owned())
        } else {
            selected.then(|| last_line(data)).flatten()
        };
        let time = room.last_activity.map(|at| clock(at, "%H:%M"));
        let first = div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .min_h(px(20.0))
            .when(!pinned, |row| {
                row.child(div().size(px(7.0)).flex_none().rounded_full().bg(dot))
            })
            .child(icon(glyph, px(14.0), color))
            .child(
                div()
                    .truncate()
                    .min_w_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(room.name.clone()),
            )
            .when(
                matches!(room.kind, ChatRoomKind::Work { .. } | ChatRoomKind::Custom),
                |row| {
                    row.child(
                        div()
                            .flex_none()
                            .text_size(px(SMALL))
                            .text_color(c.ink_2)
                            .child(room.members.len().to_string()),
                    )
                },
            )
            .children(time.map(|text| {
                div()
                    .ml_auto()
                    .flex_none()
                    .text_size(px(11.0))
                    .text_color(c.ink_2)
                    .child(text)
            }));
        let badges = if pinned {
            vec![
                badge(format!("질문 {}", room.questions), c.warn, c.warn_ink),
                badge(format!("완료 {}", room.done), c.ok, c.accent_ink),
            ]
        } else {
            let mut badges = Vec::new();
            if matches!(room.kind, ChatRoomKind::Work { .. }) {
                badges.push(badge("자동".into(), c.surface, c.ink_2));
            }
            if room.archived {
                badges.push(badge("끝남".into(), c.surface, c.ink_2));
            }
            if room.questions > 0 {
                badges.push(badge("내 차례".into(), c.warn, c.warn_ink));
            }
            if working {
                badges.push(badge("일하는 중".into(), c.accent_wash, c.ink));
            }
            if room.unread > 0 {
                badges.push(badge(room.unread.to_string(), c.notice, c.notice_ink));
            }
            badges
        };
        let id = room.id.clone();
        let row = btn::tabbable(div().id((ElementId::from("chat-room"), id.clone())), cx)
            .w_full()
            .min_h(px(30.0))
            .px(px(7.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .when(pinned, |row| {
                row.border_1()
                    .border_color(if selected { c.accent } else { c.line_strong })
            })
            .when_else(
                selected,
                |row| row.bg(c.accent_wash),
                |row| row.when(pinned, |row| row.bg(c.surface)),
            )
            .cursor_pointer()
            .hover(move |style| {
                style.bg(if selected {
                    c.accent_wash
                } else {
                    c.surface_hover
                })
            })
            .child(first)
            .when(current_text.is_some() || !badges.is_empty(), |row| {
                let sub = div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(4.0))
                    .pl(px(if pinned { 19.0 } else { 12.0 }))
                    .min_h(px(19.0))
                    .text_size(px(SMALL))
                    .text_color(c.ink_2);
                row.child(
                    sub.children(
                        current_text.map(|text| div().flex_1().min_w_0().truncate().child(text)),
                    )
                    .children(badges),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.data.update(cx, |data, cx| data.select(id.clone(), cx));
            }));
        div().child(row)
    }
}

impl Render for Rooms {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = colors(cx);
        let data = self.data.clone();
        let data = data.read(cx);
        let index = Index::new(data, cx);
        let pinned = index
            .rooms
            .iter()
            .find(|room| matches!(room.kind, ChatRoomKind::Pinned));
        self.reveal_selection(index.selected, &index);
        let query = self.search.read(cx).text().trim().to_lowercase();
        let mut rooms = Vec::new();
        let mut hidden = 0;
        for room in index
            .rooms
            .iter()
            .filter(|room| !matches!(room.kind, ChatRoomKind::Pinned))
        {
            if self.filter.shows(room, &index) && index.matches(room, &query) {
                rooms.push(room);
            } else {
                hidden += 1;
            }
        }
        let empty = rooms.is_empty();
        let rows = if self.flat {
            rooms.sort_by_key(|room| (Reverse(room.last_activity), &room.name, &room.id));
            rooms
                .into_iter()
                .map(|room| self.row(room, &index, cx).into_any_element())
                .collect()
        } else {
            self.groups(&rooms, &index, &query, cx)
        };
        let error = data.error().map(str::to_owned);
        let loading = data.loading();
        let pin_row = pinned.map(|room| self.row(room, &index, cx));
        let controls = self.controls(&index, cx);
        let new_room = btn::button("new-chat-room", cx)
            .track_focus(&self.new_room)
            .child(icon(Lucide::Plus, px(13.0), c.ink))
            .child("새 방")
            .on_click(cx.listener(Self::open_create));
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(WIDTH))
            .h_full()
            .min_h_0()
            .bg(c.panel)
            .border_r_1()
            .border_color(c.line)
            .text_color(c.ink)
            .text_size(px(13.0))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .pl(px(16.0))
                    .pr(px(12.0))
                    .pt(px(8.0))
                    .pb(px(6.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("방 목록")
                    .child(new_room),
            )
            .child(
                div()
                    .relative()
                    .mx(px(12.0))
                    .flex_none()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(self.search.clone())
                    .child(div().absolute().left(px(8.0)).top(px(8.0)).child(icon(
                        Lucide::Search,
                        px(13.0),
                        c.ink_2,
                    ))),
            )
            .children(pin_row.map(|row| div().mx(px(8.0)).mt(px(8.0)).flex_none().child(row)))
            .children(pinned.map(|_| controls))
            .child(
                div()
                    .id("chat-room-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.0))
                    .pb(px(8.0))
                    .children(error.map(|error| {
                        div()
                            .px(px(6.0))
                            .py(px(6.0))
                            .text_size(px(SMALL))
                            .text_color(c.err)
                            .child(error)
                            .child(
                                div().mt(px(4.0)).child(
                                    btn::button("retry-chat-rooms", cx)
                                        .child("다시 불러오기")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.data.update(cx, |data, cx| data.refresh(cx));
                                        })),
                                ),
                            )
                    }))
                    .when(loading && pinned.is_none(), |body| {
                        body.child(note("방 목록을 불러옵니다.", cx))
                    })
                    .when(!loading && pinned.is_some() && empty, |body| {
                        let label = if hidden == 0 {
                            "아직 다른 방이 없습니다."
                        } else {
                            "조건에 맞는 방이 없습니다."
                        };
                        body.child(note(label, cx))
                    })
                    .children(rows)
                    .when(hidden > 0, |body| {
                        body.child(
                            div()
                                .flex()
                                .items_center()
                                .flex_wrap()
                                .justify_between()
                                .gap(px(3.0))
                                .px(px(6.0))
                                .pt(px(6.0))
                                .child(note(format!("다른 방 {hidden}개는 숨겼습니다"), cx))
                                .child(
                                    btn::ghost("restore-room-list", cx)
                                        .child("전체 보기")
                                        .on_click(cx.listener(|this, _, _, cx| this.restore(cx))),
                                ),
                        )
                    }),
            )
            .child(self.create.clone())
    }
}

fn group_for(work: Option<&str>) -> Group {
    work.map_or(Group::Unassigned, |work| Group::Work(work.to_owned()))
}

fn group_id(group: &Group) -> gpui::SharedString {
    match group {
        Group::Custom => "custom".into(),
        Group::Unassigned => "unassigned".into(),
        Group::Work(work) => work.clone().into(),
    }
}

fn last_line(data: &ChatData) -> Option<String> {
    data.items().iter().rev().find_map(|item| {
        let (name, text) = match &item.entry {
            ChatEntry::Message { message } => (&message.from.name, super::message_text(message)),
            ChatEntry::Wait { node, wait, .. } => (&node.name, wait.summary.as_str()),
            _ => return None,
        };
        let first_line = text.lines().find(|line| !line.trim().is_empty())?;
        Some(format!("{name}: {first_line}"))
    })
}

fn chevron(open: bool, ink: Rgba) -> gpui::Svg {
    icon(
        if open {
            Lucide::ChevronDown
        } else {
            Lucide::ChevronRight
        },
        px(12.0),
        ink,
    )
}

fn badge(text: String, bg: Rgba, ink: Rgba) -> Div {
    div()
        .flex_none()
        .px(px(5.0))
        .rounded_full()
        .bg(bg)
        .text_size(px(11.0))
        .text_color(ink)
        .child(text)
}

fn note(text: impl Into<gpui::SharedString>, cx: &App) -> Div {
    div()
        .p(px(6.0))
        .text_size(px(SMALL))
        .text_color(colors(cx).ink_2)
        .child(text.into())
}
