use api::graph::chat::ChatRoom;
use api::graph::messages::Kind;
use gpui::prelude::FluentBuilder;
use gpui::{
    Anchor, Context, Div, ExternalPaths, FontWeight, InteractiveElement, KeyDownEvent,
    ParentElement, Role, StatefulInteractiveElement, Styled, Toggled, Window, anchored, canvas,
    deferred, div, point,
};

use super::targets::{self, Catalog, Plan, Target};
use super::{Composer, Draft, SendIntent, local, mode_label, tab, unavailable_recipient};
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::theme::mix;
use crate::views::icons::{Lucide, icon};
use crate::views::pictures;
use crate::views::tooltip::tip;
use crate::views::{status, ui, widgets::btn};

pub(super) const ATTACH_INSET: f32 = 24.0;
pub(super) fn mode_inset(cx: &gpui::App) -> f32 {
    match cx.global::<Lang>() {
        Lang::Ko => 62.0,
        Lang::En => 88.0,
    }
}

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
            local(cx, "받는 이를 고릅니다", "Choose recipients")
        } else {
            ui::say(cx, Str::ComposerPlaceholder)
        };
        let paperclip = btn::tabbable(div().id("chat-attach"), cx)
            .role(Role::Button)
            .aria_label(ui::say(cx, Str::ComposerAttachFile))
            .tooltip(tip(ui::say(cx, Str::ComposerAttachFile).into()))
            .flex()
            .items_center()
            .justify_center()
            .size(px(24.0))
            .absolute()
            .top(px(3.0))
            .left(px(4.0))
            .child(icon(Lucide::Paperclip, px(15.0), c.ink_2))
            .cursor_pointer()
            .on_click({
                let pictures = draft.pictures.clone();
                move |_, _, cx| {
                    pictures.update(cx, |pictures, cx| pictures.pick_files(cx));
                }
            });
        let input = div()
            .id("chat-message")
            .track_focus(&field.handle().tab_stop(false))
            .role(Role::MultilineTextInput)
            .aria_label(ui::say(cx, Str::ComposerPlaceholder))
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
                    .right(px(mode_inset(cx)))
                    .truncate()
                    .text_color(c.ink_3)
                    .child(placeholder)
            }))
            .child(paperclip)
            .child(self.modes(mode, writable, plan, cx));
        let count = plan.count();
        let lang = *cx.global::<Lang>();
        let sender = match (lang, count, mode) {
            (_, 0, _) => local(
                cx,
                "받는 이를 고르면 보낼 수 있습니다",
                "Choose recipients to send",
            )
            .to_owned(),
            (Lang::Ko, n, Kind::Interrupt) => format!("{n}명에게 중단하고 보내기"),
            (Lang::En, n, Kind::Interrupt) => format!("Interrupt and send to {n}"),
            (_, _, _) if plan.outside_rooms > 0 => {
                local(cx, "고른 받는 이에게 보내기", "Send to selected recipients").to_owned()
            }
            (Lang::Ko, n, _) => format!("{n}명에게 보내기"),
            (Lang::En, n, _) => format!("Send to {n}"),
        };
        let warning = if plan.unavailable {
            Some(unavailable_recipient(cx))
        } else if catalog.is_none() {
            self.data.read(cx).error().or(Some(local(
                cx,
                "받는 이 목록을 불러올 때까지 기다려 주세요.",
                "Wait for the recipient list to load.",
            )))
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
                button.child(recipient_count(plan.count(), lang))
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
                    .child(div().min_w_0().truncate().child(quote.label(cx)))
                    .child(
                        btn::ghost("chat-quote-remove", cx)
                            .role(Role::Button)
                            .aria_label(local(cx, "인용문 빼기", "Remove quote"))
                            .tooltip(tip(local(cx, "인용문 빼기", "Remove quote").into()))
                            .child(icon(Lucide::X, px(12.0), c.ink_2))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(draft) = this.draft_mut() {
                                    draft.quote = None;
                                }
                                this.review = None;
                                this.focus_input(window, cx);
                                cx.notify();
                            })),
                    )
            }))
            .children(
                (draft.selected().len() > 1 && !self.picker.open)
                    .then(|| self.selected_chips(room, draft, catalog, cx)),
            )
            .children(attachments)
            .children(busy.then(|| div().child(local(cx, "파일을 읽는 중", "Reading files…"))))
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
        let saved = draft.recoverable.len();
        if saved > 0 {
            let text = match cx.global::<Lang>() {
                Lang::Ko => format!("초안 되돌리기 ({saved})"),
                Lang::En => format!("Restore draft ({saved})"),
            };
            body = body.child(
                btn::button("chat-recover-draft", cx)
                    .self_start()
                    .child(text)
                    .on_click(cx.listener(|this, _, window, cx| this.recover_draft(window, cx))),
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
                Target::Room(id) if id == &room.id => local(cx, "방 전체", "Entire room"),
                _ => targets::name(&target, catalog, local_rooms).unwrap_or(local(
                    cx,
                    "알 수 없는 받는 이",
                    "Unknown recipient",
                )),
            };
            let unavailable = catalog.is_some_and(|catalog| match &target {
                Target::Room(id) => catalog.room(id).is_none(),
                Target::Node(node) => catalog.node(*node).is_none(),
            });
            let name = match cx.global::<Lang>() {
                Lang::Ko => format!("{label} 빼기"),
                Lang::En => format!("Remove {label}"),
            };
            let title = match &target {
                Target::Room(id) => catalog
                    .and_then(|catalog| catalog.room(id))
                    .filter(|room| !room.members.is_empty())
                    .map_or_else(
                        || label.to_owned(),
                        |room| match cx.global::<Lang>() {
                            Lang::Ko => format!("{label} · {}명", room.members.len()),
                            Lang::En => format!("{label} · {} members", room.members.len()),
                        },
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
                    .children(unavailable.then_some(ui::say(cx, Str::NodeStatusChipStopped)))
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
        let lang = *cx.global::<Lang>();
        let recipient = local(cx, "받는 이", "Recipients");
        let label = match draft.selected() {
            [] => recipient.to_owned(),
            [target] => targets::name(target, catalog, self.data.read(cx).rooms()).map_or_else(
                || recipient.to_owned(),
                |name| ui::say_with(cx, Str::ComposerToNode, &[("name", name)]),
            ),
            _ if plan.outside_rooms == 0 => recipient_count(plan.count(), lang),
            _ => match lang {
                Lang::Ko => format!("{}곳에", plan.count()),
                Lang::En => format!("To {} rooms", plan.count()),
            },
        };
        let choose = local(cx, "받는 이 고르기 (Alt+L)", "Choose recipients (Alt+L)");
        let title = format!("{label} · {choose}");
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
            .when(!self.writable(cx), btn::inert)
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
        let bounds = self.mode_bounds.clone();
        let delivery = local(cx, "전달 방식", "Delivery mode");
        let (line, fill) = match mode {
            Kind::Queue => (c.line_strong, c.bg),
            Kind::Steer => (c.run, mix(c.run, 0.13, c.surface)),
            Kind::Interrupt => (c.err, mix(c.err, 0.13, c.surface)),
        };
        let active = Kind::ALL
            .iter()
            .position(|&kind| kind == mode)
            .unwrap_or_default();
        let chip = btn::button("chat-delivery", cx)
            .track_focus(&self.mode_trigger)
            .role(Role::Button)
            .aria_label(format!("{delivery} · {}", mode_label(mode, cx)))
            .aria_expanded(self.mode_menu)
            .border_color(line)
            .bg(fill)
            .px(px(5.0))
            .py_0()
            .h(px(24.0))
            .min_h(px(24.0))
            .gap(px(2.0))
            .text_size(px(ui::SMALL))
            .font_weight(FontWeight::SEMIBOLD)
            .child(div().min_w_0().truncate().child(mode_label(mode, cx)))
            .max_w(px(mode_inset(cx) - 4.0))
            .child(icon(Lucide::ChevronDown, px(11.0), c.ink_2))
            .when(!writable, |button| btn::inert(button).opacity(0.55))
            .when(writable, |button| {
                button.on_click(cx.listener(move |this, _, window, cx| {
                    this.mode_menu = !this.mode_menu;
                    if this.mode_menu {
                        this.picker.open = false;
                        window.focus(&this.mode_options[active], cx);
                    }
                    cx.notify();
                }))
            });
        let mut modes = div()
            .absolute()
            .bottom(px(3.0))
            .right(px(3.0))
            .child(
                canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(chip);
        if writable && self.mode_menu {
            let queue_only = plan.queue_only;
            let bounds = self.mode_bounds.get();
            let mut popup = div()
                .id("chat-delivery-options")
                .role(Role::Group)
                .aria_label(delivery)
                .w(px(300.0))
                .p(px(6.0))
                .gap(px(3.0))
                .flex()
                .flex_col()
                .rounded(px(7.0))
                .border_1()
                .border_color(c.line_strong)
                .bg(c.panel)
                .occlude()
                .on_mouse_down_out(
                    cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                        if !this.mode_bounds.get().contains(&event.position) {
                            this.mode_menu = false;
                            cx.notify();
                        }
                    }),
                )
                .capture_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    this.mode_key(event, queue_only, window, cx);
                }))
                .child(div().px(px(6.0)).text_color(c.ink_2).child(delivery));
            for (index, &kind) in Kind::ALL.iter().enumerate() {
                let disabled = queue_only && kind != Kind::Queue;
                let description = mode_description(kind, cx);
                let choice =
                    btn::ghost(("chat-delivery-option", index), cx)
                        .track_focus(&self.mode_options[index])
                        .role(Role::Button)
                        .aria_toggled(Toggled::from(kind == mode))
                        .aria_label(format!("{} · {description}", mode_label(kind, cx)))
                        .justify_start()
                        .when(kind == mode, |row| row.bg(c.accent_wash))
                        .when(disabled, |row| btn::inert(row).opacity(0.55))
                        .when(!disabled, |row| {
                            row.on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(draft) = this.draft_mut() {
                                    draft.mode = kind;
                                }
                                this.mode_menu = false;
                                this.review = None;
                                window.focus(&this.mode_trigger, cx);
                                cx.notify();
                            }))
                        })
                        .child(div().size(px(14.0)).children(
                            (kind == mode).then(|| icon(Lucide::Check, px(14.0), c.accent)),
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_start()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(mode_label(kind, cx)),
                                )
                                .child(
                                    div()
                                        .text_size(px(ui::SMALL))
                                        .text_color(c.ink_2)
                                        .child(description),
                                ),
                        );
                popup = popup.child(choice);
            }
            modes = modes.child(
                deferred(
                    anchored()
                        .position(point(bounds.right(), bounds.top()))
                        .anchor(Anchor::BottomRight)
                        .offset(point(px(0.0), -px(6.0)))
                        .snap_to_window()
                        .child(popup),
                )
                .with_priority(1),
            );
        }
        modes
    }

    fn mode_key(
        &mut self,
        event: &KeyDownEvent,
        queue_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        if stroke.key == "tab"
            && !(stroke.modifiers.alt || stroke.modifiers.control || stroke.modifiers.platform)
        {
            self.mode_menu = false;
            window.focus(&self.mode_trigger, cx);
            tab(event, window, cx);
            cx.notify();
            return;
        }
        if stroke.modifiers.modified() {
            return;
        }
        match stroke.key.as_str() {
            "escape" => {
                self.mode_menu = false;
                window.focus(&self.mode_trigger, cx);
                cx.notify();
            }
            "up" | "down" => {
                let current = self
                    .mode_options
                    .iter()
                    .position(|focus| focus.is_focused(window))
                    .unwrap_or_default();
                let step = if stroke.key == "up" {
                    self.mode_options.len() - 1
                } else {
                    1
                };
                let next = if queue_only {
                    0
                } else {
                    (current + step) % self.mode_options.len()
                };
                window.focus(&self.mode_options[next], cx);
            }
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
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
            let task =
                impact
                    .task
                    .as_deref()
                    .unwrap_or(local(cx, "작업 내용 없음", "No task details"));
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
            .aria_label(local(cx, "중단 확인", "Confirm interruption"))
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
            .child(div().font_weight(FontWeight::SEMIBOLD).child(match lang {
                Lang::Ko => format!("{}명의 일을 멈춥니다.", plan.count()),
                Lang::En if plan.count() == 1 => "Stop work for one agent.".to_owned(),
                Lang::En => format!("Stop work for {} agents.", plan.count()),
            }))
            .child(rows)
            .child(local(
                cx,
                "멈춘 일은 이어서 하지 못합니다.",
                "Stopped work cannot resume.",
            ))
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
                                local(cx, "바뀐 작업 확인", "Review changed work")
                            } else {
                                local(cx, "중단하고 보내기", "Interrupt and send")
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit(SendIntent::ConfirmInterrupt, window, cx)
                            })),
                    ),
            )
    }
}

fn recipient_count(count: usize, lang: Lang) -> String {
    match lang {
        Lang::Ko => format!("{count}명에게"),
        Lang::En => format!("To {count}"),
    }
}

fn mode_description(mode: Kind, cx: &gpui::App) -> &'static str {
    match mode {
        Kind::Queue => local(
            cx,
            "하던 일이 끝나면 읽습니다",
            "Read after the current work ends",
        ),
        Kind::Steer => local(
            cx,
            "하던 일 사이에 읽습니다",
            "Read between steps of the current work",
        ),
        Kind::Interrupt => local(
            cx,
            "하던 일을 멈추고 읽습니다",
            "Stop the current work and read",
        ),
    }
}
