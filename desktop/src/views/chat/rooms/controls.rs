mod actions;
mod workspace;

use api::graph::Place;
use api::ids::NodeId;
use api::repos::{PlaceView, RepoView, Worktree};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyView, App, AppContext, ClickEvent, Context, Div, ElementId, Entity, FocusHandle, FontWeight,
    InteractiveElement, KeyDownEvent, ParentElement, Pixels, Point, Role, Stateful,
    StatefulInteractiveElement, Styled, Subscription, Window, div, point,
};

use crate::scale::px;
use crate::store::Store;
use crate::strings::Str;
use crate::views::field::Field;
use crate::views::icons::{Lucide, icon};
use crate::views::new_node::NewNodeView;
use crate::views::tooltip::tip;
use crate::views::ui::{colors, say};
use crate::views::widgets::btn;
use crate::views::workspace::{AddRepo, calls, pop};
use crate::views::worktree::{Closed, DeleteWorktreeView, Doomed, NewWorktreeView};

use super::list::Index;
use super::{Rooms, chevron, note, words};

const MENU_WIDTH: f32 = 244.0;
const PICKER_WIDTH: f32 = 360.0;
const PANEL_EDGE: f32 = 8.0;

pub(super) enum Overlay {
    Panel {
        at: Point<Pixels>,
        panel: Panel,
    },
    Dialog {
        at: Option<Point<Pixels>>,
        view: AnyView,
        _closed: Subscription,
    },
}

pub(super) enum Panel {
    Workspace,
    New,
    AgentPlace,
    WorktreeRepo,
    Agent(NodeId),
    Parent(NodeId),
    Work(Place),
    Rename {
        target: Rename,
        field: Entity<Field>,
        saving: bool,
        _keys: Subscription,
    },
}

#[derive(Clone)]
pub(super) enum Rename {
    Agent(NodeId),
    Work(Place),
}

impl Rooms {
    fn store(&self, cx: &App) -> Entity<Store> {
        self.data.read(cx).graph().clone()
    }

    fn open_panel(
        &mut self,
        panel: Panel,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay.is_none() {
            self.return_focus = window.focused(cx);
        }
        self.action_error = None;
        self.overlay = Some(Overlay::Panel { at, panel });
        window.focus(&self.panel_focus, cx);
        cx.notify();
    }

    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        if let Some(previous) = self.return_focus.take() {
            window.focus(&previous, cx);
        }
        cx.notify();
    }

    fn dialog<T>(
        &mut self,
        view: Entity<T>,
        at: Option<Point<Pixels>>,
        previous: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        T: gpui::Render + gpui::EventEmitter<Closed>,
    {
        if self.overlay.is_none() {
            self.return_focus = previous;
        }
        let closed = cx.subscribe_in(&view, window, |this, _, _: &Closed, window, cx| {
            this.close_overlay(window, cx);
        });
        self.overlay = Some(Overlay::Dialog {
            at,
            view: view.into(),
            _closed: closed,
        });
        cx.notify();
    }

    fn open_add_workspace(
        &mut self,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.store(cx);
        let hosts = store
            .read(cx)
            .snapshot()
            .into_iter()
            .flat_map(|s| &s.repos)
            .filter_map(|repo| match &repo.place {
                PlaceView::Ssh { host } => Some(host.clone()),
                _ => None,
            })
            .collect();
        let previous = window.focused(cx);
        let view = cx.new(|cx| AddRepo::new(store, hosts, at, window, cx));
        self.dialog(view, None, previous, window, cx);
    }

    fn open_agent(
        &mut self,
        place: Place,
        label: String,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.store(cx);
        let previous = window.focused(cx);
        let view = cx.new(|cx| NewNodeView::new(store, place, label, window, cx));
        self.dialog(view, Some(at), previous, window, cx);
    }

    fn open_worktree(
        &mut self,
        repo: RepoView,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.store(cx);
        let previous = window.focused(cx);
        let view = cx.new(|cx| NewWorktreeView::new(store, repo, window, cx));
        self.dialog(view, Some(at), previous, window, cx);
    }

    fn open_delete_worktree(
        &mut self,
        place: Place,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.store(cx);
        let Some(snapshot) = store.read(cx).snapshot() else {
            return;
        };
        let Some((repo, tree)) = work_in(&snapshot.repos, &place) else {
            return;
        };
        if !repo.git || tree.original {
            return;
        }
        let label = work_label(repo, tree);
        let doomed = Doomed::of(snapshot, place.repo, place.worktree, label);
        let previous = window.focused(cx);
        let view = cx.new(|cx| DeleteWorktreeView::new(store, doomed, window, cx));
        self.dialog(view, Some(at), previous, window, cx);
    }
}

impl Rooms {
    pub(super) fn overlay(
        &self,
        index: &Index<'_>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<Div> {
        let overlay = self.overlay.as_ref()?;
        let outside = cx.listener(|this, _, window, cx| this.close_overlay(window, cx));
        let (at, panel) = match overlay {
            Overlay::Dialog { at, view, .. } => {
                return Some(match at {
                    Some(at) => pop::overlay(*at, view.clone(), outside, window),
                    None => div().child(view.clone()),
                });
            }
            Overlay::Panel { at, panel } => (*at, panel),
        };
        let (title, width, body) = match panel {
            Panel::Workspace => (
                say(cx, Str::TreeRepos),
                PICKER_WIDTH,
                self.workspace_menu(index, cx),
            ),
            Panel::New => (
                words(cx, "새로 만들기", "New"),
                MENU_WIDTH,
                self.new_menu(index, at, cx),
            ),
            Panel::AgentPlace => (
                words(cx, "새 에이전트", "New agent"),
                PICKER_WIDTH,
                self.agent_places(index, cx),
            ),
            Panel::WorktreeRepo => (
                say(cx, Str::RepoNewWorktree),
                PICKER_WIDTH,
                self.worktree_repos(index, cx),
            ),
            Panel::Agent(node) => (
                words(cx, "에이전트 메뉴", "Agent menu"),
                MENU_WIDTH,
                self.agent_menu(index, *node, at, cx),
            ),
            Panel::Parent(node) => (
                words(cx, "부모 바꾸기", "Change parent"),
                PICKER_WIDTH,
                self.parent_menu(index, *node, cx),
            ),
            Panel::Work(place) => (
                words(cx, "워크트리 메뉴", "Worktree menu"),
                MENU_WIDTH,
                self.work_menu(place, at, cx),
            ),
            Panel::Rename { field, saving, .. } => (
                say(cx, Str::RenameAction),
                MENU_WIDTH,
                self.rename_form(field, *saving, cx),
            ),
        };
        let c = colors(cx);
        let max_height =
            (window.viewport_size().height - px(PANEL_EDGE * 2.0 + 60.0)).min(px(460.0));
        Some(pop::overlay(
            at,
            div()
                .id("chat-action-popover")
                .track_focus(&self.panel_focus)
                .tab_group()
                .role(Role::Dialog)
                .aria_label(title)
                .flex()
                .flex_col()
                .gap(px(4.0))
                .w(px(width))
                .p(px(PANEL_EDGE))
                .rounded(px(8.0))
                .border_1()
                .border_color(c.line_strong)
                .bg(c.surface)
                .shadow_lg()
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_overlay(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .px(px(6.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .id("chat-action-scroll")
                        .flex()
                        .flex_col()
                        .max_h(max_height)
                        .overflow_y_scroll()
                        .child(body),
                )
                .children(
                    self.action_error
                        .as_ref()
                        .map(|error| div().px(px(6.0)).text_color(c.err).child(error.clone())),
                ),
            outside,
            window,
        ))
    }
}

fn below(event: &ClickEvent) -> Point<Pixels> {
    let at = event.position();
    point(at.x, at.y + px(5.0))
}

fn work_in<'a>(repos: &'a [RepoView], place: &Place) -> Option<(&'a RepoView, &'a Worktree)> {
    let repo = repos.iter().find(|repo| repo.id == place.repo)?;
    let tree = repo
        .worktrees
        .iter()
        .find(|tree| tree.path == place.worktree)?;
    Some((repo, tree))
}

fn place_of(repo: &RepoView, tree: &Worktree) -> Place {
    Place {
        repo: repo.id,
        worktree: tree.path.clone(),
    }
}

fn work_label(repo: &RepoView, tree: &Worktree) -> String {
    if repo.git {
        tree.branch.clone().unwrap_or_else(|| tree.path.clone())
    } else {
        repo.name.clone()
    }
}

fn option(id: impl Into<ElementId>, glyph: Lucide, label: &str, cx: &App) -> Stateful<Div> {
    let c = colors(cx);
    btn::tabbable(div().id(id), cx)
        .role(Role::Button)
        .w_full()
        .min_h(px(28.0))
        .px(px(6.0))
        .rounded(px(5.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .cursor_pointer()
        .hover(move |row| row.bg(c.surface_hover))
        .child(icon(glyph, px(14.0), c.ink_2))
        .child(div().flex_1().min_w_0().truncate().child(label.to_owned()))
}

impl Rooms {
    pub(super) fn tool(
        &self,
        id: impl Into<ElementId>,
        glyph: Lucide,
        label: &'static str,
        cx: &App,
    ) -> Stateful<Div> {
        let c = colors(cx);
        btn::tabbable(div().id(id), cx)
            .role(Role::Button)
            .aria_label(label)
            .when(self.overlay.is_none(), |button| {
                button.tooltip(tip(label.to_owned()))
            })
            .flex_none()
            .size(px(23.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(move |button| button.bg(c.surface_hover))
            .child(icon(glyph, px(14.0), c.ink_2))
    }
}
