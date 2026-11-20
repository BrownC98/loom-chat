use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use api::graph::chat::{
    ChatCursor, ChatDelivery, ChatEntry, ChatItem, ChatMessage, ChatOrigin, ChatRoom, ChatRoomKind,
    ChatTranscriptItem,
};
use api::graph::events::{BatchResult, Entry};
use api::graph::messages::{Kind, MessageKind, Question};
use api::graph::text::Block;
use api::graph::waits::WaitTone;
use api::ids::{NodeId, WaitId};
use api::repos::RepoView;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Rgba, Stateful, StatefulInteractiveElement, Styled, Transformation, Window,
    canvas, div, radians, relative,
};

use super::{Quote, RowContent, Stream, StreamEvent, tr};
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::dock::blocks;
use crate::views::icons::{Lucide, icon};
use crate::views::prose::prose;
use crate::views::tooltip::{self, tip};
use crate::views::ui;
use crate::views::widgets::btn;

const MESSAGE_GROUP: &str = "stream-message";
const SENDER_WINDOW_MS: i64 = 3 * 60 * 1000;
const BUBBLE_WIDTH: f32 = 720.0;
const BUBBLE_SHARE: f32 = 0.72;

pub(super) fn hidden(item: &ChatItem) -> bool {
    match &item.entry {
        ChatEntry::Message { message } => {
            message.origin.visible_text(&message.text).trim().is_empty()
                || (message.origin == ChatOrigin::Human
                    && matches!(
                        message.what,
                        Some(MessageKind::Answer {
                            question: Question::Inbox { .. }
                        })
                    ))
        }
        ChatEntry::Transcript { item, .. } => match item {
            ChatTranscriptItem::Assistant { text, .. } | ChatTranscriptItem::User { text, .. } => {
                text.trim().is_empty()
            }
            ChatTranscriptItem::Thinking { summary } => {
                summary.as_deref().is_none_or(|text| text.trim().is_empty())
            }
            _ => false,
        },
        _ => false,
    }
}

#[derive(PartialEq, Eq)]
enum Sender<'a> {
    Human,
    Node(NodeId),
    Outside(&'a str),
    NamedAgent(&'a str),
    Loom,
}

fn sender<'a>(item: &'a ChatItem, room: Option<&'a ChatRoom>) -> Option<Sender<'a>> {
    match &item.entry {
        ChatEntry::Message { message } => Some(match message.origin {
            ChatOrigin::Human => Sender::Human,
            ChatOrigin::Loom => Sender::Loom,
            ChatOrigin::Agent => message
                .from
                .id
                .map_or(Sender::NamedAgent(&message.from.name), Sender::Node),
            ChatOrigin::Outside => Sender::Outside(&message.from.name),
        }),
        ChatEntry::Transcript {
            item: ChatTranscriptItem::User { .. },
            ..
        } => Some(Sender::Human),
        ChatEntry::Transcript {
            item: ChatTranscriptItem::Assistant { .. },
            ..
        } => {
            let room = room?;
            match room.kind {
                ChatRoomKind::Agent { node } => Some(Sender::Node(node)),
                _ => Some(Sender::NamedAgent(&room.name)),
            }
        }
        _ => None,
    }
}

impl Stream {
    pub(super) fn row(
        &mut self,
        index: usize,
        visible_waits: &Rc<RefCell<Vec<WaitId>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = &self.rows.visible[index];
        let cursor = row.cursor;
        let key = format!(
            "stream-{}-{}-{}",
            cursor.source as u8, cursor.id, cursor.part
        );
        let data = self.data.read(cx);
        let previous = index
            .checked_sub(1)
            .and_then(|index| self.rows.visible.get(index));
        let day = (cursor.at_ms > 0)
            .then(|| {
                ui::clock(
                    cursor.at_ms.div_euclid(1000),
                    tr(cx, "%Y년 %-m월 %-d일", "%b %-d, %Y"),
                )
            })
            .filter(|day| !day.is_empty());
        let date = day.filter(|day| {
            previous.is_none_or(|row| {
                ui::clock(
                    row.cursor.at_ms.div_euclid(1000),
                    tr(cx, "%Y년 %-m월 %-d일", "%b %-d, %Y"),
                ) != *day
            })
        });
        let grouped = date.is_none()
            && row.item().is_some_and(|item| {
                let item = &data.items()[item];
                previous
                    .and_then(|prev| prev.item())
                    .map(|prev| &data.items()[prev])
                    .is_some_and(|prev| {
                        cursor.at_ms > 0
                            && prev.cursor.at_ms > 0
                            && cursor.at_ms.saturating_sub(prev.cursor.at_ms) <= SENDER_WINDOW_MS
                            && sender(item, data.room())
                                .is_some_and(|author| sender(prev, data.room()) == Some(author))
                    })
            });
        let body = window.with_element_namespace(key.clone(), |window| {
            let index = match &self.rows.visible[index].content {
                RowContent::Activity(steps) => return self.activity(cursor, steps, cx),
                RowContent::Item(index) => *index,
            };
            let item = self.data.read(cx).items()[index].clone();
            match &item.entry {
                ChatEntry::Message { message } => self.message(&item, message, grouped, window, cx),
                ChatEntry::Wait { wait, closed, .. } => {
                    let visible =
                        (matches!(wait.tone, WaitTone::Ask) && closed.is_none()).then(|| {
                            let id = wait.wait.id;
                            let visible_waits = visible_waits.clone();
                            canvas(
                                |_, _, _| (),
                                move |bounds, (), window, _| {
                                    if bounds.intersects(&window.content_mask().bounds) {
                                        visible_waits.borrow_mut().push(id);
                                    }
                                },
                            )
                            .absolute()
                            .size_full()
                        });
                    self.wait_card(item, window, cx)
                        .relative()
                        .children(visible)
                }
                ChatEntry::Notice { event } => {
                    let (label, detail) = notice(event, cx);
                    self.folded(cursor, label, detail, cx)
                }
                ChatEntry::Transcript {
                    item: transcript, ..
                } => self.transcript(&item, transcript, grouped, window, cx),
            }
        });
        div()
            .id(key)
            .group(MESSAGE_GROUP)
            .w_full()
            .px(px(14.0))
            .py(px(if grouped { 2.0 } else { 6.0 }))
            .children(date.map(|day| {
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .pb(px(8.0))
                    .child(badge(day, ui::colors(cx).ink_2, cx))
            }))
            .child(body)
            .into_any_element()
    }

    pub(super) fn source<'a>(
        &self,
        named: Option<&str>,
        node: Option<NodeId>,
        cx: &'a App,
    ) -> Option<&'a ChatRoom> {
        let data = self.data.read(cx);
        if !data
            .room()
            .is_some_and(|room| matches!(room.kind, ChatRoomKind::Pinned))
        {
            return None;
        }
        let rooms = data.rooms();
        let named = named.and_then(|id| {
            rooms
                .iter()
                .find(|room| room.id == id && !matches!(room.kind, ChatRoomKind::Pinned))
        });
        named
            .filter(|room| matches!(room.kind, ChatRoomKind::Work { .. }))
            .or_else(|| {
                let node = node?;
                rooms.iter().find(|room| {
                    matches!(room.kind, ChatRoomKind::Work { .. }) && room.members.contains(&node)
                })
            })
            .or(named)
            .or_else(|| {
                let node = node?;
                rooms.iter().find(
                    |room| matches!(room.kind, ChatRoomKind::Agent { node: owner } if owner == node),
                )
            })
    }

    fn message_source(
        &self,
        named: Option<&str>,
        node: Option<NodeId>,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let room = self.source(named, node, cx)?;
        let data = self.data.read(cx);
        let repos = data
            .graph()
            .read(cx)
            .snapshot()
            .map_or(&[][..], |snapshot| snapshot.repos.as_slice());
        let name = source_name(room, repos, data.rooms(), cx);
        let id = room.id.clone();
        let c = ui::colors(cx);
        Some(
            btn::ghost("stream-source", cx)
                .min_w_0()
                .bg(c.accent_wash)
                .px(px(5.0))
                .py(px(2.0))
                .aria_label(format!(
                    "{} · {name}",
                    tr(cx, "방에서 보기", "View in room")
                ))
                .tooltip(tip(name.clone()))
                .child(div().min_w_0().truncate().child(name))
                .on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(StreamEvent::OpenRoom(id.clone()))),
                ),
        )
    }

    fn message(
        &self,
        item: &Arc<ChatItem>,
        message: &ChatMessage,
        grouped: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = ui::colors(cx);
        let mine = message.origin == ChatOrigin::Human;
        let display = match message.origin {
            ChatOrigin::Human => tr(cx, "나", "Me"),
            ChatOrigin::Loom => "loom",
            ChatOrigin::Agent | ChatOrigin::Outside => &message.from.name,
        };
        let node = message
            .from
            .id
            .or_else(|| message.receipts.iter().find_map(|receipt| receipt.to.id));
        let room = self.data.read(cx).room();
        let agent = room.and_then(|room| match room.kind {
            ChatRoomKind::Agent { node } => Some(node),
            _ => None,
        });
        let fold = (message.origin == ChatOrigin::Agent
            && !message.receipts.is_empty()
            && message
                .receipts
                .iter()
                .all(|receipt| receipt.to.id.is_some())
            && room.is_some_and(|room| {
                matches!(
                    room.kind,
                    ChatRoomKind::Work { .. } | ChatRoomKind::Custom | ChatRoomKind::Pinned
                )
            }))
        .then(|| self.is_expanded(item.cursor, cx));
        let recipient = room
            .filter(|room| {
                agent.is_none()
                    && fold.is_none()
                    && message.room.as_ref() == Some(&room.id)
                    && !matches!(room.kind, ChatRoomKind::Pinned)
            })
            .map(|room| room.name.clone())
            .or_else(|| {
                let recipients: Vec<_> = message
                    .receipts
                    .iter()
                    .filter(|receipt| agent.is_none() || receipt.to.id != agent)
                    .map(|receipt| receipt.to.name.as_str())
                    .collect();
                (!recipients.is_empty()).then(|| recipients.join(", "))
            });
        let source = self.message_source(message.room.as_deref(), node, cx);
        let author = match recipient.as_deref() {
            Some(recipient) => format!("{display} → {recipient}"),
            None => display.to_owned(),
        };
        let text = message.origin.visible_text(&message.text);
        if fold == Some(false) {
            let preview = text.lines().map(str::trim).find(|line| !line.is_empty());
            let label =
                preview.map_or_else(|| author.clone(), |preview| format!("{author} · {preview}"));
            return div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(self.expand_button(item.cursor, div().min_w_0().truncate().child(label), cx))
                .child(timestamp(item.cursor, cx))
                .children(source.map(|badge| badge.max_w(relative(0.4))));
        }
        let author = if fold == Some(true) {
            Some(
                self.expand_button(item.cursor, div().min_w_0().truncate().child(author), cx)
                    .into_any_element(),
            )
        } else if grouped {
            recipient.map(|recipient| meta(format!("→ {recipient}"), cx).into_any_element())
        } else {
            Some(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(prose(author))
                    .into_any_element(),
            )
        };
        let kind = message
            .what
            .as_ref()
            .and_then(|kind| message_kind(kind, cx));
        let mode = message.mode.and_then(|mode| match mode {
            Kind::Queue => None,
            Kind::Steer => Some(Str::EventsHowSteer),
            Kind::Interrupt => Some(Str::EventsHowInterrupt),
        });
        let show_header = author.is_some()
            || kind.is_some()
            || mode.is_some()
            || source.is_some()
            || message.reply_to.is_some();
        let receipts = self.receipts(message, agent, item.cursor, window, cx);
        let header = show_header.then(|| {
            flow()
                .children(author)
                .children(
                    (message.origin == ChatOrigin::Outside && !grouped)
                        .then(|| badge(tr(cx, "외부 에이전트", "Outside agent"), c.ink_2, cx)),
                )
                .children(mode.map(|mode| badge(ui::say(cx, mode), c.ink_2, cx)))
                .children(kind.map(|kind| badge(kind, c.ink_2, cx)))
                .children(
                    (receipts.is_none() || agent.is_none()).then(|| timestamp(item.cursor, cx)),
                )
                .children(message.reply_to.map(|id| meta(format!("#{id}"), cx)))
                .children(source)
        });
        let body = self.bubble(
            item,
            text,
            message.blocks.as_deref().unwrap_or_default(),
            window,
            cx,
        );
        let body = body.relative().children(
            (header.is_none() && receipts.is_none()).then(|| hover_time(item.cursor, mine, cx)),
        );
        let editor = self.message_editor(item, cx);
        column()
            .when(mine, |column| column.items_end())
            .children(header)
            .child(body)
            .children(receipts)
            .children(editor)
    }

    fn receipts(
        &self,
        message: &ChatMessage,
        agent: Option<NodeId>,
        cursor: ChatCursor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if message.receipts.is_empty() && !message.outside_read {
            return None;
        }
        let c = ui::colors(cx);
        let labels = message
            .receipts
            .iter()
            .map(|receipt| {
                let (label, ink) = match receipt.state {
                    ChatDelivery::Queued => (tr(cx, "대기 중", "Queued"), c.ink_2),
                    ChatDelivery::Handed => (tr(cx, "전달됨", "Delivered"), c.ink_2),
                    ChatDelivery::Printed => (tr(cx, "표시됨", "Shown"), c.ink_2),
                    ChatDelivery::Read => (tr(cx, "읽음", "Read"), c.ok),
                    ChatDelivery::Retrying => (tr(cx, "다시 전달 중", "Retrying"), c.warn),
                    ChatDelivery::Ended => (tr(cx, "전달하지 못함", "Undelivered"), c.err),
                };
                let label = if agent.is_some() && receipt.to.id == agent {
                    label.to_owned()
                } else {
                    format!("{} {label}", receipt.to.name)
                };
                let room = receipt
                    .to
                    .id
                    .filter(|_| {
                        matches!(receipt.state, ChatDelivery::Ended | ChatDelivery::Retrying)
                    })
                    .and_then(|node| {
                        self.data.read(cx).rooms().iter().find_map(|room|
                    matches!(room.kind, ChatRoomKind::Agent { node: owner } if owner == node)
                        .then(|| room.id.clone()))
                    });
                (label, ink, room)
            })
            .chain(message.outside_read.then(|| {
                (
                    tr(cx, "외부에서 읽음", "Read by outside agent").to_owned(),
                    c.ok,
                    None,
                )
            }))
            .collect::<Vec<_>>();
        let detail = labels
            .iter()
            .map(|(label, _, _)| label.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        let caption = div()
            .id("stream-receipts")
            .flex()
            .items_center()
            .min_w_0()
            .max_w_full()
            .gap(px(4.0))
            .overflow_hidden()
            .text_size(px(ui::SMALL))
            .whitespace_nowrap()
            .tooltip(tip(detail))
            .children(
                labels
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, ink, room))| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .min_w_0()
                            .child(div().min_w_0().truncate().text_color(ink).child(label))
                            .children(room.map(|id| {
                                let label = tr(cx, "1:1 세션 열기", "Open session");
                                let key = format!("stream-delivery-{index}");
                                action_button(&key, Lucide::ExternalLink, label, window, cx)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(StreamEvent::OpenRoom(id.clone()))
                                    }))
                            }))
                    }),
            );
        Some(
            div()
                .flex()
                .min_w_0()
                .max_w_full()
                .items_center()
                .gap(px(6.0))
                .child(caption)
                .children(agent.map(|_| timestamp(cursor, cx))),
        )
    }

    fn transcript(
        &self,
        row: &Arc<ChatItem>,
        item: &ChatTranscriptItem,
        grouped: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let cursor = row.cursor;
        match item {
            ChatTranscriptItem::User { text, blocks }
            | ChatTranscriptItem::Assistant { text, blocks } => {
                let mine = matches!(item, ChatTranscriptItem::User { .. });
                let name = if mine {
                    tr(cx, "나", "Me").to_owned()
                } else {
                    self.data
                        .read(cx)
                        .room()
                        .map(|room| room.name.clone())
                        .unwrap_or_else(|| tr(cx, "에이전트", "Agent").to_owned())
                };
                let body =
                    self.bubble(row, text, blocks.as_deref().unwrap_or_default(), window, cx);
                column()
                    .when(mine, |column| column.items_end())
                    .children((!grouped).then(|| {
                        flow()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                            .child(timestamp(cursor, cx))
                    }))
                    .child(
                        body.relative()
                            .children(grouped.then(|| hover_time(cursor, mine, cx))),
                    )
            }
            ChatTranscriptItem::Status { text } => {
                let text = if text == "턴이 멈췄습니다." {
                    tr(cx, "턴이 멈췄습니다.", "The turn stopped.")
                } else {
                    text
                };
                self.folded(
                    cursor,
                    tr(cx, "상태", "Status").to_owned(),
                    Some(text.to_owned()),
                    cx,
                )
            }
            _ => div(),
        }
    }

    pub(super) fn is_expanded(&self, cursor: ChatCursor, cx: &App) -> bool {
        let Some(expanded) = self.room.as_ref().and_then(|room| self.expanded.get(room)) else {
            return false;
        };
        let Ok(index) = self
            .rows
            .visible
            .binary_search_by_key(&cursor, |row| row.cursor)
        else {
            return false;
        };
        let last = self.rows.visible[index].last(self.data.read(cx).items());
        expanded.range(cursor..=last).next().is_some()
    }

    pub(super) fn expand_button(
        &self,
        cursor: ChatCursor,
        label: impl IntoElement,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let c = ui::colors(cx);
        let expanded = self.is_expanded(cursor, cx);
        btn::ghost("stream-expand", cx)
            .min_w_0()
            .aria_expanded(expanded)
            .child(icon(
                if expanded {
                    Lucide::ChevronDown
                } else {
                    Lucide::ChevronRight
                },
                px(14.0),
                c.ink_2,
            ))
            .child(label)
            .on_click(cx.listener(move |stream, _, _, cx| {
                let Some(room) = &stream.room else {
                    return;
                };
                let Ok(index) = stream
                    .rows
                    .visible
                    .binary_search_by_key(&cursor, |row| row.cursor)
                else {
                    return;
                };
                let last = stream.rows.visible[index].last(stream.data.read(cx).items());
                let expanded = stream.expanded.entry(room.clone()).or_default();
                let keys: Vec<_> = expanded.range(cursor..=last).copied().collect();
                if keys.is_empty() {
                    expanded.insert(cursor);
                } else {
                    for key in keys {
                        expanded.remove(&key);
                    }
                }
                stream.list.remeasure_items(index..index + 1);
                cx.notify();
            }))
    }

    fn folded(
        &self,
        cursor: ChatCursor,
        label: String,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = ui::colors(cx);
        let label = match &detail {
            Some(_) => self
                .expand_button(cursor, prose(label), cx)
                .into_any_element(),
            None => meta(label, cx).into_any_element(),
        };
        column()
            .px(px(8.0))
            .py(px(3.0))
            .border_y_1()
            .border_color(c.line)
            .child(flow().child(label).child(timestamp(cursor, cx)))
            .children(
                detail
                    .filter(|_| self.is_expanded(cursor, cx))
                    .map(|text| div().px(px(6.0)).child(prose(text))),
            )
    }
}

fn source_name(room: &ChatRoom, repos: &[RepoView], rooms: &[ChatRoom], cx: &App) -> String {
    let repo_for = |id| repos.iter().find(|repo| repo.id == id);
    let title = super::super::room_title(room, repo_for, cx);
    if let ChatRoomKind::Work { place } = &room.kind
        && let Some(repo) = repo_for(place.repo)
        && repo.git
        && (repos.len() > 1
            || rooms.iter().any(|room| {
                matches!(&room.kind, ChatRoomKind::Work { place: other } if other.repo != place.repo)
            }))
    {
        return format!("{} › {title}", repo.name);
    }
    title.to_owned()
}

pub(super) fn column() -> Div {
    div().flex().flex_col().min_w_0().gap(px(6.0))
}

pub(super) fn flow() -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .min_w_0()
        .gap(px(6.0))
}

pub(super) fn meta(text: impl Into<String>, cx: &App) -> Div {
    div()
        .text_size(px(ui::SMALL))
        .text_color(ui::colors(cx).ink_2)
        .child(prose(text.into()))
}

pub(super) fn timestamp(cursor: ChatCursor, cx: &App) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .text_size(px(ui::SMALL))
        .text_color(ui::colors(cx).ink_2)
        .children((cursor.at_ms > 0).then(|| ui::clock(cursor.at_ms.div_euclid(1000), "%H:%M")))
}

fn hover_time(cursor: ChatCursor, mine: bool, cx: &App) -> Div {
    timestamp(cursor, cx)
        .absolute()
        .bottom_0()
        .when(mine, |time| time.left_0())
        .when(!mine, |time| time.right_0())
        .opacity(0.0)
        .group_hover(MESSAGE_GROUP, |style| style.opacity(1.0))
}

pub(super) fn badge(text: impl Into<String>, ink: Rgba, cx: &App) -> Div {
    meta(text, cx)
        .rounded(px(4.0))
        .px(px(5.0))
        .py(px(2.0))
        .text_color(ink)
        .bg(ui::colors(cx).accent_wash)
}

impl Stream {
    fn bubble(
        &self,
        item: &Arc<ChatItem>,
        text: &str,
        content: &[Block],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = ui::colors(cx);
        let mine = matches!(sender(item, self.data.read(cx).room()), Some(Sender::Human));
        let quote = Quote(item.clone());
        let forward = (item.cursor.id >= 0).then(|| {
            action_button(
                "stream-forward",
                Lucide::CornerLeftUp,
                tr(cx, "전하기", "Forward"),
                window,
                cx,
            )
            .on_click(cx.listener(move |stream, _, _, cx| {
                if stream.current(&quote.0, cx).is_some() {
                    cx.emit(Quote(quote.0.clone()));
                }
            }))
        });
        let copy_text = text.to_owned();
        let copy = action_button(
            "stream-copy",
            Lucide::Copy,
            ui::say(cx, Str::TerminalCopy),
            window,
            cx,
        )
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()))
        });
        let edit = self.message_buttons(item, window, cx);
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .children(forward)
            .child(copy)
            .children(edit.into_iter().flatten());
        let follow = self.follow(cx);
        let ink = if mine { c.accent_ink } else { c.ink };
        let links = if mine { c.accent_ink } else { c.accent };
        let body = div()
            .min_w_0()
            .max_w(px(BUBBLE_WIDTH))
            .rounded(px(8.0))
            .bg(if mine { c.accent } else { c.surface })
            .text_color(ink)
            .when(!mine, |body| body.border_1().border_color(c.line))
            .px(px(11.0))
            .py(px(7.0))
            .child(blocks::render_selectable(
                "stream-body",
                content,
                text,
                None,
                links,
                ui::TEXT * crate::scale::get(),
                window,
                cx,
                follow,
            ));
        let body = div()
            .flex()
            .min_w_0()
            .max_w(relative(BUBBLE_SHARE))
            .child(body);
        let row = div().flex().items_start().w_full().min_w_0().gap(px(6.0));
        if mine {
            row.justify_end().child(actions).child(body)
        } else {
            row.child(body).child(actions)
        }
    }
}

pub(super) fn action_button(
    id: &str,
    glyph: Lucide,
    label: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let focus_key = format!("{id}-focus");
    let focus = window.use_keyed_state(focus_key, cx, |_, cx| cx.focus_handle().tab_stop(true));
    let focus = focus.read(cx);
    let focused = focus.is_focused(window);
    let c = ui::colors(cx);
    btn::ghost(id.to_owned(), cx)
        .track_focus(focus)
        .relative()
        .flex_none()
        .size(px(26.0))
        .px(px(0.0))
        .justify_center()
        .opacity(0.0)
        .group_hover(MESSAGE_GROUP, |style| style.opacity(1.0))
        .focus(|style| style.opacity(1.0))
        .aria_label(label)
        .when(!focused, |button| button.tooltip(tip(label.to_owned())))
        .when(focused, |button| {
            button.child(
                tooltip::label(label, cx)
                    .whitespace_nowrap()
                    .absolute()
                    .right_0()
                    .bottom_full()
                    .mb(px(4.0)),
            )
        })
        .child(
            icon(glyph, px(14.0), c.ink_2).when(glyph == Lucide::CornerLeftUp, |icon| {
                icon.with_transformation(Transformation::rotate(radians(
                    std::f32::consts::FRAC_PI_2,
                )))
            }),
        )
}

fn message_kind(kind: &MessageKind, cx: &App) -> Option<&'static str> {
    let label = match kind {
        MessageKind::Ask => Str::EventsTypeAsk,
        MessageKind::Answer { .. } => Str::EventsTypeAnswer,
        MessageKind::Done { .. } => Str::EventsTypeDone,
        MessageKind::Note => return None,
        MessageKind::Ready { .. } => Str::EventsTypeReady,
        MessageKind::Verdict { judgment, .. } => match judgment {
            api::graph::messages::Judgment::Pass => Str::EventsTypeVerdictPass,
            api::graph::messages::Judgment::Block => Str::EventsTypeVerdictBlock,
        },
        MessageKind::Withdrawn { .. } => Str::EventsTypeWithdrawn,
        MessageKind::Merged { .. } => Str::EventsTypeMerged,
        MessageKind::Rework { .. } => Str::EventsTypeRework,
        MessageKind::Failed { .. } => Str::EventsTypeFailed,
        MessageKind::Exited { .. } => Str::EventsTypeExited,
        MessageKind::SetupFailed { .. } => Str::EventsTypeSetupFailed,
        MessageKind::Removed { .. } => Str::EventsTypeRemoved,
        MessageKind::Adopted { .. } => Str::EventsTypeAdopted,
        MessageKind::HumanInput { .. } => Str::EventsTypeHumanInput,
        MessageKind::Idle => Str::EventsTypeIdle,
        MessageKind::RulesChanged { .. } => Str::EventsTypeRulesChanged,
    };
    Some(ui::say(cx, label))
}

fn notice(event: &Entry, cx: &App) -> (String, Option<String>) {
    let count = |ko, en, n| format!("{} {n}", tr(cx, ko, en));
    let (label, details) = match event {
        Entry::Created { node, .. } => (tr(cx, "들어옴", "Joined"), Some(node.name.clone())),
        Entry::Removed { node } => (tr(cx, "나감", "Left"), Some(node.name.clone())),
        Entry::Relaunched { node, error, .. } => {
            let detail = error.as_ref().map_or_else(
                || node.name.clone(),
                |error| format!("{} · {error}", node.name),
            );
            (tr(cx, "다시 띄움", "Relaunch"), Some(detail))
        }
        Entry::EndpointFailed { node, error } => (
            tr(cx, "연결 실패", "Connection failed"),
            Some(format!("{} · {error}", node.name)),
        ),
        Entry::AccountSwitched {
            moved,
            later,
            failed,
            ..
        } => (
            ui::say(cx, Str::EventsKindAccountSwitched),
            Some(
                [
                    count("전환", "Switched", moved),
                    count("대기", "Pending", later),
                    count("실패", "Failed", failed),
                ]
                .join(" · "),
            ),
        ),
        Entry::NoAccountLeft { resets_at, .. } => (
            tr(cx, "쓸 수 있는 계정 없음", "No available account"),
            resets_at.map(|at| ui::day_and_time(at, *cx.global::<Lang>())),
        ),
        Entry::Tidied { folders, logs, .. } => (
            tr(cx, "오래된 기록 정리", "Old records removed"),
            Some(
                [
                    count("폴더", "Folders", folders),
                    count("기록", "Logs", logs),
                ]
                .join(" · "),
            ),
        ),
        Entry::Batch {
            target,
            members,
            result,
            ..
        } => {
            let result = match result {
                BatchResult::Passed => tr(cx, "검사 통과", "Checks passed"),
                BatchResult::CheckFailed => tr(cx, "검사 실패", "Checks failed"),
                BatchResult::Conflict { .. } => tr(cx, "충돌", "Conflict"),
                BatchResult::TargetMoved => tr(cx, "대상 변경", "Target moved"),
                BatchResult::TargetDirty => {
                    tr(cx, "대상에 변경이 있음", "Uncommitted changes in target")
                }
                BatchResult::Interrupted => tr(cx, "중단", "Interrupted"),
                BatchResult::Error { cause } => cause,
            };
            let branches = members
                .iter()
                .map(|member| member.branch.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            (
                tr(cx, "병합 검사", "Merge checks"),
                Some(format!("{target} · {result}\n{branches}")),
            )
        }
        _ => (tr(cx, "알림", "Notice"), None),
    };
    (format!("loom · {label}"), details)
}
