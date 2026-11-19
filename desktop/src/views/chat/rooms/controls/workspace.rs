use api::graph::chat::ChatRoomKind;
use api::graph::{Place, Status};
use api::ids::RepoId;
use api::repos::RepoView;
use gpui::prelude::FluentBuilder;
use gpui::{
    ClickEvent, Context, Div, ElementId, InteractiveElement, ParentElement, Pixels, Point,
    Stateful, StatefulInteractiveElement, Styled, Window, div,
};

use crate::link;
use crate::scale::px;
use crate::views::cardlink;
use crate::views::icons::{Lucide, icon};
use crate::views::shell::OpenSettings;
use crate::views::tooltip::tip;
use crate::views::ui::colors;
use crate::views::widgets::btn;

use super::{
    Index, Overlay, Panel, Rooms, below, calls, chevron, note, option, place_of, tool, work_in,
    work_label,
};
use crate::views::chat::rooms::badge;

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
            .map_or("모든 작업공간", |repo| repo.name.as_str());
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
                    .aria_label("작업공간")
                    .child(icon(Lucide::Folder, px(14.0), c.ink_2))
                    .child(div().flex_1().min_w_0().truncate().child(title.to_owned()))
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
            .child(
                tool("chat-settings", Lucide::Settings, "설정", cx)
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            )
    }

    pub(in crate::views::chat::rooms) fn new_control(&self, cx: &Context<Self>) -> Stateful<Div> {
        let c = colors(cx);
        btn::button("new-chat", cx)
            .track_focus(&self.new_room)
            .child(icon(Lucide::Plus, px(13.0), c.ink))
            .child("새로 만들기")
            .child(icon(Lucide::ChevronDown, px(12.0), c.ink_2))
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                this.open_panel(Panel::New, below(event), window, cx);
            }))
    }

    pub(in crate::views::chat::rooms) fn empty_workspace(&self, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .items_start()
            .mx(px(12.0))
            .my(px(8.0))
            .gap(px(5.0))
            .child(note(
                "작업공간을 추가하거나 저장소 없이 에이전트를 띄울 수 있습니다.",
                cx,
            ))
            .child(self.scratch_button(cx))
            .child(
                btn::button("chat-empty-add-workspace", cx)
                    .child("작업공간 추가")
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        this.open_add_workspace(below(event), window, cx);
                    })),
            )
    }

    fn scratch_button(&self, cx: &Context<Self>) -> Stateful<Div> {
        let button = btn::button("chat-start-scratch", cx).child(if self.starting_scratch {
            "작업공간을 준비합니다…"
        } else {
            "저장소 없이 시작"
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
        self.open_panel(Panel::AgentPlace, at, window, cx);
        self.starting_scratch = true;
        let store = self.store(cx);
        let task = store.read(cx).call(cx, |base| {
            link::post::<RepoView>(base, "/api/repos/scratch", None)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.starting_scratch = false;
                if result.is_ok() {
                    this.store(cx).update(cx, |store, cx| store.reload(cx));
                }
                cx.notify();
                match result {
                    Ok(repo)
                        if matches!(
                            this.overlay,
                            Some(Overlay::Panel {
                                panel: Panel::AgentPlace,
                                ..
                            })
                        ) =>
                    {
                        let place = Place {
                            repo: repo.id,
                            worktree: repo.path,
                        };
                        this.open_agent(place, "저장소 없는 작업공간".to_owned(), at, window, cx);
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
                option("chat-workspaces-all", Lucide::Workflow, "모든 작업공간", cx)
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
                let git = repo.git;
                let status = format!("에이전트 {total} · 일하는 중 {working}");
                let kind = if git {
                    format!("워크트리 {}", repo.worktrees.len())
                } else {
                    "git 없음".to_owned()
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
                                row.child(badge(format!("내 차례 {asked}"), c.warn, c.warn_ink))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(3.0))
                            .when(git, |row| {
                                let create = btn::ghost(("chat-new-worktree-in", id.0), cx)
                                    .child("새 워크트리");
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
                                    btn::inert(create)
                                        .opacity(0.55)
                                        .tooltip(tip("첫 커밋이 필요합니다.".to_owned()))
                                };
                                row.child(create)
                            })
                            .child(
                                btn::ghost(("chat-remove-workspace", id.0), cx)
                                    .child("목록에서 빼기")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_workspace(id, cx)
                                    })),
                            ),
                    )
            }))
            .child(
                option("chat-add-workspace", Lucide::Plus, "작업공간 추가", cx).on_click(
                    cx.listener(|this, event: &ClickEvent, window, cx| {
                        this.open_add_workspace(below(event), window, cx);
                    }),
                ),
            )
    }

    fn remove_workspace(&mut self, repo: RepoId, cx: &mut Context<Self>) {
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
                option("chat-new-agent", Lucide::Terminal, "새 에이전트", cx).on_click(
                    cx.listener(move |this, _, window, cx| this.new_agent(at, window, cx)),
                ),
            )
            .child(
                option("chat-new-room-option", Lucide::MessageCircle, "새 방", cx).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.close_overlay(window, cx);
                        this.create.update(cx, |create, cx| create.open(window, cx));
                    }),
                ),
            )
            .when(has_git, |menu| {
                menu.child(
                    option(
                        "chat-new-worktree",
                        Lucide::GitBranchPlus,
                        "새 워크트리",
                        cx,
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.new_worktree(at, window, cx)),
                    ),
                )
            })
            .child(
                option("chat-new-workspace", Lucide::Folder, "작업공간 추가", cx).on_click(
                    cx.listener(move |this, _, window, cx| this.open_add_workspace(at, window, cx)),
                ),
            )
    }

    fn new_agent(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
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

    fn scoped_repos<'a>(&self, index: &Index<'a>) -> Vec<&'a RepoView> {
        index
            .repos
            .iter()
            .filter(|repo| self.workspace.is_none_or(|id| repo.id == id))
            .collect()
    }

    pub(super) fn agent_places(&self, index: &Index<'_>, cx: &Context<Self>) -> Div {
        if index.repos.is_empty() || self.starting_scratch {
            return div().p(px(3.0)).child(self.scratch_button(cx));
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
                        btn::inert(row).opacity(0.6).child("첫 커밋 필요")
                    }
                }),
        )
    }
}
