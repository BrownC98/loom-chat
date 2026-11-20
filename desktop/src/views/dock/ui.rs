pub use crate::views::ui::{
    GAP, PAD, SMALL, TEXT, ago, button, clock, colors, day_and_time, open_url, say, say_with,
};

use gpui::{App, Div, FontWeight, ParentElement, Styled, div};

use crate::scale::px;
use crate::views::prose::prose;

pub fn note(text: String, error: bool, cx: &App) -> Div {
    let c = colors(cx);
    let base = div().p(px(16.0));
    if error {
        base.text_size(px(SMALL))
            .text_color(c.err)
            .child(crate::fonts::latin_mono(&text, c.err, FontWeight::NORMAL))
    } else {
        base.text_size(px(TEXT))
            .text_color(c.ink_3)
            .child(prose(text))
    }
}

pub fn hint(text: &str, cx: &App) -> Div {
    let c = colors(cx);
    div()
        .flex_none()
        .px(px(16.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(c.line)
        .text_size(px(SMALL))
        .text_color(c.ink_3)
        .child(prose(text.to_owned()))
}
