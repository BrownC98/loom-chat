use std::ops::Range;

use api::graph::messages::Kind;
use api::graph::{Status, chat::ChatRecipient};
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, AnyElement, App, Context, Entity, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Role, ScrollStrategy, StatefulInteractiveElement,
    Styled, Subscription, Toggled, UniformListScrollHandle, Window, anchored, deferred, div, point,
    uniform_list,
};

use super::targets::{Catalog, Target, group};
use super::{Composer, state_label, tab};
use crate::scale::px;
use crate::strings::Str;
use crate::theme::Colors;
use crate::views::field::{self, Field, FieldEvent};
use crate::views::icons::{Lucide, icon};
use crate::views::{status, ui, widgets::btn};

const ROW_HEIGHT: f32 = 34.0;
const PANEL_WIDTH: f32 = 620.0;
const PANEL_GAP: f32 = 6.0;
const LIST_HEIGHT: f32 = 306.0;
const TIME: &str = "%H:%M";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Human,
    Working,
    Idle,
    Rooms,
}

impl Filter {
    const ALL: [Self; 5] = [
        Self::All,
        Self::Human,
        Self::Working,
        Self::Idle,
        Self::Rooms,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::All => "전체",
            Self::Human => "내 답을 기다림",
            Self::Working => "일하는 중",
            Self::Idle => "쉬는 중",
            Self::Rooms => "방",
        }
    }

    fn of(status: &Status) -> Self {
        match status {
            Status::Human => Self::Human,
            Status::Working { .. } => Self::Working,
            Status::Idle => Self::Idle,
            _ => Self::All,
        }
    }
}

enum Row {
    Head(String),
    Room(usize),
    Agent(usize),
}

impl Row {
    fn target(&self, catalog: &Catalog) -> Option<Target> {
        match self {
            Self::Head(_) => None,
            Self::Room(index) => Some(Target::Room(catalog.rooms.get(*index)?.id.clone())),
            Self::Agent(index) => Some(Target::Node(catalog.agents.get(*index)?.node)),
        }
    }
}

#[derive(Default)]
struct Listing {
    rows: Vec<Row>,
    counts: [usize; Filter::ALL.len()],
}

impl Listing {
    fn count(&mut self, filter: Filter) {
        self.counts[Filter::All as usize] += 1;
        if filter != Filter::All {
            self.counts[filter as usize] += 1;
        }
    }
}

pub(super) struct Picker {
    pub open: bool,
    query: Entity<Field>,
    focus: FocusHandle,
    list_focus: FocusHandle,
    filter: Filter,
    highlight: Option<Target>,
    scroll: UniformListScrollHandle,
    focus_out: Option<Subscription>,
    _query: Subscription,
}

impl Picker {
    pub fn new(cx: &mut Context<Composer>) -> Self {
        let query = field::boxed("이름·방·하네스로 찾기", false, false, cx);
        let query_subscription = cx.subscribe(&query, |this, _, event, cx| {
            if let FieldEvent::Changed = event {
                this.picker.reset_highlight();
                cx.notify();
            }
        });
        Self {
            open: false,
            query,
            focus: cx.focus_handle(),
            list_focus: cx.focus_handle().tab_stop(true),
            filter: Filter::All,
            highlight: None,
            scroll: UniformListScrollHandle::new(),
            focus_out: None,
            _query: query_subscription,
        }
    }

    fn reset_highlight(&mut self) {
        self.highlight = None;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn listing(&self, catalog: &Catalog, cx: &App) -> Listing {
        let query = self.query.read(cx).text().trim().to_lowercase();
        let matches = |values: &[&str]| {
            query.split_whitespace().all(|part| {
                values
                    .iter()
                    .any(|value| value.to_lowercase().contains(part))
            })
        };
        let mut listing = Listing::default();
        for (index, room) in catalog.rooms.iter().enumerate() {
            if !group(room) || !matches(&[&room.name]) {
                continue;
            }
            listing.count(Filter::Rooms);
            if matches!(self.filter, Filter::All | Filter::Rooms) {
                if listing.rows.is_empty() {
                    listing
                        .rows
                        .push(Row::Head("방 · 구성원 모두가 받습니다".to_owned()));
                }
                listing.rows.push(Row::Room(index));
            }
        }
        let mut last = None;
        for (index, agent) in catalog.agents.iter().enumerate() {
            let work = catalog
                .room(&agent.work)
                .map_or("작업 없음", |room| room.name.as_str());
            let harness = harness(agent);
            if !matches(&[&agent.name, work, harness]) {
                continue;
            }
            let filter = Filter::of(&agent.status);
            listing.count(filter);
            if self.filter == Filter::All || self.filter == filter {
                if last != Some(agent.work.as_str()) {
                    listing.rows.push(Row::Head(work.to_owned()));
                    last = Some(agent.work.as_str());
                }
                listing.rows.push(Row::Agent(index));
            }
        }
        listing
    }
}

impl Composer {
    pub fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_room(window, cx);
        if !self.writable(cx) {
            return;
        }
        if !self.picker.open {
            self.picker.open = true;
            self.review = None;
            self.picker.filter = Filter::All;
            self.picker.reset_highlight();
            self.picker.query.update(cx, |field, cx| field.clear(cx));
            self.picker.focus_out =
                Some(
                    cx.on_focus_out(&self.picker.focus, window, |this, _, window, cx| {
                        if !this.trigger.is_focused(window) {
                            this.picker.open = false;
                            cx.notify();
                        }
                    }),
                );
        }
        let focus = self.picker.query.read(cx).handle();
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(super) fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.open {
            self.picker.open = false;
            window.focus(&self.trigger, cx);
            cx.notify();
        }
    }

    fn picker_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        let query = self.picker.query.read(cx);
        let in_query = query.focused(window);
        if in_query && query.composing() {
            return;
        }
        let in_list = self.picker.list_focus.is_focused(window);
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => self.close_picker(window, cx),
            "up" | "down" if in_list || in_query => {
                let step = if key == "up" { -1 } else { 1 };
                if self.move_highlight(step, cx) {
                    window.focus(&self.picker.list_focus, cx);
                }
            }
            "enter" if in_list || in_query => self.toggle_highlight(cx),
            "space" if in_list => self.toggle_highlight(cx),
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    fn move_highlight(&mut self, by: isize, cx: &mut Context<Self>) -> bool {
        let Some(recipients) = self.data.read(cx).recipients() else {
            return false;
        };
        let catalog = Catalog::new(recipients);
        let listing = self.picker.listing(&catalog, cx);
        let rows: Vec<_> = listing
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.target(&catalog).map(|target| (index, target)))
            .collect();
        if rows.is_empty() {
            return false;
        }
        let at = self
            .picker
            .highlight
            .as_ref()
            .and_then(|target| rows.iter().position(|(_, row)| row == target))
            .map_or_else(
                || if by < 0 { rows.len() - 1 } else { 0 },
                |at| at.saturating_add_signed(by).min(rows.len() - 1),
            );
        let (index, target) = &rows[at];
        self.picker.highlight = Some(target.clone());
        self.picker
            .scroll
            .scroll_to_item(*index, ScrollStrategy::Nearest);
        cx.notify();
        true
    }

    fn toggle_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(recipients) = self.data.read(cx).recipients() else {
            return;
        };
        let catalog = Catalog::new(recipients);
        let listing = self.picker.listing(&catalog, cx);
        let mut targets = listing.rows.iter().filter_map(|row| row.target(&catalog));
        let target = if let Some(current) = &self.picker.highlight {
            targets.find(|target| target == current)
        } else {
            targets.next()
        };
        if let Some(target) = target {
            self.picker.highlight = Some(target.clone());
            self.toggle(target, cx);
        }
    }

    pub(super) fn picker_view(
        &self,
        catalog: Option<&Catalog>,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let c = ui::colors(cx);
        let listing =
            catalog.map_or_else(Listing::default, |catalog| self.picker.listing(catalog, cx));
        let bounds = self.bounds.get();
        let available = (bounds.origin.y - px(164.0)).max(px(68.0));
        let list_height =
            px((listing.rows.len() as f32 * ROW_HEIGHT).clamp(ROW_HEIGHT, LIST_HEIGHT))
                .min(available);
        let width = px(PANEL_WIDTH).min(window.viewport_size().width - px(16.0));
        let panel = div()
            .id("chat-recipient-picker")
            .track_focus(&self.picker.focus)
            .role(Role::Group)
            .aria_label("받는 이 목록")
            .capture_key_down(cx.listener(Self::picker_key))
            .on_key_down(tab)
            .occlude()
            .on_mouse_down_out(
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    if !this.bounds.get().contains(&event.position) {
                        this.close_picker(window, cx);
                    }
                }),
            )
            .flex()
            .flex_col()
            .gap(px(6.0))
            .w(width)
            .p(px(8.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(c.line_strong)
            .bg(c.panel)
            .text_color(c.ink)
            .text_size(px(ui::TEXT))
            .shadow_lg()
            .child(self.picker_header(cx));
        let panel = if listing.rows.is_empty() {
            panel.child(
                div()
                    .p(px(8.0))
                    .text_color(c.ink_2)
                    .child(if catalog.is_some() {
                        ui::say(cx, Str::InboxNoMatch)
                    } else {
                        "받는 이 목록을 불러오는 중입니다."
                    }),
            )
        } else {
            let rows = listing.rows;
            let list = uniform_list(
                "chat-recipient-list",
                rows.len(),
                cx.processor(move |this, range: Range<usize>, window, cx| {
                    let Some(data) = this.data.read(cx).recipients() else {
                        return Vec::new();
                    };
                    let catalog = Catalog::new(data);
                    range
                        .map(|index| this.picker_row(index, &rows[index], &catalog, window, cx))
                        .collect()
                }),
            )
            .track_scroll(&self.picker.scroll)
            .h(list_height)
            .w_full();
            panel.child(
                div()
                    .id("chat-picker-rows")
                    .track_focus(&self.picker.list_focus)
                    .role(Role::ListBox)
                    .aria_label("검색 결과")
                    .focus_visible(move |style| style.border_color(c.accent))
                    .border_1()
                    .border_color(c.line)
                    .rounded(px(6.0))
                    .child(list),
            )
        };
        let query = self.picker.query.read(cx);
        let panel = panel
            .child(
                div()
                    .id("chat-picker-query")
                    .track_focus(&query.handle().tab_stop(false))
                    .role(Role::TextInput)
                    .aria_label("이름·방·하네스로 찾기")
                    .aria_value(query.text().to_owned())
                    .child(self.picker.query.clone()),
            )
            .child(self.filters(listing.counts, cx))
            .child(
                div()
                    .text_color(c.ink_2)
                    .text_size(px(ui::SMALL))
                    .child("↑↓ 이동 · Space 선택 · Esc 닫기"),
            );
        deferred(
            anchored()
                .position(bounds.origin)
                .anchor(Anchor::BottomLeft)
                .offset(point(px(0.0), -px(PANEL_GAP)))
                .snap_to_window()
                .child(panel),
        )
        .with_priority(1)
        .into_any_element()
    }

    fn picker_header(&self, cx: &Context<Self>) -> gpui::Div {
        let picked = self.draft().map_or(0, |draft| draft.selected().len());
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("받는 이 목록"),
            )
            .child(
                div()
                    .flex_1()
                    .text_color(ui::colors(cx).ink_2)
                    .child(format!("{picked}곳 고름")),
            )
            .child(
                btn::ghost("chat-picker-clear", cx)
                    .role(Role::Button)
                    .aria_label("받는 이 모두 빼기")
                    .when(picked > 0, |button| {
                        button.on_click(
                            cx.listener(|this, _, _, cx| this.replace_targets(Vec::new(), cx)),
                        )
                    })
                    .when(picked == 0, btn::inert)
                    .child("모두 풀기"),
            )
            .child(
                btn::ghost("chat-picker-close", cx)
                    .aria_label("받는 이 목록 닫기")
                    .child(icon(Lucide::X, px(13.0), ui::colors(cx).ink_2))
                    .on_click(cx.listener(|this, _, window, cx| this.close_picker(window, cx))),
            )
    }

    fn filters(&self, counts: [usize; Filter::ALL.len()], cx: &Context<Self>) -> gpui::Div {
        let c = ui::colors(cx);
        Filter::ALL
            .into_iter()
            .fold(div().flex().flex_wrap().gap(px(6.0)), |row, filter| {
                let selected = filter == self.picker.filter;
                let label = format!("{} {}", filter.label(), counts[filter as usize]);
                row.child(
                    btn::button(("chat-picker-filter", filter as usize), cx)
                        .aria_label(label.clone())
                        .aria_toggled(Toggled::from(selected))
                        .when(selected, |button| button.bg(c.accent_wash))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.picker.filter = filter;
                            this.picker.reset_highlight();
                            let focus = this.picker.query.read(cx).handle();
                            window.focus(&focus, cx);
                            cx.notify();
                        }))
                        .child(label),
                )
            })
    }

    fn picker_row(
        &self,
        index: usize,
        row: &Row,
        catalog: &Catalog,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let c = ui::colors(cx);
        let base = div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(6.0));
        let (Some(target), Some(draft)) = (row.target(catalog), self.draft()) else {
            return base
                .text_color(c.ink_2)
                .font_weight(FontWeight::SEMIBOLD)
                .children(if let Row::Head(label) = row {
                    Some(label.clone())
                } else {
                    None
                })
                .into_any_element();
        };
        let (name, description, last, glyph, dot) = match row {
            Row::Agent(index) => {
                let agent = &catalog.agents[*index];
                let state = state_label(&agent.status, cx);
                let task = agent
                    .task
                    .as_deref()
                    .map_or(state.clone(), |task| format!("{state} · {task}"));
                (
                    &agent.name,
                    format!(
                        "{}{} · {task}",
                        harness(agent),
                        if agent.modes.contains(&Kind::Steer) {
                            ""
                        } else {
                            " · 일반만"
                        }
                    ),
                    agent.last_activity,
                    None,
                    state_color(&agent.status, &c),
                )
            }
            Row::Room(index) => {
                let room = &catalog.rooms[*index];
                let working = room
                    .members
                    .iter()
                    .filter(|&&node| {
                        catalog
                            .node(node)
                            .is_some_and(|agent| matches!(agent.status, Status::Working { .. }))
                    })
                    .count();
                let summary = if room.members.is_empty() {
                    "외부 에이전트 · 일반만".to_owned()
                } else {
                    format!(
                        "구성원 {}명 · {}명이 일하는 중",
                        room.members.len(),
                        working
                    )
                };
                (
                    &room.name,
                    summary,
                    room.last_activity,
                    Some(Lucide::Folder),
                    c.ink_3,
                )
            }
            Row::Head(_) => return base.into_any_element(),
        };
        let selected = draft.selected().contains(&target);
        let highlighted = self.picker.highlight.as_ref() == Some(&target)
            && self.picker.list_focus.is_focused(window);
        let time = last.map_or(String::new(), |seconds| ui::clock(seconds, TIME));
        base.id(("chat-picker-row", index))
            .role(Role::CheckBox)
            .aria_label(name.clone())
            .aria_description(format!("{description} {time}"))
            .aria_toggled(Toggled::from(selected))
            .when(highlighted, |row| row.aria_active_descendant())
            .cursor_pointer()
            .border_1()
            .border_color(if highlighted {
                c.accent.into()
            } else {
                gpui::transparent_black()
            })
            .rounded(px(4.0))
            .when(selected, |row| row.bg(c.accent_wash))
            .hover(move |style| style.bg(c.surface_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.picker.highlight = Some(target.clone());
                this.toggle(target.clone(), cx);
                window.focus(&this.picker.list_focus, cx);
            }))
            .child(
                div()
                    .size(px(14.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(if selected { c.accent } else { c.line_strong })
                    .when(selected, |mark| {
                        mark.bg(c.accent)
                            .child(icon(Lucide::Check, px(11.0), c.accent_ink))
                    }),
            )
            .children(glyph.map(|glyph| icon(glyph, px(13.0), dot)))
            .children(
                glyph
                    .is_none()
                    .then(|| div().size(px(7.0)).flex_none().rounded_full().bg(dot)),
            )
            .child(
                div()
                    .max_w(px(142.0))
                    .flex_none()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(c.ink_2)
                    .child(description),
            )
            .child(div().flex_none().text_color(c.ink_2).child(time))
            .into_any_element()
    }
}

fn harness(agent: &ChatRecipient) -> &str {
    agent
        .harness
        .as_ref()
        .map_or("명령 노드", |id| id.0.as_str())
}

fn state_color(state: &Status, c: &Colors) -> gpui::Rgba {
    match state {
        Status::Human => c.warn,
        Status::Working { .. } => c.run,
        Status::Done | Status::Merged { .. } => c.ok,
        _ if status::problem(state) => c.err,
        _ => c.ink_3,
    }
}
