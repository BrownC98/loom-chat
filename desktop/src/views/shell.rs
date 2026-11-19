mod status;
mod title;

use api::graph::pty::Pace;
use gpui::{
    AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseDownEvent, ParentElement, Render, Styled, Subscription, Window, div,
};

use super::chat::Chat;
use crate::settings::{Dialog, DialogEvent};
use crate::store::Store;
use crate::strings::Lang;
use crate::theme::Theme;
use crate::{fonts, link};

gpui::actions!(loom, [OpenSettings]);

pub struct Shell {
    store: Entity<Store>,
    chat: Entity<Chat>,
    focus: FocusHandle,
    settings: Option<(Entity<Dialog>, Subscription)>,
}

impl Shell {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat = cx.new(|cx| Chat::new(store.clone(), window, cx));
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        cx.observe(&chat, |_, _, cx| cx.notify()).detach();
        cx.observe_window_activation(window, |_, window, _| {
            link::pace(if window.is_window_active() {
                Pace::Front
            } else {
                Pace::Back
            });
        })
        .detach();
        Self {
            store,
            chat,
            focus: cx.focus_handle(),
            settings: None,
        }
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            return;
        }
        let store = self.store.clone();
        let dialog = cx.new(|cx| Dialog::new(store, window, cx));
        let closed = cx.subscribe_in(
            &dialog,
            window,
            |this, dialog, _: &DialogEvent, window, cx| {
                let back = dialog.read(cx).restore().cloned();
                window.focus(back.as_ref().unwrap_or(&this.focus), cx);
                this.settings = None;
                cx.notify();
            },
        );
        self.settings = Some((dialog, closed));
        cx.notify();
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.focused(cx).is_none() {
            window.focus(&self.focus, cx);
        }
        let theme = cx.global::<Theme>();
        let lang = *cx.global::<Lang>();
        let title = title::bar(window, &theme.colors, lang);
        let status = status::bar(&self.store, self.chat.read(cx).repo(cx), &theme.colors, cx);
        let notice = self.store.read(cx).notice().map(|notice| {
            super::dock::ui::note(notice.text.clone(), true, cx)
                .flex_none()
                .bg(theme.colors.panel)
                .border_t_1()
                .border_color(theme.colors.line)
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .track_focus(&self.focus)
            .bg(theme.colors.bg)
            .text_color(theme.colors.ink)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let stroke = &event.keystroke;
                if stroke.modifiers.alt
                    && !stroke.modifiers.secondary(cx)
                    && stroke.key.eq_ignore_ascii_case("l")
                    && this.settings.is_none()
                {
                    if !event.is_held {
                        this.chat
                            .update(cx, |chat, cx| chat.open_picker(window, cx));
                    }
                    cx.stop_propagation();
                } else if stroke.modifiers.secondary(cx)
                    && !stroke.modifiers.alt
                    && stroke.key == ","
                {
                    this.open_settings(window, cx);
                    cx.stop_propagation();
                } else if this.settings.is_none() && stroke.modifiers.alt {
                    this.chat
                        .update(cx, |chat, cx| chat.alert_key(event, window, cx));
                }
            }))
            .font_family(fonts::UI_FAMILY)
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.store.update(cx, |store, cx| store.clear_notice(cx));
            }))
            .child(title)
            .child(div().relative().flex_1().min_h_0().child(self.chat.clone()))
            .children(notice)
            .child(status)
            .children(self.settings.as_ref().map(|(dialog, _)| dialog.clone()))
    }
}
