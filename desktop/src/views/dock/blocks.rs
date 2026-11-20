use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use api::graph::text::{Align, Block, TextPart};
use api::graph::waits::WaitDoc;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, ClipboardItem, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ParentElement, RenderOnce, Rgba, StatefulInteractiveElement, Styled, Task, Window,
    div,
};

use super::body::{self, Follow, Selectable, Target};
use super::ui;
use crate::fonts;
use crate::scale::fixed as px;
use crate::strings::{Lang, Str};
use crate::theme::{Colors, mix};
use crate::views::icons::{Lucide, icon};
use crate::views::tooltip::tip;
use crate::views::widgets::btn;

const GAP: f32 = 6.0;
const TIGHT: f32 = 2.0;
const CELL_MAX: f32 = 320.0;
const CODE_GROUP: &str = "block-code";
const COPIED_TIME: Duration = Duration::from_millis(1500);

pub(crate) struct Style {
    colors: Colors,
    link: Rgba,
    selected: Rgba,
}

impl Style {
    pub(crate) fn new(colors: Colors, link: Rgba) -> Self {
        Self {
            colors,
            link,
            selected: colors.accent.opacity(0.25),
        }
    }

    pub(crate) fn on_accent(mut colors: Colors) -> Self {
        let ink = colors.accent_ink;
        colors.bg = colors.accent;
        colors.ink = ink;
        colors.ink_2 = ink;
        colors.line = mix(ink, 0.16, colors.bg);
        colors.line_strong = mix(ink, 0.32, colors.bg);
        Self {
            colors,
            link: ink,
            selected: ink.opacity(0.25),
        }
    }
}

struct Draw<'a> {
    root: ElementId,
    next: Cell<usize>,
    doc: Option<&'a WaitDoc>,
    size: f32,
    cx: &'a App,
    style: Style,
    follow: Follow,
    selection: &'a Selectable,
}

fn mentions_doc(blocks: &[Block]) -> bool {
    blocks.iter().any(|block| match block {
        Block::Paragraph { parts } | Block::Heading { parts, .. } => body::has_doc(parts),
        Block::List { items, .. } => items.iter().any(|item| mentions_doc(item)),
        Block::Quote { blocks } => mentions_doc(blocks),
        Block::Table { head, rows, .. } => std::iter::once(head)
            .chain(rows)
            .any(|row| row.iter().any(|cell| body::has_doc(cell))),
        Block::Code { .. } | Block::Rule => false,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_selectable(
    id: impl Into<ElementId>,
    blocks: &[Block],
    plain: &str,
    doc: Option<&WaitDoc>,
    style: Style,
    size: f32,
    window: &mut Window,
    cx: &mut App,
    follow: impl Fn(Target, &mut App) + 'static,
) -> Div {
    let id = id.into();
    let selection = Selectable::new(id.clone(), window, cx);
    let follow: Follow = Rc::new(follow);
    let draw = Draw {
        root: id,
        next: Cell::new(0),
        doc,
        size,
        cx,
        style,
        follow: follow.clone(),
        selection: &selection,
    };
    let column = draw.render(blocks, plain);
    selection.attach(column, follow, cx)
}

impl Draw<'_> {
    fn render(&self, blocks: &[Block], plain: &str) -> Div {
        let mut column = div().flex().flex_col().gap(px(GAP)).min_w_0();
        if blocks.is_empty() {
            column = column.children(self.text(&[], plain));
        } else {
            column = column.children(blocks.iter().map(|block| self.block(block)));
        }
        if let Some(doc) = self.doc.filter(|_| !mentions_doc(blocks)) {
            column = column.child(
                body::tail(self.id("doc"), doc, self.style.link, self.cx, &self.follow)
                    .self_start(),
            );
        }
        column
    }

    fn id(&self, kind: &str) -> ElementId {
        let at = self.next.get();
        self.next.set(at + 1);
        ElementId::NamedChild(Arc::new(self.root.clone()), format!("{kind}-{at}").into())
    }

    fn text(&self, parts: &[TextPart], plain: &str) -> Option<AnyElement> {
        self.selection
            .text(
                body::build(
                    parts,
                    plain,
                    self.doc,
                    self.style.link,
                    self.style.colors.line,
                ),
                self.style.selected,
            )
            .map(IntoElement::into_any_element)
    }

    fn stack(&self, blocks: &[Block], gap: f32) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(px(gap))
            .min_w_0()
            .children(blocks.iter().map(|block| self.block(block)))
    }

    fn block(&self, block: &Block) -> AnyElement {
        match block {
            Block::Paragraph { parts } => self
                .text(parts, "")
                .unwrap_or_else(|| div().into_any_element()),
            Block::Heading { level, parts } => div()
                .pt(px(4.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(self.size + (4.0 - f32::from(*level)).clamp(0.0, 3.0)))
                .children(self.text(parts, ""))
                .into_any_element(),
            Block::Code { text, language } => self.code(text, language.as_deref()),
            Block::List { start, items } => self.list(*start, items),
            Block::Quote { blocks } => self
                .stack(blocks, GAP)
                .pl(px(10.0))
                .border_l(px(3.0))
                .border_color(self.style.colors.line_strong)
                .text_color(self.style.colors.ink_2)
                .into_any_element(),
            Block::Table { align, head, rows } => self.table(align, head, rows),
            Block::Rule => div()
                .my(px(2.0))
                .h(px(1.0))
                .w_full()
                .bg(self.style.colors.line_strong)
                .into_any_element(),
        }
    }

    fn code(&self, text: &str, language: Option<&str>) -> AnyElement {
        let shown = text.trim_end_matches('\n');
        let shown = self.text(&[], shown);
        let c = self.style.colors;
        let label = language.unwrap_or(match self.cx.global::<Lang>() {
            Lang::Ko => "코드",
            Lang::En => "Code",
        });
        div()
            .id(self.id("code"))
            .group(CODE_GROUP)
            .w_full()
            .min_w_0()
            .border_1()
            .border_color(c.line)
            .rounded(px(4.0))
            .bg(c.bg)
            .text_color(c.ink)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.0))
                    .border_b_1()
                    .border_color(c.line)
                    .pl(px(8.0))
                    .pr(px(3.0))
                    .text_size(px(self.size - 2.0))
                    .text_color(c.ink_2)
                    .child(div().min_w_0().truncate().child(label.to_owned()))
                    .child(CodeCopy {
                        id: self.id("code-copy"),
                        text: text.to_owned(),
                    }),
            )
            .child(
                div()
                    .id(self.id("code-scroll"))
                    .w_full()
                    .min_w_0()
                    .overflow_x_scroll()
                    .font_family(fonts::TERMINAL_FAMILY)
                    .text_size(px(self.size - 1.0))
                    .whitespace_nowrap()
                    .child(div().flex_none().px(px(8.0)).py(px(6.0)).children(shown)),
            )
            .into_any_element()
    }

    fn list(&self, start: Option<u32>, items: &[Vec<Block>]) -> AnyElement {
        let first = start.unwrap_or(1);
        let last = first.saturating_add(items.len() as u32);
        let digits = last.to_string().len() as f32;
        let width = match start {
            Some(_) => (digits + 1.0) * self.size * 0.6 + 6.0,
            None => 0.0,
        }
        .max(self.size + 6.0);
        div()
            .flex()
            .flex_col()
            .gap(px(TIGHT))
            .min_w_0()
            .children(items.iter().enumerate().map(|(at, item)| {
                let marker = match start {
                    Some(_) => format!("{}.", first.saturating_add(at as u32)),
                    None => "\u{2022}".to_owned(),
                };
                div()
                    .flex()
                    .items_start()
                    .child(
                        div()
                            .flex_none()
                            .w(px(width))
                            .pr(px(6.0))
                            .flex()
                            .justify_end()
                            .text_color(self.style.colors.ink_2)
                            .child(marker),
                    )
                    .child(self.stack(item, TIGHT).flex_1())
            }))
            .into_any_element()
    }

    fn table(
        &self,
        align: &[Align],
        head: &[Vec<TextPart>],
        rows: &[Vec<Vec<TextPart>>],
    ) -> AnyElement {
        let c = self.style.colors;
        let mut grid = div()
            .grid()
            .grid_cols_max_content(head.len() as u16)
            .flex_none()
            .border_t_1()
            .border_l_1()
            .border_color(c.line_strong);
        for (row, cells) in std::iter::once(head)
            .chain(rows.iter().map(Vec::as_slice))
            .enumerate()
        {
            for (column, cell) in cells.iter().enumerate() {
                let side = align.get(column).copied().unwrap_or(Align::None);
                grid = grid.child(
                    div()
                        .flex()
                        .px(px(8.0))
                        .py(px(3.0))
                        .max_w(px(CELL_MAX))
                        .border_r_1()
                        .border_b_1()
                        .border_color(c.line_strong)
                        .when(row == 0, |cell| {
                            cell.bg(c.bg).font_weight(FontWeight::SEMIBOLD)
                        })
                        .map(|cell| match side {
                            Align::Right => cell.justify_end(),
                            Align::Center => cell.justify_center(),
                            Align::None | Align::Left => cell,
                        })
                        .children(self.text(cell, "")),
                );
            }
        }
        div()
            .id(self.id("table"))
            .w_full()
            .min_w_0()
            .flex()
            .overflow_x_scroll()
            .child(grid)
            .into_any_element()
    }
}

pub(crate) fn copy_feedback(
    id: ElementId,
    window: &mut Window,
    cx: &mut App,
) -> (bool, impl Fn(&str, &mut App) + 'static) {
    let state = window.use_keyed_state(id, cx, |_, _| None::<Task<()>>);
    (state.read(cx).is_some(), move |text, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        state.update(cx, |state, cx| {
            *state = Some(cx.spawn(async move |entity, cx| {
                cx.background_executor().timer(COPIED_TIME).await;
                let _ = entity.update(cx, |state, cx| {
                    *state = None;
                    cx.notify();
                });
            }));
            cx.notify();
        });
    })
}

#[derive(IntoElement)]
struct CodeCopy {
    id: ElementId,
    text: String,
}

impl RenderOnce for CodeCopy {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (active, copy) = copy_feedback(self.id.clone(), window, cx);
        let label = Str::TerminalCopy.text(*cx.global::<Lang>());
        let c = ui::colors(cx);
        btn::inert(btn::ghost(self.id, cx))
            .flex_none()
            .min_h(px(20.0))
            .px(px(4.0))
            .py_0()
            .aria_label(label)
            .tooltip(tip(label.to_owned()))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when(!active, |button| {
                button
                    .opacity(0.0)
                    .group_hover(CODE_GROUP, |style| style.opacity(1.0))
                    .focus(|style| style.opacity(1.0))
            })
            .child(icon(
                if active { Lucide::Check } else { Lucide::Copy },
                px(12.0),
                c.ink_2,
            ))
            .child(if active {
                Str::RoomCopied.text(*cx.global::<Lang>())
            } else {
                label
            })
            .on_click(move |_, _, cx| copy(&self.text, cx))
    }
}
