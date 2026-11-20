use std::ops::Range;

use api::graph::Status;
use api::graph::chat::{ChatRoom, ChatRoomKind};
use api::ids::NodeId;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, Div, FontWeight, InteractiveElement, ParentElement, Role,
    StatefulInteractiveElement, Styled, div, uniform_list,
};

use super::super::{guides, keep_ime_focus, room_title};
use super::{SideEvent, SideSlot, SideTab, room_waiting, words};
use crate::scale::px;
use crate::strings::Lang;
use crate::theme::Colors;
use crate::views::icons::{Lucide, icon};
use crate::views::status;
use crate::views::tooltip::tip;
use crate::views::ui::{self, SMALL};
use crate::views::widgets::btn;

mod hierarchy;

use hierarchy::{Entry, Index};
pub(super) use hierarchy::{Member, MemberKey};

const ROW_HEIGHT: f32 = 44.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum State {
    Rest,
    Stopped,
    Working,
    Human,
}

impl State {
    fn of(status: Option<&Status>) -> Self {
        match status {
            Some(Status::Human) => Self::Human,
            Some(Status::Working { .. }) => Self::Working,
            Some(Status::Stopped { .. } | Status::SetupFailed { .. }) => Self::Stopped,
            _ => Self::Rest,
        }
    }

    fn label(self, cx: &App) -> &'static str {
        match self {
            Self::Human => words(cx, "내 답을 기다림", "Waiting for your reply"),
            Self::Working => words(cx, "일하는 중", "Working"),
            Self::Rest => words(cx, "쉬는 중", "Resting"),
            Self::Stopped => words(cx, "꺼짐", "Stopped"),
        }
    }

    fn dot(self, c: &Colors) -> Div {
        let color = match self {
            Self::Human => c.warn,
            Self::Working => c.run,
            Self::Rest => c.ok,
            Self::Stopped => c.err,
        };
        div().flex_none().size(px(7.0)).rounded(px(4.0)).bg(color)
    }
}

struct Row {
    id: String,
    name: String,
    harness: Option<String>,
    detail: String,
    guides: Vec<bool>,
    state: Option<State>,
    glyph: Option<Lucide>,
    room: Option<String>,
    terminal: Option<NodeId>,
    member: bool,
}

impl Row {
    fn from_entry(
        entry: Entry<'_>,
        guides: Vec<bool>,
        index: &Index<'_>,
        room: &ChatRoom,
        cx: &App,
    ) -> Self {
        let mut row = Self {
            id: String::new(),
            name: String::new(),
            harness: None,
            detail: String::new(),
            guides,
            state: None,
            glyph: None,
            room: None,
            terminal: None,
            member: false,
        };
        match entry {
            Entry::Member(member) => {
                row.name = member.name.to_owned();
                row.harness = member.harness.map(str::to_owned);
                match member.key {
                    MemberKey::Local(id) => {
                        row.id = format!("node-{id}");
                        row.state = Some(member_state(&member));
                        row.detail = member_line(&member, id, index, cx);
                        row.room = member.session.map(|room| room.id.clone());
                        row.terminal = (!room.archived).then(|| member.terminal()).flatten();
                        row.member = true;
                    }
                    MemberKey::Outside(ref name) => {
                        row.id = format!("outside-{name}");
                        row.detail = words(
                            cx,
                            "외부 에이전트 · 터미널 없음",
                            "Outside agent · No terminal",
                        )
                        .to_owned();
                    }
                }
            }
            Entry::Above(node) => {
                row.id = format!("above-{}", node.id);
                row.name = format!("{} · {}", words(cx, "위", "Above"), node.name);
                row.glyph = Some(Lucide::ArrowUp);
                row.room = index.sessions.get(&node.id).map(|room| room.id.clone());
                row.detail = [
                    index
                        .repos
                        .get(&node.place.repo)
                        .map(|repo| repo.name.as_str()),
                    index
                        .work_at(&node.place)
                        .map(|room| room_title(room, |repo| index.repos.get(&repo).copied(), cx)),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" › ");
            }
            Entry::Work(work) => {
                row.id = work.id.clone();
                row.name = room_title(work, |repo| index.repos.get(&repo).copied(), cx).to_owned();
                row.room = Some(work.id.clone());
                let folder = matches!(&work.kind, ChatRoomKind::Work { place }
                    if index.repos.get(&place.repo).is_some_and(|repo| !repo.git));
                row.glyph = Some(if folder {
                    Lucide::Folder
                } else {
                    Lucide::GitBranch
                });
                let (state, detail) = work_summary(work, index, cx);
                row.state = Some(state);
                row.detail = detail;
            }
            Entry::Workspace { id, rooms, members } => {
                row.id = format!("workspace-{}", id.0);
                let repo = index.repos.get(&id);
                row.name = repo.map_or_else(
                    || format!("{} {}", words(cx, "작업공간", "Workspace"), id.0),
                    |repo| repo.name.clone(),
                );
                row.glyph = Some(Lucide::Folder);
                let count = member_count(members, cx);
                row.detail = if repo.is_some_and(|repo| !repo.git) {
                    format!("{} · {count}", words(cx, "폴더", "Folder"))
                } else {
                    match cx.global::<Lang>() {
                        Lang::Ko => format!("워크트리 {rooms} · {count}"),
                        Lang::En => format!("{rooms} worktrees · {count}"),
                    }
                };
            }
            Entry::Unassigned => {
                row.id = "unassigned".to_owned();
                row.name =
                    words(cx, "작업공간 없는 세션", "Sessions without a workspace").to_owned();
                row.glyph = Some(Lucide::Folder);
            }
        }
        row
    }

    fn render(&self, cx: &Context<SideSlot>) -> Div {
        let c = ui::colors(cx);
        let body = div()
            .id((gpui::ElementId::from("chat-member-room"), self.id.clone()))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .children(self.state.map(|state| state.dot(&c)))
                    .children(self.glyph.map(|glyph| icon(glyph, px(13.0), c.ink_2)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.name.clone()),
                    )
                    .children(self.harness.as_ref().map(|harness| {
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(SMALL))
                            .text_color(c.ink_2)
                            .child(harness.clone())
                    })),
            )
            .when(!self.detail.is_empty(), |body| {
                body.child(
                    div()
                        .id(format!("chat-hierarchy-detail-{}", self.id))
                        .truncate()
                        .text_size(px(SMALL))
                        .text_color(c.ink_2)
                        .tooltip(tip(self.detail.clone()))
                        .child(self.detail.clone()),
                )
            });
        let body = if let Some(room) = &self.room {
            let room = room.clone();
            btn::tabbable(body, cx)
                .role(Role::Button)
                .aria_label(self.name.clone())
                .justify_center()
                .h_full()
                .rounded(px(5.0))
                .cursor_pointer()
                .hover(move |row| row.bg(c.surface_hover))
                .capture_any_mouse_down(keep_ime_focus)
                .on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(SideEvent::OpenRoom(room.clone()))),
                )
        } else {
            body
        };
        div()
            .w_full()
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap(px(4.0))
            .children(guides(&self.guides, ROW_HEIGHT, cx))
            .child(body)
            .when(self.member, |row| row.child(self.terminal_button(cx)))
    }

    fn terminal_button(&self, cx: &Context<SideSlot>) -> impl gpui::IntoElement {
        let c = ui::colors(cx);
        let id = format!("chat-member-terminal-{}", self.id);
        let Some(node) = self.terminal else {
            return div().id(id).size(px(26.0)).flex_none();
        };
        let label = words(cx, "터미널 보기", "Show terminal");
        btn::ghost(id, cx)
            .role(Role::Button)
            .aria_label(format!("{} · {label}", self.name))
            .size(px(26.0))
            .p_0()
            .flex_none()
            .justify_center()
            .tooltip(tip(label.to_owned()))
            .child(icon(Lucide::SquareTerminal, px(14.0), c.ink_2))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.inspect_terminal(node, cx);
                this.focus_tab(window, cx);
            }))
    }
}

fn member_count(count: usize, cx: &App) -> String {
    match cx.global::<Lang>() {
        Lang::Ko => format!("{count}명"),
        Lang::En if count == 1 => "1 member".to_owned(),
        Lang::En => format!("{count} members"),
    }
}

fn counted_state(state: State, count: usize, cx: &App) -> String {
    match cx.global::<Lang>() {
        Lang::Ko if state == State::Working => format!("{count}명이 {}", state.label(cx)),
        Lang::En if state == State::Working => format!("{count} working"),
        _ => format!("{} {count}", state.label(cx)),
    }
}

fn work_summary(room: &ChatRoom, index: &Index<'_>, cx: &App) -> (State, String) {
    let states = room
        .members
        .iter()
        .copied()
        .map(|id| State::of(index.state(id)));
    let working = states
        .clone()
        .filter(|&state| state == State::Working)
        .count();
    let state = if room.questions > 0 {
        State::Human
    } else if room.archived {
        State::Stopped
    } else {
        states.max().unwrap_or(State::Rest)
    };
    let detail = match state {
        State::Human if room.questions > 0 => counted_state(state, room.questions as usize, cx),
        State::Working => counted_state(state, working, cx),
        _ => state.label(cx).to_owned(),
    };
    (
        state,
        format!("{} · {detail}", member_count(room.members.len(), cx)),
    )
}

fn member_state(member: &Member<'_>) -> State {
    match State::of(member.status()) {
        State::Human => State::Human,
        _ if member.session.is_some_and(|room| room.archived) => State::Stopped,
        state => state,
    }
}

fn member_line(member: &Member<'_>, id: NodeId, index: &Index<'_>, cx: &App) -> String {
    let lang = *cx.global::<Lang>();
    let state = member.status();
    let node = member.node;
    let parent = node
        .and_then(|node| index.nodes.get(&node.parent?))
        .map(|node| node.name.as_str());
    let archived = member.session.is_some_and(|room| room.archived);
    let label = match state {
        Some(Status::Idle | Status::Human | Status::Working { .. }) | None => {
            member_state(member).label(cx).to_owned()
        }
        _ if archived => State::Stopped.label(cx).to_owned(),
        Some(state) => status::chip(state, lang)
            .map_or_else(|| State::Rest.label(cx).to_owned(), |chip| chip.text),
    };
    let elapsed = node.and_then(|node| status::since_short(node.since, lang));
    let activity = match state {
        Some(Status::Answer) => parent.map(str::to_owned),
        Some(Status::Human) => node
            .and_then(|node| {
                node.waits
                    .iter()
                    .filter(|wait| status::asking(wait))
                    .min_by_key(|wait| wait.wait.since)
            })
            .filter(|wait| wait.wait.answerable)
            .map(|wait| format!("{} · {}", wait.wait.id, wait.summary)),
        _ => None,
    }
    .or_else(|| match state {
        Some(Status::Idle | Status::Done) => last_message(index, id, cx),
        _ => None,
    })
    .or_else(|| node.and_then(|node| status::line(node, parent, lang)))
    .or_else(|| member.recipient.and_then(|agent| agent.task.clone()))
    .or_else(|| last_message(index, id, cx));

    [Some(label), elapsed, activity]
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .map(|part| part.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn last_message(index: &Index<'_>, id: NodeId, cx: &App) -> Option<String> {
    let &(message, at_ms) = index.messages.get(&id)?;
    let time = ui::clock(at_ms / 1000, "%H:%M");
    let names = message
        .receipts
        .iter()
        .map(|receipt| {
            if receipt.to.id.is_none() && receipt.to.name == "human" {
                words(cx, "나", "Me")
            } else {
                receipt.to.name.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    if names.is_empty() {
        return Some(format!("{time} {}", message.text));
    }
    Some(match cx.global::<Lang>() {
        Lang::Ko => format!("{names}에게 보냄 {time}"),
        Lang::En => format!("Wrote to {names} {time}"),
    })
}

impl SideSlot {
    pub(super) fn members<'a>(&'a self, room: &'a ChatRoom, cx: &'a App) -> Vec<Member<'a>> {
        Index::new(self.data.read(cx), cx).members(room)
    }

    pub(super) fn member_panel(&self, room: Option<&ChatRoom>, cx: &Context<Self>) -> Div {
        let c = ui::colors(cx);
        let mut panel = div().size_full().flex().flex_col();
        let Some(room) = room else {
            return panel.p(px(14.0)).child(room_waiting(cx, None));
        };
        let data = self.data.read(cx);
        let index = Index::new(data, cx);
        let entries = index.room_rows(room);
        let summary = match room.kind {
            ChatRoomKind::Pinned => {
                let workspaces = entries
                    .iter()
                    .filter(|(entry, _)| matches!(entry, Entry::Workspace { .. }))
                    .count();
                let members = room.members.len();
                match cx.global::<Lang>() {
                    Lang::Ko => format!("작업공간 {workspaces} · 에이전트 {members}"),
                    Lang::En => format!("{workspaces} workspaces · {members} agents"),
                }
            }
            _ => {
                let count = member_count(room.members.len(), cx);
                let title = room_title(room, |repo| index.repos.get(&repo).copied(), cx);
                let summary = format!("{title} · {count}");
                let working = room
                    .members
                    .iter()
                    .filter(|&&id| matches!(index.state(id), Some(Status::Working { .. })))
                    .count();
                if !room.archived && working > 0 {
                    format!("{summary} · {}", counted_state(State::Working, working, cx))
                } else {
                    summary
                }
            }
        };
        let rows: Vec<_> = entries
            .into_iter()
            .map(|(entry, guides)| Row::from_entry(entry, guides, &index, room, cx))
            .collect();
        panel = panel.child(
            div()
                .flex_none()
                .px(px(12.0))
                .py(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .border_b_1()
                .border_color(c.line)
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SideTab::Members.label(cx)),
                )
                .child(
                    div()
                        .id("chat-hierarchy-summary")
                        .min_w_0()
                        .truncate()
                        .text_size(px(SMALL))
                        .text_color(c.ink_2)
                        .tooltip(tip(summary.clone()))
                        .child(summary),
                ),
        );
        if rows.is_empty() {
            panel = panel.child(
                div()
                    .p(px(12.0))
                    .text_color(c.ink_2)
                    .child(room_waiting(cx, Some(room))),
            );
        } else {
            let count = rows.len();
            let list = uniform_list(
                "chat-member-hierarchy",
                count,
                cx.processor(move |_, range: Range<usize>, _, cx| {
                    rows[range]
                        .iter()
                        .map(|row| row.render(cx))
                        .collect::<Vec<_>>()
                }),
            )
            .size_full()
            .px(px(10.0))
            .track_scroll(self.member_scroll.handle());
            panel = panel.child(
                self.member_scroll
                    .on_overflow(list)
                    .flex_1()
                    .min_h_0()
                    .max_h(px(ROW_HEIGHT * count as f32)),
            );
        }
        panel
    }
}
