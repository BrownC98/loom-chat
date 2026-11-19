use std::sync::Arc;

use api::graph::chat::{ChatEntry, ChatItem};
use api::graph::text::Block;
use api::graph::waits::{Closed, Press, ScreenWait, TermKey, WaitTone};
use gpui::{App, Context, Div, FontWeight, ParentElement, StatefulInteractiveElement, Styled, div};

use super::acts::{Input, WaitAction};
use super::rows::{badge, column, flow, meta, timestamp};
use super::{Stream, StreamEvent, tr};
use crate::scale::{self, px};
use crate::strings::Str;
use crate::theme::mix;
use crate::views::dock::entry::{self, Choice, Look};
use crate::views::dock::questions::{key_label, kind_label};
use crate::views::prose::prose;
use crate::views::ui;
use crate::views::widgets::btn;

const SCREEN_KEYS: [(&str, Press); 8] = [
    ("Enter", Press::Key(TermKey::Enter)),
    ("Esc", Press::Key(TermKey::Escape)),
    ("←", Press::Key(TermKey::Left)),
    ("→", Press::Key(TermKey::Right)),
    ("↑", Press::Key(TermKey::Up)),
    ("↓", Press::Key(TermKey::Down)),
    ("y", Press::Char('y')),
    ("n", Press::Char('n')),
];

impl Stream {
    pub(super) fn wait_card(&mut self, item: Arc<ChatItem>, cx: &mut Context<Self>) -> Div {
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
        let head = flow()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(node.name.clone()),
            )
            .child(badge(label, entry::tone_of(wait.tone, &c), cx))
            .child(meta(id.to_string(), cx))
            .child(timestamp(item.cursor, cx))
            .children(source);
        if let Some(closed) = closed {
            return column()
                .px(px(8.0))
                .py(px(6.0))
                .border_y_1()
                .border_color(c.line)
                .child(head)
                .child(meta(closed_text(closed, wait, cx), cx));
        }

        let active = !self.data.read(cx).room().is_some_and(|room| room.archived);
        let busy = self.wait_busy(id, cx);
        let mut card = column()
            .border_1()
            .border_color(if self.jumped == Some(item.cursor) {
                c.accent
            } else {
                mix(entry::tone_of(wait.tone, &c), 0.36, c.line)
            })
            .bg(c.surface)
            .rounded(px(8.0))
            .p(px(10.0))
            .child(head);
        if let Some(instruction) = &wait.wait.instruction {
            card = card.child(meta(instruction.clone(), cx));
        }
        if let Some(prompt) = &wait.wait.prompt {
            let text = if prompt.body.is_empty() {
                prompt.summary.clone()
            } else {
                prompt.body.join("\n")
            };
            card = card.child(
                div()
                    .p(px(7.0))
                    .rounded(px(4.0))
                    .bg(c.panel)
                    .font_family(crate::fonts::TERMINAL_FAMILY)
                    .child(prose(text)),
            );
        } else if !wait.wait.text.is_empty() {
            let text = if wait.wait.blocks.is_empty() {
                wait.wait.text.clone()
            } else {
                Block::shown(&wait.wait.blocks)
            };
            card = card.child(prose(text));
        }

        if let Some(doc) = &wait.wait.doc {
            let id = doc.id.clone();
            card = card.child(
                flow().child(
                    btn::ghost("stream-wait-doc", cx)
                        .child(doc.title.clone())
                        .on_click(cx.listener(move |stream, _, _, cx| {
                            let result = stream
                                .data
                                .read(cx)
                                .graph()
                                .read(cx)
                                .doc_url(&id)
                                .and_then(|url| ui::open_url(&url));
                            if let Err(error) = result {
                                stream.result = Some((error, true));
                                cx.notify();
                            }
                        })),
                ),
            );
        }
        if !active {
            return card;
        }
        if busy {
            return card.child(meta(tr(cx, "요청을 보내는 중", "Sending request"), cx));
        }

        if matches!(wait.tone, WaitTone::Done) {
            let confirm = item.clone();
            let actions = flow()
                .child(
                    btn::button("stream-wait-confirm", cx)
                        .child(ui::say(cx, Str::InboxConfirm))
                        .on_click(cx.listener(move |stream, _, _, cx| {
                            stream.act_on_wait(confirm.clone(), WaitAction::Confirm, cx)
                        })),
                )
                .children(node.id.map(|node| {
                    let item = item.clone();
                    btn::button("stream-wait-follow", cx)
                        .child(tr(cx, "이어서 맡기기", "Follow up"))
                        .on_click(cx.listener(move |stream, _, _, cx| {
                            if stream.current(&item, cx).is_some_and(|item| {
                                matches!(item.entry, ChatEntry::Wait { closed: None, .. })
                            }) && !stream.wait_busy(id, cx)
                            {
                                cx.emit(StreamEvent::FollowUp { node, wait: id });
                            }
                        }))
                }));
            card.child(actions)
        } else {
            card.children(self.wait_actions(&item, wait, cx))
        }
    }

    fn wait_actions(
        &mut self,
        item: &Arc<ChatItem>,
        wait: &ScreenWait,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let id = wait.wait.id.0;
        let hint = tr(cx, "답을 글로 씁니다", "Write an answer");
        let mut actions = column();
        if let Some(prompt) = &wait.wait.prompt {
            for (index, option) in prompt.options.iter().enumerate() {
                let key = key_label(&option.key, index);
                if option.writes {
                    let input = Input::Prompt(id, Some(index));
                    let field = self.wait_input(input, item, &option.label, cx);
                    actions = actions.child(answer_field(input, field, &option.label, cx));
                } else {
                    let row = item.clone();
                    actions = actions.child(
                        flow().child(
                            btn::button(("stream-prompt", index), cx)
                                .child(format!("{key} · {}", option.label))
                                .on_click(cx.listener(move |stream, _, _, cx| {
                                    stream.act_on_wait(row.clone(), WaitAction::Choose(index), cx)
                                })),
                        ),
                    );
                }
            }
            if prompt.field.is_some() {
                let input = Input::Prompt(id, None);
                let field = self.wait_input(input, item, hint, cx);
                actions = actions.child(answer_field(input, field, tr(cx, "답하기", "Answer"), cx));
            }
        } else if wait.wait.answerable {
            for (index, choice) in wait.wait.choices.iter().enumerate() {
                let row = item.clone();
                let key = choice.key;
                let look = if choice.recommended {
                    Look::Recommended
                } else {
                    Look::Plain
                };
                actions = actions.child(
                    entry::choice_face(
                        btn::button(("stream-answer-choice", index), cx),
                        Choice {
                            key: key.to_string(),
                            hint: None,
                            label: &choice.label,
                            recommended: choice.recommended,
                            look,
                            tone: wait.tone,
                        },
                        scale::get(),
                        cx,
                    )
                    .on_click(cx.listener(move |stream, _, _, cx| {
                        stream.act_on_wait(row.clone(), WaitAction::Answer(key.to_string()), cx)
                    })),
                );
            }
            let input = Input::Answer(id);
            let field = self.wait_input(input, item, hint, cx);
            actions = actions.child(answer_field(input, field, tr(cx, "답하기", "Answer"), cx));
        } else if !wait.wait.screen.is_empty() {
            let input = Input::Screen(id);
            let field = self.wait_input(input, item, hint, cx);
            actions =
                actions
                    .child(
                        div()
                            .font_family(crate::fonts::TERMINAL_FAMILY)
                            .child(prose(wait.wait.screen.join("\n"))),
                    )
                    .child(flow().children(SCREEN_KEYS.iter().enumerate().map(
                        |(index, (label, press))| {
                            let row = item.clone();
                            let press = press.clone();
                            btn::button(("stream-screen-key", index), cx)
                                .child(*label)
                                .on_click(cx.listener(move |stream, _, _, cx| {
                                    stream.act_on_wait(
                                        row.clone(),
                                        WaitAction::Press(press.clone()),
                                        cx,
                                    )
                                }))
                        },
                    )))
                    .child(answer_field(input, field, tr(cx, "입력", "Enter text"), cx));
        } else {
            return None;
        }
        Some(actions)
    }
}

fn answer_field(
    input: Input,
    field: gpui::Entity<crate::views::field::Field>,
    label: &str,
    cx: &mut Context<Stream>,
) -> Div {
    let index = match input {
        Input::Prompt(_, Some(index)) => index,
        _ => usize::MAX,
    };
    flow()
        .child(div().flex_1().min_w(px(130.0)).child(field))
        .child(
            btn::button(("stream-submit-answer", index), cx)
                .child(label.to_owned())
                .on_click(cx.listener(move |stream, _, _, cx| stream.submit(input, cx))),
        )
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
            format!("{} · {answer}", tr(cx, "답함", "Answered"))
        }
        Closed::Instructed { text } => format!("{} · {text}", tr(cx, "이어서 맡김", "Followed up")),
        Closed::Confirmed => tr(cx, "확인함", "Confirmed").to_owned(),
        Closed::Unanswered { .. } => tr(cx, "답 없이 닫힘", "Closed without an answer").to_owned(),
        Closed::NodeRemoved => tr(
            cx,
            "에이전트 삭제로 닫힘",
            "Closed when the agent was removed",
        )
        .to_owned(),
    }
}
