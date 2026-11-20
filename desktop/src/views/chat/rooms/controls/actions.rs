use api::graph::chat::{ChatRoom, ChatRoomKind, SaveChatRoom};
use api::graph::{Place, RelaunchOffer};
use api::ids::NodeId;
use api::repos::{RepoView, Worktree};
use gpui::prelude::FluentBuilder;
use gpui::{
    ClickEvent, Context, Div, Entity, InteractiveElement, KeyDownEvent, ParentElement, Pixels,
    Point, StatefulInteractiveElement, Styled, Window, div, point,
};

use crate::link;
use crate::problem::Failure;
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::cardlink;
use crate::views::field::{self, Field, FieldEvent};
use crate::views::icons::Lucide;
use crate::views::ui::{colors, say};
use crate::views::widgets::btn;

use super::{
    Index, MENU_WIDTH, Overlay, Panel, Removal, Rename, Rooms, below, calls, note, option, words,
    work_in, work_label,
};

impl Rooms {
    pub(in crate::views::chat) fn room_actions(
        &self,
        room: &ChatRoom,
        cx: &Context<Self>,
    ) -> Option<gpui::Stateful<Div>> {
        if room.archived
            && !matches!(room.kind, ChatRoomKind::Agent { node }
                if self.store(cx).read(cx).snapshot().is_some_and(|snapshot| snapshot.nodes.iter().any(|agent| agent.id == node)))
        {
            return None;
        }
        let id = room.id.clone();
        let mut button = self.tool(
            "chat-room-menu",
            Lucide::Ellipsis,
            words(cx, "방 메뉴", "Room menu"),
            cx,
        );
        if let ChatRoomKind::Agent { node } = room.kind {
            button = button.on_key_down(cx.listener(move |this, event, window, cx| {
                this.agent_rename_key(node, event, window, cx);
            }));
        }
        Some(
            button.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.open_panel(Panel::Room(id.clone()), below(event), window, cx);
            })),
        )
    }

    pub(in crate::views::chat::rooms) fn work_actions(
        &self,
        room: &ChatRoom,
        place: &Place,
        tree: &Worktree,
        repo: &RepoView,
        cx: &Context<Self>,
    ) -> Div {
        let label = work_label(repo, tree);
        let work = place.clone();
        let add = self
            .tool(
                "chat-work-add",
                Lucide::Plus,
                words(cx, "새 에이전트", "New agent"),
                cx,
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.open_agent(work.clone(), label.clone(), below(event), window, cx);
            }));
        div()
            .flex()
            .flex_none()
            .gap(px(2.0))
            .child(super::super::reveal_action(add))
            .children(self.room_actions(room, cx).map(super::super::reveal_action))
    }

    pub(in crate::views::chat) fn agent_rename_key(
        &mut self,
        node: NodeId,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        if stroke.key == "f2"
            && stroke.modifiers == gpui::Modifiers::default()
            && self.overlay.is_none()
        {
            self.rename_agent(node, point(px(240.0), px(90.0)), window, cx);
            cx.stop_propagation();
        }
    }

    fn rename_agent(
        &mut self,
        node: NodeId,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = Index::new(self.data.read(cx), cx);
        if let Some(node) = index.node(node) {
            let name = node.name.clone();
            self.open_rename(Rename::Agent(node.id), name, at, window, cx);
        }
    }

    fn rename_work(
        &mut self,
        place: Place,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = Index::new(self.data.read(cx), cx);
        if let Some((repo, tree)) = work_in(index.repos, &place)
            && repo.git
            && !tree.original
            && let Some(name) = tree.branch.clone()
        {
            self.open_rename(Rename::Work(place), name, at, window, cx);
        }
    }

    fn open_rename(
        &mut self,
        target: Rename,
        name: String,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = match target {
            Rename::Room { .. } => Str::RoomName,
            _ => Str::RenameLabel,
        };
        let field = field::new(label, cx);
        field.update(cx, |field, cx| field.set_text(&name, cx));
        let keys =
            cx.subscribe_in(
                &field,
                window,
                |this, _, event: &FieldEvent, window, cx| match event {
                    FieldEvent::Enter => this.save_name(window, cx),
                    FieldEvent::Escape => this.close_overlay(window, cx),
                    FieldEvent::Changed => {
                        this.action_error = None;
                        cx.notify();
                    }
                    FieldEvent::Step(_) => {}
                },
            );
        let focus = field.read(cx).handle();
        self.open_panel(
            Panel::Rename {
                target,
                field,
                saving: false,
                _keys: keys,
            },
            at,
            window,
            cx,
        );
        window.focus(&focus, cx);
    }

    fn save_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Panel {
            panel:
                Panel::Rename {
                    target,
                    field,
                    saving,
                    ..
                },
            ..
        }) = &mut self.overlay
        else {
            return;
        };
        if *saving {
            return;
        }
        let name = field.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.action_error = Some(words(cx, "이름을 써 주세요.", "Enter a name.").to_owned());
            cx.notify();
            return;
        }
        *saving = true;
        let mut target = target.clone();
        if let Rename::Room { id, members } = &mut target
            && let Some(room) = self
                .data
                .read(cx)
                .rooms()
                .iter()
                .find(|room| room.id == *id)
        {
            members.clone_from(&room.members);
        }
        let refresh_room = matches!(target, Rename::Room { .. });
        let field = field.clone();
        field.update(cx, |field, cx| field.set_locked(true, cx));
        let store = self.store(cx);
        let task = store.read(cx).call(cx, move |base| match target {
            Rename::Agent(node) => cardlink::rename(base, node.0, &name),
            Rename::Work(place) => {
                calls::rename_worktree(base, place.repo.0, &place.worktree, &name)
            }
            Rename::Room { id, members } => link::put(
                base,
                &format!("/api/chat/rooms/{}", link::escape(&id)),
                &SaveChatRoom { name, members },
            ),
        });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if result.is_ok() {
                    if refresh_room {
                        this.data.update(cx, |data, cx| data.refresh(cx));
                    } else {
                        this.store(cx).update(cx, |store, cx| store.reload(cx));
                    }
                }
                let Some(Overlay::Panel {
                    panel:
                        Panel::Rename {
                            field: current,
                            saving,
                            ..
                        },
                    ..
                }) = &mut this.overlay
                else {
                    return;
                };
                if *current != field {
                    return;
                }
                match result {
                    Ok(()) => this.close_overlay(window, cx),
                    Err(error) => {
                        *saving = false;
                        field.update(cx, |field, cx| field.set_locked(false, cx));
                        this.action_error = Some(error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn apply(
        &mut self,
        work: impl FnOnce(&str) -> Result<(), Failure> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.action_error = None;
        let store = self.store(cx);
        cardlink::run(&store, cx, work, |this, result, cx| {
            match result {
                Ok(()) => this.store(cx).update(cx, |store, cx| store.reload(cx)),
                Err(error) => this.action_error = Some(error),
            }
            cx.notify();
        })
        .detach();
    }
    pub(super) fn agent_menu(
        &self,
        index: &Index<'_>,
        id: NodeId,
        at: Point<Pixels>,
        cx: &Context<Self>,
    ) -> Div {
        let Some(node) = index.node(id) else {
            return note(
                words(
                    cx,
                    "이 에이전트는 방을 나갔습니다.",
                    "This agent has left the room.",
                ),
                cx,
            );
        };
        let name = node.name.clone();
        let marked = node.bookmark.is_some();
        let relaunch = node.relaunch;
        let c = colors(cx);
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                option(
                    "chat-agent-rename",
                    Lucide::Pencil,
                    words(cx, "이름 바꾸기 (F2)", "Rename (F2)"),
                    cx,
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| this.rename_agent(id, at, window, cx)),
                ),
            )
            .children(relaunch.map(|offer| {
                let row = option(
                    "chat-agent-relaunch",
                    Lucide::RotateCw,
                    say(cx, Str::NodeRelaunch),
                    cx,
                );
                if offer == RelaunchOffer::Ready {
                    row.on_click(cx.listener(move |this, _, window, cx| {
                        this.close_overlay(window, cx);
                        this.apply(move |base| cardlink::relaunch(base, id.0), cx);
                    }))
                } else {
                    btn::inert(row)
                        .opacity(0.6)
                        .child(words(cx, "대기 중", "Pending"))
                }
            }))
            .child(
                option(
                    "chat-agent-bookmark",
                    Lucide::Bookmark,
                    if marked {
                        say(cx, Str::BookmarkRemove)
                    } else {
                        say(cx, Str::BookmarkAdd)
                    },
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.close_overlay(window, cx);
                    this.apply(move |base| link::set_bookmark(base, id.0, !marked), cx);
                })),
            )
            .when(index.nodes.len() > 1, |menu| {
                menu.child(
                    option(
                        "chat-agent-parent",
                        Lucide::CornerLeftUp,
                        words(cx, "상위 에이전트 바꾸기", "Change parent"),
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_panel(Panel::Parent(id), at, window, cx);
                    })),
                )
            })
            .child(
                option(
                    "chat-agent-remove",
                    Lucide::Trash2,
                    say(cx, Str::NodeRemove),
                    cx,
                )
                .text_color(c.err)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_panel(
                        Panel::Confirm(Removal::Agent(id), name.clone()),
                        at,
                        window,
                        cx,
                    );
                })),
            )
    }

    pub(super) fn parent_menu(&self, index: &Index<'_>, id: NodeId, cx: &Context<Self>) -> Div {
        let Some(node) = index.node(id) else {
            return note(
                words(
                    cx,
                    "이 에이전트는 방을 나갔습니다.",
                    "This agent has left the room.",
                ),
                cx,
            );
        };
        let current = node.parent;
        let parent_name = current
            .and_then(|id| index.node(id))
            .map_or(say(cx, Str::FindNone), |node| node.name.as_str());
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(note(
                match cx.global::<Lang>() {
                    Lang::Ko => format!("현재 부모 · {parent_name}"),
                    Lang::En => format!("Current parent · {parent_name}"),
                },
                cx,
            ))
            .when(current.is_some(), |body| {
                body.child(
                    option(
                        "chat-agent-detach",
                        Lucide::Minus,
                        say(cx, Str::EventsNoParent),
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_overlay(window, cx);
                        this.apply(
                            move |base| {
                                link::post::<()>(base, &format!("/api/nodes/{id}/detach"), None)
                            },
                            cx,
                        );
                    })),
                )
            })
            .children(
                index
                    .nodes
                    .iter()
                    .filter(|node| {
                        node.id != id
                            && Some(node.id) != current
                            && !std::iter::successors(node.parent, |&parent| {
                                index.node(parent).and_then(|node| node.parent)
                            })
                            .any(|parent| parent == id)
                    })
                    .map(|parent| {
                        let new_parent = parent.id;
                        let repo = index.repo(parent.place.repo);
                        let label = repo.map_or_else(
                            || parent.name.clone(),
                            |repo| format!("{} · {}", parent.name, repo.name),
                        );
                        option(
                            ("chat-parent-choice", new_parent.0),
                            Lucide::Terminal,
                            &label,
                            cx,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.close_overlay(window, cx);
                                this.apply(move |base| link::adopt(base, id.0, new_parent.0), cx);
                            },
                        ))
                    }),
            )
    }

    pub(super) fn work_menu(
        &self,
        index: &Index<'_>,
        place: &Place,
        at: Point<Pixels>,
        cx: &Context<Self>,
    ) -> Div {
        if !work_in(index.repos, place).is_some_and(|(repo, tree)| repo.git && !tree.original) {
            return div();
        }
        let work = place.clone();
        let delete = place.clone();
        let c = colors(cx);
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                option(
                    "chat-worktree-rename",
                    Lucide::Pencil,
                    say(cx, Str::RenameAction),
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.rename_work(work.clone(), at, window, cx)
                })),
            )
            .child(
                option(
                    "chat-worktree-delete",
                    Lucide::Trash2,
                    say(cx, Str::DeleteWorktreeAction),
                    cx,
                )
                .text_color(c.err)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_delete_worktree(delete.clone(), at, window, cx)
                })),
            )
    }

    pub(super) fn custom_menu(&self, room: &ChatRoom, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        let rename = Rename::Room {
            id: room.id.clone(),
            members: room.members.clone(),
        };
        let original_name = room.name.clone();
        let members = room.clone();
        let id = room.id.clone();
        let name = room.name.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                option(
                    "chat-custom-rename",
                    Lucide::Pencil,
                    say(cx, Str::RenameAction),
                    cx,
                )
                .on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        this.open_rename(
                            rename.clone(),
                            original_name.clone(),
                            below(event),
                            window,
                            cx,
                        );
                    },
                )),
            )
            .child(
                option(
                    "chat-custom-members",
                    Lucide::Users,
                    words(cx, "구성원 편집", "Edit members"),
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.close_overlay(window, cx);
                    this.create
                        .update(cx, |editor, cx| editor.edit_members(&members, window, cx));
                })),
            )
            .child(
                option(
                    "chat-custom-delete",
                    Lucide::Trash2,
                    words(cx, "방 삭제", "Delete room"),
                    cx,
                )
                .text_color(c.err)
                .on_click(cx.listener(
                    move |this, event: &ClickEvent, window, cx| {
                        this.open_panel(
                            Panel::Confirm(Removal::Room(id.clone()), name.clone()),
                            below(event),
                            window,
                            cx,
                        );
                    },
                )),
            )
    }

    pub(super) fn removal_prompt(
        &self,
        removal: &Removal,
        name: &str,
        cx: &Context<Self>,
    ) -> (&'static str, f32, Div) {
        let (title, accept) = match removal {
            Removal::Agent(_) => (
                words(cx, "에이전트를 삭제할까요?", "Delete this agent?"),
                say(cx, Str::NodeRemove),
            ),
            Removal::Workspace(_) => (
                words(
                    cx,
                    "작업공간을 목록에서 뺄까요?",
                    "Remove this workspace from the list?",
                ),
                words(cx, "작업공간 빼기", "Remove workspace"),
            ),
            Removal::Room(_) => (
                words(cx, "이 방을 삭제할까요?", "Delete this room?"),
                words(cx, "방 삭제", "Delete room"),
            ),
        };
        let removal = removal.clone();
        let c = colors(cx);
        let body = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(note(name.to_owned(), cx))
            .when(matches!(removal, Removal::Room(_)), |body| {
                body.child(note(
                    words(
                        cx,
                        "대화 기록은 보관합니다.",
                        "Chat history stays available.",
                    ),
                    cx,
                ))
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(5.0))
                    .child(
                        btn::ghost("chat-remove-cancel", cx)
                            .child(say(cx, Str::CommonCancel))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
                            ),
                    )
                    .child(
                        btn::button("chat-remove-confirm", cx)
                            .text_color(c.err)
                            .child(accept)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close_overlay(window, cx);
                                match &removal {
                                    Removal::Agent(id) => {
                                        let id = *id;
                                        this.apply(
                                            move |base| cardlink::remove_node(base, id.0),
                                            cx,
                                        );
                                    }
                                    Removal::Workspace(repo) => this.remove_workspace(*repo, cx),
                                    Removal::Room(id) => this.delete_room(id.clone(), cx),
                                }
                            })),
                    ),
            );
        (title, MENU_WIDTH, body)
    }

    fn delete_room(&mut self, id: String, cx: &mut Context<Self>) {
        let path = format!("/api/chat/rooms/{}", link::escape(&id));
        cardlink::run(
            &self.store(cx),
            cx,
            move |base| link::delete(base, &path),
            move |this, result, cx| {
                match result {
                    Ok(()) => this.data.update(cx, |data, cx| data.refresh(cx)),
                    Err(error) => this.action_error = Some(error),
                }
                cx.notify();
            },
        )
        .detach();
    }

    pub(super) fn copy_option(
        &self,
        room: &ChatRoom,
        cx: &Context<Self>,
    ) -> Option<gpui::Stateful<Div>> {
        if room.archived {
            return None;
        }
        let graph = self.store(cx);
        let base = graph.read(cx).base()?;
        let url = format!("{}{}", base.trim_end_matches('/'), room.outside);
        Some(
            option(
                "chat-copy-outside",
                if self.copied_address {
                    Lucide::Check
                } else {
                    Lucide::Copy
                },
                if self.copied_address {
                    say(cx, Str::RoomCopied)
                } else {
                    words(cx, "외부 주소 복사", "Copy outside address")
                },
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
                this.copied_address = true;
                cx.notify();
            })),
        )
    }

    pub(super) fn rename_form(
        &self,
        field: &Entity<Field>,
        saving: bool,
        cx: &Context<Self>,
    ) -> Div {
        let save = btn::button("chat-name-save", cx).child(say(cx, Str::SettingsRolesSave));
        div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .child(field.clone())
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(4.0))
                    .child(
                        btn::ghost("chat-name-cancel", cx)
                            .child(say(cx, Str::CommonCancel))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
                            ),
                    )
                    .child(if saving {
                        btn::inert(save).opacity(0.6)
                    } else {
                        save.on_click(cx.listener(|this, _, window, cx| this.save_name(window, cx)))
                    }),
            )
    }
}
