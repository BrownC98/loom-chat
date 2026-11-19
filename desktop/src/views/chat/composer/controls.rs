use api::graph::chat::ChatRoom;
use api::graph::messages::Kind;
use gpui::prelude::FluentBuilder;
use gpui::{
    Context, Div, ExternalPaths, FontWeight, InteractiveElement, KeyDownEvent, ParentElement, Role,
    StatefulInteractiveElement, Styled, Toggled, div,
};

use super::targets::{self, Catalog, Plan, Target};
use super::{Composer, Draft, SendIntent, mode_label};
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::icons::{Lucide, icon};
use crate::views::pictures;
use crate::views::tooltip::tip;
use crate::views::{status, ui, widgets::btn};

pub(super) const ATTACH_INSET: f32 = 24.0;
pub(super) const MODES_INSET: f32 = 132.0;

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
        let busy = draft.pictures.read(cx).busy();
        let writable = catalog.is_some() && self.writable(cx);
        let ready =
            writable && draft.has_content(cx) && !busy && plan.count() > 0 && !plan.unavailable;
        let placeholder = if plan.count() == 0 {
            "받는 이를 고릅니다"
        } else {
            "보낼 말"
        };
        let paperclip = btn::tabbable(div().id("chat-attach"), cx)
            .role(Role::Button)
            .aria_label("파일 첨부")
            .tooltip(tip("파일 첨부".into()))
            .flex()
            .items_center()
            .justify_center()
            .size(px(24.0))
            .absolute()
            .top(px(3.0))
            .left(px(4.0))
            .child(icon(Lucide::Paperclip, px(15.0), c.ink_2))
            .when(!draft.sending, |button| {
                let pictures = draft.pictures.clone();
                button.cursor_pointer().on_click(move |_, _, cx| {
                    pictures.update(cx, |pictures, cx| pictures.pick_files(cx));
                })
            })
            .when(draft.sending, btn::inert);
        let input = div()
            .id("chat-message")
            .track_focus(&field.handle().tab_stop(false))
            .role(Role::MultilineTextInput)
            .aria_label("보낼 말")
            .aria_value(field.text().to_owned())
            .aria_placeholder(placeholder)
            .relative()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .id("chat-input-scroll")
                    .max_h(px(140.0))
                    .overflow_y_scroll()
                    .child(draft.field.clone()),
            )
            .children(field.text().is_empty().then(|| {
                div()
                    .absolute()
                    .top(px(5.0))
                    .left(px(8.0 + ATTACH_INSET))
                    .right(px(MODES_INSET))
                    .truncate()
                    .text_color(c.ink_3)
                    .child(placeholder)
            }))
            .child(paperclip)
            .child(self.modes(mode, writable, plan, cx));
        let sender = match (draft.sending, plan.count(), mode) {
            (true, _, _) => "보내는 중".to_owned(),
            (false, 0, _) => "받는 이를 고르면 보낼 수 있습니다".to_owned(),
            (false, n, Kind::Interrupt) => format!("{n}명에게 중단하고 보내기"),
            (false, _, _) if plan.outside_rooms > 0 => "고른 받는 이에게 보내기".to_owned(),
            (false, n, _) => format!("{n}명에게 보내기"),
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
        let (fill, ink) = match mode {
            Kind::Interrupt => (c.err, c.err_ink),
            _ => (c.accent, c.accent_ink),
        };
        let send = btn::filled("chat-send", fill, ink, cx)
            .role(Role::Button)
            .aria_label(sender.clone())
            .aria_keyshortcuts("Enter")
            .tooltip(tip(sender))
            .flex_none()
            .h(px(30.0))
            .when(ready, |button| {
                button.on_click(cx.listener(|this, _, window, cx| {
                    this.submit(SendIntent::Request, window, cx);
                }))
            })
            .when(!ready, |button| btn::inert(button).opacity(0.55))
            .when(plan.count() > 1 && plan.outside_rooms == 0, |button| {
                button.child(format!("{}명에게", plan.count()))
            })
            .child(icon(Lucide::ArrowUp, px(15.0), ink));
        let viewer = self.viewer.clone();
        let attachments = pictures::chips(
            &draft.pictures,
            move |picture, window, cx| {
                viewer.update(cx, |viewer, cx| viewer.open(picture, window, cx));
            },
            "chat-attachment",
            0,
            crate::scale::get(),
            cx,
        );
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .children(draft.quote.as_ref().map(|quote| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .self_start()
                    .max_w_full()
                    .border_1()
                    .border_color(c.line_strong)
                    .rounded(px(6.0))
                    .pl(px(7.0))
                    .bg(c.surface)
                    .child(icon(Lucide::MessageSquare, px(13.0), c.ink_2))
                    .child(div().min_w_0().truncate().child(quote.label()))
                    .child(
                        btn::ghost("chat-quote-remove", cx)
                            .role(Role::Button)
                            .aria_label("인용문 빼기")
                            .tooltip(tip("인용문 빼기".into()))
                            .child(icon(Lucide::X, px(12.0), c.ink_2))
                            .when(draft.sending, btn::inert)
                            .when(!draft.sending, |button| {
                                button.on_click(cx.listener(|this, _, window, cx| {
                                    if let Some(draft) = this.draft_mut() {
                                        draft.quote = None;
                                    }
                                    this.review = None;
                                    this.focus_input(window, cx);
                                    cx.notify();
                                }))
                            }),
                    )
            }))
            .children(
                (draft.selected().len() > 1 && !self.picker.open)
                    .then(|| self.selected_chips(room, draft, catalog, cx)),
            )
            .children(attachments)
            .children(busy.then(|| div().child("파일을 읽는 중")))
            .children(
                (draft.sending && !draft.pictures.read(cx).is_empty())
                    .then(|| div().child("첨부 파일을 올리고 보내는 중")),
            )
            .child(
                div()
                    .id("chat-composer-row")
                    .flex()
                    .items_end()
                    .gap(px(7.0))
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.rounded(px(6.0)).bg(c.accent_wash)
                    })
                    .on_drop({
                        let pictures = draft.pictures.clone();
                        move |paths: &ExternalPaths, _, cx| {
                            pictures.update(cx, |pictures, cx| {
                                pictures.take_paths(paths.paths().to_vec(), cx);
                            });
                        }
                    })
                    .child(self.recipient_button(draft, catalog, plan, cx))
                    .child(input)
                    .child(send),
            );
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
        }
        body
    }

    pub(super) fn selected_chips(
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
            .gap(px(6.0));
        let can_edit = self.writable(cx) && catalog.is_some();
        let local_rooms = data.rooms();
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
                            let focus = if this.picker.open {
                                this.picker.query_focus(cx)
                            } else {
                                this.trigger.clone()
                            };
                            this.toggle(target.clone(), cx);
                            window.focus(&focus, cx);
                        }))
                    })
                    .when(!can_edit, btn::inert)
                    .child(div().truncate().max_w(px(180.0)).child(title))
                    .children(unavailable.then_some("끝남"))
                    .child(icon(Lucide::X, px(12.0), c.ink_2)),
            );
        }
        chips
    }

    fn recipient_button(
        &self,
        draft: &Draft,
        catalog: Option<&Catalog>,
        plan: &Plan,
        cx: &Context<Self>,
    ) -> gpui::Stateful<Div> {
        let c = ui::colors(cx);
        let label = match draft.selected() {
            [] => "받는 이".to_owned(),
            [target] => targets::name(target, catalog, self.data.read(cx).rooms())
                .map_or_else(|| "받는 이".to_owned(), |name| format!("{name}에게")),
            _ if plan.outside_rooms == 0 => format!("{}명에게", plan.count()),
            _ => format!("{}곳에", plan.count()),
        };
        let title = format!("{label} · 받는 이 고르기 (Alt+L)");
        btn::button("chat-picker-trigger", cx)
            .role(Role::Button)
            .track_focus(&self.trigger)
            .aria_label(title.clone())
            .aria_keyshortcuts("Alt+L")
            .aria_expanded(self.picker.open)
            .tooltip(tip(title))
            .flex_none()
            .max_w(px(124.0))
            .h(px(30.0))
            .when(!draft.selected().is_empty(), |button| {
                button.bg(c.accent_wash)
            })
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
            .child(div().min_w_0().truncate().child(label))
            .child(icon(
                if self.picker.open {
                    Lucide::ChevronUp
                } else {
                    Lucide::ChevronDown
                },
                px(12.0),
                c.ink_2,
            ))
    }

    fn modes(&self, mode: Kind, writable: bool, plan: &Plan, cx: &Context<Self>) -> Div {
        let c = ui::colors(cx);
        let mut modes = div()
            .flex()
            .absolute()
            .right(px(4.0))
            .top(px(3.0))
            .items_center()
            .gap(px(2.0));
        for (index, &kind) in Kind::ALL.iter().enumerate() {
            let disabled = !writable || (plan.queue_only && kind != Kind::Queue);
            modes = modes.child(
                btn::button(("chat-delivery", index), cx)
                    .role(Role::Button)
                    .aria_label(mode_label(kind))
                    .tooltip(tip(match kind {
                        Kind::Queue => "일반 · 하던 일이 끝나면 읽습니다",
                        Kind::Steer => "조정 · 하던 일 사이에 읽습니다",
                        Kind::Interrupt => "중단 · 하던 일을 멈추고 읽습니다",
                    }
                    .into()))
                    .px_0()
                    .py_0()
                    .h(px(24.0))
                    .w(px(40.0))
                    .justify_center()
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
                        btn::filled("chat-interrupt-confirm", c.err, c.err_ink, cx)
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
