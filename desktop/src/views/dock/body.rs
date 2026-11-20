use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use api::graph::text::TextPart;
use api::graph::waits::WaitDoc;
use gpui::{
    AnyElement, App, ClipboardItem, CursorStyle, Div, ElementId, Entity, FocusHandle, FontStyle,
    FontWeight, HighlightStyle, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Pixels, Point, Rgba, Stateful, StatefulInteractiveElement, Styled, StyledText, Task,
    TextLayout, UnderlineStyle, Window, combine_highlights, div,
};

use super::ui;
use crate::scale::fixed as px;
use crate::store::Store;
use crate::strings::Str;
use crate::views::icons::{Lucide, icon};
use crate::views::prose::prose;
use crate::views::tooltip::tip;

#[derive(Clone)]
pub enum Target {
    Doc(String),
    Url(String),
}

#[derive(Default)]
pub(super) struct Built {
    shown: String,
    links: Vec<(Range<usize>, Target)>,
    styles: Vec<(Range<usize>, HighlightStyle)>,
}

impl Built {
    fn push(&mut self, label: &str, target: Option<Target>) {
        let start = self.shown.len();
        self.shown.push_str(label);
        if let Some(target) = target {
            self.links.push((start..self.shown.len(), target));
        }
    }

    fn styled(&mut self, text: &str, style: HighlightStyle) {
        let start = self.shown.len();
        self.shown.push_str(text);
        self.styles.push((start..self.shown.len(), style));
    }
}

pub fn open(store: &Entity<Store>, target: Target, cx: &mut App) -> Task<Result<(), String>> {
    match target {
        Target::Url(url) => Task::ready(ui::open_url(&url)),
        Target::Doc(id) => Task::ready(
            store
                .read(cx)
                .doc_url(&id)
                .and_then(|url| ui::open_url(&url)),
        ),
    }
}

pub type Follow = Rc<dyn Fn(Target, &mut App)>;

pub fn has_doc(parts: &[TextPart]) -> bool {
    parts
        .iter()
        .any(|part| matches!(part, TextPart::Doc { .. }))
}

pub fn render(
    id: impl Into<ElementId>,
    parts: &[TextPart],
    plain: &str,
    doc: Option<&WaitDoc>,
    color: Rgba,
    cx: &App,
    follow: impl Fn(Target, &mut App) + 'static,
) -> AnyElement {
    let id = id.into();
    let follow: Follow = Rc::new(follow);
    let text = inline(id.clone(), parts, plain, doc, color, cx, &follow);
    let tail = doc
        .filter(|_| !has_doc(parts))
        .map(|doc| tail(id, doc, color, cx, &follow));
    match (text, tail) {
        (Some(text), None) => text,
        (None, Some(tail)) => tail.into_any_element(),
        (Some(text), Some(tail)) => div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(4.0))
            .child(text)
            .child(tail)
            .into_any_element(),
        (None, None) => div().into_any_element(),
    }
}

pub fn tail(id: ElementId, doc: &WaitDoc, color: Rgba, cx: &App, follow: &Follow) -> Stateful<Div> {
    let target = Target::Doc(doc.id.clone());
    let follow = follow.clone();
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(3.0))
        .text_color(color)
        .underline()
        .cursor_pointer()
        .tooltip(tip(ui::say_with(
            cx,
            Str::InboxOpenDocLabel,
            &[("id", &doc.id), ("title", &doc.title)],
        )))
        .child(format!("{} {}", doc.id, doc.title))
        .child(icon(Lucide::ExternalLink, px(12.0), color))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| follow(target.clone(), cx))
}

pub fn inline(
    id: ElementId,
    parts: &[TextPart],
    plain: &str,
    doc: Option<&WaitDoc>,
    color: Rgba,
    cx: &App,
    follow: &Follow,
) -> Option<AnyElement> {
    let Built {
        shown,
        links,
        styles,
    } = build(parts, plain, doc, color, cx);
    if shown.is_empty() {
        return None;
    }
    let (ranges, targets): (Vec<_>, Vec<_>) = links.into_iter().unzip();
    let follow = follow.clone();
    Some(
        prose(shown)
            .with_highlights(styles)
            .on_click(
                ElementId::NamedChild(Arc::new(id), "text".into()),
                ranges,
                move |index, _, cx| follow(targets[index].clone(), cx),
            )
            .into_any_element(),
    )
}

pub(super) fn build(
    parts: &[TextPart],
    plain: &str,
    doc: Option<&WaitDoc>,
    color: Rgba,
    cx: &App,
) -> Built {
    let mut built = Built::default();
    for part in parts {
        match part {
            TextPart::Text { text } => built.push(text, None),
            TextPart::Doc { label } => built.push(label, doc.map(|d| Target::Doc(d.id.clone()))),
            TextPart::Link { label, href } => built.push(label, Some(Target::Url(href.clone()))),
            TextPart::Strong { text } => built.styled(
                text,
                HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
            ),
            TextPart::Emphasis { text } => built.styled(
                text,
                HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                },
            ),
            TextPart::Code { text } => built.styled(
                text,
                HighlightStyle {
                    background_color: Some(ui::colors(cx).line.into()),
                    ..Default::default()
                },
            ),
        }
    }
    if parts.is_empty() {
        built.push(plain, None);
    }
    let highlight = HighlightStyle {
        color: Some(color.into()),
        underline: Some(UnderlineStyle {
            color: Some(color.into()),
            thickness: px(1.0),
            wavy: false,
        }),
        ..Default::default()
    };
    built.styles.extend(
        built
            .links
            .iter()
            .map(|(range, _)| (range.clone(), highlight)),
    );
    built.styles.sort_by_key(|(range, _)| range.start);
    built
}

struct Selection {
    focus: FocusHandle,
    anchor: usize,
    head: usize,
    down: bool,
}

impl Selection {
    fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

struct Section {
    offset: usize,
    len: usize,
    layout: TextLayout,
    links: Vec<(Range<usize>, Target)>,
}

#[derive(Default)]
struct Document {
    text: String,
    sections: Vec<Section>,
}

impl Document {
    fn at(&self, point: Point<Pixels>) -> Option<(&Section, usize)> {
        let section = self.sections.iter().min_by_key(|section| {
            let bounds = section.layout.bounds();
            let x = (bounds.left() - point.x)
                .max(point.x - bounds.right())
                .max(Pixels::ZERO);
            let y = (bounds.top() - point.y)
                .max(point.y - bounds.bottom())
                .max(Pixels::ZERO);
            (y, x)
        })?;
        let index = match section.layout.index_for_position(point) {
            Ok(index) | Err(index) => index.min(section.len),
        };
        let offset = self.text.floor_char_boundary(section.offset + index);
        Some((section, offset))
    }
}

pub(super) struct Selectable {
    selection: Entity<Selection>,
    range: Range<usize>,
    document: RefCell<Document>,
}

impl Selectable {
    pub(super) fn new(id: ElementId, window: &mut Window, cx: &mut App) -> Self {
        let selection = window.use_keyed_state(
            ElementId::NamedChild(Arc::new(id), "select".into()),
            cx,
            |_, cx| Selection {
                focus: cx.focus_handle().tab_stop(true),
                anchor: 0,
                head: 0,
                down: false,
            },
        );
        let state = selection.read(cx);
        let range = if state.focus.is_focused(window) {
            state.range()
        } else {
            0..0
        };
        Self {
            selection,
            range,
            document: RefCell::default(),
        }
    }

    pub(super) fn text(&self, built: Built, cx: &App) -> Option<StyledText> {
        let Built {
            shown,
            links,
            styles,
        } = built;
        if shown.is_empty() {
            return None;
        }
        let mut doc = self.document.borrow_mut();
        if !doc.text.is_empty() {
            doc.text.push('\n');
        }
        let offset = doc.text.len();
        doc.text.push_str(&shown);
        let len = shown.len();
        let range = shown.floor_char_boundary(self.range.start.saturating_sub(offset).min(len))
            ..shown.floor_char_boundary(self.range.end.saturating_sub(offset).min(len));
        let c = ui::colors(cx);
        let selected = HighlightStyle {
            background_color: Some(c.accent_wash.into()),
            ..Default::default()
        };
        let styles = if range.is_empty() {
            styles
        } else {
            combine_highlights(styles, [(range, selected)]).collect()
        };
        let text = StyledText::new(shown).with_highlights(styles);
        doc.sections.push(Section {
            offset,
            len,
            layout: text.layout().clone(),
            links,
        });
        Some(text)
    }

    pub(super) fn attach(self, base: Div, follow: Follow, cx: &App) -> Div {
        let document = Rc::new(self.document.into_inner());
        let selection = self.selection;
        let focus = selection.read(cx).focus.clone();
        base.track_focus(&focus)
            .cursor(CursorStyle::IBeam)
            .on_mouse_down(MouseButton::Left, {
                let (document, selection) = (document.clone(), selection.clone());
                move |event, window, cx| {
                    let Some((_, offset)) = document.at(event.position) else {
                        return;
                    };
                    focus.focus(window, cx);
                    selection.update(cx, |selection, cx| {
                        if !event.modifiers.shift {
                            selection.anchor = offset;
                        }
                        selection.head = offset;
                        selection.down = true;
                        cx.notify();
                    });
                }
            })
            .on_mouse_move({
                let (document, selection) = (document.clone(), selection.clone());
                move |event, _, cx| {
                    if !event.dragging() || !selection.read(cx).down {
                        return;
                    }
                    if let Some((_, offset)) = document.at(event.position) {
                        selection.update(cx, |selection, cx| {
                            if selection.head != offset {
                                selection.head = offset;
                                cx.notify();
                            }
                        });
                    }
                }
            })
            .on_mouse_up_out(MouseButton::Left, {
                let (document, selection) = (document.clone(), selection.clone());
                move |event, _, cx| {
                    if selection.read(cx).down {
                        selection.update(cx, |selection, cx| {
                            selection.down = false;
                            if let Some((_, offset)) = document.at(event.position)
                                && selection.head != offset
                            {
                                selection.head = offset;
                                cx.notify();
                            }
                        });
                    }
                }
            })
            .on_mouse_up(MouseButton::Left, {
                let (document, selection) = (document.clone(), selection.clone());
                move |event, _, cx| {
                    if !selection.read(cx).down {
                        return;
                    }
                    let Some((section, offset)) = document.at(event.position) else {
                        return;
                    };
                    let link = selection.update(cx, |selection, cx| {
                        selection.down = false;
                        if selection.head != offset {
                            selection.head = offset;
                            cx.notify();
                        }
                        if selection.range().is_empty()
                            && section.layout.index_for_position(event.position).is_ok()
                        {
                            let index = offset.saturating_sub(section.offset);
                            section
                                .links
                                .iter()
                                .find(|(range, _)| range.contains(&index))
                                .map(|(_, target)| target.clone())
                        } else {
                            None
                        }
                    });
                    if let Some(link) = link {
                        follow(link, cx);
                    }
                }
            })
            .on_key_down(move |event, window, cx| {
                let key = &event.keystroke;
                if key.modifiers.alt || !key.modifiers.secondary(cx) {
                    return;
                }
                match key.key.as_str() {
                    "a" => selection.update(cx, |selection, cx| {
                        selection.anchor = 0;
                        selection.head = document.text.len();
                        cx.notify();
                    }),
                    "c" => {
                        let range = selection.read(cx).range();
                        if let Some(text) = document.text.get(range).filter(|text| !text.is_empty())
                        {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                        }
                    }
                    _ => return,
                }
                window.prevent_default();
                cx.stop_propagation();
            })
    }
}
