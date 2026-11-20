use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;

use api::graph::chat::{ChatCursor, ChatEntry, ChatItem, ChatTranscriptItem};
use gpui::{App, Context, Div, ParentElement, Styled, div};

use super::rows::{column, flow, meta, timestamp};
use super::{Stream, tr};
use crate::scale::px;
use crate::strings::Lang;
use crate::views::icons::{Lucide, icon};
use crate::views::prose::prose;
use crate::views::ui;

const TOOL_NAMES: usize = 3;

struct Step<'a> {
    cursor: ChatCursor,
    item: &'a ChatTranscriptItem,
    result: Option<&'a ChatTranscriptItem>,
}

impl Stream {
    pub(super) fn activity(
        &self,
        cursor: ChatCursor,
        indices: &[usize],
        cx: &Context<Self>,
    ) -> Div {
        let steps = steps(indices, self.data.read(cx).items());
        let mut seen = BTreeSet::new();
        let names: Vec<_> = steps
            .iter()
            .filter_map(|step| match step.item {
                ChatTranscriptItem::ToolCall { name, .. } if seen.insert(name.as_str()) => {
                    Some(name.as_str())
                }
                _ => None,
            })
            .take(TOOL_NAMES)
            .collect();
        let count = steps.len();
        let label = match *cx.global::<Lang>() {
            Lang::Ko => format!("작업 과정 {count}단계"),
            Lang::En if count == 1 => "Activity · 1 step".to_owned(),
            Lang::En => format!("Activity · {count} steps"),
        };
        let label = if names.is_empty() {
            label
        } else {
            format!("{label} · {}", names.join(", "))
        };
        let c = ui::colors(cx);
        column()
            .items_start()
            .child(
                self.expand_button(cursor, div().min_w_0().truncate().child(label), cx)
                    .rounded_full()
                    .border_color(c.line)
                    .bg(c.surface),
            )
            .children(self.is_expanded(cursor, cx).then(|| {
                column()
                    .pl(px(12.0))
                    .children(steps.iter().map(|step| step.render(cx)))
            }))
    }
}

fn steps<'a>(indices: &[usize], items: &'a [Arc<ChatItem>]) -> Vec<Step<'a>> {
    let mut steps: Vec<Step<'_>> = Vec::new();
    let mut pending: HashMap<Option<&str>, VecDeque<usize>> = HashMap::new();
    for &index in indices {
        let row = &items[index];
        let ChatEntry::Transcript { item, .. } = &row.entry else {
            continue;
        };
        match item {
            ChatTranscriptItem::ToolCall { id, .. } => {
                pending
                    .entry(id.as_deref())
                    .or_default()
                    .push_back(steps.len());
            }
            ChatTranscriptItem::ToolResult { id, .. } => {
                if let Some(call) = pending
                    .get_mut(&id.as_deref())
                    .and_then(VecDeque::pop_front)
                {
                    steps[call].result = Some(item);
                    continue;
                }
            }
            _ => {}
        }
        steps.push(Step {
            cursor: row.cursor,
            item,
            result: None,
        });
    }
    steps
}

impl Step<'_> {
    fn render(&self, cx: &App) -> Div {
        let c = ui::colors(cx);
        let (label, text, result) = match self.item {
            ChatTranscriptItem::Thinking { summary } => {
                (tr(cx, "생각", "Thinking"), summary.as_deref(), None)
            }
            ChatTranscriptItem::ToolCall { name, summary, .. } => {
                (name.as_str(), summary.as_deref(), self.result)
            }
            ChatTranscriptItem::ToolResult { .. } => {
                (tr(cx, "도구", "Tool"), None, Some(self.item))
            }
            _ => unreachable!(),
        };
        let result = match result {
            Some(ChatTranscriptItem::ToolResult {
                summary, failed, ..
            }) => {
                let (glyph, label, color) = match failed {
                    Some(true) => (Lucide::X, tr(cx, "실패", "Failed"), c.err),
                    Some(false) => (Lucide::Check, tr(cx, "성공", "Succeeded"), c.ok),
                    None => (Lucide::Terminal, tr(cx, "결과", "Result"), c.ink_2),
                };
                Some((glyph, label, color, summary.as_deref()))
            }
            _ => None,
        };
        column()
            .gap(px(2.0))
            .child(
                flow()
                    .child(meta(label, cx))
                    .children(result.map(|(glyph, label, color, _)| {
                        flow()
                            .gap(px(3.0))
                            .child(icon(glyph, px(12.0), color))
                            .child(meta(label, cx).text_color(color))
                    }))
                    .child(timestamp(self.cursor, cx)),
            )
            .children(text.map(|text| prose(text.to_owned())))
            .children(
                result
                    .and_then(|(_, _, _, summary)| summary)
                    .filter(|&summary| Some(summary) != text)
                    .map(|summary| prose(summary.to_owned())),
            )
    }
}
