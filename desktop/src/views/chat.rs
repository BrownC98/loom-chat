mod composer;
mod data;
mod rooms;
mod side;
mod stream;

use api::graph::chat::{ChatRoom, ChatRoomKind, ChatTranscript};
use api::graph::{Place, ProcessState, Status};
use api::ids::{NodeId, RepoId};
use api::repos::RepoView;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Div, Entity, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Point, Render,
    StatefulInteractiveElement, Styled, Subscription, Window, div, point,
};

use crate::link::LinkState;
use crate::scale::px;
use crate::store::{OpenAgent, Store};
use crate::strings::{Lang, Str};

use super::compose;
use super::icons::{Lucide, icon};
use super::status;
use super::status::pop::{StatusEvent, StatusView};
use super::tooltip::tip;
use super::ui::{self, SMALL, TEXT, say};
use super::widgets::btn;
use super::workspace::pop;
use composer::Composer;
use data::ChatData;
use rooms::{Rooms, SelectRoom};
use side::{SideEvent, SideSlot, SideTab};
use stream::{Quote, Stream, StreamEvent};

const ROOMS_WIDTH: f32 = 280.0;
const SIDE_WIDTH: f32 = 334.0;
const OVERLAY_WIDTH: f32 = 1050.0;

fn words<'a>(cx: &App, ko: &'a str, en: &'a str) -> &'a str {
    match cx.global::<Lang>() {
        Lang::Ko => ko,
        Lang::En => en,
    }
}

fn room_title<'a>(
    room: &'a ChatRoom,
    repo: impl FnOnce(RepoId) -> Option<&'a RepoView>,
    cx: &App,
) -> &'a str {
    if matches!(room.kind, ChatRoomKind::Pinned) {
        return words(cx, "종합 채팅방", "All conversations");
    }
    let ChatRoomKind::Work { place } = &room.kind else {
        return &room.name;
    };
    let Some(repo) = repo(place.repo) else {
        return &room.name;
    };
    if !repo.git {
        return &repo.name;
    }
    repo.worktrees
        .iter()
        .find(|work| work.path == place.worktree)
        .and_then(|work| work.branch.as_deref())
        .unwrap_or(&room.name)
}

fn guides(path: &[bool], height: f32, cx: &App) -> Option<Div> {
    const WIDTH: f32 = 10.0;
    const MAX_DEPTH: usize = 6;
    let (&continues, ancestors) = path.split_last()?;
    let ancestors = &ancestors[ancestors.len().saturating_sub(MAX_DEPTH - 1)..];
    let line = ui::colors(cx).line_strong;
    let elbow = height / 2.0 - 8.0;
    let vertical = |height| div().ml(px(4.0)).w(px(1.0)).h(px(height)).bg(line);
    Some(
        div()
            .flex()
            .flex_none()
            .h_full()
            .children(ancestors.iter().map(|&continues| {
                div()
                    .w(px(WIDTH))
                    .when(continues, |column| column.child(vertical(height)))
            }))
            .child(
                div()
                    .relative()
                    .w(px(WIDTH))
                    .child(vertical(if continues { height } else { elbow }))
                    .child(
                        div()
                            .absolute()
                            .left(px(4.0))
                            .top(px(elbow))
                            .right_0()
                            .h(px(1.0))
                            .bg(line),
                    ),
            ),
    )
}

fn keep_ime_focus(event: &gpui::MouseDownEvent, window: &mut Window, cx: &mut App) {
    if event.button == MouseButton::Left && compose::busy(window, cx) {
        window.prevent_default();
    }
}

fn first_run(data: &ChatData, cx: &App) -> bool {
    data.graph()
        .read(cx)
        .snapshot()
        .is_some_and(|snapshot| snapshot.nodes.is_empty())
}

fn narrow(window: &Window) -> bool {
    window.viewport_size().width < px(OVERLAY_WIDTH)
}

#[derive(Default)]
enum Sidebar {
    #[default]
    Default,
    Hidden,
    Overlay,
}

struct StatusOverlay {
    view: Entity<StatusView>,
    at: Point<Pixels>,
    restore: Option<FocusHandle>,
    _events: Subscription,
}

pub(super) struct Chat {
    graph: Entity<Store>,
    data: Entity<ChatData>,
    rooms: Entity<Rooms>,
    stream: Entity<Stream>,
    composer: Entity<Composer>,
    side: Entity<SideSlot>,
    status: Option<StatusOverlay>,
    focus_room: Option<String>,
    sidebar: Sidebar,
    sidebar_focus: FocusHandle,
    members_focus: FocusHandle,
    terminal_focus: FocusHandle,
}

impl Chat {
    pub(super) fn new(graph: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let data = cx.new(|cx| ChatData::new(graph.clone(), cx));
        let rooms = cx.new(|cx| Rooms::new(data.clone(), cx));
        let stream = cx.new(|cx| Stream::new(data.clone(), cx));
        let composer = cx.new(|cx| Composer::new(data.clone(), cx));
        let side = cx.new(|cx| SideSlot::new(data.clone(), cx));
        cx.observe(&data, |_, _, cx| cx.notify()).detach();
        cx.observe(&graph, |_, _, cx| cx.notify()).detach();
        cx.observe(&rooms, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&rooms, window, |this, _, room: &SelectRoom, window, cx| {
            this.close_sidebar_overlay(window, cx);
            this.open_room(&room.0, cx);
        })
        .detach();
        cx.subscribe(&graph, |this, _, state: &LinkState, cx| {
            if *state == LinkState::Connected {
                this.data.update(cx, |data, cx| data.refresh(cx));
            }
        })
        .detach();
        cx.subscribe(&graph, |this, _, agent: &OpenAgent, cx| {
            this.open_room(&format!("agent-{}", agent.0), cx);
        })
        .detach();
        cx.observe(&side, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&stream, window, |this, _, event, window, cx| match event {
            StreamEvent::FollowUp { node, wait } => this.composer.update(cx, |c, cx| {
                c.follow_up(*node, *wait, window, cx);
            }),
            StreamEvent::OpenRoom(room) => this.open_room(room, cx),
            StreamEvent::VisibleWaits(ids) => this.composer.update(cx, |composer, cx| {
                composer.set_visible_waits(ids, cx);
            }),
        })
        .detach();
        cx.subscribe_in(&stream, window, |this, _, quote: &Quote, window, cx| {
            this.composer
                .update(cx, |composer, cx| composer.quote(&quote.0, window, cx));
        })
        .detach();
        cx.subscribe_in(&side, window, |this, _, event, window, cx| match event {
            SideEvent::OpenRoom(room) => {
                if narrow(window) {
                    this.dismiss_side(window, cx);
                }
                this.open_room(room, cx);
            }
            SideEvent::Close => this.dismiss_side(window, cx),
        })
        .detach();

        Self {
            graph,
            data,
            rooms,
            stream,
            composer,
            side,
            status: None,
            focus_room: None,
            sidebar: Sidebar::Default,
            sidebar_focus: cx.focus_handle().tab_stop(true),
            members_focus: cx.focus_handle().tab_stop(true),
            terminal_focus: cx.focus_handle().tab_stop(true),
        }
    }

    fn place<'a>(&'a self, room: &'a ChatRoom, cx: &'a App) -> Option<&'a Place> {
        match &room.kind {
            ChatRoomKind::Work { place } => Some(place),
            ChatRoomKind::Agent { node } => self
                .graph
                .read(cx)
                .snapshot()?
                .nodes
                .iter()
                .find(|agent| agent.id == *node)
                .map(|agent| &agent.place),
            _ => None,
        }
    }

    pub(super) fn repo(&self, cx: &App) -> Option<RepoId> {
        self.place(self.data.read(cx).room()?, cx)
            .map(|place| place.repo)
    }

    fn open_status(
        &mut self,
        node: NodeId,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let restore = window.focused(cx);
        let view = cx.new(|cx| StatusView::new(self.graph.clone(), node.0, window, cx));
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
            this.close_status(window, cx);
            if matches!(event, StatusEvent::History) {
                this.set_side(SideTab::History, window, cx);
            }
        });
        self.status = Some(StatusOverlay {
            view,
            at,
            restore,
            _events: events,
        });
        cx.notify();
    }

    fn close_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(overlay) = self.status.take() {
            if let Some(restore) = overlay.restore {
                window.focus(&restore, cx);
            }
            cx.notify();
        }
    }

    pub(super) fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status.is_none() && !first_run(self.data.read(cx), cx) {
            self.composer
                .update(cx, |composer, cx| composer.open_picker(window, cx));
        }
    }

    pub(super) fn alert_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.status.is_none() && !first_run(self.data.read(cx), cx) {
            self.composer
                .update(cx, |composer, cx| composer.alert_key(event, window, cx));
        }
    }

    fn open_room(&mut self, room: &str, cx: &mut Context<Self>) {
        self.focus_room = Some(room.to_owned());
        self.data
            .update(cx, |data, cx| data.select(room.to_owned(), cx));
        cx.notify();
    }

    pub(super) fn sidebar_visible(&self, window: &Window) -> bool {
        match self.sidebar {
            Sidebar::Default => !narrow(window),
            Sidebar::Hidden => false,
            Sidebar::Overlay => true,
        }
    }

    pub(super) fn sidebar_focus(&self) -> &FocusHandle {
        &self.sidebar_focus
    }

    pub(super) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar = match (narrow(window), self.sidebar_visible(window)) {
            (true, false) => {
                self.side.update(cx, |side, cx| side.hide(cx));
                Sidebar::Overlay
            }
            (false, true) => Sidebar::Hidden,
            _ => Sidebar::Default,
        };
        window.focus(&self.sidebar_focus, cx);
        cx.notify();
    }

    fn close_sidebar_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !matches!(self.sidebar, Sidebar::Overlay) {
            return false;
        }
        self.sidebar = Sidebar::Default;
        window.focus(&self.sidebar_focus, cx);
        cx.notify();
        true
    }

    fn toggle_side(&mut self, tab: SideTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.side.read(cx).tab() == Some(tab) {
            self.dismiss_side(window, cx);
        } else {
            self.set_side(tab, window, cx);
        }
    }

    fn set_side(&mut self, tab: SideTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_sidebar_overlay(window, cx);
        self.side.update(cx, |side, cx| {
            side.show(tab, cx);
            side.focus_tab(window, cx);
        });
    }

    pub(super) fn shortcut(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.status.is_some() || self.rooms.read(cx).blocks_shortcuts(cx) {
            return false;
        }
        let key = event.keystroke.key.as_str();
        if event.is_held {
            return matches!(key, "k" | "n" | "b");
        }
        match key {
            "k" => {
                if first_run(self.data.read(cx), cx) {
                    return true;
                }
                if !self.sidebar_visible(window) {
                    self.toggle_sidebar(window, cx);
                }
                self.rooms
                    .update(cx, |rooms, cx| rooms.focus_search(window, cx));
            }
            "n" => {
                let at = point(px(ROOMS_WIDTH), px(80.0));
                self.rooms
                    .update(cx, |rooms, cx| rooms.new_agent(at, window, cx));
            }
            "b" if self.data.read(cx).room().is_some() && !first_run(self.data.read(cx), cx) => {
                if self.side.read(cx).tab().is_some() {
                    self.dismiss_side(window, cx);
                } else {
                    self.set_side(SideTab::Members, window, cx);
                }
            }
            "b" => {}
            _ => return false,
        }
        true
    }

    pub(super) fn close_narrow_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !narrow(window)
            || self.status.is_some()
            || self.rooms.read(cx).blocks_shortcuts(cx)
            || compose::busy(window, cx)
        {
            return false;
        }
        if self.side.read(cx).tab().is_some() {
            self.dismiss_side(window, cx);
            true
        } else {
            self.close_sidebar_overlay(window, cx)
        }
    }

    fn dismiss_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = if self.side.read(cx).tab() == Some(SideTab::Terminal) {
            &self.terminal_focus
        } else {
            &self.members_focus
        };
        self.side.update(cx, |side, cx| side.hide(cx));
        window.focus(focus, cx);
    }

    fn header(&self, room: &ChatRoom, cx: &mut Context<Self>) -> Div {
        let colors = ui::colors(cx);
        let data = self.data.read(cx);
        let lang = *cx.global::<Lang>();
        let agent = match room.kind {
            ChatRoomKind::Agent { node } => self
                .graph
                .read(cx)
                .snapshot()
                .and_then(|snapshot| snapshot.nodes.iter().find(|agent| agent.id == node)),
            _ => None,
        };
        let place = self.place(room, cx);
        let repo = place.and_then(|place| {
            self.graph
                .read(cx)
                .snapshot()?
                .repos
                .iter()
                .find(|repo| repo.id == place.repo)
        });
        let title = room_title(room, |_| repo, cx).to_owned();
        let workspace = place.zip(repo).and_then(|(place, repo)| {
            let branch = match room.kind {
                ChatRoomKind::Work { .. } if title == repo.name => return None,
                ChatRoomKind::Work { .. } => None,
                _ => repo
                    .worktrees
                    .iter()
                    .find(|tree| tree.path == place.worktree)
                    .and_then(|tree| tree.branch.as_deref()),
            };
            Some(match branch {
                Some(branch) => format!("{} › {branch}", repo.name),
                None => repo.name.clone(),
            })
        });
        let state = agent.map(|agent| {
            let node = agent.id;
            let label = status::chip(&agent.status, lang).map_or_else(
                || words(cx, "내 답을 기다리는 중", "Waiting for your reply").to_owned(),
                |chip| chip.text,
            );
            btn::ghost("chat-agent-status", cx)
                .child(label)
                .on_click(
                    cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        this.open_status(node, event.position(), window, cx);
                    }),
                )
        });
        let working = data.recipients().map_or(0, |catalog| {
            catalog
                .agents
                .iter()
                .filter(|agent| {
                    matches!(agent.status, Status::Working { .. })
                        && room.members.contains(&agent.node)
                })
                .count()
        });
        let members = room.members.len();
        let facts = match room.kind {
            ChatRoomKind::Pinned => {
                let workspaces = self
                    .graph
                    .read(cx)
                    .snapshot()
                    .map_or(0, |snapshot| snapshot.repos.len());
                match lang {
                    Lang::Ko => {
                        format!("작업공간 {workspaces} · 에이전트 {members} · 일하는 중 {working}")
                    }
                    Lang::En => {
                        format!("Workspaces: {workspaces} · Agents: {members} · Working: {working}")
                    }
                }
            }
            ChatRoomKind::Agent { .. } => {
                if room.archived {
                    words(cx, "끝난 에이전트", "Archived agent").to_owned()
                } else {
                    agent
                        .and_then(|agent| agent.agent.as_ref())
                        .map_or_else(String::new, |agent| agent.0.clone())
                }
            }
            _ => match lang {
                Lang::Ko => format!("구성원 {members} · 일하는 중 {working}"),
                Lang::En => format!("Members: {members} · Working: {working}"),
            },
        };
        let room_icon = match room.kind {
            ChatRoomKind::Pinned => Lucide::Mail,
            ChatRoomKind::Work { .. } if repo.is_none_or(|repo| repo.git) => Lucide::GitBranch,
            ChatRoomKind::Work { .. } => Lucide::Folder,
            _ => Lucide::MessageCircle,
        };
        let show_working = agent.is_none() && working > 0;
        let actions = self
            .rooms
            .update(cx, |rooms, cx| rooms.room_actions(room, cx))
            .map(|button| button.size(px(28.0)));
        let selected = self.side.read(cx).tab();
        let side_buttons = [SideTab::Members, SideTab::Terminal].map(|tab| {
            let active = selected == Some(tab);
            let label = tab.label(cx);
            let focus = if tab == SideTab::Members {
                &self.members_focus
            } else {
                &self.terminal_focus
            };
            btn::ghost(("chat-side-toggle", tab as usize), cx)
                .track_focus(focus)
                .aria_label(label)
                .aria_expanded(active)
                .tooltip(tip(label.to_owned()))
                .h(px(28.0))
                .min_w(px(30.0))
                .px(px(7.0))
                .gap(px(4.0))
                .justify_center()
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(SMALL))
                .when(active, |button| button.bg(colors.accent_wash))
                .child(icon(tab.icon(), px(16.0), colors.ink_2))
                .when(tab == SideTab::Members, |button| {
                    button.child(members.to_string())
                })
                .on_click(cx.listener(move |this, _, window, cx| this.toggle_side(tab, window, cx)))
        });
        div()
            .flex_none()
            .flex()
            .flex_nowrap()
            .items_center()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(9.0))
            .bg(colors.panel)
            .border_b_1()
            .border_color(colors.line)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(icon(room_icon, px(16.0), colors.ink_2))
                    .child(
                        div()
                            .min_w_0()
                            .text_size(px(TEXT))
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(title),
                    )
                    .children(state)
                    .when(!facts.is_empty(), |row| {
                        row.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .min_w_0()
                                .text_size(px(SMALL))
                                .text_color(colors.ink_2)
                                .when(show_working, |facts| {
                                    facts.child(
                                        div()
                                            .size(px(7.0))
                                            .rounded_full()
                                            .flex_none()
                                            .bg(colors.run),
                                    )
                                })
                                .child(div().truncate().child(facts)),
                        )
                    })
                    .children(workspace.map(|label| {
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(SMALL))
                            .text_color(colors.ink_2)
                            .child(label)
                    })),
            )
            .children(actions)
            .child(div().flex().flex_none().gap(px(2.0)).children(side_buttons))
    }

    fn stopped(&self, room: &ChatRoom, cx: &Context<Self>) -> Option<Div> {
        let ChatRoomKind::Agent { node } = room.kind else {
            return None;
        };
        let snapshot = self.graph.read(cx).snapshot()?;
        let agent = snapshot.nodes.iter().find(|agent| agent.id == node)?;
        if !matches!(agent.process, ProcessState::Stopped { .. }) {
            return None;
        }
        let colors = ui::colors(cx);
        let parent = agent
            .parent
            .and_then(|parent| snapshot.nodes.iter().find(|node| node.id == parent));
        let cause = status::line(
            agent,
            parent.map(|node| node.name.as_str()),
            *cx.global::<Lang>(),
        )
        .unwrap_or_else(|| {
            words(cx, "에이전트가 꺼져 있습니다.", "The agent has stopped.").to_owned()
        });
        let recovery = if agent.relaunch.is_some() {
            btn::button("chat-relaunch", cx)
                .child(say(cx, Str::NodeRelaunch))
                .on_click(cx.listener(move |this, _, _, cx| {
                    super::cardlink::run(
                        &this.graph,
                        cx,
                        move |base| super::cardlink::relaunch(base, node.0),
                        |this, result, cx| {
                            if let Err(error) = result {
                                this.graph.update(cx, |store, cx| store.tell(error, cx));
                            }
                        },
                    )
                    .detach();
                }))
        } else {
            btn::button("chat-start-new-agent", cx)
                .child(words(cx, "새 에이전트 시작", "Start a new agent"))
                .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                    this.rooms.update(cx, |rooms, cx| {
                        rooms.new_agent(event.position(), window, cx)
                    });
                }))
        };
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .px(px(16.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(colors.line)
                .bg(colors.panel)
                .text_size(px(TEXT))
                .text_color(colors.ink_2)
                .child(div().flex_1().min_w_0().child(cause))
                .child(
                    btn::button("chat-stopped-terminal", cx)
                        .child(words(cx, "터미널 보기", "Show terminal"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.side.update(cx, |side, cx| {
                                side.inspect_terminal(node, cx);
                                side.focus_tab(window, cx);
                            });
                        })),
                )
                .child(recovery),
        )
    }

    fn conversation(&self, cx: &mut Context<Self>) -> Div {
        let colors = ui::colors(cx);
        let data = self.data.read(cx);
        let screen = div().flex().flex_col().flex_1().min_w_0().min_h_0();
        let room = data.room().cloned();
        let first = first_run(data, cx);
        let connection = match self.graph.read(cx).connection() {
            LinkState::Connected => None,
            LinkState::Reconnecting => Some(words(
                cx,
                "loom 서비스에 다시 연결합니다.",
                "Reconnecting to the loom service…",
            )),
            LinkState::Lost => Some(words(
                cx,
                "loom 서비스와 연결이 끊겼습니다.",
                "Connection to the loom service lost.",
            )),
        };
        let fault = connection.or_else(|| data.error()).map(|error| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(16.0))
                .py(px(6.0))
                .text_size(px(TEXT))
                .text_color(colors.err)
                .child(div().flex_1().child(error.to_owned()))
                .child(
                    btn::button("chat-reconnect", cx)
                        .child(if connection.is_some() {
                            words(cx, "다시 연결", "Reconnect")
                        } else {
                            say(cx, Str::StatusBarRefresh)
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            let has_base = this.graph.read(cx).base().is_some();
                            this.graph.update(cx, |store, cx| store.reload(cx));
                            if has_base {
                                this.data.update(cx, |data, cx| data.refresh(cx));
                            }
                        })),
                )
        });
        if first {
            let welcome = self.rooms.update(cx, |rooms, cx| rooms.welcome(cx));
            return screen.children(fault).child(welcome);
        }
        let Some(room) = room else {
            return screen.children(fault).child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(TEXT))
                    .text_color(colors.ink_2)
                    .child(if data.loading() {
                        words(cx, "방을 읽는 중입니다.", "Loading room…")
                    } else {
                        words(
                            cx,
                            "방 목록에서 대화를 고릅니다.",
                            "Select a conversation from the room list.",
                        )
                    }),
            );
        };
        let fallback = match data.transcript() {
            Some(ChatTranscript::Fallback { reason }) if !room.archived => Some(reason.clone()),
            _ => None,
        };
        screen
            .child(self.header(&room, cx))
            .children(fault)
            .children(self.stopped(&room, cx))
            .when(room.archived, |screen| {
                screen.child(
                    div()
                        .flex_none()
                        .px(px(16.0))
                        .py(px(8.0))
                        .bg(colors.panel)
                        .text_color(colors.ink_2)
                        .text_size(px(TEXT))
                        .child(words(
                            cx,
                            "끝난 방입니다. 대화 기록을 읽을 수 있습니다.",
                            "This room is archived. You can read its history.",
                        )),
                )
            })
            .children(fallback.map(|reason| {
                div()
                    .id("chat-transcript-fallback")
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .px(px(16.0))
                    .py(px(6.0))
                    .bg(colors.panel)
                    .text_size(px(SMALL))
                    .text_color(colors.ink_2)
                    .child(
                        div()
                            .id("chat-transcript-fallback-reason")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .tooltip(tip(reason.clone()))
                            .child(reason),
                    )
                    .child(
                        btn::ghost("chat-fallback-terminal", cx)
                            .flex_none()
                            .child(words(cx, "터미널 열기", "Open terminal"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_side(SideTab::Terminal, window, cx)
                            })),
                    )
            }))
            .child(self.stream.clone())
            .child(self.composer.clone())
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = narrow(window);
        if !narrow && matches!(self.sidebar, Sidebar::Overlay) {
            self.sidebar = Sidebar::Default;
        }
        let first = first_run(self.data.read(cx), cx);
        if first && self.side.read(cx).tab().is_some() {
            self.side.update(cx, |side, cx| side.hide(cx));
        }
        if let Some(wanted) = &self.focus_room {
            let data = self.data.read(cx);
            if data.selected() != Some(wanted.as_str()) {
                self.focus_room = None;
            } else if !first && let Some(room) = data.room() {
                let focus = !room.archived;
                if focus && compose::busy(window, cx) {
                    compose::retry(window, cx);
                } else {
                    self.focus_room = None;
                    if focus {
                        self.composer
                            .update(cx, |composer, cx| composer.focus(window, cx));
                    }
                }
            }
        }
        let colors = ui::colors(cx);
        let side_open = self.side.read(cx).tab().is_some();
        let sidebar_open = self.sidebar_visible(window);
        let sidebar = sidebar_open.then(|| {
            div()
                .flex_none()
                .w(px(ROOMS_WIDTH))
                .h_full()
                .border_r_1()
                .border_color(colors.line)
                .when(narrow, |sidebar| {
                    sidebar.absolute().left_0().top_0().shadow_lg().occlude()
                })
                .child(self.rooms.update(cx, |rooms, cx| rooms.sidebar(cx)))
        });
        let (docked, overlay) = if narrow {
            (None, sidebar)
        } else {
            (sidebar, None)
        };
        div()
            .relative()
            .size_full()
            .flex()
            .children(docked)
            .child(self.conversation(cx))
            .when(narrow && (side_open || sidebar_open), |view| {
                view.child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(colors.scrim)
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                if !this.close_sidebar_overlay(window, cx) {
                                    this.dismiss_side(window, cx);
                                }
                                cx.stop_propagation();
                            }),
                        ),
                )
            })
            .children(overlay)
            .when(side_open, |view| {
                view.child(
                    div()
                        .flex_none()
                        .w(px(SIDE_WIDTH))
                        .h_full()
                        .border_l_1()
                        .border_color(colors.line)
                        .bg(colors.panel)
                        .when(narrow, |slot| {
                            slot.absolute().top_0().right_0().shadow_lg().occlude()
                        })
                        .child(self.side.clone()),
                )
            })
            .child(
                self.rooms
                    .update(cx, |rooms, cx| rooms.overlays(window, cx)),
            )
            .children(self.status.as_ref().map(|overlay| {
                pop::overlay(
                    overlay.at,
                    overlay.view.clone(),
                    cx.listener(|this, _, window, cx| this.close_status(window, cx)),
                    window,
                )
            }))
    }
}
