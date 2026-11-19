use std::sync::Arc;

use api::graph::chat::{
    ChatCursor, ChatDelivery, ChatEntry, ChatItem, ChatMessage, ChatOrigin, ChatRoom, ChatRoomKind,
    ChatTranscriptItem,
};
use api::graph::events::{BatchResult, Entry};
use api::graph::messages::{Kind, MessageKind};
use api::ids::NodeId;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Rgba, StatefulInteractiveElement, Styled, div,
};

use super::{Stream, StreamEvent, tr};
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::icons::{Lucide, icon};
use crate::views::prose::prose;
use crate::views::ui;
use crate::views::widgets::btn;

impl Stream {
    pub(super) fn row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self
            .rows
            .visible
            .get(index)
            .and_then(|row| self.data.read(cx).items().get(row.item).cloned())
        else {
            return div().into_any_element();
        };
        let cursor = item.cursor;
        let body = match &item.entry {
            ChatEntry::Message { message } => self.message(&item, message, cx),
            ChatEntry::Wait { .. } => self.wait_card(item, cx),
            ChatEntry::Notice { event } => {
                let (label, detail) = notice(event, cx);
                self.folded(cursor, label, detail, cx)
            }
            ChatEntry::Transcript { item } => self.transcript(cursor, item, cx),
        };
        div()
            .id(format!(
                "stream-{}-{}-{}",
                cursor.source as u8, cursor.id, cursor.part
            ))
            .w_full()
            .px(px(14.0))
            .py(px(6.0))
            .child(body)
            .into_any_element()
    }

    pub(super) fn source<'a>(
        &self,
        room: Option<&str>,
        node: Option<NodeId>,
        cx: &'a App,
    ) -> Option<&'a ChatRoom> {
        let rooms = self.data.read(cx).rooms();
        room
            .and_then(|id| rooms.iter().find(|room| room.id == id && !matches!(room.kind, ChatRoomKind::Pinned)))
            .or_else(|| {
                let node = node?;
                rooms.iter().find(|room| matches!(room.kind, ChatRoomKind::Work { .. }) && room.members.contains(&node))
                    .or_else(|| rooms.iter().find(|room| matches!(room.kind, ChatRoomKind::Agent { node: owner } if owner == node)))
            })
    }

    pub(super) fn source_link(
        &self,
        room: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let data = self.data.read(cx);
        if !data
            .room()
            .is_some_and(|room| matches!(room.kind, ChatRoomKind::Pinned))
        {
            return None;
        }
        let (id, name) = room?;
        Some(
            flow().child(meta(name, cx)).child(
                btn::ghost("stream-source", cx)
                    .child(tr(cx, "방에서 보기", "View in room"))
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(StreamEvent::OpenRoom(id.clone()))),
                    ),
            ),
        )
    }

    fn message(&self, item: &Arc<ChatItem>, message: &ChatMessage, cx: &mut Context<Self>) -> Div {
        let c = ui::colors(cx);
        let mine = message.origin == ChatOrigin::Human;
        let display = match message.origin {
            ChatOrigin::Human => tr(cx, "나", "Me"),
            ChatOrigin::Loom => "loom",
            ChatOrigin::Agent | ChatOrigin::Outside => &message.from.name,
        };
        let node = message.from.id.or(match message.receipts.as_slice() {
            [receipt] => receipt.to.id,
            _ => None,
        });
        let room = self.source(message.room.as_deref(), node, cx);
        let recipient = self
            .data
            .read(cx)
            .room()
            .and_then(|current| {
                let named = message.room.as_ref()?;
                (named == &current.id && !matches!(current.kind, ChatRoomKind::Pinned))
                    .then(|| current.name.clone())
            })
            .or_else(|| {
                let recipients: Vec<_> = message
                    .receipts
                    .iter()
                    .map(|receipt| receipt.to.name.as_str())
                    .collect();
                (!recipients.is_empty()).then(|| recipients.join(", "))
            });
        let author = match recipient {
            Some(recipient) => format!("{display} → {recipient}"),
            None => display.to_owned(),
        };
        let header = flow()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(prose(author)))
            .children(
                (message.origin == ChatOrigin::Outside)
                    .then(|| badge(tr(cx, "외부 에이전트", "Outside agent"), c.ink_2, cx)),
            )
            .children(
                message
                    .mode
                    .map(|mode| badge(mode_name(mode, cx), c.ink_2, cx)),
            )
            .children(
                message
                    .what
                    .as_ref()
                    .and_then(|kind| message_kind(kind, cx))
                    .map(|kind| badge(kind, c.ink_2, cx)),
            )
            .children(message.reply_to.map(|id| meta(format!("#{id}"), cx)))
            .child(timestamp(item.cursor, cx))
            .children(self.source_link(room.map(|room| (room.id.clone(), room.name.clone())), cx));
        let receipts = (!message.receipts.is_empty() || message.outside_read).then(|| {
            flow()
                .children(message.receipts.iter().enumerate().map(|(index, receipt)| {
                    let (label, ink) = match receipt.state {
                        ChatDelivery::Queued => (tr(cx, "대기 중", "Queued"), c.ink_2),
                        ChatDelivery::Handed => (tr(cx, "전달됨", "Delivered"), c.ink_2),
                        ChatDelivery::Printed => (tr(cx, "표시됨", "Shown"), c.ink_2),
                        ChatDelivery::Read => (tr(cx, "읽음", "Read"), c.ok),
                        ChatDelivery::Retrying => (tr(cx, "다시 전달 중", "Retrying"), c.warn),
                        ChatDelivery::Ended => (tr(cx, "전달하지 못함", "Undelivered"), c.err),
                    };
                    let target = receipt.to.id.filter(|_| {
                        matches!(receipt.state, ChatDelivery::Ended | ChatDelivery::Retrying)
                    });
                    let room = target.and_then(|node| {
                        self.data.read(cx).rooms().iter().find_map(|room| {
                        matches!(room.kind, ChatRoomKind::Agent { node: owner } if owner == node)
                            .then(|| room.id.clone())
                    })
                    });
                    flow()
                        .child(badge(format!("{} {label}", receipt.to.name), ink, cx))
                        .children(room.map(|id| {
                            btn::ghost(("stream-delivery-room", index), cx)
                                .child(tr(cx, "1:1 세션 열기", "Open session"))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(StreamEvent::OpenRoom(id.clone()))
                                }))
                        }))
                }))
                .children(
                    message
                        .outside_read
                        .then(|| badge(tr(cx, "외부에서 읽음", "Read by outside agent"), c.ok, cx)),
                )
        });
        let actions = self.message_actions(item, cx);
        column()
            .when(mine, |column| column.items_end())
            .child(header)
            .child(bubble(
                super::super::message_text(message),
                if mine { c.accent_wash } else { c.surface },
                cx,
            ))
            .children(receipts)
            .children(actions)
    }

    fn transcript(
        &self,
        cursor: ChatCursor,
        item: &ChatTranscriptItem,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = ui::colors(cx);
        match item {
            ChatTranscriptItem::User { text } | ChatTranscriptItem::Assistant { text } => {
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
                column()
                    .when(mine, |column| column.items_end())
                    .child(
                        flow()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                            .child(timestamp(cursor, cx)),
                    )
                    .child(bubble(
                        text,
                        if mine { c.accent_wash } else { c.surface },
                        cx,
                    ))
            }
            ChatTranscriptItem::ToolCall { name, summary, .. } => {
                let label = tr(cx, "도구", "Tool");
                self.folded(cursor, format!("{label} · {name}"), summary.clone(), cx)
            }
            ChatTranscriptItem::ToolResult {
                summary, failed, ..
            } => self.folded(
                cursor,
                if failed == &Some(true) {
                    tr(cx, "도구 실패", "Tool failed")
                } else {
                    tr(cx, "도구 결과", "Tool result")
                }
                .to_owned(),
                summary.clone(),
                cx,
            ),
            ChatTranscriptItem::Thinking { summary } => self.folded(
                cursor,
                tr(cx, "생각", "Thinking").to_owned(),
                summary.clone(),
                cx,
            ),
            ChatTranscriptItem::Status { text } => self.folded(
                cursor,
                tr(cx, "상태", "Status").to_owned(),
                Some(text.clone()),
                cx,
            ),
        }
    }

    fn folded(
        &self,
        cursor: ChatCursor,
        label: String,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = ui::colors(cx);
        let expanded = self.expanded.contains(&cursor);
        let label = match &detail {
            Some(_) => btn::ghost("stream-expand", cx)
                .child(icon(
                    if expanded {
                        Lucide::ChevronDown
                    } else {
                        Lucide::ChevronRight
                    },
                    px(14.0),
                    c.ink_2,
                ))
                .child(prose(label))
                .on_click(cx.listener(move |stream, _, _, cx| {
                    if !stream.expanded.remove(&cursor) {
                        stream.expanded.insert(cursor);
                    }
                    if let Ok(index) = stream
                        .rows
                        .visible
                        .binary_search_by_key(&cursor, |row| row.cursor)
                    {
                        stream.list.remeasure_items(index..index + 1);
                    }
                    cx.notify();
                }))
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
                    .filter(|_| expanded)
                    .map(|text| div().px(px(6.0)).child(prose(text))),
            )
    }
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
        .children(
            (cursor.at_ms > 0).then(|| ui::clock(cursor.at_ms.div_euclid(1000), "%m.%d %H:%M")),
        )
}

pub(super) fn badge(text: impl Into<String>, ink: Rgba, cx: &App) -> Div {
    meta(text, cx)
        .rounded(px(4.0))
        .px(px(5.0))
        .py(px(2.0))
        .text_color(ink)
        .bg(ui::colors(cx).accent_wash)
}

fn bubble(text: &str, fill: Rgba, cx: &App) -> Div {
    div()
        .min_w_0()
        .max_w_full()
        .rounded(px(8.0))
        .border_1()
        .border_color(ui::colors(cx).line)
        .bg(fill)
        .px(px(11.0))
        .py(px(7.0))
        .child(prose(text.to_owned()))
}

fn mode_name(mode: Kind, cx: &App) -> &'static str {
    match mode {
        Kind::Queue => tr(cx, "일반", "Queue"),
        Kind::Steer => tr(cx, "조정", "Steer"),
        Kind::Interrupt => tr(cx, "중단", "Interrupt"),
    }
}

fn message_kind(kind: &MessageKind, cx: &App) -> Option<&'static str> {
    let label = match kind {
        MessageKind::Ask => Str::EventsTypeAsk,
        MessageKind::Answer { .. } => Str::EventsTypeAnswer,
        MessageKind::Done { .. } => Str::EventsTypeDone,
        MessageKind::Note => return None,
        _ => return Some(kind.word()),
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
            tr(cx, "계정 전환", "Account switch"),
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
