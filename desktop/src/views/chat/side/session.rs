use api::ids::NodeId;
use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window, div,
};

use super::unavailable;
use crate::store::Store;
use crate::views::inputs::Inputs;
use crate::views::terminal::{self, TerminalView};

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
