use std::sync::Arc;

use api::graph::chat::{ChatEntry, ChatItem, ChatOrigin};
use gpui::{
    App, Context, Div, Entity, ParentElement, Stateful, StatefulInteractiveElement, Styled,
    Subscription, Task, Window,
};

use super::rows::{action_button, flow, meta};
use super::{Stream, tr};
use crate::scale::px;
use crate::strings::Str;
use crate::views::field::{self, Field, FieldEvent};
use crate::views::icons::Lucide;
use crate::views::ui;
use crate::views::widgets::btn;

pub(super) struct Draft {
    item: Arc<ChatItem>,
    field: Entity<Field>,
    _sub: Subscription,
}

enum Change {
    Edit(String),
    Withdraw,
}

impl Stream {
    pub(super) fn prune_drafts(&mut self, cx: &App) {
        let items = self.data.read(cx).items();
        self.drafts.retain(|_, draft| {
            items
                .binary_search_by_key(&draft.item.cursor, |item| item.cursor)
                .ok()
                .and_then(|index| items.get(index))
                .is_some_and(|item| matches!(&item.entry, ChatEntry::Message { message } if message.editable))
        });
        if let Some(group) = self.withdrawing
            && !items
                .iter()
                .any(|item| self.editable_group(item, cx) == Some(group))
        {
            self.withdrawing = None;
        }
    }

    fn submit_edit(&mut self, group: u32, cx: &mut Context<Self>) {
        let Some(draft) = self.drafts.get(&group) else {
            return;
        };
        let field = draft.field.read(cx);
        if field.composing() || field.is_blank() {
            return;
        }
        self.change_message(
            draft.item.clone(),
            group,
            Change::Edit(field.text().to_owned()),
            cx,
        );
    }

    fn editable_group(&self, item: &ChatItem, cx: &App) -> Option<u32> {
        let ChatEntry::Message { message } = &item.entry else {
            return None;
        };
        if item.cursor.id >= 0
            && message.editable
            && message.origin == ChatOrigin::Human
            && self.data.read(cx).room().is_some_and(|room| !room.archived)
        {
            message.group
        } else {
            None
        }
    }

    pub(super) fn message_editor(
        &self,
        item: &Arc<ChatItem>,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let group = self.editable_group(item, cx)?;
        if self.busy.contains(&group) {
            return Some(meta(tr(cx, "메시지를 바꾸는 중", "Updating message"), cx));
        }
        if self.withdrawing == Some(group) {
            let item = item.clone();
            let c = ui::colors(cx);
            return Some(
                flow()
                    .child(meta(
                        tr(
                            cx,
                            "이 메시지의 전송을 취소할까요?",
                            "Cancel sending this message?",
                        ),
                        cx,
                    ))
                    .child(
                        btn::filled("stream-withdraw-confirm", c.err, c.err_ink, cx)
                            .child(tr(cx, "전송 취소", "Cancel send"))
                            .on_click(cx.listener(move |stream, _, _, cx| {
                                stream.withdrawing = None;
                                stream.change_message(item.clone(), group, Change::Withdraw, cx);
                            })),
                    )
                    .child(
                        btn::ghost("stream-withdraw-keep", cx)
                            .child(tr(cx, "메시지 유지", "Keep message"))
                            .on_click(cx.listener(|stream, _, _, cx| {
                                stream.withdrawing = None;
                                cx.notify();
                            })),
                    ),
            );
        }
        if let Some(draft) = self.drafts.get(&group) {
            let field = draft.field.clone();
            return Some(
                flow()
                    .w_full()
                    .child(gpui::div().flex_1().min_w(px(130.0)).child(field))
                    .child(
                        btn::button("stream-save-edit", cx)
                            .child(tr(cx, "저장", "Save"))
                            .on_click(
                                cx.listener(move |stream, _, _, cx| stream.submit_edit(group, cx)),
                            ),
                    )
                    .child(
                        btn::ghost("stream-cancel-edit", cx)
                            .child(ui::say(cx, Str::CommonCancel))
                            .on_click(cx.listener(move |stream, _, _, cx| {
                                stream.drafts.remove(&group);
                                cx.notify();
                            })),
                    ),
            );
        }
        None
    }

    pub(super) fn message_buttons(
        &self,
        item: &Arc<ChatItem>,
        cx: &mut Context<Self>,
    ) -> Option<[Stateful<Div>; 2]> {
        let group = self.editable_group(item, cx)?;
        if self.busy.contains(&group)
            || self.drafts.contains_key(&group)
            || self.withdrawing == Some(group)
        {
            return None;
        }
        let edit = item.clone();
        Some([
            action_button("stream-edit", Lucide::Pencil, tr(cx, "고치기", "Edit"), cx).on_click(
                cx.listener(move |stream, _, window, cx| {
                    stream.begin_edit(edit.clone(), group, window, cx)
                }),
            ),
            action_button(
                "stream-withdraw",
                Lucide::Trash2,
                tr(cx, "전송 취소", "Cancel send"),
                cx,
            )
            .on_click(cx.listener(move |stream, _, _, cx| {
                stream.withdrawing = Some(group);
                cx.notify();
            })),
        ])
    }

    fn begin_edit(
        &mut self,
        item: Arc<ChatItem>,
        group: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ChatEntry::Message { message } = &item.entry else {
            return;
        };
        if self.busy.contains(&group)
            || self
                .current(&item, cx)
                .and_then(|current| self.editable_group(current, cx))
                != Some(group)
        {
            return;
        }
        let hint = tr(cx, "메시지 고치기", "Edit message");
        let field = field::boxed(hint, false, true, cx);
        field.update(cx, |field, cx| field.set_text_at_end(&message.text, cx));
        let sub = cx.subscribe(&field, move |stream, _, event, cx| match event {
            FieldEvent::Enter => stream.submit_edit(group, cx),
            FieldEvent::Escape => {
                stream.drafts.remove(&group);
                cx.notify();
            }
            _ => {}
        });
        self.drafts.insert(
            group,
            Draft {
                item,
                field: field.clone(),
                _sub: sub,
            },
        );
        field.read(cx).handle().focus(window, cx);
        cx.notify();
    }

    pub(super) fn current<'a>(&self, item: &ChatItem, cx: &'a App) -> Option<&'a ChatItem> {
        let index = self
            .rows
            .visible
            .binary_search_by_key(&item.cursor, |row| row.cursor)
            .ok()?;
        self.data
            .read(cx)
            .items()
            .get(self.rows.visible[index].item()?)
            .map(Arc::as_ref)
    }

    fn change_message(
        &mut self,
        item: Arc<ChatItem>,
        group: u32,
        change: Change,
        cx: &mut Context<Self>,
    ) {
        if self.busy.contains(&group) {
            return;
        }
        let data = self.data.read(cx);
        let editable = data.room().is_some_and(|room| !room.archived)
            && self.current(&item, cx).is_some_and(|current| {
                matches!((&item.entry, &current.entry), (
                    ChatEntry::Message { message: original },
                    ChatEntry::Message { message },
                ) if message.group == Some(group) && message.editable && message.origin == ChatOrigin::Human && message.text == original.text)
            });
        if !editable {
            self.drafts.remove(&group);
            self.show_result(
                Err(tr(
                    cx,
                    "메시지가 바뀌었습니다. 새 상태를 확인해 주세요.",
                    "The message changed. Check its new state.",
                )
                .to_owned()),
                cx,
            );
            return;
        }
        let (task, success) = match change {
            Change::Edit(text) => {
                let task = self.data.update(cx, |data, cx| data.edit(group, text, cx));
                let task = cx.spawn(async move |_, _| task.await.map(|_| ()));
                (task, tr(cx, "메시지를 고쳤습니다", "Message updated"))
            }
            Change::Withdraw => {
                let task = self.data.update(cx, |data, cx| data.withdraw(group, cx));
                (task, tr(cx, "전송을 취소했습니다", "Send canceled"))
            }
        };
        self.finish_message_change(group, task, success, cx);
    }

    fn finish_message_change(
        &mut self,
        group: u32,
        task: Task<Result<(), String>>,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.busy.insert(group);
        cx.notify();
        let room = self.room.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |stream, cx| {
                stream.busy.remove(&group);
                if stream.room == room {
                    let result = result.map(|()| {
                        stream.drafts.remove(&group);
                        success.to_owned()
                    });
                    stream.show_result(result, cx);
                }
                stream.data.update(cx, super::ChatData::refresh);
                cx.notify();
            });
        })
        .detach();
    }
}
