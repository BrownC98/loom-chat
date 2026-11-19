use api::ids::NodeId;
use gpui::{
    Context, Div, Entity, FontWeight, IntoElement, ParentElement, Render, Role,
    StatefulInteractiveElement, Styled, Subscription, Window, div,
};

use super::members::Member;
use super::{SideSlot, scroller, unavailable, words};
use crate::scale::px;
use crate::store::Store;
use crate::strings::Lang;
use crate::views::icons::{Lucide, icon};
use crate::views::inputs::Inputs;
use crate::views::terminal::{self, TerminalView};
use crate::views::ui::{self, SMALL};
use crate::views::widgets::btn;

pub struct SessionTerminal {
    store: Entity<Store>,
    inputs: Entity<Inputs>,
    node: Option<NodeId>,
    terminal: Option<Entity<TerminalView>>,
    _store: Subscription,
}

impl SessionTerminal {
    pub fn new(store: Entity<Store>, inputs: Entity<Inputs>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&store, |this, _, cx| {
            if this.node.is_some() && this.terminal.is_none() {
                cx.notify();
            }
        });
        Self {
            store,
            inputs,
            node: None,
            terminal: None,
            _store: subscription,
        }
    }

    pub fn set_node(&mut self, node: Option<NodeId>, cx: &mut Context<Self>) {
        if self.node != node {
            self.node = node;
            self.terminal = None;
            cx.notify();
        }
    }
}

impl Render for SessionTerminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.terminal.is_none()
            && let Some(node) = self.node
            && let Some(base) = self.store.read(cx).base().map(str::to_owned)
        {
            self.terminal = Some(terminal::open(&self.inputs, &base, node.0, window, cx));
        }
        let body = match &self.terminal {
            Some(view) => view.clone().into_any_element(),
            None => unavailable(cx).into_any_element(),
        };
        div().size_full().overflow_hidden().child(body)
    }
}

impl SideSlot {
    pub(super) fn terminal_panel(&mut self, cx: &mut Context<Self>) -> Div {
        let data = self.data.read(cx);
        let room = data.room();
        let members = room.map_or_else(Vec::new, |room| self.members(room, cx));
        let chosen = match &self.member {
            Some(key) => members.iter().find(|member| &member.key == key),
            None => members
                .iter()
                .find(|member| member.terminal().is_some())
                .or_else(|| members.first()),
        };
        let initial = chosen.map(|member| member.key.clone());
        let node = room
            .filter(|room| !room.archived)
            .and_then(|_| chosen.and_then(Member::terminal));
        let c = ui::colors(cx);
        let entity = cx.entity().downgrade();
        let rows: Vec<_> = if self.choosing {
            members
                .iter()
                .enumerate()
                .map(|(index, member)| {
                    let key = member.key.clone();
                    let active = chosen.is_some_and(|chosen| chosen.key == key);
                    let entity = entity.clone();
                    btn::button(("chat-terminal-member", index), cx)
                        .role(Role::Button)
                        .aria_label(member.name.to_owned())
                        .w_full()
                        .bg(if active { c.accent_wash } else { c.surface })
                        .child(div().min_w_0().truncate().child(member.name.to_owned()))
                        .children(member.terminal().is_none().then(|| {
                            div().ml_auto().text_color(c.ink_2).child(words(
                                cx,
                                "터미널 없음",
                                "No terminal",
                            ))
                        }))
                        .on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                this.member = Some(key.clone());
                                this.choosing = false;
                                this.scroll.reset();
                                cx.notify();
                            });
                        })
                })
                .collect()
        } else {
            Vec::new()
        };
        let head = !members.is_empty() || chosen.is_some();
        let head = head.then(|| {
            let title = chosen.map_or_else(
                || super::SideTab::Terminal.label(cx).to_owned(),
                |member| match *cx.global::<Lang>() {
                    Lang::Ko => format!("{}의 터미널", member.name),
                    Lang::En => format!("{}'s terminal", member.name),
                },
            );
            div()
                .flex_none()
                .px(px(12.0))
                .py(px(8.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .border_b_1()
                .border_color(c.line)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .children(chosen.and_then(|member| member.harness).map(|harness| {
                    div()
                        .text_size(px(SMALL))
                        .text_color(c.ink_2)
                        .child(harness.to_owned())
                }))
                .children((members.len() > 1 || chosen.is_none()).then(|| {
                    let label = words(cx, "구성원 고르기", "Select member");
                    btn::button("chat-terminal-choose", cx)
                        .role(Role::Button)
                        .aria_label(label)
                        .aria_expanded(self.choosing)
                        .flex_none()
                        .child(label)
                        .child(icon(Lucide::ChevronDown, px(13.0), c.ink_2))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.choosing = !this.choosing;
                            cx.notify();
                        }))
                }))
                .children((members.len() == 1 && node.is_some()).then(|| {
                    div().text_color(c.ink_2).text_size(px(SMALL)).child(words(
                        cx,
                        "직접 칠 수 있습니다",
                        "Type in the terminal",
                    ))
                }))
        });
        if self.member.is_none() {
            self.member = initial;
        }
        self.terminal
            .update(cx, |terminal, cx| terminal.set_node(node, cx));
        let choices = (!rows.is_empty()).then(|| {
            scroller(
                &self.scroll,
                "chat-terminal-members",
                div()
                    .flex()
                    .flex_col()
                    .p(px(6.0))
                    .gap(px(4.0))
                    .children(rows),
            )
            .h(px(120.0))
            .flex_none()
            .border_b_1()
            .border_color(c.line)
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .children(head)
            .children(choices)
            .child(div().flex_1().min_h_0().child(self.terminal.clone()))
    }
}
