use api::graph::chat::ChatRoom;
use api::graph::messages::Kind;
use gpui::prelude::FluentBuilder;
use gpui::{
    Context, Div, FontWeight, InteractiveElement, KeyDownEvent, ParentElement, Role,
    StatefulInteractiveElement, Styled, Toggled, div,
};

use super::targets::{self, Catalog, Plan, Target};
use super::{Composer, Draft, SendIntent, mode_label};
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::icons::{Lucide, icon};
use crate::views::tooltip::tip;
use crate::views::{status, ui, widgets::btn};

const INLINE_MEMBERS: usize = 6;

impl Composer {
    pub(super) fn controls(
        &self,
        room: &ChatRoom,
        draft: &Draft,
        catalog: Option<&Catalog>,
        plan: &Plan,
        cx: &Context<Self>,
    ) -> Div {
        let c = ui::colors(cx);
        let mode = draft.mode(plan);
        let field = draft.field.read(cx);
        let selected = draft.selected();
        let writable = catalog.is_some() && self.writable(cx);
        let ready = writable && !field.is_blank() && plan.count() > 0 && !plan.unavailable;
        let placeholder = if plan.count() == 0 {
            "받는 이를 먼저 고릅니다".to_owned()
        } else if selected.len() == 1 {
            targets::name(&selected[0], catalog, self.data.read(cx).rooms())
                .map_or_else(|| "보낼 말".into(), |name| format!("{name}에게 보낼 말"))
        } else {
            "보낼 말".into()
        };
        let input = div()
            .id("chat-message")
            .track_focus(&field.handle().tab_stop(false))
            .role(Role::MultilineTextInput)
            .aria_label("보낼 말")
            .aria_value(field.text().to_owned())
            .aria_placeholder(placeholder.clone())
            .relative()
            .max_h(px(140.0))
            .overflow_y_scroll()
            .child(draft.field.clone())
            .children(field.text().is_empty().then(|| {
                div()
                    .absolute()
                    .top(px(5.0))
                    .left(px(9.0))
                    .text_color(c.ink_3)
                    .child(placeholder)
            }));
        let (hint, sender) = match (draft.sending, plan.count(), mode) {
            (true, _, _) => ("", "보내는 중…".to_owned()),
            (false, 0, _) => ("받는 이를 고르면 보낼 수 있습니다.", "보내기".to_owned()),
            (false, _, Kind::Interrupt) => (
                "하던 일을 바로 멈추고 이 말을 읽게 합니다.",
                format!("{}명에게 중단하고 보내기", plan.count()),
            ),
            (false, _, _) if plan.outside_rooms > 0 => {
                let rooms = plan.outside_rooms;
                let nodes = plan.nodes.len();
                let caption = if nodes == 0 {
                    format!("외부 방 {rooms}곳에 보내기")
                } else {
                    format!("{nodes}명과 외부 방 {rooms}곳에 보내기")
                };
                (
                    "외부 에이전트는 방에서 이 말을 가져옵니다. 일반으로만 보낼 수 있습니다.",
                    caption,
                )
            }
            (false, count, _) => {
                let hint = if plan.queue_only {
                    "고른 받는 이에게는 일반으로만 보낼 수 있습니다."
                } else if plan.overlap {
                    "여러 곳에서 고른 에이전트도 한 번만 받습니다."
                } else if mode == Kind::Steer {
                    "하던 일을 멈추지 않고 이 말을 읽게 합니다."
                } else {
                    "하던 일이 끝나면 읽습니다. Enter로 보냅니다."
                };
                (hint, format!("{count}명에게 보내기"))
            }
        };
        let warning = if plan.unavailable {
            Some("보낼 수 없는 받는 이를 목록에서 빼 주세요.")
        } else if catalog.is_none() {
            self.data
                .read(cx)
                .error()
                .or(Some("받는 이 목록을 불러올 때까지 기다려 주세요."))
        } else {
            None
        };
        let send = btn::button("chat-send", cx)
            .role(Role::Button)
            .when(!ready, |button| btn::inert(button).opacity(0.55))
            .when(ready, |button| {
                button
                    .bg(if mode == Kind::Interrupt {
                        c.err
                    } else {
                        c.accent
                    })
                    .text_color(if mode == Kind::Interrupt {
                        c.err_ink
                    } else {
                        c.accent_ink
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.submit(SendIntent::Request, window, cx)
                    }))
            })
            .aria_label(sender.clone())
            .child(sender);
        let footer = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(ui::SMALL))
                    .text_color(c.ink_2)
                    .child(hint),
            )
            .child(send.flex_none());
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(self.recipients(room, draft, catalog, cx))
                    .child(self.modes(mode, writable, plan, cx)),
            )
            .child(input);
        if let Some(text) = draft.error.as_deref().or(warning) {
            body = body.child(
                div()
                    .id("chat-send-error")
                    .role(Role::Alert)
                    .text_color(c.err)
                    .child(text.to_owned()),
            );
        }
        if self.review.is_some()
            && mode == Kind::Interrupt
            && let Some(catalog) = catalog
        {
            body = body.child(self.confirmation(catalog, plan, cx));
        } else {
            body = body.child(footer);
        }
        body
    }

    fn recipients(
        &self,
        room: &ChatRoom,
        draft: &Draft,
        catalog: Option<&Catalog>,
        cx: &Context<Self>,
    ) -> gpui::Stateful<Div> {
        let c = ui::colors(cx);
        let data = self.data.read(cx);
        let selected = draft.selected();
        let mut chips = div()
            .id("chat-recipients")
            .min_w_0()
            .max_h(px(100.0))
            .overflow_y_scroll()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .child(div().text_color(c.ink_2).child("받는 이"));
        let local = targets::group(room);
        let can_edit = self.writable(cx) && catalog.is_some();
        let local_rooms = data.rooms();
        let inline = local && room.members.len() <= INLINE_MEMBERS;
        for (index, target) in selected.iter().cloned().enumerate() {
            let label = match &target {
                Target::Room(id) if id == &room.id => "방 전체",
                _ => targets::name(&target, catalog, local_rooms).unwrap_or("알 수 없는 받는 이"),
            };
            let unavailable = catalog.is_some_and(|catalog| match &target {
                Target::Room(id) => catalog.room(id).is_none(),
                Target::Node(node) => catalog.node(*node).is_none(),
            });
            let name = format!("{label} 빼기");
            let title = match &target {
                Target::Room(id) => catalog
                    .and_then(|catalog| catalog.room(id))
                    .filter(|room| !room.members.is_empty())
                    .map_or_else(
                        || label.to_owned(),
                        |room| format!("{label} · {}명", room.members.len()),
                    ),
                _ => label.to_owned(),
            };
            chips = chips.child(
                btn::button(("chat-remove", index), cx)
                    .role(Role::Button)
                    .bg(if unavailable {
                        c.surface
                    } else {
                        c.accent_wash
                    })
                    .aria_label(name.clone())
                    .tooltip(tip(name))
                    .when(can_edit, |button| {
                        button.on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle(target.clone(), cx);
                            window.focus(&this.trigger, cx);
                        }))
                    })
                    .when(!can_edit, btn::inert)
                    .child(div().truncate().max_w(px(180.0)).child(title))
                    .children(unavailable.then_some("끝남"))
                    .child(icon(Lucide::X, px(12.0), c.ink_2)),
            );
        }
        if inline {
            for node in &room.members {
                let target = Target::Node(*node);
                if selected.contains(&target) {
                    continue;
                }
                let Some(name) = targets::name(&target, catalog, local_rooms) else {
                    continue;
                };
                let live = catalog.is_some_and(|catalog| catalog.node(*node).is_some());
                chips = chips.child(
                    btn::button(("chat-member", node.0 as usize), cx)
                        .role(Role::Button)
                        .aria_label(if live {
                            format!("{name} 더하기")
                        } else {
                            format!("{name}, 끝남")
                        })
                        .when(can_edit && live, |button| {
                            button.on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle(target.clone(), cx);
                                window.focus(&this.trigger, cx);
                            }))
                        })
                        .when(!can_edit || !live, |button| {
                            btn::inert(button).opacity(0.55)
                        })
                        .child(div().truncate().max_w(px(160.0)).child(name.to_owned()))
                        .children((!live).then_some("끝남")),
                );
            }
        }
        let target = Target::Room(room.id.clone());
        if local && !selected.contains(&target) {
            chips = chips.child(
                btn::button("chat-room-all", cx)
                    .role(Role::Button)
                    .aria_label("방 전체 더하기")
                    .when(can_edit, |button| {
                        button.on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle(target.clone(), cx);
                            window.focus(&this.trigger, cx);
                        }))
                    })
                    .when(!can_edit, btn::inert)
                    .child("방 전체"),
            );
        }
        let add = if selected.is_empty() {
            "받는 이 고르기"
        } else {
            "받는 이 더하기"
        };
        chips.child(
            btn::button("chat-picker-trigger", cx)
                .role(Role::Button)
                .track_focus(&self.trigger)
                .aria_label(add)
                .aria_keyshortcuts("Alt+L")
                .aria_expanded(self.picker.open)
                .when(self.writable(cx), |button| {
                    button.on_click(cx.listener(|this, _, window, cx| {
                        if this.picker.open {
                            this.close_picker(window, cx);
                        } else {
                            this.open_picker(window, cx);
                        }
                    }))
                })
                .when(draft.sending, btn::inert)
                .child(icon(Lucide::Plus, px(12.0), c.ink_2))
                .child(add),
        )
    }

    fn modes(&self, mode: Kind, writable: bool, plan: &Plan, cx: &Context<Self>) -> Div {
        let c = ui::colors(cx);
        let mut modes = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .child(div().text_color(c.ink_2).child("전달"));
        for (index, &kind) in Kind::ALL.iter().enumerate() {
            let disabled = !writable || (plan.queue_only && kind != Kind::Queue);
            modes = modes.child(
                btn::button(("chat-delivery", index), cx)
                    .role(Role::Button)
                    .aria_label(mode_label(kind))
                    .aria_toggled(Toggled::from(kind == mode))
                    .when(kind == mode, |button| button.bg(c.accent_wash))
                    .when(disabled, |button| btn::inert(button).opacity(0.55))
                    .when(!disabled, |button| {
                        button.on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(draft) = this.draft_mut() {
                                draft.mode = kind;
                            }
                            this.review = None;
                            cx.notify();
                        }))
                    })
                    .child(mode_label(kind)),
            );
        }
        modes
    }

    fn confirmation(
        &self,
        catalog: &Catalog,
        plan: &Plan,
        cx: &Context<Self>,
    ) -> gpui::Stateful<Div> {
        let c = ui::colors(cx);
        let lang = *cx.global::<Lang>();
        let impacts = self.impacts(catalog, plan, cx);
        let changes = self
            .review
            .as_ref()
            .is_some_and(|previous| *previous != impacts);
        let mut rows = div()
            .id("chat-interrupt-targets")
            .max_h(px(128.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(3.0));
        for impact in impacts {
            let mut detail = format!("{} · {}", impact.name, impact.status);
            if let Some(since) = status::since_short(impact.since, lang) {
                detail.push_str(&format!(" · {since}"));
            }
            let task = impact.task.as_deref().unwrap_or("작업 내용 없음");
            detail.push_str(&format!(" · {task}"));
            rows = rows.child(
                div()
                    .id(("chat-interrupt-agent", impact.node.0 as usize))
                    .role(Role::Label)
                    .aria_label(detail.clone())
                    .child(detail),
            );
        }
        div()
            .id("chat-interrupt-review")
            .role(Role::Group)
            .aria_label("중단 확인")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_review(window, cx);
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(10.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(c.err)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{}명의 일을 멈춥니다.", plan.count())),
            )
            .child(rows)
            .child("멈춘 일은 이어서 하지 못합니다.")
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(6.0))
                    .child(
                        btn::button("chat-interrupt-cancel", cx)
                            .track_focus(&self.cancel)
                            .child(ui::say(cx, Str::CommonCancel))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.cancel_review(window, cx)),
                            ),
                    )
                    .child(
                        btn::button("chat-interrupt-confirm", cx)
                            .bg(c.err)
                            .text_color(c.err_ink)
                            .child(if changes {
                                "바뀐 작업 확인"
                            } else {
                                "중단하고 보내기"
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit(SendIntent::ConfirmInterrupt, window, cx)
                            })),
                    ),
            )
    }
}
