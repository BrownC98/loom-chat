use api::graph::NodeView;
use api::graph::chat::{ChatCursor, ChatEntry, ChatOrigin, ChatPage, ChatRoomKind, ChatSource};
use api::graph::waits::{ScreenWait, WaitTone};
use api::ids::{NodeId, WaitId};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Div, Entity, FontWeight, InteractiveElement, KeyDownEvent,
    ParentElement, Role, StatefulInteractiveElement, Styled, Subscription, Task, Window, div,
};

use super::{Composer, local};
use crate::link;
use crate::problem::Failure;
use crate::scale::{self, px};
use crate::store::Store;
use crate::strings::Str;
use crate::theme::mix;
use crate::views::dock::answers::{self, Answers};
use crate::views::dock::entry::tone_of;
use crate::views::icons::{Lucide, icon};
use crate::views::tooltip::tip;
use crate::views::{ui, widgets::btn};

type Position = (i64, u32);

struct Alert<'a> {
    node: &'a NodeView,
    wait: &'a ScreenWait,
}

impl Alert<'_> {
    fn position(&self) -> Position {
        (self.wait.wait.since, self.wait.wait.id.0)
    }
}

#[derive(Default)]
struct AlertContext {
    last_message: Option<String>,
    previous_reply: Option<String>,
}

pub(super) struct Alerts {
    answers: Entity<Answers>,
    pub(super) expanded: bool,
    selected: Option<Position>,
    focus: bool,
    context: Option<(WaitId, AlertContext)>,
    reading: Option<Task<()>>,
    _answers: Subscription,
}

impl Alerts {
    pub fn new(graph: Entity<Store>, cx: &mut Context<Composer>) -> Self {
        let answers = cx.new(|cx| Answers::new(graph, cx));
        let subscription = cx.observe(&answers, |_, _, cx| cx.notify());
        Self {
            answers,
            expanded: false,
            selected: None,
            focus: false,
            context: None,
            reading: None,
            _answers: subscription,
        }
    }

    fn current(&self, notices: &[Alert<'_>]) -> usize {
        let Some(selected) = self.selected else {
            return 0;
        };
        let index = notices.partition_point(|notice| notice.position() < selected);
        index % notices.len()
    }
}

enum Move {
    Fold,
    Step(isize),
    Choose(usize),
}

impl Composer {
    fn notices<'a>(&self, cx: &'a App) -> Vec<Alert<'a>> {
        let graph = self.data.read(cx).graph().read(cx);
        let mut notices: Vec<_> = graph
            .snapshot()
            .into_iter()
            .flat_map(|snapshot| &snapshot.nodes)
            .flat_map(|node| {
                node.waits
                    .iter()
                    .filter(|wait| {
                        answers::in_alert_strip(wait) && !self.visible_waits.contains(&wait.wait.id)
                    })
                    .map(move |wait| Alert { node, wait })
            })
            .collect();
        notices.sort_by_key(Alert::position);
        notices
    }

    fn move_alert(&mut self, movement: Move, cx: &mut Context<Self>) -> bool {
        if self.picker.open || self.viewer.read(cx).is_open() {
            return false;
        }
        let notices = self.notices(cx);
        if notices.is_empty() {
            return false;
        }
        let current = self.alerts.current(&notices);
        let index = match movement {
            Move::Step(by) => (current as isize + by).rem_euclid(notices.len() as isize) as usize,
            _ => current,
        };
        let notice = &notices[index];
        let wait = notice.wait.clone();
        let position = notice.position();
        self.alerts.expanded = !matches!(movement, Move::Fold) || !self.alerts.expanded;
        self.mode_menu = false;
        self.alerts.selected = Some(position);
        self.alerts.focus = self.alerts.expanded;
        if !self.alerts.expanded {
            self.alerts.reading = None;
            self.alerts.context = None;
        }
        if let Move::Choose(index) = movement {
            self.alerts.answers.update(cx, |answers, cx| {
                answers.choose_nth(&wait, index, cx);
            });
        }
        cx.notify();
        true
    }

    pub(in crate::views::chat) fn alert_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        let modifiers = stroke.modifiers;
        if stroke.key == "escape"
            && !modifiers.alt
            && !modifiers.control
            && !modifiers.platform
            && !modifiers.shift
            && self.alerts.expanded
        {
            if self.move_alert(Move::Fold, cx) {
                self.focus_input(window, cx);
                window.prevent_default();
                cx.stop_propagation();
            }
            return;
        }
        if !modifiers.alt || modifiers.control || modifiers.platform || modifiers.shift {
            return;
        }
        let movement = match stroke.key.as_bytes() {
            b"up" => Move::Step(-1),
            b"down" => Move::Step(1),
            [digit @ b'1'..=b'9'] if !event.is_held => Move::Choose(usize::from(digit - b'1')),
            _ => return,
        };
        if self.move_alert(movement, cx) {
            window.prevent_default();
            cx.stop_propagation();
        }
    }

    fn read_context(&mut self, node: NodeId, wait: &ScreenWait, cx: &mut Context<Self>) {
        let id = wait.wait.id;
        if self
            .alerts
            .context
            .as_ref()
            .is_some_and(|(seen, _)| *seen == id)
        {
            return;
        }
        self.alerts.context = Some((id, AlertContext::default()));
        self.alerts.reading = None;
        let data = self.data.read(cx);
        let Some(room) = data.rooms().iter().find(
            |room| matches!(room.kind, ChatRoomKind::Agent { node: member } if member == node),
        ) else {
            return;
        };
        let path = format!("/api/chat/rooms/{}/stream", link::escape(&room.id));
        let before = ChatCursor {
            at_ms: wait.wait.since.saturating_mul(1000),
            source: ChatSource::Wait,
            id: i64::from(id.0),
            part: 0,
        };
        let task = data.graph().read(cx).call(cx, move |base| {
            let cursor = serde_json::to_string(&before)
                .map_err(|error| Failure::Other(error.to_string()))?;
            let query = link::query(&[("before", Some(&cursor))]);
            link::read::<ChatPage>(base, &format!("{path}{query}"))
        });
        self.alerts.reading = Some(cx.spawn(async move |this, cx| {
            let Ok(page) = task.await else {
                return;
            };
            let mut context = AlertContext::default();
            for item in page.items.into_iter().rev().filter(|item| {
                item.cursor.at_ms < before.at_ms || item.cursor.source == ChatSource::Wait
            }) {
                match item.entry {
                    ChatEntry::Message { message } if message.origin == ChatOrigin::Human => {
                        context.last_message.get_or_insert(message.text);
                    }
                    ChatEntry::Message { message }
                        if message.from.id == Some(node)
                            && message.receipts.iter().all(|receipt| {
                                receipt.to.name == "human" && receipt.to.id.is_none()
                            }) =>
                    {
                        context.previous_reply.get_or_insert(message.text);
                    }
                    ChatEntry::Wait { wait, .. }
                        if matches!(wait.tone, WaitTone::Done) && !wait.wait.text.is_empty() =>
                    {
                        context.previous_reply.get_or_insert(wait.wait.text);
                    }
                    _ => {}
                }
                if context.last_message.is_some() && context.previous_reply.is_some() {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this
                    .alerts
                    .context
                    .as_ref()
                    .is_some_and(|(waiting, _)| *waiting == id)
                {
                    this.alerts.context = Some((id, context));
                    cx.notify();
                }
            });
        }));
    }

    pub(super) fn alert_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let notices = self.notices(cx);
        if notices.is_empty() {
            if std::mem::take(&mut self.alerts.expanded) {
                self.focus_input(window, cx);
            }
            self.alerts.selected = None;
            self.alerts.context = None;
            self.alerts.reading = None;
            return None;
        }
        let index = self.alerts.current(&notices);
        let notice = &notices[index];
        let total = notices.len();
        let wait = notice.wait.clone();
        let node = notice.node.id;
        let name = notice.node.name.clone();
        let (repo, task) = {
            let data = self.data.read(cx);
            let repos = &data.graph().read(cx).snapshot()?.repos;
            let repo = repos.iter().find(|repo| repo.id == notice.node.place.repo);
            let task = data.recipients().and_then(|data| {
                data.agents
                    .iter()
                    .find(|agent| agent.node == node)?
                    .task
                    .clone()
            });
            (repo.map(|repo| repo.name.clone()), task)
        };
        if self.alerts.expanded {
            self.read_context(node, &wait, cx);
        }
        let colors = ui::colors(cx);
        let label = if self.alerts.expanded {
            local(cx, "알림 접기", "Collapse alert")
        } else {
            local(cx, "알림 펼치기", "Expand alert")
        };
        let tone = tone_of(wait.tone, &colors);
        let name = repo.map_or_else(|| name.clone(), |repo| format!("{repo} › {name}"));
        let head = self.alert_header(index, total, &wait, name.clone(), label, cx);
        let strip = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(mix(tone, 0.45, colors.line))
            .bg(mix(tone, 0.09, colors.panel))
            .child(head);
        if !self.alerts.expanded {
            return Some(strip);
        }
        let focus = std::mem::take(&mut self.alerts.focus);
        self.alerts.answers.update(cx, |answers, cx| {
            answers.prepare(node, &wait, cx);
            if focus {
                answers.focus(wait.wait.id, window, cx);
            }
        });
        let context = self
            .alerts
            .context
            .as_ref()
            .filter(|(id, _)| *id == wait.wait.id)
            .map(|(_, context)| context);
        let last = context.and_then(|context| context.last_message.as_deref());
        let previous = context.and_then(|context| context.previous_reply.as_deref());
        let info = [
            (local(cx, "하던 일", "Task"), task.as_deref()),
            (local(cx, "내 마지막 말", "My last message"), last),
            (local(cx, "앞선 답", "Previous reply"), previous),
        ];
        let details = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(info.map(|(label, value)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(ui::SMALL))
                    .text_color(colors.ink_2)
                    .child(
                        div()
                            .flex_none()
                            .w(px(115.0))
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(label),
                    )
                    .child(
                        div().min_w_0().truncate().child(
                            value
                                .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                                .unwrap_or_else(|| "—".to_owned()),
                        ),
                    )
            }));
        Some(
            strip
                .child(details)
                .child(
                    div()
                        .id("chat-alert-content")
                        .max_h(px(205.0))
                        .overflow_y_scroll()
                        .child(answers::content(
                            &self.alerts.answers,
                            &wait,
                            scale::get(),
                            cx,
                        )),
                )
                .children(answers::composer(
                    &self.alerts.answers,
                    &wait,
                    &name,
                    scale::get(),
                    cx,
                )),
        )
    }

    fn alert_header(
        &self,
        index: usize,
        total: usize,
        wait: &ScreenWait,
        name: String,
        label: &'static str,
        cx: &Context<Self>,
    ) -> Div {
        let colors = ui::colors(cx);
        let tone = tone_of(wait.tone, &colors);
        let (icon_kind, kind) = match wait.tone {
            WaitTone::Ask => (Lucide::CircleQuestionMark, ui::say(cx, Str::WaitQuestion)),
            WaitTone::Done => (Lucide::Check, ui::say(cx, Str::WaitDone)),
        };
        let arrow = |direction, id, hint: &'static str, glyph| {
            btn::ghost(id, cx)
                .role(Role::Button)
                .aria_label(hint)
                .tooltip(tip(hint.into()))
                .when(total > 1, |button| {
                    button.on_click(cx.listener(move |this, _, _, cx| {
                        this.move_alert(Move::Step(direction), cx);
                    }))
                })
                .when(total < 2, |button| btn::inert(button).opacity(0.55))
                .child(icon(glyph, px(13.0), colors.ink_2))
        };
        div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(
                btn::button("chat-alert-fold", cx)
                    .role(Role::Button)
                    .aria_label(format!("{label} · {name}"))
                    .aria_expanded(self.alerts.expanded)
                    .aria_keyshortcuts(
                        "Alt+Up Alt+Down Alt+1 Alt+2 Alt+3 Alt+4 Alt+5 Alt+6 Alt+7 Alt+8 Alt+9",
                    )
                    .tooltip(tip(format!(
                        "{} · {label} · {}",
                        wait.wait.id,
                        local(
                            cx,
                            "Alt+↑↓ 알림 이동 · Alt+1~9 선택",
                            "Alt+↑↓ Move between alerts · Alt+1–9 Choose",
                        )
                    )))
                    .flex_1()
                    .min_w_0()
                    .bg(mix(tone, 0.17, colors.panel))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.move_alert(Move::Fold, cx) && !this.alerts.expanded {
                            this.focus_input(window, cx);
                        }
                    }))
                    .child(icon(icon_kind, px(15.0), tone))
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(230.0))
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(wait.summary.replace('\n', " ")),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(format!("{kind} {}/{}", index + 1, total)),
                    )
                    .child(icon(
                        if self.alerts.expanded {
                            Lucide::ChevronUp
                        } else {
                            Lucide::ChevronDown
                        },
                        px(13.0),
                        colors.ink_2,
                    )),
            )
            .child(arrow(
                -1,
                "chat-alert-prev",
                local(cx, "앞 알림 (Alt+↑)", "Previous alert (Alt+↑)"),
                Lucide::ChevronUp,
            ))
            .child(arrow(
                1,
                "chat-alert-next",
                local(cx, "다음 알림 (Alt+↓)", "Next alert (Alt+↓)"),
                Lucide::ChevronDown,
            ))
    }
}
