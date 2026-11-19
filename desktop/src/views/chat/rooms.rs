mod controls;
mod create;
mod list;

use std::collections::HashSet;
use std::ops::Range;

use api::graph::Status;
use api::graph::chat::{ChatEntry, ChatRoom, ChatRoomKind};
use api::ids::RepoId;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClickEvent, Context, Div, ElementId, Entity, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, Rgba, Role, Stateful,
    StatefulInteractiveElement, Styled, UniformListScrollHandle, Window, div, uniform_list,
};

use crate::scale::px;
use crate::strings::Lang;
use crate::views::field::{self, Field, FieldEvent};
use crate::views::icons::{Lucide, icon};
use crate::views::scrollbar::Bar;
use crate::views::status;
use crate::views::ui::{clock, colors};
use crate::views::widgets::btn;

use super::data::ChatData;
use controls::{Overlay, tool};
use create::CreateRoom;
use list::{Filter, Index, Row};

const ROW_HEIGHT: f32 = 48.0;
const SMALL: f32 = 12.0;
const MAX_GUIDES: usize = 6;

pub struct Rooms {
    data: Entity<ChatData>,
    search: Entity<Field>,
    create: Entity<CreateRoom>,
    new_room: FocusHandle,
    filter: Filter,
    flat: bool,
    folded: HashSet<String>,
    custom_folded: bool,
    shown_room: Option<String>,
    workspace: Option<RepoId>,
    overlay: Option<Overlay>,
    panel_focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    starting_scratch: bool,
    action_error: Option<String>,
    bar: Bar<UniformListScrollHandle>,
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
        let graph = data.read(cx).graph().clone();
        cx.observe(&graph, |_, _, cx| cx.notify()).detach();
        let new_room = cx.focus_handle().tab_stop(true);
        let create = cx.new(|cx| CreateRoom::new(data.clone(), new_room.clone(), cx));
        cx.subscribe(&create, |this, _, created: &create::Created, cx| {
            this.restore(cx);
            this.workspace = None;
            this.data
                .update(cx, |data, cx| data.select(created.0.clone(), cx));
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
            custom_folded: false,
            shown_room: None,
            workspace: None,
            overlay: None,
            panel_focus: cx.focus_handle(),
            return_focus: None,
            starting_scratch: false,
            action_error: None,
            bar: Bar::over(UniformListScrollHandle::new()),
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
        match selected.map(|room| &room.kind) {
            Some(ChatRoomKind::Custom) => self.custom_folded = false,
            Some(ChatRoomKind::Agent { node }) => {
                if let Some(work) = index.work(*node) {
                    for room in index.work_path(work) {
                        self.folded.remove(&room.id);
                    }
                }
            }
            _ => {}
        }
    }

    fn controls(&self, index: &Index<'_>, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        let filters = Filter::ALL.into_iter().enumerate().map(|(i, filter)| {
            let count = if filter == Filter::MyTurn && self.workspace.is_none() {
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
                    .filter(|room| index.in_workspace(room, self.workspace))
                    .filter(|room| filter.shows(room, index))
                    .count()
            };
            let current = filter == self.filter;
            btn::tabbable(div().id(("room-filter", i)), cx)
                .role(Role::Button)
                .flex()
                .items_center()
                .gap(px(3.0))
                .h(px(24.0))
                .px(px(4.0))
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

    fn custom_heading(&self, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        div().h_full().flex().items_center().child(
            btn::tabbable(div().id("custom-rooms-heading"), cx)
                .role(Role::Button)
                .flex()
                .items_center()
                .gap(px(4.0))
                .px(px(6.0))
                .rounded(px(5.0))
                .text_size(px(SMALL))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c.ink_2)
                .cursor_pointer()
                .hover(move |style| style.bg(c.surface_hover))
                .child(chevron(!self.custom_folded, c.ink_2))
                .child("내가 만든 방")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.custom_folded = !this.custom_folded;
                    cx.notify();
                })),
        )
    }

    fn room_row(
        &self,
        room: &ChatRoom,
        selected: bool,
        pinned: bool,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let c = colors(cx);
        let (glyph, ink) = if pinned {
            (Lucide::Mail, c.accent)
        } else {
            (Lucide::MessageCircle, c.ink_2)
        };
        let first = div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .child(icon(glyph, px(14.0), ink))
            .child(name(&room.name))
            .children(room.last_activity.map(|time| stamp(time, cx)));
        let details = if pinned {
            Some(format!("모든 작업공간 · 에이전트 {}", room.members.len()))
        } else {
            selected.then(|| last_line(self.data.read(cx))).flatten()
        };
        let mut sub = subtitle(cx)
            .pl(px(18.0))
            .children(details.map(|text| div().flex_1().min_w_0().truncate().child(text)));
        if room.questions > 0 {
            sub = sub.child(badge(
                if pinned {
                    format!("질문 {}", room.questions)
                } else {
                    "내 차례".to_owned()
                },
                c.warn,
                c.warn_ink,
            ));
        }
        if pinned && room.done > 0 {
            sub = sub.child(badge(format!("완료 {}", room.done), c.ok, c.accent_ink));
        }
        if !pinned {
            sub = sub
                .when(room.archived, |sub| sub.child("끝남"))
                .when(room.unread > 0, |sub| {
                    sub.child(badge(room.unread.to_string(), c.notice, c.notice_ink))
                });
        }
        self.select_button(room, cx)
            .px(px(7.0))
            .py(px(4.0))
            .when(pinned, |row| row.h(px(ROW_HEIGHT)))
            .when(selected, |row| row.bg(c.accent_wash))
            .when(pinned, |row| {
                row.border_1()
                    .border_color(if selected { c.accent } else { c.line_strong })
            })
            .when(pinned && !selected, |row| row.bg(c.surface))
            .child(first)
            .child(sub)
    }

    fn work_row(
        &self,
        room: &ChatRoom,
        index: &Index<'_>,
        depth: usize,
        folded: bool,
        selected: bool,
        cx: &Context<Self>,
    ) -> Div {
        let ChatRoomKind::Work { place } = &room.kind else {
            return div();
        };
        let c = colors(cx);
        let repo = index.repo(place.repo);
        let tree = repo.and_then(|repo| {
            repo.worktrees
                .iter()
                .find(|tree| tree.path == place.worktree)
        });
        let glyph = if repo.is_some_and(|repo| !repo.git) {
            Lucide::Folder
        } else {
            Lucide::GitBranch
        };
        let id = room.id.clone();
        let gesture = if folded {
            "하위 방 펴기"
        } else {
            "하위 방 접기"
        };
        let toggle = tool(
            "chat-work-fold",
            if folded {
                Lucide::ChevronRight
            } else {
                Lucide::ChevronDown
            },
            gesture,
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if !this.folded.remove(&id) {
                this.folded.insert(id.clone());
            }
            cx.notify();
        }));
        let urgent = room.questions > 0
            || room.members.iter().any(|&id| {
                index
                    .node(id)
                    .is_some_and(|node| matches!(node.status, Status::Human))
            });
        let problem = room.members.iter().any(|&id| {
            index
                .node(id)
                .is_some_and(|node| status::problem(&node.status))
        });
        let working = Filter::Working.shows(room, index);
        let state = if urgent {
            c.warn
        } else if problem {
            c.err
        } else if working {
            c.run
        } else if room.done > 0 {
            c.ok
        } else {
            c.ink_3
        };
        let label = index.work_title(room);
        let detail = match repo {
            Some(repo) if repo.git => format!("{} · {}명", repo.name, room.members.len()),
            _ => format!("구성원 {}명", room.members.len()),
        };
        let head = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(dot(state))
            .child(icon(glyph, px(13.0), c.ink_2))
            .child(name(label))
            .children(room.last_activity.map(|time| stamp(time, cx)));
        let info = subtitle(cx)
            .child(div().min_w_0().truncate().child(detail))
            .when(urgent, |row| {
                row.child(badge("내 차례".to_owned(), c.warn, c.warn_ink))
            })
            .when(!urgent && problem, |row| {
                row.child(badge("문제".to_owned(), c.err, c.err_ink))
            });
        div().h_full().child(
            div()
                .id((ElementId::from("chat-work-row"), room.id.clone()))
                .h_full()
                .flex()
                .items_center()
                .rounded(px(6.0))
                .pr(px(3.0))
                .gap(px(3.0))
                .when(selected, |row| row.bg(c.accent_wash))
                .child(guides(depth, cx))
                .when(!self.flat, |row| row.child(toggle))
                .child(self.select_button(room, cx).child(head).child(info))
                .children(
                    repo.zip(tree)
                        .map(|(repo, tree)| self.work_actions(place, tree, repo, cx)),
                ),
        )
    }

    fn agent_row(
        &self,
        room: &ChatRoom,
        index: &Index<'_>,
        depth: usize,
        selected: bool,
        cx: &Context<Self>,
    ) -> Div {
        let ChatRoomKind::Agent { node } = room.kind else {
            return div();
        };
        let c = colors(cx);
        let live = index.node(node);
        let lang = *cx.global::<Lang>();
        let (label, tone) = if room.questions > 0
            || live.is_some_and(|node| matches!(node.status, Status::Human))
        {
            ("내 답을 기다림".to_owned(), c.warn)
        } else if let Some(chip) = live.and_then(|node| status::chip(&node.status, lang)) {
            let color = match chip.tone {
                status::Tone::Problem => c.err,
                status::Tone::Working => c.run,
                status::Tone::Result => c.ok,
                _ => c.ink_3,
            };
            (chip.text, color)
        } else {
            ("끝남".to_owned(), c.ink_3)
        };
        let role = live.map(|node| {
            node.role.as_deref().unwrap_or_else(|| {
                if node.agent.is_none() {
                    "셸"
                } else if node.parent.is_some() {
                    "하위 에이전트"
                } else {
                    "상위 에이전트"
                }
            })
        });
        let time = room
            .last_activity
            .or_else(|| live.map(|node| node.since).filter(|since| *since > 0));
        let title = live.map_or(room.name.as_str(), |node| node.name.as_str());
        let first = div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .child(dot(tone))
            .child(name(title))
            .when(live.is_some_and(|node| node.bookmark.is_some()), |row| {
                row.child(icon(Lucide::Bookmark, px(11.0), c.ink_2))
            })
            .children(time.map(|time| stamp(time, cx)));
        let info = subtitle(cx).child(div().truncate().child(match role {
            Some(role) => format!("{role} · {label}"),
            None => label,
        }));
        let select = self.select_button(room, cx).child(first).child(info);
        div().h_full().child(
            div()
                .id((ElementId::from("chat-agent-row"), room.id.clone()))
                .h_full()
                .flex()
                .items_center()
                .rounded(px(6.0))
                .pr(px(3.0))
                .gap(px(3.0))
                .when(selected, |row| row.bg(c.accent_wash))
                .when(live.is_some(), |row| {
                    row.on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        this.agent_rename_key(node, event, window, cx);
                    }))
                })
                .child(guides(depth, cx))
                .child(select)
                .when(live.is_some(), |row| {
                    row.child(self.agent_actions(node, cx))
                }),
        )
    }

    fn select_button(&self, room: &ChatRoom, cx: &Context<Self>) -> Stateful<Div> {
        let c = colors(cx);
        let id = room.id.clone();
        btn::tabbable(div().id((ElementId::from("chat-room"), id.clone())), cx)
            .role(Role::Button)
            .flex()
            .flex_col()
            .justify_center()
            .flex_1()
            .min_w_0()
            .h_full()
            .rounded(px(5.0))
            .cursor_pointer()
            .hover(move |row| row.bg(c.surface_hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.data.update(cx, |data, cx| data.select(id.clone(), cx));
            }))
    }

    fn room_list(&self, rows: Vec<Row>, selection: Option<String>, cx: &Context<Self>) -> Div {
        let list = uniform_list(
            "chat-room-list",
            rows.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let index = Index::new(this.data.read(cx), cx);
                let selected = |room: &ChatRoom| selection.as_deref() == Some(room.id.as_str());
                range
                    .map(|slot| {
                        let body = match &rows[slot] {
                            Row::Room(slot) => index.rooms.get(*slot).map(|room| {
                                div().child(this.room_row(room, selected(room), false, cx))
                            }),
                            Row::Work(slot, depth) => index.rooms.get(*slot).map(|room| {
                                let folded = this.folded.contains(&room.id);
                                this.work_row(room, &index, *depth, folded, selected(room), cx)
                            }),
                            Row::Agent(slot, depth) => index.rooms.get(*slot).map(|room| {
                                this.agent_row(room, &index, *depth, selected(room), cx)
                            }),
                            Row::Custom => Some(this.custom_heading(cx)),
                            Row::Worktrees => {
                                let label = if index
                                    .repos
                                    .iter()
                                    .filter(|repo| this.workspace.is_none_or(|id| repo.id == id))
                                    .any(|repo| !repo.git)
                                {
                                    "작업공간 방"
                                } else {
                                    "워크트리 방"
                                };
                                Some(heading(label, cx))
                            }
                            Row::Unassigned => Some(heading("작업공간 없는 방", cx)),
                            Row::Hidden(count) => Some(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(3.0))
                                    .child(note(format!("다른 방 {count}개는 숨겼습니다"), cx))
                                    .child(
                                        btn::ghost("restore-room-list", cx)
                                            .child("전체 보기")
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.restore(cx)),
                                            ),
                                    ),
                            ),
                        };
                        div().h(px(ROW_HEIGHT)).w_full().children(body)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .px(px(8.0))
        .pb(px(8.0))
        .track_scroll(self.bar.handle());
        self.bar.on_overflow(list).flex_1().min_h_0()
    }
}

impl Render for Rooms {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = colors(cx);
        let data = self.data.clone();
        let data = data.read(cx);
        let index = Index::new(data, cx);
        let connected = data.graph().read(cx).snapshot().is_some();
        if connected && self.workspace.is_some_and(|id| index.repo(id).is_none()) {
            self.workspace = None;
        }
        let pinned = index
            .rooms
            .iter()
            .find(|room| matches!(room.kind, ChatRoomKind::Pinned));
        self.reveal_selection(data.room(), &index);
        let query = self.search.read(cx).text().trim().to_lowercase();
        let (mut rows, hidden) = self.list_rows(&index, &query);
        let empty = rows.is_empty();
        if hidden > 0 {
            rows.push(Row::Hidden(hidden));
        }
        let loading = data.loading();
        let selection = data.room().map(|room| room.id.clone());
        let pin_row = pinned.map(|room| {
            self.room_row(
                room,
                selection.as_deref() == Some(room.id.as_str()),
                true,
                cx,
            )
        });
        let controls = self.controls(&index, cx);
        let list = self.room_list(rows, selection, cx);
        let overlay = self.overlay(&index, window, cx);
        let zero_workspaces = connected && index.repos.is_empty();
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .h_full()
            .min_h_0()
            .bg(c.panel)
            .text_color(c.ink)
            .text_size(px(13.0))
            .child(self.workspace_controls(&index, cx))
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
                    .child(self.new_control(cx)),
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
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .children(data.error().map(|error| {
                        div()
                            .mx(px(8.0))
                            .flex_none()
                            .p(px(6.0))
                            .text_size(px(SMALL))
                            .text_color(c.err)
                            .child(error.to_owned())
                            .child(
                                btn::button("retry-chat-rooms", cx)
                                    .child("다시 불러오기")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.data.update(cx, |data, cx| data.refresh(cx));
                                    })),
                            )
                    }))
                    .children(
                        self.action_error
                            .as_ref()
                            .filter(|_| overlay.is_none())
                            .map(|error| {
                                div()
                                    .mx(px(8.0))
                                    .p(px(6.0))
                                    .text_color(c.err)
                                    .child(error.clone())
                            }),
                    )
                    .when(loading && pinned.is_none(), |body| {
                        body.child(note("방 목록을 불러옵니다.", cx).mx(px(8.0)).flex_none())
                    })
                    .when(zero_workspaces, |body| body.child(self.empty_workspace(cx)))
                    .when(
                        !zero_workspaces && !loading && pinned.is_some() && empty,
                        |body| {
                            let label = if hidden == 0 {
                                "아직 다른 방이 없습니다."
                            } else {
                                "조건에 맞는 방이 없습니다."
                            };
                            body.child(note(label, cx).mx(px(8.0)).flex_none())
                        },
                    )
                    .child(list),
            )
            .children(
                overlay.map(|panel| div().absolute().top_0().left_0().size_full().child(panel)),
            )
            .child(self.create.clone())
    }
}

fn name(text: &str) -> Div {
    div()
        .min_w_0()
        .truncate()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.to_owned())
}

fn stamp(time: i64, cx: &App) -> Div {
    div()
        .ml_auto()
        .flex_none()
        .text_size(px(11.0))
        .text_color(colors(cx).ink_2)
        .child(clock(time, "%H:%M"))
}

fn dot(color: Rgba) -> Div {
    div().size(px(7.0)).flex_none().rounded_full().bg(color)
}

fn subtitle(cx: &App) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.0))
        .min_h(px(17.0))
        .whitespace_nowrap()
        .text_size(px(SMALL))
        .text_color(colors(cx).ink_2)
}

fn guides(depth: usize, cx: &App) -> Div {
    let line = colors(cx).line_strong;
    let depth = depth.min(MAX_GUIDES);
    div()
        .h_full()
        .flex()
        .flex_none()
        .children((0..depth).map(|level| {
            div().w(px(10.0)).border_l_1().border_color(line).when_else(
                level + 1 == depth,
                |guide| guide.h(px(18.0)).border_b_1(),
                |guide| guide.h_full(),
            )
        }))
}

fn heading(title: &'static str, cx: &App) -> Div {
    div()
        .h_full()
        .flex()
        .items_end()
        .px(px(6.0))
        .pb(px(4.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(SMALL))
        .text_color(colors(cx).ink_2)
        .child(title)
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
