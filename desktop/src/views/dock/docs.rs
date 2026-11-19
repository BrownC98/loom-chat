use api::graph::docs::{DocGroup, DocRow};
use gpui::{
    App, Div, FontWeight, InteractiveElement, IntoElement, ParentElement, Role,
    StatefulInteractiveElement, Styled, div,
};

use super::ui::{self, SMALL, TEXT};
use crate::fonts;
use crate::scale::px;
use crate::strings::Lang;
use crate::strings::Str;
use crate::views::icons::{Lucide, icon};
use crate::views::tooltip::tip;
use crate::views::widgets::btn;

fn kind_label(kind: DocGroup) -> Str {
    match kind {
        DocGroup::Mockup => Str::DocsKindMockup,
        DocGroup::Report => Str::DocsKindReport,
    }
}

pub(crate) fn document_rows<'a>(
    shown: impl IntoIterator<Item = &'a DocRow>,
    cx: &App,
    open: impl Fn(String, &mut App) + Clone + 'static,
) -> Div {
    let c = ui::colors(cx);
    let lang = *cx.global::<Lang>();
    div()
        .flex()
        .flex_col()
        .px(px(8.0))
        .py(px(4.0))
        .children(shown.into_iter().enumerate().map(|(index, doc)| {
            let openable = doc.source || doc.snapshot;
            let mut meta = vec![ui::say(cx, kind_label(doc.kind)).to_owned()];
            if openable {
                meta.push(doc.folder.clone());
                if !doc.source {
                    meta.push(ui::say(cx, Str::DocsLastCopy).to_owned());
                }
                if let Some(at) = doc.attached_at {
                    meta.push(ui::day_and_time(at, lang));
                }
            } else {
                meta.push(ui::say(cx, Str::DocsGone).to_owned());
            }
            let meta = meta.join(" \u{00b7} ");
            let id = doc.id.clone();
            let ink = if openable { c.ink } else { c.ink_3 };
            let title = if openable {
                let open = open.clone();
                btn::tabbable(div().id(("doc", index)), cx)
                    .role(Role::Link)
                    .aria_label(doc.title.clone())
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .min_w_0()
                    .cursor_pointer()
                    .text_color(c.ink)
                    .underline()
                    .child(div().min_w_0().truncate().child(doc.title.clone()))
                    .child(icon(Lucide::ExternalLink, px(12.0), c.ink))
                    .on_click(move |_, _, cx| open(id.clone(), cx))
                    .into_any_element()
            } else {
                div()
                    .min_w_0()
                    .truncate()
                    .child(doc.title.clone())
                    .into_any_element()
            };
            let hover = c.surface_hover;
            div()
                .id(("doc-row", index))
                .flex()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(7.0))
                .rounded(px(6.0))
                .text_size(px(TEXT))
                .hover(move |style| style.bg(hover))
                .child(
                    div()
                        .w(px(44.0))
                        .flex_none()
                        .pt(px(1.0))
                        .text_size(px(SMALL))
                        .font_weight(FontWeight::SEMIBOLD)
                        .font_family(fonts::TERMINAL_FAMILY)
                        .text_color(if openable { c.ink_2 } else { c.ink_3 })
                        .child(doc.id.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .text_color(ink)
                        .child(title)
                        .child(
                            div()
                                .id(("doc-meta", index))
                                .truncate()
                                .text_size(px(SMALL))
                                .text_color(c.ink_3)
                                .tooltip(tip(meta.clone()))
                                .child(meta),
                        ),
                )
        }))
}
