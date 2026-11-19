mod composer;
mod data;
mod rooms;
mod side;
mod stream;

use api::graph::chat::{ChatMessage, ChatOrigin, ChatRoom, ChatRoomKind, ChatTranscript};
use api::graph::{Place, ProcessState, Status};
use api::ids::{NodeId, RepoId};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClipboardItem, Context, Div, Entity, FocusHandle, FontWeight, IntoElement,
    KeyDownEvent, ParentElement, Pixels, Point, Render, StatefulInteractiveElement, Styled,
    Subscription, Window, div,
};

use crate::link::LinkState;
use crate::scale::px;
use crate::store::{OpenAgent, Store};
use crate::strings::Lang;

use super::compose;
use super::icons::{Lucide, icon};
use super::status;
use super::status::pop::{StatusEvent, StatusView};
use super::ui::{self, SMALL, TEXT};
use super::widgets::btn;
use super::workspace::pop;
use composer::Composer;
use data::ChatData;
use rooms::Rooms;
use side::{SessionTerminal, SideEvent, SideSlot, SideTab};
use stream::{Quote, Stream, StreamEvent};

const ROOMS_WIDTH: f32 = 270.0;
const SIDE_WIDTH: f32 = 334.0;
const RAIL_WIDTH: f32 = 60.0;
const MIN_ROOM_WIDTH: f32 = 350.0;
fn message_text(message: &ChatMessage) -> &str {
    const OUTSIDE_INTRO: &str = "An agent outside loom that calls itself ";
    const REPLY_INSTRUCTION: &str =
        "\nReply in the room with loom answer <this message id> \"<reply>\".";
    let text = &message.text;
    if message.origin == ChatOrigin::Outside
        && text.starts_with(OUTSIDE_INTRO)
        && let Some((_, body)) = text.split_once('\n')
        && let Some(body) = body.strip_suffix(REPLY_INSTRUCTION)
    {
        body
    } else {
        text
    }
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
    terminal: Entity<SessionTerminal>,
    status: Option<StatusOverlay>,
    focus_room: Option<String>,
}

impl Chat {
    pub(super) fn new(graph: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let data = cx.new(|cx| ChatData::new(graph.clone(), cx));
        let rooms = cx.new(|cx| Rooms::new(data.clone(), cx));
        let stream = cx.new(|cx| Stream::new(data.clone(), cx));
        let composer = cx.new(|cx| Composer::new(data.clone(), cx));
        let side = cx.new(|cx| SideSlot::new(data.clone(), cx));
        let terminal = cx.new(|cx| SessionTerminal::new(graph.clone(), cx));

        cx.observe(&data, |this, _, cx| this.sync(cx)).detach();
        cx.observe(&graph, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&graph, |this, _, agent: &OpenAgent, cx| {
            let room = format!("agent-{}", agent.0);
            this.focus_room = Some(room.clone());
            this.open_room(&room, cx);
        })
        .detach();
        cx.observe(&side, |this, _, cx| this.sync(cx)).detach();
        cx.subscribe_in(&stream, window, |this, _, event, window, cx| match event {
            StreamEvent::FollowUp { node, wait } => this.composer.update(cx, |c, cx| {
                c.follow_up(*node, *wait, window, cx);
            }),
            StreamEvent::OpenRoom(room) => this.open_room(room, cx),
        })
        .detach();
        cx.subscribe_in(&stream, window, |this, _, quote: &Quote, window, cx| {
            this.composer
                .update(cx, |composer, cx| composer.quote(&quote.0, window, cx));
        })
        .detach();
        cx.subscribe(&side, |this, _, event, cx| match event {
            SideEvent::Close => this.toggle_side(cx),
            SideEvent::OpenRoom(room) => this.open_room(room, cx),
        })
        .detach();

        Self {
            graph,
            data,
            rooms,
            stream,
            composer,
            side,
            terminal,
            status: None,
            focus_room: None,
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
                this.set_side(SideTab::History, cx);
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
        if self.status.is_none() {
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
        if self.status.is_none() {
            self.composer
                .update(cx, |composer, cx| composer.alert_key(event, window, cx));
        }
    }

    fn open_room(&mut self, room: &str, cx: &mut Context<Self>) {
        self.data
            .update(cx, |data, cx| data.select(room.to_owned(), cx));
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let node = self.fallback(cx).map(|(node, _)| node);
        self.terminal.update(cx, |view, cx| view.set_node(node, cx));
        cx.notify();
    }

    fn fallback<'a>(&'a self, cx: &'a App) -> Option<(NodeId, &'a str)> {
        if self.side.read(cx).tab() == Some(SideTab::Terminal) {
            return None;
        }
        let data = self.data.read(cx);
        let room = data.room().filter(|room| !room.archived)?;
        let ChatRoomKind::Agent { node } = room.kind else {
            return None;
        };
        match data.transcript()? {
            ChatTranscript::Fallback { reason } => Some((node, reason)),
            ChatTranscript::Available => None,
        }
    }

    fn toggle_side(&mut self, cx: &mut Context<Self>) {
        if self.side.read(cx).tab().is_some() {
            self.side.update(cx, |side, cx| side.hide(cx));
        } else {
            self.set_side(SideTab::Members, cx);
        }
    }

    fn set_side(&mut self, tab: SideTab, cx: &mut Context<Self>) {
        self.side.update(cx, |side, cx| side.show(tab, cx));
    }

    fn copy_address(&self, cx: &mut Context<Self>) {
        let Some((base, path)) = self
            .graph
            .read(cx)
            .base()
            .zip(self.data.read(cx).room().map(|room| &room.outside))
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(format!("{base}{path}")));
    }

    fn header(&self, room: &ChatRoom, cx: &mut Context<Self>) -> Div {
        let colors = ui::colors(cx);
        let data = self.data.read(cx);
        let agent = match room.kind {
            ChatRoomKind::Agent { node } => self
                .graph
                .read(cx)
                .snapshot()
                .and_then(|snapshot| snapshot.nodes.iter().find(|agent| agent.id == node)),
            _ => None,
        };
        let workspace = self.place(room, cx).and_then(|place| {
            let repo = self
                .graph
                .read(cx)
                .snapshot()?
                .repos
                .iter()
                .find(|repo| repo.id == place.repo)?;
            let branch = repo
                .worktrees
                .iter()
                .find(|tree| tree.path == place.worktree)
                .and_then(|tree| tree.branch.as_deref());
            Some(match branch {
                Some(branch) => format!("{} › {branch}", repo.name),
                None => repo.name.clone(),
            })
        });
        let state = agent.map(|agent| {
            let node = agent.id;
            let label = status::chip(&agent.status, Lang::Ko)
                .map_or_else(|| "내 답을 기다리는 중".to_owned(), |chip| chip.text);
            btn::ghost("chat-agent-status", cx)
                .child(label)
                .on_click(
                    cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        this.open_status(node, event.position(), window, cx);
                    }),
                )
        });
        let facts = match room.kind {
            ChatRoomKind::Pinned => {
                let workspaces = self
                    .graph
                    .read(cx)
                    .snapshot()
                    .map_or(0, |snapshot| snapshot.repos.len());
                let working = data.recipients().map_or(0, |catalog| {
                    catalog
                        .agents
                        .iter()
                        .filter(|agent| matches!(agent.status, Status::Working { .. }))
                        .count()
                });
                format!(
                    "모든 작업공간 · 작업공간 {workspaces} · 에이전트 {} · 일하는 중 {working}",
                    room.members.len()
                )
            }
            ChatRoomKind::Agent { .. } => {
                if room.archived {
                    "끝난 에이전트".to_owned()
                } else {
                    let harness = agent
                        .and_then(|agent| agent.agent.as_ref())
                        .map(|agent| agent.0.as_str());
                    let transcript =
                        matches!(data.transcript(), Some(ChatTranscript::Fallback { .. }));
                    [harness, transcript.then_some("터미널 본문")]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ")
                }
            }
            _ => {
                let working = data.recipients().map_or(0, |r| {
                    r.agents
                        .iter()
                        .filter(|a| {
                            matches!(a.status, Status::Working { .. })
                                && room.members.contains(&a.node)
                        })
                        .count()
                });
                format!("구성원 {} · 일하는 중 {}", room.members.len(), working)
            }
        };
        let agent_actions = match room.kind {
            ChatRoomKind::Agent { node } => Some(
                self.rooms
                    .update(cx, |rooms, cx| rooms.agent_actions(node, cx)),
            ),
            _ => None,
        };
        let side_open = self.side.read(cx).tab().is_some();
        div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(colors.line)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(17.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(room.name.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(px(SMALL))
                            .text_color(colors.ink_2)
                            .children(state)
                            .child(div().truncate().child(facts))
                            .children(workspace.map(|label| div().truncate().child(label))),
                    ),
            )
            .children(agent_actions)
            .when(!room.archived, |row| {
                row.child(
                    btn::button("chat-copy-outside", cx)
                        .child("외부 주소 복사")
                        .on_click(cx.listener(|this, _, _, cx| this.copy_address(cx))),
                )
            })
            .child(
                btn::button("chat-side-toggle", cx)
                    .aria_expanded(side_open)
                    .child(icon(Lucide::PanelRightOpen, px(14.0), colors.ink_2))
                    .child(if side_open {
                        "옆 칸 닫기"
                    } else {
                        "옆 칸"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_side(cx))),
            )
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
        let relaunch = agent.relaunch.is_some().then(|| {
            btn::button("chat-relaunch", cx)
                .child("다시 띄우기")
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
        });
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
                .child("에이전트가 꺼져 있습니다.")
                .children(relaunch),
        )
    }

    fn conversation(&self, cx: &mut Context<Self>) -> Div {
        let colors = ui::colors(cx);
        let data = self.data.read(cx);
        let screen = div().flex().flex_col().flex_1().min_w_0().min_h_0();
        let room = data.room().cloned();
        let connection = match self.graph.read(cx).connection() {
            LinkState::Connected => None,
            LinkState::Reconnecting => Some("데몬에 다시 연결합니다."),
            LinkState::Lost => Some("데몬과 연결이 끊겼습니다."),
        };
        let fault = data.error().or(connection).map(|error| {
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
                        .child("다시 읽기")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.data.update(cx, |data, cx| data.refresh(cx));
                        })),
                )
        });
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
                        "방을 읽는 중입니다."
                    } else {
                        "방 목록에서 대화를 고릅니다."
                    }),
            );
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
                        .child("끝난 방입니다. 대화 기록을 읽을 수 있습니다."),
                )
            })
            .map(|screen| match self.fallback(cx) {
                Some((_, reason)) => screen
                    .child(
                        div()
                            .px(px(16.0))
                            .py(px(8.0))
                            .flex_none()
                            .bg(colors.panel)
                            .text_size(px(SMALL))
                            .text_color(colors.ink_2)
                            .child(format!("{reason} 터미널에서 직접 입력할 수 있습니다.")),
                    )
                    .child(div().flex_1().min_h_0().child(self.terminal.clone())),
                None => screen.child(self.stream.clone()),
            })
            .child(self.composer.clone())
    }

    fn closed_rail(&self, cx: &Context<Self>) -> Div {
        let colors = ui::colors(cx);
        div()
            .flex_none()
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .p(px(4.0))
            .bg(colors.panel)
            .border_l_1()
            .border_color(colors.line)
            .children(SideTab::ALL.into_iter().enumerate().map(|(i, tab)| {
                let label = tab.label(cx);
                btn::ghost(("chat-side-rail", i), cx)
                    .aria_label(label)
                    .justify_center()
                    .px(px(0.0))
                    .child(icon(tab.icon(), px(15.0), colors.ink_2))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_side(tab, cx)))
            }))
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(wanted) = &self.focus_room {
            let data = self.data.read(cx);
            if data.selected() != Some(wanted.as_str()) {
                self.focus_room = None;
            } else if data.room().is_some() {
                if compose::busy(window, cx) {
                    compose::retry(window, cx);
                } else {
                    self.focus_room = None;
                    self.composer
                        .update(cx, |composer, cx| composer.focus(window, cx));
                }
            }
        }
        let colors = ui::colors(cx);
        let width = window.viewport_size().width;
        let side_width = px(SIDE_WIDTH).min(width * 0.34);
        let has_room = self.data.read(cx).room().is_some();
        let side_open = self.side.read(cx).tab().is_some();
        let reserve = if side_open {
            side_width
        } else {
            px(RAIL_WIDTH)
        };
        let room_width = px(ROOMS_WIDTH)
            .min(width * 0.29)
            .min((width - reserve - px(MIN_ROOM_WIDTH)).max(width * 0.21));
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .flex_none()
                    .w(room_width)
                    .h_full()
                    .border_r_1()
                    .border_color(colors.line)
                    .bg(colors.panel)
                    .child(self.rooms.clone()),
            )
            .child(self.conversation(cx))
            .when(side_open, |view| {
                view.child(
                    div()
                        .flex_none()
                        .w(side_width)
                        .h_full()
                        .border_l_1()
                        .border_color(colors.line)
                        .bg(colors.panel)
                        .child(self.side.clone()),
                )
            })
            .when(!side_open && has_room, |view| {
                view.child(self.closed_rail(cx))
            })
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
