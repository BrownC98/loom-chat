use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use api::graph::Status;
use api::graph::chat::{
    ChatCursor, ChatEntry, ChatItem, ChatOrigin, ChatRoomKind, ChatTranscriptItem,
};
use api::graph::waits::WaitTone;
use api::ids::{NodeId, WaitId};
use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, App, AppContext, Context, Div, Entity, EventEmitter,
    InteractiveElement, IntoElement, KeyDownEvent, ListAlignment, ListOffset, ListState,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Task, Window, div,
    list,
};

use super::data::ChatData;
use super::words as tr;
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::dock::answers::Answers;
use crate::views::icons::{Lucide, icon};
use crate::views::scrollbar::Bar;
use crate::views::widgets::btn;
use crate::views::{status, ui};

mod activity;
mod acts;
mod rows;
mod waits;

use acts::Draft;

const OVERDRAW: f32 = 240.0;
const RESULT_TIME: Duration = Duration::from_millis(2500);

pub(super) enum StreamEvent {
    FollowUp { node: NodeId, wait: WaitId },
    OpenRoom(String),
    VisibleWaits(Vec<WaitId>),
}

pub(super) struct Quote(pub(super) Arc<ChatItem>);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Questions,
    Done,
    Notices,
    Mine,
}

impl Filter {
    const ALL: [Self; 5] = [
        Self::All,
        Self::Questions,
        Self::Done,
        Self::Notices,
        Self::Mine,
    ];

    fn of(item: &ChatItem) -> Self {
        match &item.entry {
            ChatEntry::Wait { wait, .. } => match wait.tone {
                WaitTone::Ask => Self::Questions,
                WaitTone::Done => Self::Done,
            },
            ChatEntry::Notice { .. } => Self::Notices,
            ChatEntry::Message { message } if message.origin == ChatOrigin::Human => Self::Mine,
            ChatEntry::Message { message } if message.origin == ChatOrigin::Loom => Self::Notices,
            _ => Self::All,
        }
    }

    fn label(self, cx: &App) -> &'static str {
        match self {
            Self::All => tr(cx, "전체", "All"),
            Self::Questions => tr(cx, "질문", "Questions"),
            Self::Done => ui::say(cx, Str::EventsTypeDone),
            Self::Notices => ui::say(cx, Str::EventsCategoryNotice),
            Self::Mine => tr(cx, "내가 보낸 말", "Sent by me"),
        }
    }
}

struct Row {
    cursor: ChatCursor,
    content: RowContent,
}

enum RowContent {
    Item(usize),
    Activity(Vec<usize>),
}

impl Row {
    fn item(&self) -> Option<usize> {
        match self.content {
            RowContent::Item(index) => Some(index),
            RowContent::Activity(_) => None,
        }
    }

    fn last(&self, items: &[Arc<ChatItem>]) -> ChatCursor {
        match &self.content {
            RowContent::Activity(steps) => items[*steps.last().unwrap()].cursor,
            RowContent::Item(_) => self.cursor,
        }
    }
}

#[derive(Default)]
struct Rows {
    visible: Vec<Row>,
    pending: Vec<usize>,
    notices: usize,
    mine: usize,
}

impl Rows {
    fn new(items: &[Arc<ChatItem>], filter: Filter) -> Self {
        let mut rows = Self::default();
        let mut ordered: Vec<_> = items.iter().enumerate().collect();
        if !ordered.is_sorted_by_key(|(_, entry)| entry.cursor) {
            ordered.sort_unstable_by_key(|(_, entry)| entry.cursor);
        }
        let mut in_activity = None;
        for (index, entry) in ordered {
            if rows::hidden(entry) {
                continue;
            }
            let kind = Filter::of(entry);
            match kind {
                Filter::Notices => rows.notices += 1,
                Filter::Mine => rows.mine += 1,
                _ => {}
            }
            if filter != Filter::All && filter != kind {
                continue;
            }
            let activity = match &entry.entry {
                ChatEntry::Transcript {
                    item,
                    activity_turn,
                } if item.is_activity() => Some(*activity_turn),
                _ => None,
            };
            if activity.is_some()
                && in_activity == activity
                && let Some(Row {
                    content: RowContent::Activity(steps),
                    ..
                }) = rows.visible.last_mut()
            {
                steps.push(index);
                continue;
            }
            in_activity = activity;
            if matches!(&entry.entry, ChatEntry::Wait { wait, closed: None, .. }
                if matches!(wait.tone, WaitTone::Ask))
            {
                rows.pending.push(rows.visible.len());
            }
            rows.visible.push(Row {
                cursor: entry.cursor,
                content: if activity.is_some() {
                    RowContent::Activity(vec![index])
                } else {
                    RowContent::Item(index)
                },
            });
        }
        rows
    }
}

enum Banner {
    Success { text: String, _clear: Task<()> },
    Error(String),
}

pub(super) struct Stream {
    data: Entity<ChatData>,
    room: Option<String>,
    revision: Option<u64>,
    filter: Filter,
    rows: Rows,
    list: ListState,
    bar: Bar<ListState>,
    expanded: HashMap<String, BTreeSet<ChatCursor>>,
    jumped: Option<ChatCursor>,
    jump_before: Option<ChatCursor>,
    drafts: HashMap<u32, Draft>,
    busy: HashSet<u32>,
    withdrawing: Option<u32>,
    answers: Entity<Answers>,
    result: Option<Banner>,
    new_messages: usize,
    _subs: [Subscription; 3],
}

impl Stream {
    pub(super) fn new(data: Entity<ChatData>, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW));
        let view = cx.entity().downgrade();
        list.set_scroll_handler(move |event, _, cx| {
            let view = view.clone();
            let load_older = event.is_scrolled && event.visible_range.start <= 1;
            cx.defer(move |cx| {
                let _ = view.update(cx, |stream, cx| {
                    if load_older {
                        stream.load_older(cx);
                    }
                    cx.notify();
                });
            });
        });
        let graph = data.read(cx).graph().clone();
        let answers = cx.new(|cx| Answers::new(graph.clone(), cx));
        let subs = [
            cx.observe(&data, |stream, _, cx| stream.sync(cx)),
            cx.observe(&answers, |_, _, cx| cx.notify()),
            cx.observe(&graph, |_, _, cx| cx.notify()),
        ];
        let mut stream = Self {
            data,
            room: None,
            revision: None,
            filter: Filter::All,
            rows: Rows::default(),
            bar: Bar::over(list.clone()),
            list,
            expanded: HashMap::new(),
            jumped: None,
            jump_before: None,
            drafts: HashMap::new(),
            busy: HashSet::new(),
            withdrawing: None,
            answers,
            result: None,
            new_messages: 0,
            _subs: subs,
        };
        stream.sync(cx);
        stream
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let data = self.data.read(cx);
        let room = data.room().map(|room| room.id.as_str());
        let changed_room = self.room.as_deref() != room;
        let revision = data.stream_revision();
        if changed_room {
            self.room = room.map(str::to_owned);
            self.filter = Filter::All;
            self.drafts.clear();
            self.jumped = None;
            self.jump_before = None;
            self.withdrawing = None;
            self.answers
                .update(cx, |answers, _| answers.retain_images(|_| false));
            self.result = None;
            self.new_messages = 0;
        }
        if changed_room || self.revision != Some(revision) {
            self.revision = Some(revision);
            self.rebuild(!changed_room, cx);
            self.prune_drafts(cx);
            let waits = self
                .data
                .read(cx)
                .items()
                .iter()
                .filter_map(|item| match &item.entry {
                    ChatEntry::Wait {
                        wait, closed: None, ..
                    } => Some(wait.wait.id),
                    _ => None,
                })
                .collect();
            cx.emit(StreamEvent::VisibleWaits(waits));
        }
        if let Some(before) = self.jump_before {
            let data = self.data.read(cx);
            if data.before() != Some(before) {
                self.jump_before = None;
                self.next_question(cx);
            } else if !data.loading_older() && data.error().is_some() {
                self.jump_before = None;
            }
        }
        cx.notify();
    }

    fn at_bottom(&self) -> bool {
        self.list.logical_scroll_top().item_ix >= self.rows.visible.len()
            || self.list.is_scrolled_to_end() == Some(true)
    }

    fn rebuild(&mut self, keep_position: bool, cx: &App) {
        let items = self.data.read(cx).items();
        let new_send = items.last().is_some_and(|item| {
            item.cursor.id < 0
                && !self
                    .rows
                    .visible
                    .iter()
                    .any(|row| row.cursor == item.cursor)
        });
        if new_send {
            self.filter = Filter::All;
        }
        let top = self.list.logical_scroll_top();
        let anchor = (keep_position && !new_send && !self.at_bottom())
            .then(|| self.rows.visible.get(top.item_ix).map(|row| row.cursor))
            .flatten();
        let next = Rows::new(items, self.filter);
        if let (Some(_), Some(last)) = (anchor, self.rows.visible.last()) {
            self.new_messages += next
                .visible
                .iter()
                .skip(
                    next.visible
                        .partition_point(|row| row.cursor <= last.cursor),
                )
                .filter_map(Row::item)
                .filter(|&index| {
                    items[index].cursor.id >= 0
                        && matches!(
                            &items[index].entry,
                            ChatEntry::Message { .. }
                                | ChatEntry::Wait { .. }
                                | ChatEntry::Transcript {
                                    item: ChatTranscriptItem::Assistant { .. }
                                        | ChatTranscriptItem::User { .. },
                                    ..
                                }
                        )
                })
                .count();
        } else {
            self.new_messages = 0;
        }
        if keep_position
            && self.rows.visible.len() == next.visible.len()
            && self
                .rows
                .visible
                .iter()
                .zip(&next.visible)
                .all(|(old, new)| old.cursor == new.cursor)
        {
            self.list.remeasure_items(0..next.visible.len());
        } else {
            self.list.reset(next.visible.len());
            if let Some(cursor) = anchor {
                let item_ix = next.visible.partition_point(|row| row.last(items) < cursor);
                let offset_in_item = next
                    .visible
                    .get(item_ix)
                    .filter(|row| row.cursor == cursor)
                    .map_or(px(0.0), |_| top.offset_in_item);
                self.list.scroll_to(ListOffset {
                    item_ix,
                    offset_in_item,
                });
            }
        }
        if new_send {
            self.list.scroll_to_end();
        }
        self.rows = next;
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.filter = filter;
            self.jumped = None;
            self.jump_before = None;
            self.rebuild(false, cx);
            cx.notify();
        }
    }

    fn load_older(&mut self, cx: &mut Context<Self>) {
        let data = self.data.read(cx);
        if data.before().is_some() && !data.loading_older() {
            self.data.update(cx, ChatData::load_older);
        }
    }

    fn next_question(&mut self, cx: &mut Context<Self>) {
        if self.jump_before.is_some() {
            return;
        }
        let data = self.data.read(cx);
        let total = data.room().map_or(0, |room| room.questions as usize);
        let before = data.before();
        if !matches!(self.filter, Filter::All | Filter::Questions) {
            self.set_filter(Filter::Questions, cx);
        }
        let end = self.jumped.map_or(self.rows.pending.len(), |cursor| {
            self.rows
                .pending
                .partition_point(|&index| self.rows.visible[index].cursor < cursor)
        });
        let next = end.checked_sub(1).map(|index| self.rows.pending[index]);
        if next.is_none()
            && self.rows.pending.len() < total
            && let Some(before) = before
        {
            self.jump_before = Some(before);
            self.load_older(cx);
        } else if let Some(index) = next.or_else(|| self.rows.pending.last().copied()) {
            self.jumped = Some(self.rows.visible[index].cursor);
            self.list.scroll_to(ListOffset {
                item_ix: index,
                offset_in_item: px(0.0),
            });
        }
        cx.notify();
    }

    fn latest(&mut self, cx: &mut Context<Self>) {
        let Some(room) = self.room.clone() else {
            return;
        };
        self.jumped = None;
        self.jump_before = None;
        self.new_messages = 0;
        self.list.scroll_to_end();
        self.data.update(cx, |data, cx| data.select(room, cx));
        cx.notify();
    }

    fn show_result(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        self.result = Some(match result {
            Ok(text) => Banner::Success {
                text,
                _clear: cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(RESULT_TIME).await;
                    let _ = this.update(cx, |stream, cx| {
                        stream.result = None;
                        cx.notify();
                    });
                }),
            },
            Err(text) => Banner::Error(text),
        });
        cx.notify();
    }

    fn follow(
        &self,
        cx: &Context<Self>,
    ) -> impl Fn(crate::views::dock::body::Target, &mut App) + 'static {
        let graph = self.data.read(cx).graph().clone();
        let stream = cx.weak_entity();
        let room = self.room.clone();
        move |target, cx| {
            let task = crate::views::dock::body::open(&graph, target, cx);
            let (stream, room) = (stream.clone(), room.clone());
            cx.spawn(async move |cx| {
                if let Err(error) = task.await {
                    let _ = stream.update(cx, |stream, cx| {
                        if stream.room == room {
                            stream.show_result(Err(error), cx);
                        }
                    });
                }
            })
            .detach();
        }
    }

    fn working(&self, cx: &App) -> Option<Div> {
        let data = self.data.read(cx);
        let room = data.room().filter(|room| !room.archived)?;
        let snapshot = data.graph().read(cx).snapshot()?;
        let mut working = snapshot.nodes.iter().filter(|node| {
            room.members.contains(&node.id) && matches!(node.status, Status::Working { .. })
        });
        let node = working.next()?;
        let extra = working.count();
        let mut text = format!(
            "{}{}",
            node.name,
            tr(cx, " 님이 작업 중입니다…", " is working…"),
        );
        if let Some(tool) = status::line(node, None, *cx.global::<Lang>()) {
            text.push_str(&format!(" · {tool}"));
        }
        if extra > 0 {
            text.push_str(&format!(" · +{extra}"));
        }
        let c = ui::colors(cx);
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(14.0))
                .py(px(5.0))
                .text_color(c.ink_2)
                .text_size(px(ui::SMALL))
                .child(
                    div()
                        .flex_none()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(c.run)
                        .with_animation(
                            "stream-working-dot",
                            Animation::new(Duration::from_millis(1500))
                                .repeat()
                                .with_max_fps(24.0),
                            |dot, phase| {
                                dot.opacity(0.78 + 0.22 * (phase * std::f32::consts::TAU).cos())
                            },
                        ),
                )
                .child(div().min_w_0().truncate().child(text)),
        )
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let data = self.data.read(cx);
        let room = data.room()?;
        let pinned = matches!(room.kind, ChatRoomKind::Pinned);
        let questions = (room.questions as usize).max(self.rows.pending.len());
        let count = |filter: Filter| match filter {
            Filter::All => 0,
            Filter::Questions => questions,
            Filter::Done => room.done as usize,
            Filter::Notices => self.rows.notices,
            Filter::Mine => self.rows.mine,
        };
        let c = ui::colors(cx);
        let chips = (pinned
            && (self.filter != Filter::All
                || !self.rows.visible.is_empty()
                || Filter::ALL.into_iter().any(|filter| count(filter) > 0)))
        .then(|| {
            Filter::ALL
                .into_iter()
                .enumerate()
                .map(|(index, filter)| {
                    let n = count(filter);
                    let label = filter.label(cx);
                    let active = filter == self.filter;
                    btn::button(("stream-filter", index), cx)
                        .when(active, |face| face.bg(c.accent_wash).border_color(c.accent))
                        .child(if n == 0 {
                            label.to_owned()
                        } else {
                            format!("{label} {n}")
                        })
                        .on_click(
                            cx.listener(move |stream, _, _, cx| stream.set_filter(filter, cx)),
                        )
                })
                .collect::<Vec<_>>()
        });
        let jump = (questions > 0).then(|| {
            let label = tr(cx, "다음 질문", "Next question");
            let current = self.jumped.and_then(|cursor| {
                self.rows
                    .pending
                    .iter()
                    .position(|&index| self.rows.visible[index].cursor == cursor)
            });
            btn::button("stream-next-question", cx)
                .child(match current {
                    Some(index) => {
                        format!("{label} {}/{questions}", self.rows.pending.len() - index)
                    }
                    None => format!("{label} {questions}"),
                })
                .on_click(cx.listener(|stream, _, _, cx| stream.next_question(cx)))
        });
        let older = data.loading_older().then(|| {
            div().text_size(px(ui::SMALL)).text_color(c.ink_2).child(tr(
                cx,
                "이전 대화를 읽는 중",
                "Loading older messages",
            ))
        });
        if chips.is_none() && jump.is_none() && older.is_none() {
            return None;
        }
        Some(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .px(px(14.0))
                .py(px(7.0))
                .border_b_1()
                .border_color(c.line)
                .children(chips.into_iter().flatten())
                .child(div().flex_1())
                .children(older)
                .children(jump),
        )
    }
}

impl EventEmitter<StreamEvent> for Stream {}
impl EventEmitter<Quote> for Stream {}

impl Render for Stream {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.at_bottom() {
            self.new_messages = 0;
        }
        let toolbar = self.toolbar(cx);
        let data = self.data.read(cx);
        let c = ui::colors(cx);
        let content = if self.rows.visible.is_empty() {
            let label = if data.loading() {
                tr(cx, "대화를 읽는 중", "Loading messages")
            } else if self.filter != Filter::All {
                tr(cx, "이 종류의 대화가 없습니다", "No messages of this kind")
            } else if data.error().is_some() {
                tr(cx, "대화를 읽지 못했습니다", "Could not load messages")
            } else {
                tr(cx, "아직 대화가 없습니다", "No messages yet")
            };
            div()
                .flex_1()
                .items_center()
                .justify_center()
                .flex()
                .text_color(c.ink_2)
                .child(label)
        } else {
            self.bar
                .reserved(list(
                    self.list.clone(),
                    cx.processor(|stream, index, window, cx| stream.row(index, window, cx)),
                ))
                .flex_1()
                .min_h_0()
        };
        let jump_to_new = (!self.at_bottom() && !self.rows.visible.is_empty()).then(|| {
            let label = tr(cx, "맨 아래로", "Jump to bottom");
            let text = (self.new_messages > 0).then(|| match *cx.global::<Lang>() {
                Lang::Ko => format!("새 말 {}", self.new_messages),
                Lang::En => format!("{} new", self.new_messages),
            });
            btn::button("stream-new-messages", cx)
                .absolute()
                .right(px(28.0))
                .bottom(px(10.0))
                .aria_label(label)
                .tooltip(crate::views::tooltip::tip(label.to_owned()))
                .children(text)
                .child(icon(Lucide::ArrowDown, px(14.0), c.ink_2))
                .on_click(cx.listener(|stream, _, _, cx| stream.latest(cx)))
        });
        let banner = self.result.as_ref().map(|result| {
            let (text, error) = match result {
                Banner::Success { text, .. } => (text, false),
                Banner::Error(text) => (text, true),
            };
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(14.0))
                .py(px(6.0))
                .border_t_1()
                .border_color(c.line)
                .text_color(if error { c.err } else { c.ink_2 })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(crate::views::prose::prose(text.clone())),
                )
                .children(error.then(|| {
                    let label = ui::say(cx, Str::CommonClose);
                    btn::ghost("stream-dismiss-result", cx)
                        .aria_label(label)
                        .child(icon(Lucide::X, px(14.0), c.ink_2))
                        .on_click(cx.listener(|stream, _, _, cx| {
                            stream.result = None;
                            cx.notify();
                        }))
                }))
        });
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(c.panel)
            .text_size(px(ui::TEXT))
            .text_color(c.ink)
            .on_key_down(|event: &KeyDownEvent, window, cx| {
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
            })
            .children(toolbar)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(content)
                    .children(jump_to_new),
            )
            .children(self.working(cx))
            .children(banner)
    }
}
