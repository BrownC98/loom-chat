mod composer;
mod data;
mod rooms;
mod side;
mod stream;

use api::graph::Status;
use api::graph::chat::{ChatMessage, ChatOrigin, ChatRoom, ChatRoomKind, ChatTranscript};
use api::ids::NodeId;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClipboardItem, Context, Div, Entity, EventEmitter, FontWeight, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div,
};

use crate::link::LinkState;
use crate::scale::px;
use crate::store::Store;
use crate::strings::Lang;

use super::icons::{Lucide, icon};
use super::inputs::Inputs;
use super::status;
use super::ui::{self, SMALL, TEXT};
use super::widgets::{btn, segmented_fit};
use composer::Composer;
use data::ChatData;
use rooms::Rooms;
use side::{SessionTerminal, SideEvent, SideSlot, SideTab};
use stream::{Stream, StreamEvent};

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Placement {
    Hidden,
    Graph,
    Full,
}

#[derive(Clone, Copy, PartialEq)]
enum GraphTab {
    Room,
    Side(SideTab),
}

pub(super) enum ChatEvent {
    OpenFull,
    CloseGraphRoom,
    GoGraph(Vec<u32>),
}

pub(super) struct Chat {
    graph: Entity<Store>,
    data: Entity<ChatData>,
    rooms: Entity<Rooms>,
    stream: Entity<Stream>,
    composer: Entity<Composer>,
    side: Entity<SideSlot>,
    terminal: Entity<SessionTerminal>,
    placement: Placement,
    full_side: Option<SideTab>,
    graph_tab: GraphTab,
}

impl EventEmitter<ChatEvent> for Chat {}

impl Chat {
    pub(super) fn new(graph: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let inputs = cx.new(|_| Inputs::default());
        let data = cx.new(|cx| ChatData::new(graph.clone(), cx));
        let rooms = cx.new(|cx| Rooms::new(data.clone(), cx));
        let stream = cx.new(|cx| Stream::new(data.clone(), cx));
        let composer = cx.new(|cx| Composer::new(data.clone(), cx));
        let side = cx.new(|cx| SideSlot::new(data.clone(), inputs.clone(), cx));
        let terminal = cx.new(|cx| SessionTerminal::new(graph.clone(), inputs, cx));

        cx.observe(&data, |this, _, cx| this.sync(cx)).detach();
        cx.observe(&graph, |this, _, cx| {
            if this.placement != Placement::Hidden {
                cx.notify();
            }
        })
        .detach();
        cx.observe(&side, |this, _, cx| this.sync(cx)).detach();
        cx.subscribe_in(&stream, window, |this, _, event, window, cx| match event {
            StreamEvent::FollowUp { node, wait } => this.composer.update(cx, |c, cx| {
                c.follow_up(*node, *wait, window, cx);
            }),
            StreamEvent::OpenRoom(room) => this.open_full_room(room, cx),
        })
        .detach();
        cx.subscribe(&side, |this, _, event, cx| match event {
            SideEvent::Close if this.placement == Placement::Graph => {
                this.set_graph_tab(GraphTab::Room, cx);
            }
            SideEvent::Close => this.toggle_side(cx),
            SideEvent::OpenRoom(room) => this.open_full_room(room, cx),
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
            placement: Placement::Hidden,
            full_side: None,
            graph_tab: GraphTab::Room,
        }
    }

    pub(super) fn placement(&self) -> Placement {
        self.placement
    }

    pub(super) fn place(&mut self, placement: Placement, cx: &mut Context<Self>) {
        if self.placement == placement {
            return;
        }
        if self.placement == Placement::Full {
            self.full_side = self.side.read(cx).tab();
        }
        self.placement = placement;
        if placement == Placement::Hidden {
            self.data.update(cx, |data, cx| data.deactivate(cx));
        } else {
            self.data.update(cx, |data, cx| data.activate(cx));
        }
        self.sync_side(cx);
        self.sync(cx);
    }

    pub(super) fn preview(&mut self, node: NodeId, cx: &mut Context<Self>) {
        self.data.update(cx, |data, cx| data.preview(node, cx));
        self.graph_tab = GraphTab::Room;
        self.place(Placement::Graph, cx);
        self.sync_side(cx);
        self.sync(cx);
    }

    pub(super) fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.placement == Placement::Hidden
            || self.placement == Placement::Graph && self.graph_tab != GraphTab::Room
        {
            return;
        }
        self.composer.update(cx, |c, cx| c.open_picker(window, cx));
    }

    fn open_full_room(&mut self, room: &str, cx: &mut Context<Self>) {
        self.data
            .update(cx, |data, cx| data.select(room.to_owned(), cx));
        if self.placement == Placement::Graph {
            cx.emit(ChatEvent::OpenFull);
        }
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        if self.placement != Placement::Hidden {
            let node = self.fallback(cx).map(|(node, _)| node);
            self.terminal.update(cx, |view, cx| view.set_node(node, cx));
            cx.notify();
        } else {
            self.terminal.update(cx, |view, cx| view.set_node(None, cx));
        }
    }

    fn sync_side(&mut self, cx: &mut Context<Self>) {
        self.side.update(cx, |side, cx| {
            if let (Placement::Graph, GraphTab::Side(tab)) = (self.placement, self.graph_tab) {
                side.show_body(tab, cx);
            } else if let (Placement::Full, Some(tab)) = (self.placement, self.full_side) {
                side.show(tab, cx);
            } else {
                side.hide(cx);
            }
        });
    }

    fn fallback<'a>(&'a self, cx: &'a App) -> Option<(NodeId, &'a str)> {
        if self.placement == Placement::Graph && self.graph_tab != GraphTab::Room
            || self.placement == Placement::Full
                && self.side.read(cx).tab() == Some(SideTab::Terminal)
        {
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
        self.set_side(self.full_side.is_none().then_some(SideTab::Members), cx);
    }

    fn set_side(&mut self, tab: Option<SideTab>, cx: &mut Context<Self>) {
        self.full_side = tab;
        self.sync_side(cx);
        self.sync(cx);
    }

    fn set_graph_tab(&mut self, tab: GraphTab, cx: &mut Context<Self>) {
        self.graph_tab = tab;
        self.sync_side(cx);
        self.sync(cx);
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

    fn header(&self, room: &ChatRoom, cx: &Context<Self>) -> Div {
        let colors = ui::colors(cx);
        let data = self.data.read(cx);
        let agent = match room.kind {
            ChatRoomKind::Agent { node } => data
                .recipients()
                .and_then(|r| r.agents.iter().find(|a| a.node == node)),
            _ => None,
        };
        let facts = match room.kind {
            ChatRoomKind::Pinned => format!(
                "에이전트 {} · 질문 {} · 완료 {}",
                room.members.len(),
                room.questions,
                room.done
            ),
            ChatRoomKind::Agent { .. } => {
                if room.archived {
                    "끝난 에이전트".to_owned()
                } else {
                    let harness = agent.and_then(|a| a.harness.as_ref()).map(|a| a.0.as_str());
                    let state = agent.map(|a| {
                        status::chip(&a.status, Lang::Ko)
                            .map_or_else(|| "내 답을 기다리는 중".to_owned(), |chip| chip.text)
                    });
                    let transcript =
                        matches!(data.transcript(), Some(ChatTranscript::Fallback { .. }));
                    [
                        harness,
                        state.as_deref(),
                        transcript.then_some("터미널 본문"),
                    ]
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
        let pinned = matches!(room.kind, ChatRoomKind::Pinned);
        let full = self.placement == Placement::Full;
        let graph = self.graph.read(cx);
        let can_show = !pinned
            && graph.snapshot().is_some_and(|snapshot| {
                snapshot
                    .nodes
                    .iter()
                    .any(|node| room.members.contains(&node.id))
            });
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
                            .text_size(px(SMALL))
                            .text_color(colors.ink_2)
                            .child(facts),
                    ),
            )
            .when(!room.archived, |row| {
                row.child(
                    btn::button("chat-copy-outside", cx)
                        .child("외부 주소 복사")
                        .on_click(cx.listener(|this, _, _, cx| this.copy_address(cx))),
                )
            })
            .when(full && can_show, |row| {
                row.child(
                    btn::button("chat-show-graph", cx)
                        .child("그래프에서 보기")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(room) = this.data.read(cx).room() {
                                let nodes = room.members.iter().map(|node| node.0).collect();
                                cx.emit(ChatEvent::GoGraph(nodes));
                            }
                        })),
                )
            })
            .when(full, |row| {
                row.child(
                    btn::button("chat-side-toggle", cx)
                        .aria_expanded(self.full_side.is_some())
                        .child(icon(Lucide::PanelRightOpen, px(14.0), colors.ink_2))
                        .child(if self.full_side.is_some() {
                            "옆 칸 닫기"
                        } else {
                            "옆 칸"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_side(cx))),
                )
            })
    }

    fn conversation(&self, cx: &Context<Self>) -> Div {
        let colors = ui::colors(cx);
        let data = self.data.read(cx);
        let screen = div().flex().flex_col().flex_1().min_w_0().min_h_0();
        let room = data.room();
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
            .child(self.header(room, cx))
            .children(fault)
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

    fn graph_preview(&self, cx: &Context<Self>) -> Div {
        let colors = ui::colors(cx);
        let entity = cx.entity();
        let tabs = segmented_fit(
            "chat-graph-tabs",
            SideTab::ALL
                .into_iter()
                .map(|tab| (GraphTab::Side(tab), SharedString::from(tab.label(cx))))
                .chain([(GraphTab::Room, SharedString::from("방"))])
                .collect(),
            self.graph_tab,
            move |tab, _, cx| entity.update(cx, |this, cx| this.set_graph_tab(tab, cx)),
            cx,
        );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .bg(colors.panel)
                    .border_b_1()
                    .border_color(colors.line)
                    .child(
                        btn::button("chat-expand-preview", cx)
                            .child("채팅에서 열기")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::OpenFull))),
                    )
                    .child(
                        btn::ghost("chat-close-preview", cx)
                            .child("방 닫기")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(ChatEvent::CloseGraphRoom)),
                            ),
                    ),
            )
            .child(div().flex_none().p(px(8.0)).child(tabs))
            .when(self.graph_tab == GraphTab::Room, |view| {
                view.child(self.conversation(cx))
            })
            .when(self.graph_tab != GraphTab::Room, |view| {
                view.child(div().flex_1().min_h_0().child(self.side.clone()))
            })
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
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_side(Some(tab), cx)))
            }))
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.placement == Placement::Graph {
            return self.graph_preview(cx);
        }
        let colors = ui::colors(cx);
        let width = window.viewport_size().width;
        let side_width = px(SIDE_WIDTH).min(width * 0.34);
        let has_room = self.data.read(cx).room().is_some();
        let reserve = if self.full_side.is_some() {
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
            .when(self.full_side.is_some(), |view| {
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
            .when(self.full_side.is_none() && has_room, |view| {
                view.child(self.closed_rail(cx))
            })
    }
}
