use std::collections::{HashSet, VecDeque};

use api::graph::chat::{ChatEntry, ChatRoom};
use api::graph::docs::DocRow;
use api::graph::events::{EventLog, EventRecord};
use gpui::{
    App, Context, Div, Entity, IntoElement, ListAlignment, ListState, ParentElement, Styled, Task,
    div, list,
};

use super::super::data::ChatData;
use super::{SideSlot, SideTab, scroller, words};
use crate::link;
use crate::scale::px;
use crate::store::Store;
use crate::strings::Str;
use crate::views::dock::body;
use crate::views::dock::docs::document_rows;
use crate::views::dock::events::{EventRow, what};
use crate::views::dock::ui;
use crate::views::scrollbar::Bar;

const HISTORY_LIMIT: usize = 500;

pub(super) enum Panel {
    Members,
    Terminal,
    Docs(Load<Vec<DocRow>>),
    History(History),
}

pub(super) enum Load<T> {
    Reading { _task: Task<()> },
    Ready(T),
    Failed(String),
}

impl<T> From<Result<T, String>> for Load<T> {
    fn from(result: Result<T, String>) -> Self {
        match result {
            Ok(value) => Self::Ready(value),
            Err(error) => Self::Failed(error),
        }
    }
}

pub(super) struct History {
    load: Load<EventLog>,
    pending: VecDeque<EventRecord>,
    list: ListState,
    bar: Bar<ListState>,
    open: Option<u32>,
}

impl History {
    fn read(graph: &Entity<Store>, cx: &mut Context<SideSlot>) -> Self {
        let read = graph.update(cx, |store, cx| {
            store.call(cx, |base| link::read(base, "/api/events/log"))
        });
        let task = cx.spawn(async move |this, cx| {
            let result = read.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(Panel::History(history)) = &mut this.panel {
                    history.finish(result);
                    cx.notify();
                }
            });
        });
        let list = ListState::new(0, ListAlignment::Top, px(160.0));
        Self {
            load: Load::Reading { _task: task },
            pending: VecDeque::new(),
            bar: Bar::over(list.clone()),
            list,
            open: None,
        }
    }

    fn finish(&mut self, result: Result<EventLog, String>) {
        self.load = result.into();
        for record in std::mem::take(&mut self.pending) {
            self.record(&record);
        }
    }

    pub(super) fn receive(
        &mut self,
        record: &EventRecord,
        graph: &Entity<Store>,
        cx: &mut Context<SideSlot>,
    ) {
        if matches!(&self.load, Load::Ready(log) if record.id > log.last.saturating_add(1)) {
            *self = Self::read(graph, cx);
        }
        self.record(record);
    }

    fn record(&mut self, record: &EventRecord) {
        match &mut self.load {
            Load::Reading { .. } => {
                if self.pending.len() == HISTORY_LIMIT {
                    self.pending.pop_front();
                }
                self.pending.push_back(record.clone());
            }
            Load::Ready(log) if record.id > log.last => {
                log.last = record.id;
                if !what::hidden(&record.entry) {
                    log.records.insert(0, record.clone());
                    log.records.truncate(HISTORY_LIMIT);
                }
            }
            _ => {}
        }
    }

    pub(super) fn reset(&mut self) {
        self.open = None;
        self.list.reset(0);
    }

    fn toggle(&mut self, id: u32) {
        self.open = (self.open != Some(id)).then_some(id);
        self.list.remeasure();
    }
}

impl Panel {
    pub(super) fn open(tab: SideTab, data: &Entity<ChatData>, cx: &mut Context<SideSlot>) -> Self {
        let graph = data.read(cx).graph().clone();
        match tab {
            SideTab::Members => Self::Members,
            SideTab::Terminal => Self::Terminal,
            SideTab::Docs => {
                let read = graph.update(cx, |store, cx| store.list_docs(cx));
                Self::Docs(Load::Reading {
                    _task: cx.spawn(async move |this, cx| {
                        let result = read.await;
                        let _ = this.update(cx, |this, cx| {
                            if let Some(Self::Docs(docs)) = &mut this.panel {
                                *docs = result.into();
                                cx.notify();
                            }
                        });
                    }),
                })
            }
            SideTab::History => Self::History(History::read(&graph, cx)),
        }
    }

    pub(super) fn tab(&self) -> SideTab {
        match self {
            Self::Members => SideTab::Members,
            Self::Terminal => SideTab::Terminal,
            Self::Docs(_) => SideTab::Docs,
            Self::History(_) => SideTab::History,
        }
    }
}

impl SideSlot {
    pub(super) fn body(&mut self, cx: &mut Context<Self>) -> Div {
        let room = self.data.read(cx).room();
        match &self.panel {
            Some(Panel::Members) => self.member_panel(room, cx),
            Some(Panel::Terminal) => self.terminal_panel(cx),
            Some(Panel::Docs(docs)) => self.docs_panel(room, docs, cx),
            Some(Panel::History(history)) => self.history_panel(room, history, cx),
            None => div(),
        }
    }

    fn docs_panel(
        &self,
        room: Option<&ChatRoom>,
        docs: &Load<Vec<DocRow>>,
        cx: &Context<Self>,
    ) -> Div {
        let docs = match docs {
            Load::Reading { .. } => return notice(Str::DocsReading, cx),
            Load::Failed(error) => return ui::note(error.clone(), true, cx),
            Load::Ready(docs) => docs,
        };
        let Some(room) = room else {
            return notice(Str::DocsEmpty, cx);
        };
        let data = self.data.read(cx);
        let graph = data.graph();
        let ids = doc_ids(data, room, cx);
        let mut shown = docs
            .iter()
            .filter(|doc| ids.contains(doc.id.as_str()))
            .peekable();
        if shown.peek().is_none() {
            return notice(Str::DocsEmpty, cx);
        }
        let graph = graph.clone();
        let rows = document_rows(shown, cx, move |id, cx| {
            follow_link(&graph, body::Target::Doc(id), cx);
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(ui::hint(ui::say(cx, Str::DocsOrder), cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(scroller(&self.scroll, "chat-docs", rows)),
            )
    }

    fn history_panel(&self, room: Option<&ChatRoom>, history: &History, cx: &Context<Self>) -> Div {
        let log = match &history.load {
            Load::Reading { .. } => return notice(Str::EventsReading, cx),
            Load::Failed(error) => return ui::note(error.clone(), true, cx),
            Load::Ready(log) => log,
        };
        let Some(room) = room else {
            return notice(Str::EventsEmpty, cx);
        };
        let members: HashSet<_> = room.members.iter().copied().collect();
        let shown: Vec<_> = log
            .records
            .iter()
            .enumerate()
            .filter(|(_, record)| {
                what::who_of(&record.entry)
                    .iter()
                    .any(|who| who.id.is_some_and(|id| members.contains(&id)))
            })
            .map(|(index, _)| index)
            .collect();
        if history.list.item_count() != shown.len() {
            let top = history.list.logical_scroll_top();
            history.list.reset(shown.len());
            history.list.scroll_to(top);
        }
        let mut panel = div().size_full().flex().flex_col().child(ui::hint(
            words(cx, "구성원 활동 · 최신순", "Member activity · newest first"),
            cx,
        ));
        if shown.is_empty() {
            panel = panel.child(notice(Str::EventsEmpty, cx));
        } else {
            let rows = list(
                history.list.clone(),
                cx.processor(move |this: &mut Self, index: usize, _, cx| {
                    this.history_row(shown[index], cx)
                }),
            );
            panel = panel.child(history.bar.reserved(rows).flex_1().min_h_0());
        }
        panel
    }

    fn history_row(&self, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(Panel::History(history)) = &self.panel else {
            return div().into_any_element();
        };
        let Load::Ready(log) = &history.load else {
            return div().into_any_element();
        };
        let Some(record) = log.records.get(index) else {
            return div().into_any_element();
        };
        let graph = self.data.read(cx).graph().clone();
        let id = record.id;
        let row = EventRow {
            record,
            current: history.open == Some(id),
            agents: &[],
            nodes: graph
                .read(cx)
                .snapshot()
                .map_or(&[], |snapshot| &snapshot.nodes),
        };
        let follow = graph.clone();
        row.render(
            cx,
            cx.listener(move |this, _, _, cx| {
                if let Some(Panel::History(history)) = &mut this.panel {
                    history.toggle(id);
                    cx.notify();
                }
            }),
            move |target, cx| follow_link(&follow, target, cx),
        )
    }
}

pub(super) fn doc_ids<'a>(data: &'a ChatData, room: &ChatRoom, cx: &'a App) -> HashSet<&'a str> {
    let members: HashSet<_> = room.members.iter().copied().collect();
    let from_stream = data.items().iter().filter_map(|item| match &item.entry {
        ChatEntry::Wait { node, wait, .. } if node.id.is_some_and(|id| members.contains(&id)) => {
            wait.wait.doc.as_ref()
        }
        _ => None,
    });
    let from_graph = data
        .graph()
        .read(cx)
        .snapshot()
        .into_iter()
        .flat_map(|snapshot| &snapshot.nodes)
        .filter(|node| members.contains(&node.id))
        .flat_map(|node| &node.waits)
        .filter_map(|wait| wait.wait.doc.as_ref());
    from_stream
        .chain(from_graph)
        .map(|doc| doc.id.as_str())
        .collect()
}

fn notice(text: Str, cx: &App) -> Div {
    ui::note(ui::say(cx, text).to_owned(), false, cx)
}

fn follow_link(store: &Entity<Store>, target: body::Target, cx: &mut App) {
    let task = body::open(store, target, cx);
    let store = store.downgrade();
    cx.spawn(async move |cx| {
        if let Err(error) = task.await {
            let _ = cx.update(|cx| store.update(cx, |store, cx| store.tell(error, cx)));
        }
    })
    .detach();
}
