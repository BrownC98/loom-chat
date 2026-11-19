use api::graph::GraphEvent;
use api::graph::chat::ChatRoom;
use gpui::{
    App, AppContext, Context, Div, Entity, EventEmitter, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, Role,
    StatefulInteractiveElement, Styled, Subscription, UniformListScrollHandle, Window, div,
    transparent_black,
};

use super::data::ChatData;
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::icons::{Lucide, icon};
use crate::views::inputs::Inputs;
use crate::views::scrollbar::{Bar, Track};
use crate::views::tooltip::tip;
use crate::views::ui::{self, SMALL};
use crate::views::widgets::btn;

mod members;
mod panels;
mod session;

use members::MemberKey;
use panels::Panel;
pub use session::SessionTerminal;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SideTab {
    Members,
    Terminal,
    Docs,
    History,
}

impl SideTab {
    pub(super) const ALL: [Self; 4] = [Self::Members, Self::Terminal, Self::Docs, Self::History];

    pub(super) fn label(self, cx: &App) -> &'static str {
        match self {
            Self::Members => words(cx, "구성원", "Members"),
            Self::Terminal => ui::say(cx, Str::DockTerminal),
            Self::Docs => ui::say(cx, Str::DocsTitle),
            Self::History => words(cx, "기록", "History"),
        }
    }

    fn icon(self) -> Lucide {
        match self {
            Self::Members => Lucide::Workflow,
            Self::Terminal => Lucide::SquareTerminal,
            Self::Docs => Lucide::FileText,
            Self::History => Lucide::AlignLeft,
        }
    }
}

pub enum SideEvent {
    Close,
    OpenRoom(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Header {
    Tabs,
    External,
}

pub struct SideSlot {
    data: Entity<ChatData>,
    terminal: Entity<SessionTerminal>,
    panel: Option<Panel>,
    header: Header,
    room: Option<String>,
    member: Option<MemberKey>,
    choosing: bool,
    copied: bool,
    scroll: Bar,
    member_scroll: Bar<UniformListScrollHandle>,
    tab_focus: [FocusHandle; SideTab::ALL.len()],
    _subscriptions: [Subscription; 3],
}

impl EventEmitter<SideEvent> for SideSlot {}

impl SideSlot {
    pub fn new(data: Entity<ChatData>, inputs: Entity<Inputs>, cx: &mut Context<Self>) -> Self {
        let graph = data.read(cx).graph().clone();
        let terminal = cx.new(|cx| SessionTerminal::new(graph.clone(), inputs, cx));
        let subscriptions = [
            cx.observe(&data, |this, _, cx| {
                if this.panel.is_some() {
                    this.sync_room(cx);
                    cx.notify();
                }
            }),
            cx.observe(&graph, |this, _, cx| {
                if this.panel.is_some() {
                    cx.notify();
                }
            }),
            cx.subscribe(&graph, |this, graph, event: &GraphEvent, cx| {
                if let (Some(Panel::History(history)), GraphEvent::Recorded { record }) =
                    (&mut this.panel, event)
                {
                    history.receive(record, &graph, cx);
                    cx.notify();
                }
            }),
        ];
        Self {
            data,
            terminal,
            panel: None,
            header: Header::Tabs,
            room: None,
            member: None,
            choosing: false,
            copied: false,
            scroll: Bar::new(),
            member_scroll: Bar::default(),
            tab_focus: std::array::from_fn(|_| cx.focus_handle()),
            _subscriptions: subscriptions,
        }
    }

    pub fn tab(&self) -> Option<SideTab> {
        self.panel.as_ref().map(Panel::tab)
    }

    pub fn show(&mut self, tab: SideTab, cx: &mut Context<Self>) {
        self.open(tab, Header::Tabs, cx);
    }

    pub fn show_body(&mut self, tab: SideTab, cx: &mut Context<Self>) {
        self.open(tab, Header::External, cx);
    }

    fn open(&mut self, tab: SideTab, header: Header, cx: &mut Context<Self>) {
        self.sync_room(cx);
        let moved = header != self.header;
        self.header = header;
        if self.tab() != Some(tab) {
            self.clear_tab(cx);
            self.panel = Some(Panel::open(tab, &self.data, cx));
            cx.notify();
        } else if moved {
            cx.notify();
        }
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if self.panel.take().is_some() {
            self.clear_tab(cx);
            cx.notify();
        }
    }

    fn clear_tab(&mut self, cx: &mut Context<Self>) {
        self.terminal.update(cx, |view, cx| view.set_node(None, cx));
        self.choosing = false;
        self.copied = false;
        self.scroll.reset();
        self.member_scroll.reset();
    }

    fn sync_room(&mut self, cx: &mut Context<Self>) {
        let room = self.data.read(cx).room().map(|room| &room.id);
        if self.room.as_ref() == room {
            return;
        }
        self.room = room.cloned();
        self.member = None;
        self.clear_tab(cx);
        if let Some(Panel::History(history)) = &mut self.panel {
            history.reset();
        }
    }

    fn tabs(&self, selected: SideTab, cx: &mut Context<Self>) -> Div {
        let c = ui::colors(cx);
        let tabs = SideTab::ALL.into_iter().map(|tab| {
            let active = selected == tab;
            let color = if active { c.ink } else { c.ink_2 };
            let label = tab.label(cx);
            btn::tabbable(div().id(("chat-side-tab", tab as usize)), cx)
                .track_focus(&self.tab_focus[tab as usize])
                .tab_stop(active)
                .role(Role::Tab)
                .aria_label(label)
                .aria_selected(active)
                .flex()
                .flex_auto()
                .min_w_0()
                .justify_center()
                .items_center()
                .gap(px(3.0))
                .h(px(36.0))
                .px(px(2.0))
                .border_b_2()
                .border_color(if active {
                    c.accent.into()
                } else {
                    transparent_black()
                })
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_size(px(SMALL))
                .text_color(color)
                .bg(c.panel)
                .cursor_pointer()
                .hover(move |style| style.bg(c.surface_hover))
                .tooltip(tip(label.to_owned()))
                .child(icon(tab.icon(), px(13.0), color))
                .child(div().truncate().child(label))
                .on_click(cx.listener(move |this, _, _, cx| this.show(tab, cx)))
        });
        let close = ui::say(cx, Str::CommonClose);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .px(px(6.0))
            .border_b_1()
            .border_color(c.line)
            .child(
                div()
                    .id("chat-side-tabs")
                    .role(Role::TabList)
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        let step = match event.keystroke.key.as_str() {
                            "left" => -1,
                            "right" => 1,
                            _ => return,
                        };
                        let count = SideTab::ALL.len() as isize;
                        let index = (selected as isize + step).rem_euclid(count) as usize;
                        this.show(SideTab::ALL[index], cx);
                        window.focus(&this.tab_focus[index], cx);
                        cx.stop_propagation();
                    }))
                    .children(tabs),
            )
            .child(
                btn::ghost("chat-side-close", cx)
                    .role(Role::Button)
                    .aria_label(close)
                    .size(px(26.0))
                    .p_0()
                    .flex_none()
                    .justify_center()
                    .tooltip(tip(close.to_owned()))
                    .child(icon(Lucide::X, px(14.0), c.ink_2))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hide(cx);
                        cx.emit(SideEvent::Close);
                    })),
            )
    }
}

impl Render for SideSlot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(tab) = self.tab() else {
            return div();
        };
        let c = ui::colors(cx);
        let tabs = (self.header == Header::Tabs).then(|| self.tabs(tab, cx));
        let body = self.body(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(c.panel)
            .text_color(c.ink)
            .text_size(px(ui::TEXT))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let stroke = &event.keystroke;
                let modifiers = stroke.modifiers;
                if modifiers.alt || modifiers.control || modifiers.platform {
                    return;
                }
                match stroke.key.as_str() {
                    "tab" if modifiers.shift => window.focus_prev(cx),
                    "tab" => window.focus_next(cx),
                    "pageup" | "pagedown" if this.tab() == Some(SideTab::Members) => {
                        let scroll = this.member_scroll.handle();
                        let height = scroll.0.borrow().base_handle.bounds().size.height.as_f32();
                        let step = height * if stroke.key == "pageup" { -0.9 } else { 0.9 };
                        scroll.set((scroll.offset() + step).clamp(0.0, scroll.max()));
                        if this.header == Header::Tabs {
                            window.focus(&this.tab_focus[SideTab::Members as usize], cx);
                        }
                        cx.notify();
                    }
                    _ => return,
                }
                window.prevent_default();
                cx.stop_propagation();
            }))
            .children(tabs)
            .child(div().flex_1().min_h_0().child(body))
    }
}

fn words(cx: &App, ko: &'static str, en: &'static str) -> &'static str {
    match cx.global::<Lang>() {
        Lang::Ko => ko,
        Lang::En => en,
    }
}

fn scroller(bar: &Bar, id: &'static str, body: impl IntoElement) -> Div {
    bar.on_overflow(
        div()
            .id(id)
            .overflow_y_scroll()
            .track_scroll(bar.handle())
            .child(body),
    )
    .size_full()
}

fn unavailable(cx: &App) -> Div {
    div()
        .p(px(14.0))
        .text_color(ui::colors(cx).ink_2)
        .child(words(cx, "터미널 없음", "No terminal"))
}

fn room_waiting(cx: &App, room: Option<&ChatRoom>) -> &'static str {
    if room.is_some() {
        words(
            cx,
            "아직 구성원이 없습니다.",
            "This room has no members yet.",
        )
    } else {
        words(cx, "방을 고르세요.", "Select a room.")
    }
}
