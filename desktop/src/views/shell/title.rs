use gpui::{
    App, Bounds, Div, FocusHandle, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Rgba, StatefulInteractiveElement, Styled, Window, canvas, div, fill, point, size,
};

use super::{OpenSettings, ToggleSidebar};
use crate::platform;
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::icons::{Lucide, icon};
use crate::views::tooltip::tip;
use crate::views::ui::{colors, say};
use crate::views::widgets::btn;

pub const HEIGHT: f32 = 36.0;
const MARK: f32 = 16.0;
const MARK_BEFORE: f32 = 14.0;
const MARK_AFTER: f32 = 10.0;
const CELLS: usize = 3;
const PITCH: f32 = 5.0;
const LONG: f32 = 4.0;
const SHORT: f32 = 2.0;

fn mark(color: Rgba) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            for row in 0..CELLS {
                for col in 0..CELLS {
                    let x = 0.5 + col as f32 * PITCH;
                    let y = 0.5 + row as f32 * PITCH;
                    let (dx, dy, w, h) = if (row + col) % 2 == 0 {
                        (0.0, 1.0, LONG, SHORT)
                    } else {
                        (1.0, 0.0, SHORT, LONG)
                    };
                    let cell = Bounds::new(
                        bounds.origin + point(px(x + dx), px(y + dy)),
                        size(px(w), px(h)),
                    );
                    window.paint_quad(fill(cell, color));
                }
            }
        },
    )
    .w(px(MARK))
    .h(px(MARK))
}

pub fn bar(window: &Window, sidebar: &FocusHandle, expanded: bool, cx: &App) -> Div {
    let c = colors(cx);
    let lang = *cx.global::<Lang>();
    let sidebar_icon = if expanded {
        Lucide::PanelLeftClose
    } else {
        Lucide::PanelLeftOpen
    };
    let sidebar_label = match (lang, expanded) {
        (Lang::Ko, true) => "방 목록 접기",
        (Lang::Ko, false) => "방 목록 펴기",
        (Lang::En, true) => "Collapse rooms",
        (Lang::En, false) => "Expand rooms",
    };
    let settings = say(cx, Str::SettingsTitle);
    div()
        .h(px(HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .pl(platform::leading_gap())
        .bg(c.panel)
        .border_b_1()
        .border_color(c.line)
        .child(drag(
            div()
                .h_full()
                .flex()
                .items_center()
                .ml(px(MARK_BEFORE))
                .mr(px(MARK_AFTER))
                .gap(px(6.0))
                .child(mark(c.ink_2))
                .text_color(c.ink)
                .child(crate::frame::NAME),
        ))
        .child(
            btn::ghost("title-sidebar", cx)
                .track_focus(sidebar)
                .aria_label(sidebar_label)
                .aria_expanded(expanded)
                .tooltip(tip(sidebar_label.to_owned()))
                .size(px(28.0))
                .p_0()
                .justify_center()
                .child(icon(sidebar_icon, px(16.0), c.ink_2))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleSidebar), cx)),
        )
        .child(drag(div().flex_1().h_full()))
        .child(
            btn::ghost("title-settings", cx)
                .aria_label(settings)
                .tooltip(tip(settings.to_owned()))
                .size(px(28.0))
                .p_0()
                .justify_center()
                .child(icon(Lucide::Settings, px(16.0), c.ink_2))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
        )
        .children(platform::captions(window, &c, lang))
}

fn drag(area: Div) -> Div {
    platform::drag_area(area.on_mouse_down(MouseButton::Left, |event, window, _| {
        if event.click_count >= 2 {
            window.titlebar_double_click();
        } else {
            window.start_window_move();
        }
    }))
}
