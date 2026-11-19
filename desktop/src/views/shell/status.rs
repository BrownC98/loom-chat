mod usage;

use api::graph::Status;
use api::ids::RepoId;
use gpui::{App, Div, Entity, IntoElement, ParentElement, RenderOnce, Styled, Window, div};

use crate::scale::px;
use crate::store::Store;
use crate::theme::Colors;

pub const HEIGHT: f32 = 22.0;

#[derive(IntoElement)]
struct Usage(Entity<Store>, Option<RepoId>);

impl RenderOnce for Usage {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let store = self.0;
        let view = window.use_keyed_state("status-usage", cx, |_, cx| usage::Usage::new(store, cx));
        view.update(cx, |usage, cx| {
            if usage.repo != self.1 {
                usage.repo = self.1;
                cx.notify();
            }
        });
        view
    }
}

pub fn bar(store: &Entity<Store>, repo: Option<RepoId>, colors: &Colors, cx: &App) -> Div {
    let counts = store.read(cx).snapshot().map(|snapshot| {
        let working = snapshot
            .nodes
            .iter()
            .filter(|node| matches!(node.status, Status::Working { .. }))
            .count();
        format!("에이전트 {} · 일하는 중 {working}", snapshot.nodes.len())
    });
    div()
        .h(px(HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .px(px(4.0))
        .gap(px(8.0))
        .bg(colors.panel)
        .border_t_1()
        .border_color(colors.line)
        .text_size(px(12.0))
        .text_color(colors.ink_2)
        .child(Usage(store.clone(), repo))
        .child(div().flex_1())
        .children(counts.map(|count| div().child(count)))
        .child(usage::version())
}
