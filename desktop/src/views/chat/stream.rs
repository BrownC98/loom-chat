use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use api::graph::chat::{ChatCursor, ChatEntry, ChatItem, ChatOrigin, ChatRoomKind};
use api::graph::messages::{MessageKind, Question};
use api::graph::waits::WaitTone;
use api::ids::{NodeId, WaitId};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, Div, Entity, EventEmitter, IntoElement, ListAlignment, ListOffset, ListState,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription, Window, div, list,
};

use super::data::ChatData;
use crate::scale::px;
use crate::store::BusyChanged;
use crate::strings::Lang;
use crate::views::scrollbar::Bar;
use crate::views::ui;
use crate::views::widgets::btn;

mod acts;
mod rows;
mod waits;

use acts::{Draft, Input, Target};

const OVERDRAW: f32 = 240.0;

pub(super) enum StreamEvent {
    FollowUp { node: NodeId, wait: WaitId },
    OpenRoom(String),
}

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
            Self::Done => tr(cx, "완료", "Done"),
            Self::Notices => tr(cx, "알림", "Notices"),
            Self::Mine => tr(cx, "내가 보낸 말", "Sent by me"),
        }
    }
}

#[derive(Clone, Copy)]
struct Row {
    cursor: ChatCursor,
    item: usize,
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
        for (item, entry) in items.iter().enumerate() {
            if matches!(&entry.entry, ChatEntry::Message { message }
            if message.origin == ChatOrigin::Human && matches!(
                message.what,
                Some(MessageKind::Answer { question: Question::Inbox { .. } })
            )) {
                continue;
            }
            let kind = Filter::of(entry);
            match kind {
                Filter::Notices => rows.notices += 1,
                Filter::Mine => rows.mine += 1,
                _ => {}
            }
            if filter == Filter::All || filter == kind {
                rows.visible.push(Row {
                    cursor: entry.cursor,
                    item,
                });
            }
        }
        if !rows.visible.is_sorted_by_key(|row| row.cursor) {
            rows.visible.sort_unstable_by_key(|row| row.cursor);
        }
        rows.pending.extend(
            rows.visible
                .iter()
                .enumerate()
                .filter(|(_, row)| {
                    matches!(&items[row.item].entry, ChatEntry::Wait { wait, closed: None, .. }
                        if matches!(wait.tone, WaitTone::Ask))
                })
                .map(|(index, _)| index),
        );
        rows
    }
}

pub(super) struct Stream {
    data: Entity<ChatData>,
    room: Option<String>,
    revision: Option<u64>,
    filter: Filter,
    rows: Rows,
    list: ListState,
    bar: Bar<ListState>,
    expanded: BTreeSet<ChatCursor>,
    jumped: Option<ChatCursor>,
    jump_before: Option<ChatCursor>,
    paged: bool,
    drafts: HashMap<Input, Draft>,
    busy: HashSet<Target>,
    result: Option<(String, bool)>,
    _subs: [Subscription; 2],
}

impl Stream {
    pub(super) fn new(data: Entity<ChatData>, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW));
        let view = cx.entity().downgrade();
        list.set_scroll_handler(move |event, _, cx| {
            if event.is_scrolled {
                let view = view.clone();
                cx.defer(move |cx| {
                    let _ = view.update(cx, |stream, cx| {
                        if stream.list.logical_scroll_top().item_ix <= 1 {
                            stream.load_older(cx);
                        }
                    });
                });
            }
        });
        let graph = data.read(cx).graph().clone();
        let subs = [
            cx.observe(&data, |stream, _, cx| stream.sync(cx)),
            cx.subscribe(&graph, |_, _, _: &BusyChanged, cx| cx.notify()),
        ];
        let mut stream = Self {
            data,
            room: None,
            revision: None,
            filter: Filter::All,
            rows: Rows::default(),
            bar: Bar::over(list.clone()),
            list,
            expanded: BTreeSet::new(),
            jumped: None,
            jump_before: None,
            paged: false,
            drafts: HashMap::new(),
            busy: HashSet::new(),
            result: None,
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
            self.expanded.clear();
            self.drafts.clear();
            self.jumped = None;
            self.jump_before = None;
            self.paged = false;
            self.result = None;
        }
        if changed_room || self.revision != Some(revision) {
            self.revision = Some(revision);
            self.rebuild(!changed_room, cx);
            self.prune_drafts(cx);
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

    fn rebuild(&mut self, keep_position: bool, cx: &App) {
        let top = self.list.logical_scroll_top();
        let anchor = keep_position
            .then(|| self.rows.visible.get(top.item_ix).map(|row| row.cursor))
            .flatten();
        let next = Rows::new(self.data.read(cx).items(), self.filter);
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
                let item_ix = next.visible.partition_point(|row| row.cursor < cursor);
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
            self.paged = true;
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
        self.paged = false;
        self.list.scroll_to_end();
        self.data.update(cx, |data, cx| data.select(room, cx));
        cx.notify();
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
        let chips = pinned.then(|| {
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
        let older = data.before().map(|_| {
            let label = if data.loading_older() {
                tr(cx, "이전 대화를 읽는 중", "Loading older messages")
            } else {
                tr(cx, "이전 대화", "Older messages")
            };
            btn::button("stream-older", cx)
                .child(label)
                .on_click(cx.listener(|stream, _, _, cx| stream.load_older(cx)))
        });
        let latest = self.paged.then(|| {
            btn::button("stream-latest", cx)
                .child(tr(cx, "최근 대화", "Latest messages"))
                .on_click(cx.listener(|stream, _, _, cx| stream.latest(cx)))
        });
        if chips.is_none() && jump.is_none() && older.is_none() && latest.is_none() {
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
                .children(latest)
                .children(jump),
        )
    }
}

impl EventEmitter<StreamEvent> for Stream {}

impl Render for Stream {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    cx.processor(|stream, index, _, cx| stream.row(index, cx)),
                ))
                .flex_1()
                .min_h_0()
        };
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(c.panel)
            .text_size(px(ui::TEXT))
            .text_color(c.ink)
            .children(toolbar)
            .child(content)
            .children(self.result.as_ref().map(|(text, failed)| {
                div()
                    .flex_none()
                    .px(px(14.0))
                    .py(px(6.0))
                    .border_t_1()
                    .border_color(c.line)
                    .text_color(if *failed { c.err } else { c.ink_2 })
                    .child(text.clone())
            }))
    }
}

fn tr<'a>(cx: &App, ko: &'a str, en: &'a str) -> &'a str {
    match cx.global::<Lang>() {
        Lang::Ko => ko,
        Lang::En => en,
    }
}
