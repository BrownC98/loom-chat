use api::graph::NodeView;
use api::graph::events::{Entry, EventRecord};
use api::profile::AgentView;
use gpui::{
    AnyElement, App, ClickEvent, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Role, StatefulInteractiveElement, Styled, Window, div,
};

use super::body;
use super::ui::{self, GAP, PAD, SMALL};
use crate::fonts;
use crate::scale::px;
use crate::strings::Lang;
use crate::theme::Colors;
use crate::views::widgets::btn;
use what::{Ctx, Family, Seg, Tone};

pub mod refusal;
pub(crate) mod what;

const TIME: &str = "%H:%M:%S";
const INDENT: f32 = 80.0;

pub(crate) struct EventRow<'a> {
    pub record: &'a EventRecord,
    pub current: bool,
    pub agents: &'a [AgentView],
    pub nodes: &'a [NodeView],
}

impl EventRow<'_> {
    pub(crate) fn render(
        &self,
        cx: &App,
        pick: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        follow: impl Fn(body::Target, &mut App) + Clone + 'static,
    ) -> AnyElement {
        let c = ui::colors(cx);
        let by_name = |name: &str| {
            self.nodes
                .iter()
                .find(|node| node.name == name)
                .map(|node| node.id.0)
        };
        let known = |id: u32| self.nodes.iter().any(|node| node.id.0 == id);
        let ctx = Ctx {
            lang: *cx.global::<Lang>(),
            agents: self.agents,
            by_name: &by_name,
        };
        let record = self.record;
        let current = self.current;
        let segs = what::what(record, &ctx);
        let body_parts = what::body_of(record, &ctx);
        let (message_kind, doc) = match &record.entry {
            Entry::Message { what, .. } => (what.as_ref(), None),
            Entry::Asked { doc, .. } | Entry::Reported { doc, .. } => (None, doc.as_ref()),
            _ => (None, None),
        };
        let facts = message_kind.zip(record.message.as_ref());
        let summary = message_kind.and_then(|kind| what::summary(kind, &ctx));
        let preview = match (&summary, &body_parts) {
            (Some(text), _) => {
                (!record.message.as_ref().is_some_and(|f| f.from_loom)).then(|| text.clone())
            }
            (None, Some(parts)) => what::plain(parts).lines().next().map(str::to_owned),
            (None, None) => None,
        }
        .filter(|text| !text.is_empty());
        let key = record.id as usize;

        let line = div()
            .flex()
            .flex_wrap()
            .items_baseline()
            .gap_x(px(GAP))
            .children(segs.into_iter().map(|seg| seg_element(seg, &known, &c)));
        let head = btn::tabbable(div().id(("event-head", key)), cx)
            .role(Role::Button)
            .aria_expanded(current)
            .flex()
            .gap(px(10.0))
            .px(px(PAD))
            .py(px(7.0))
            .cursor_pointer()
            .child(
                div()
                    .w(px(62.0))
                    .flex_none()
                    .text_size(px(SMALL))
                    .font_family(fonts::TERMINAL_FAMILY)
                    .text_color(c.ink_3)
                    .child(ui::clock(record.at, TIME)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(line)
                    .children(preview.filter(|_| !current).map(|text| {
                        div()
                            .text_size(px(SMALL))
                            .font_family(fonts::TERMINAL_FAMILY)
                            .text_color(c.ink_2)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(text)
                    })),
            )
            .on_click(pick);

        let mut row = div()
            .id(("event", key))
            .relative()
            .mx(px(8.0))
            .rounded(px(6.0))
            .hover(move |style| style.bg(c.surface_hover))
            .child(head);
        if current {
            row = row.bg(c.accent_wash);
        }
        if let Some(family) = what::family_of(&record.entry) {
            row = row.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(7.0))
                    .bottom(px(7.0))
                    .w(px(3.0))
                    .rounded(px(2.0))
                    .bg(family_color(family, &c)),
            );
        }
        if current {
            row = row.child(self.details(facts, body_parts, doc, cx, follow));
        }
        row.into_any_element()
    }

    fn details(
        &self,
        facts: Option<(
            &api::graph::messages::MessageKind,
            &api::graph::events::MessageFacts,
        )>,
        body_parts: Option<Vec<api::graph::text::TextPart>>,
        doc: Option<&api::graph::waits::WaitDoc>,
        cx: &App,
        follow: impl Fn(body::Target, &mut App) + Clone + 'static,
    ) -> Div {
        let record = self.record;
        let key = record.id as usize;
        let c = ui::colors(cx);
        let link = if what::family_of(&record.entry) == Some(Family::Done) {
            c.ok
        } else {
            c.warn
        };
        let mut details = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .pl(px(INDENT))
            .pr(px(PAD))
            .pb(px(PAD))
            .text_size(px(SMALL));
        let line = |label: &str, value: AnyElement| {
            div()
                .flex()
                .gap(px(10.0))
                .child(
                    div()
                        .w(px(72.0))
                        .flex_none()
                        .text_color(c.ink_3)
                        .child(label.to_owned()),
                )
                .child(div().flex_1().min_w_0().text_color(c.ink).child(value))
        };
        let Some((kind, facts)) = facts else {
            if body_parts.is_some() || doc.is_some() {
                let text = body::render(
                    ("event-body", key),
                    body_parts.as_deref().unwrap_or_default(),
                    "",
                    doc,
                    link,
                    cx,
                    follow,
                );
                details = details.child(div().text_color(c.ink_2).child(text));
            }
            return details;
        };
        for (at, field) in facts.fields.iter().enumerate() {
            let text = body::render(
                ("event-field", key * 32 + at),
                &field.parts,
                "",
                None,
                link,
                cx,
                follow.clone(),
            );
            details = details.child(line(
                ui::say(cx, what::field_name(field.name)),
                text.into_any_element(),
            ));
        }
        if let Some(parts) = body_parts {
            let text = body::render(("event-body", key), &parts, "", None, link, cx, follow);
            let mut value = div()
                .flex()
                .flex_col()
                .gap(px(GAP))
                .text_color(c.ink_2)
                .child(text);
            if !record.choices.is_empty() {
                value = value.child(div().flex().flex_wrap().gap(px(4.0)).children(
                    record.choices.iter().map(|choice| {
                        div()
                            .flex()
                            .gap(px(GAP))
                            .px(px(8.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(if choice.recommended {
                                c.warn
                            } else {
                                c.line_strong
                            })
                            .bg(c.surface)
                            .text_color(c.ink)
                            .child(div().text_color(c.ink_3).child(choice.key.to_string()))
                            .child(choice.label.clone())
                    }),
                ));
            }
            details = details.child(line(
                ui::say(cx, what::body_name(kind)),
                value.into_any_element(),
            ));
        }
        details
    }
}

fn family_color(family: Family, c: &Colors) -> gpui::Hsla {
    match family {
        Family::Question => c.warn,
        Family::Done => c.ok,
        Family::Fail => c.err,
    }
    .into()
}

fn seg_element(seg: Seg, known: &dyn Fn(u32) -> bool, c: &Colors) -> AnyElement {
    let base = div().text_size(px(SMALL));
    match seg {
        Seg::Kind(text, tone) => base
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(match tone {
                Tone::Plain => c.ink_2.into(),
                Tone::Family(family) => family_color(family, c),
            })
            .child(text),
        Seg::Who(id, name) => base
            .text_size(px(ui::TEXT))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(if id.is_some_and(known) {
                c.ink
            } else {
                c.ink_3
            })
            .child(name),
        Seg::Loom => base
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c.ink_3)
            .child("loom"),
        Seg::Text(text) => base.text_size(px(ui::TEXT)).child(text),
        Seg::Note(text, error) => base
            .text_color(if error { c.err } else { c.ink_3 })
            .child(text),
    }
    .into_any_element()
}
