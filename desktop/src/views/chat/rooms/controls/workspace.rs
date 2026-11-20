mod welcome;

use api::graph::chat::ChatRoomKind;
use api::graph::{Place, Status};
use api::ids::RepoId;
use api::repos::{PlaceView, RepoView};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClickEvent, Context, Div, ElementId, Entity, FontWeight, InteractiveElement,
    ParentElement, Pixels, Point, Stateful, StatefulInteractiveElement, Styled, Task, WeakEntity,
    Window, div,
};

use crate::link;
use crate::scale::px;
use crate::store::Store;
use crate::strings::{Lang, Str};
use crate::views::cardlink;
use crate::views::icons::{Lucide, icon};
use crate::views::tooltip::tip;
use crate::views::ui::{SMALL, colors, say};
use crate::views::widgets::btn;
use crate::views::workspace::{AddRepo, AddRepoEvent};
use welcome::{Slot, Welcome};

use super::{
    Index, Overlay, Panel, Removal, Rooms, below, calls, chevron, option, place_of, words, work_in,
    work_label,
};
use crate::views::chat::rooms::{RoomsEvent, badge};

impl Rooms {
    pub(in crate::views::chat::rooms) fn workspace_controls(
        &self,
        index: &Index<'_>,
        cx: &Context<Self>,
    ) -> Div {
        let c = colors(cx);
        let title = self
            .workspace
            .and_then(|repo| index.repo(repo))
            .map_or(say(cx, Str::GraphScopeAll), |repo| repo.name.as_str());
        div()
            .flex()
            .flex_none()
            .items_center()
            .mx(px(12.0))
            .mt(px(10.0))
            .gap(px(4.0))
            .child(
                btn::button("chat-workspaces", cx)
                    .flex_1()
                    .min_w_0()
                    .justify_start()
                    .aria_label(say(cx, Str::TreeRepos))
                    .h(px(30.0))
                    .child(icon(Lucide::Folder, px(14.0), c.ink_2))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title.to_owned()),
                    )
                    .child(chevron(
                        matches!(
                            self.overlay,
                            Some(Overlay::Panel {
                                panel: Panel::Workspace,
                                ..
                            })
                        ),
                        c.ink_2,
                    ))
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        this.open_panel(Panel::Workspace, below(event), window, cx);
                    })),
            )
    }

    pub(in crate::views::chat::rooms) fn new_control(&self, cx: &Context<Self>) -> Stateful<Div> {
        let c = colors(cx);
        btn::button("new-chat", cx)
            .track_focus(&self.new_room)
            .h(px(26.0))
            .px(px(8.0))
            .text_size(px(SMALL))
            .child(icon(Lucide::Plus, px(13.0), c.ink))
            .child(words(cx, "새로 만들기", "New"))
            .child(icon(Lucide::ChevronDown, px(12.0), c.ink_2))
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                this.open_panel(Panel::New, below(event), window, cx);
            }))
    }

    pub(in crate::views::chat) fn welcome(&mut self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(Slot::new(self.store(cx), cx.weak_entity()))
    }

    fn pick_welcome_folder(
        &mut self,
        welcome: WeakEntity<Welcome>,
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
        let closed = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &AddRepoEvent, window, cx| {
                this.close_overlay(window, cx);
                if let AddRepoEvent::Added(repo) = event {
                    let repo = repo.clone();
                    let _ = welcome.update(cx, |welcome, cx| welcome.set_folder(repo, cx));
                }
            },
        );
        self.show_dialog(view.into(), None, previous, closed, cx);
    }

    fn scratch_button(&self, button: Stateful<Div>, cx: &Context<Self>) -> Stateful<Div> {
        let button = button.child(if self.starting_scratch {
            words(cx, "작업공간을 준비합니다…", "Preparing the workspace…")
        } else {
            words(
                cx,
                "저장소 없이 바로 대화 시작",
                "Start a chat without a repository",
            )
        });
        if self.starting_scratch {
            btn::inert(button).opacity(0.6)
        } else {
            button.on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                this.start_scratch(below(event), window, cx);
            }))
        }
    }

    fn start_scratch(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if self.starting_scratch {
            return;
        }
        self.action_error = None;
        self.starting_scratch = true;
        cx.notify();
        cx.emit(RoomsEvent::ScratchChanged);
        let from_picker = self.overlay.is_some();
        let store = self.store(cx);
        let task = scratch(&store, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.starting_scratch = false;
                cx.emit(RoomsEvent::ScratchChanged);
                if result.is_ok() {
                    this.store(cx).update(cx, |store, cx| store.reload(cx));
                }
                cx.notify();
                match result {
                    Ok(repo)
                        if if from_picker {
                            matches!(
                                this.overlay,
                                Some(Overlay::Panel {
                                    panel: Panel::AgentPlace,
                                    ..
                                })
                            )
                        } else {
                            this.overlay.is_none()
                        } =>
                    {
                        this.agent_at_repo(&repo, at, window, cx);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        this.action_error = Some(error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(super) fn workspace_menu(&self, index: &Index<'_>, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        let (all, nodes) = (self.workspace.is_none(), index.nodes);
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                option(
                    "chat-workspaces-all",
                    Lucide::Workflow,
                    say(cx, Str::GraphScopeAll),
                    cx,
                )
                .when(all, |row| {
                    row.bg(c.accent_wash)
                        .child(icon(Lucide::Check, px(13.0), c.ink))
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.workspace = None;
                    this.close_overlay(window, cx);
                })),
            )
            .children(index.repos.iter().map(|repo| {
                let id = repo.id;
                let repo = repo.clone();
                let mut total = 0;
                let mut working = 0;
                let mut asked = 0;
                for node in nodes.iter().filter(|node| node.place.repo == id) {
                    total += 1;
                    working += usize::from(matches!(node.status, Status::Working { .. }));
                    asked += usize::from(matches!(node.status, Status::Human));
                }
                let agent_repo = repo.clone();
                let remove_name = repo.name.clone();
                let git = repo.git;
                let lang = *cx.global::<Lang>();
                let status = match lang {
                    Lang::Ko => format!("에이전트 {total} · 일하는 중 {working}"),
                    Lang::En => format!("Agents: {total} · Working: {working}"),
                };
                let kind = if git {
                    let count = repo.worktrees.len();
                    match lang {
                        Lang::Ko => format!("워크트리 {count}"),
                        Lang::En => format!("Worktrees: {count}"),
                    }
                } else {
                    say(cx, Str::WorktreePlain).to_owned()
                };
                let here = self.workspace == Some(id);
                div()
                    .id(("chat-workspace", id.0))
                    .p(px(3.0))
                    .rounded(px(6.0))
                    .when(here, |row| row.bg(c.accent_wash))
                    .child(
                        option(
                            ("chat-select-workspace", id.0),
                            Lucide::Folder,
                            &repo.name,
                            cx,
                        )
                        .when(here, |row| row.child(icon(Lucide::Check, px(13.0), c.ink)))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.workspace = Some(id);
                                this.close_overlay(window, cx);
                            },
                        )),
                    )
                    .child(
                        div()
                            .px(px(6.0))
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(c.ink_2)
                            .child(repo.shown_path.clone()),
                    )
                    .child(
                        div()
                            .px(px(6.0))
                            .flex()
                            .flex_wrap()
                            .gap(px(5.0))
                            .text_size(px(12.0))
                            .text_color(c.ink_2)
                            .child(format!("{kind} · {status}"))
                            .when(asked > 0, |row| {
                                row.child(badge(
                                    format!("{} {asked}", words(cx, "내 차례", "My turn")),
                                    c.warn,
                                    c.warn_ink,
                                ))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .flex_wrap()
                            .gap(px(3.0))
                            .child(
                                btn::ghost(("chat-new-agent-in", id.0), cx)
                                    .child(words(cx, "새 에이전트", "New agent"))
                                    .on_click(cx.listener(
                                        move |this, event: &ClickEvent, window, cx| {
                                            this.agent_at_repo(
                                                &agent_repo,
                                                below(event),
                                                window,
                                                cx,
                                            );
                                        },
                                    )),
                            )
                            .when(git, |row| {
                                let create = btn::ghost(("chat-new-worktree-in", id.0), cx)
                                    .child(say(cx, Str::RepoNewWorktree));
                                let create = if repo.has_commits {
                                    create.on_click(cx.listener(
                                        move |this, event: &ClickEvent, window, cx| {
                                            this.open_worktree(
                                                repo.clone(),
                                                below(event),
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                } else {
                                    btn::inert(create).opacity(0.55).tooltip(tip(say(
                                        cx,
                                        Str::RepoNoCommit,
                                    )
                                    .to_owned()))
                                };
                                row.child(create)
                            })
                            .child(
                                btn::ghost(("chat-remove-workspace", id.0), cx)
                                    .child(words(cx, "작업공간 빼기", "Remove workspace"))
                                    .on_click(cx.listener(
                                        move |this, event: &ClickEvent, window, cx| {
                                            this.open_panel(
                                                Panel::Confirm(
                                                    Removal::Workspace(id),
                                                    remove_name.clone(),
                                                ),
                                                below(event),
                                                window,
                                                cx,
                                            );
                                        },
                                    )),
                            ),
                    )
            }))
            .child(
                option(
                    "chat-add-workspace",
                    Lucide::Plus,
                    say(cx, Str::TreeAddRepo),
                    cx,
                )
                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                    this.open_add_workspace(below(event), window, cx);
                })),
            )
    }

    pub(super) fn remove_workspace(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        self.action_error = None;
        let store = self.store(cx);
        cardlink::run(
            &store,
            cx,
            move |base| calls::remove_repo(base, repo.0),
            move |this, result, cx| {
                match result {
                    Ok(()) => {
                        if this.workspace == Some(repo) {
                            this.workspace = None;
                        }
                        this.store(cx).update(cx, |store, cx| store.reload(cx));
                    }
                    Err(error) => this.action_error = Some(error),
                }
                cx.notify();
            },
        )
        .detach();
    }

    pub(super) fn new_menu(&self, index: &Index<'_>, at: Point<Pixels>, cx: &Context<Self>) -> Div {
        let eligible = self.workspace.and_then(|id| index.repo(id));
        let has_git =
            eligible.map_or_else(|| index.repos.iter().any(|repo| repo.git), |repo| repo.git);
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                option(
                    "chat-new-agent",
                    Lucide::Terminal,
                    words(cx, "새 에이전트", "New agent"),
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| this.new_agent(at, window, cx))),
            )
            .when(!index.nodes.is_empty(), |menu| {
                menu.child(
                    option(
                        "chat-new-room-option",
                        Lucide::MessageCircle,
                        words(cx, "새 방", "New room"),
                        cx,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_overlay(window, cx);
                        this.create.update(cx, |create, cx| create.open(window, cx));
                    })),
                )
            })
            .when(has_git, |menu| {
                menu.child(
                    option(
                        "chat-new-worktree",
                        Lucide::GitBranchPlus,
                        say(cx, Str::RepoNewWorktree),
                        cx,
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.new_worktree(at, window, cx)),
                    ),
                )
            })
            .child(
                option(
                    "chat-new-workspace",
                    Lucide::Folder,
                    say(cx, Str::TreeAddRepo),
                    cx,
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| this.open_add_workspace(at, window, cx)),
                ),
            )
    }

    pub(in crate::views::chat) fn new_agent(
        &mut self,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = Index::new(self.data.read(cx), cx);
        let place = self.data.read(cx).room().and_then(|room| match &room.kind {
            ChatRoomKind::Work { place } => Some(place),
            ChatRoomKind::Agent { node } => index.node(*node).map(|node| &node.place),
            _ => None,
        });
        let here = place
            .filter(|place| self.workspace.is_none_or(|id| place.repo == id))
            .and_then(|place| work_in(index.repos, place));
        let repos = self.scoped_repos(&index);
        let first = repos
            .iter()
            .flat_map(|repo| repo.worktrees.iter().map(move |tree| (*repo, tree)))
            .take(2)
            .collect::<Vec<_>>();
        let only = if let [(repo, tree)] = first.as_slice() {
            Some((*repo, *tree))
        } else {
            None
        };
        if let Some((repo, tree)) = here.or(only) {
            let place = place_of(repo, tree);
            let label = work_label(repo, tree);
            self.open_agent(place, label, at, window, cx);
        } else {
            self.open_panel(Panel::AgentPlace, at, window, cx);
        }
    }

    pub(super) fn agent_at_repo(
        &mut self,
        repo: &RepoView,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let place = Place {
            repo: repo.id,
            worktree: repo.path.clone(),
        };
        self.open_agent(place, repo.name.clone(), at, window, cx);
    }

    fn scoped_repos<'a>(&self, index: &Index<'a>) -> Vec<&'a RepoView> {
        index
            .repos
            .iter()
            .filter(|repo| self.workspace.is_none_or(|id| repo.id == id))
            .collect()
    }

    pub(super) fn agent_places(&self, index: &Index<'_>, cx: &Context<Self>) -> Div {
        if index.repos.is_empty() || self.starting_scratch {
            let button = btn::button("chat-scratch-picker", cx);
            return div().p(px(3.0)).child(self.scratch_button(button, cx));
        }
        div().flex().flex_col().gap(px(2.0)).children(
            self.scoped_repos(index).into_iter().flat_map(|repo| {
                repo.worktrees.iter().map(move |tree| {
                    let place = place_of(repo, tree);
                    let label = work_label(repo, tree);
                    let title = if repo.git {
                        format!("{} · {label}", repo.name)
                    } else {
                        label.clone()
                    };
                    option(
                        ElementId::Name(format!("agent-place-{}-{}", repo.id.0, tree.path).into()),
                        if repo.git {
                            Lucide::GitBranch
                        } else {
                            Lucide::Folder
                        },
                        &title,
                        cx,
                    )
                    .tooltip(tip(tree.path.clone()))
                    .on_click(cx.listener(
                        move |this, event: &ClickEvent, window, cx| {
                            this.open_agent(place.clone(), label.clone(), below(event), window, cx);
                        },
                    ))
                })
            }),
        )
    }

    fn new_worktree(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let index = Index::new(self.data.read(cx), cx);
        let repos: Vec<_> = self
            .scoped_repos(&index)
            .into_iter()
            .filter(|repo| repo.git)
            .collect();
        if let [repo] = repos.as_slice()
            && repo.has_commits
        {
            let repo = (**repo).clone();
            self.open_worktree(repo, at, window, cx);
        } else {
            self.open_panel(Panel::WorktreeRepo, at, window, cx);
        }
    }

    pub(super) fn worktree_repos(&self, index: &Index<'_>, cx: &Context<Self>) -> Div {
        div().flex().flex_col().gap(px(2.0)).children(
            self.scoped_repos(index)
                .into_iter()
                .filter(|repo| repo.git)
                .map(|repo| {
                    let row = option(
                        ("worktree-repo", repo.id.0),
                        Lucide::GitBranch,
                        &repo.name,
                        cx,
                    );
                    if repo.has_commits {
                        let repo = repo.clone();
                        row.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.open_worktree(repo.clone(), below(event), window, cx);
                        }))
                    } else {
                        btn::inert(row)
                            .opacity(0.6)
                            .child(say(cx, Str::RepoNoCommit))
                    }
                }),
        )
    }
}

pub(super) fn scratch(store: &Entity<Store>, cx: &App) -> Task<Result<RepoView, String>> {
    store.read(cx).call(cx, |base| {
        link::post::<RepoView>(base, "/api/repos/scratch", None)
    })
}
