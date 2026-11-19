use api::graph::waits::WaitTone;
use gpui::{Div, FontWeight, IntoElement, ParentElement, Rgba, Styled, div};

use super::ui::{self, SMALL};
use crate::fonts;
use crate::scale::fixed as px;
use crate::strings::Str;
use crate::theme::{Colors, mix};

pub(crate) fn tone_of(tone: WaitTone, c: &Colors) -> Rgba {
    match tone {
        WaitTone::Ask => c.warn,
        WaitTone::Done => c.ok,
    }
}

pub(crate) fn tone_ink(tone: WaitTone, c: &Colors) -> Rgba {
    match tone {
        WaitTone::Ask => c.warn_ink,
        WaitTone::Done => c.surface,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Look {
    Plain,
    Recommended,
    Chosen,
}

pub(crate) struct Choice<'a> {
    pub key: String,
    pub hint: Option<String>,
    pub label: &'a str,
    pub recommended: bool,
    pub look: Look,
    pub tone: WaitTone,
}

pub(crate) fn choice_face<E: Styled + ParentElement>(
    base: E,
    choice: Choice,
    z: f32,
    cx: &gpui::App,
) -> E {
    let Choice {
        key,
        hint,
        label,
        recommended,
        look,
        tone,
    } = choice;
    let c = ui::colors(cx);
    let color = tone_of(tone, &c);
    let chosen = look == Look::Chosen;
    let mut face = base
        .flex()
        .items_baseline()
        .gap(px(6.0 * z))
        .min_h(px(26.0 * z))
        .px(px(if chosen { 9.0 } else { 10.0 } * z))
        .py(px(if chosen { 2.0 } else { 3.0 } * z))
        .rounded(px(6.0 * z))
        .border_color(if chosen {
            c.ink
        } else if recommended {
            color
        } else {
            c.line_strong
        })
        .bg(if recommended {
            mix(color, 0.1, c.surface)
        } else {
            c.surface
        })
        .text_color(c.ink)
        .text_size(px(SMALL * z))
        .font_weight(if chosen {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        });
    face = if chosen {
        face.border(px(2.0 * z))
    } else {
        face.border(px(z))
    };
    let mut label_box = div().flex().items_baseline().child(label.to_owned());
    if recommended {
        label_box = label_box.child(
            div()
                .ml(px(6.0 * z))
                .text_size(px(11.0 * z))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color)
                .whitespace_nowrap()
                .child(ui::say(cx, Str::InboxRecommended)),
        );
    }
    face.children(hint.map(|text| {
        div()
            .text_size(px(10.0 * z))
            .font_family(fonts::TERMINAL_FAMILY)
            .text_color(c.ink_3)
            .child(text)
    }))
    .child(
        div()
            .text_size(px(11.0 * z))
            .font_weight(FontWeight::SEMIBOLD)
            .font_family(fonts::TERMINAL_FAMILY)
            .text_color(c.ink_3)
            .child(key),
    )
    .child(label_box)
}

pub(crate) fn prompt_box(text: impl IntoElement, z: f32, c: &Colors) -> Div {
    div()
        .px(px(8.0 * z))
        .py(px(6.0 * z))
        .rounded(px(6.0 * z))
        .border_1()
        .border_color(c.line)
        .bg(c.bg)
        .text_size(px(SMALL * z))
        .text_color(c.ink)
        .font_family(fonts::TERMINAL_FAMILY)
        .child(text)
}
