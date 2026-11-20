use std::sync::Arc;

use api::graph::chat::{ChatEntry, ChatItem};
use api::graph::events::ClosedBy;
use api::graph::waits::{Closed, ScreenWait, WaitTone};
use gpui::{
    App, Context, Div, FontWeight, ParentElement, StatefulInteractiveElement, Styled, Window, div,
};

use super::rows::{badge, column, flow, meta, timestamp};
use super::{Stream, StreamEvent, tr};
use crate::scale::{self, px};
use crate::strings::Str;
use crate::theme::mix;
use crate::views::dock::answers::{self, CardState};
use crate::views::dock::entry;
use crate::views::dock::questions::kind_label;
use crate::views::ui;
use crate::views::widgets::btn;

impl Stream {
    pub(super) fn wait_card(
        &mut self,
        item: Arc<ChatItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let ChatEntry::Wait { node, wait, closed } = &item.entry else {
            return div();
        };
        let c = ui::colors(cx);
        let id = wait.wait.id;
        let label = ui::say(cx, kind_label(wait.wait.kind));
        let source = self.source(None, node.id, cx).map(|room| {
            let id = room.id.clone();
            flow().child(meta(room.name.clone(), cx)).child(
                btn::ghost("stream-source", cx)
                    .child(tr(cx, "방에서 보기", "View in room"))
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(StreamEvent::OpenRoom(id.clone()))),
                    ),
            )
        });
        let confirmed = matches!(closed, Some(Closed::Confirmed));
        let head = flow()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(node.name.clone()),
            )
            .child(badge(label, entry::tone_of(wait.tone, &c), cx))
            .children(confirmed.then(|| badge(ui::say(cx, Str::InboxClosedConfirmed), c.ink_2, cx)))
            .child(timestamp(item.cursor, cx))
            .children(source);
        let active_node = node.id.filter(|_| {
            closed.is_none() && self.data.read(cx).room().is_some_and(|room| !room.archived)
        });
        let state = if let Some(node) = active_node {
            self.answers
                .update(cx, |answers, cx| answers.prepare(node, wait, cx));
            CardState::Inline
        } else {
            self.answers
                .update(cx, |answers, cx| answers.load_images(wait, cx));
            CardState::ReadOnly
        };
        let open = state == CardState::Inline;
        let done = matches!(wait.tone, WaitTone::Done);
        let busy = self.data.read(cx).graph().read(cx).is_busy(id.0);
        column()
            .border_1()
            .border_color(if self.jumped == Some(item.cursor) {
                c.accent
            } else if !open {
                c.line
            } else {
                mix(entry::tone_of(wait.tone, &c), 0.36, c.line)
            })
            .bg(c.surface)
            .rounded(px(8.0))
            .p(px(10.0))
            .child(head)
            .child(answers::content(
                &self.answers,
                wait,
                state,
                scale::get(),
                window,
                cx,
            ))
            .children(
                closed
                    .as_ref()
                    .filter(|_| !confirmed)
                    .map(|closed| meta(closed_text(closed, wait, cx), cx)),
            )
            .children(active_node.filter(|_| done && !busy).map(|node| {
                let item = item.clone();
                btn::button("stream-wait-follow", cx)
                    .self_start()
                    .child(tr(cx, "이어서 맡기기", "Follow up"))
                    .on_click(cx.listener(move |stream, _, _, cx| {
                        if stream.current(&item, cx).is_some_and(|item| {
                            matches!(item.entry, ChatEntry::Wait { closed: None, .. })
                        }) && !stream.data.read(cx).graph().read(cx).is_busy(id.0)
                        {
                            cx.emit(StreamEvent::FollowUp { node, wait: id });
                        }
                    }))
            }))
            .children(
                (open && !done)
                    .then(|| answers::composer(&self.answers, wait, &node.name, scale::get(), cx))
                    .flatten(),
            )
    }
}

fn closed_text(closed: &Closed, wait: &ScreenWait, cx: &App) -> String {
    match closed {
        Closed::Answered { text } => {
            let answer = wait
                .wait
                .choices
                .iter()
                .find(|choice| text.starts_with(choice.key) && text.len() == choice.key.len_utf8())
                .map_or_else(
                    || text.clone(),
                    |choice| format!("{} · {}", choice.key, choice.label),
                );
            format!("{} · {answer}", ui::say(cx, Str::InboxClosedAnswered))
        }
        Closed::Instructed { text } => {
            format!("{} · {text}", ui::say(cx, Str::InboxClosedInstructed))
        }
        Closed::Confirmed => ui::say(cx, Str::InboxClosedConfirmed).to_owned(),
        Closed::Unanswered { by } => ui::say(
            cx,
            match by {
                ClosedBy::Human => Str::InboxClosedByHuman,
                ClosedBy::Agent => Str::InboxClosedByAgent,
                ClosedBy::Restart => Str::InboxClosedByRestart,
                ClosedBy::Exit => Str::InboxClosedByExit,
            },
        )
        .to_owned(),
        Closed::NodeRemoved => ui::say(cx, Str::InboxClosedNodeRemoved).to_owned(),
    }
}
