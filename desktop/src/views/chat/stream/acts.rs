use std::sync::Arc;

use api::graph::chat::{ChatEntry, ChatItem, ChatOrigin};
use api::graph::waits::{InputField, Press, PressKeys};
use api::ids::WaitId;
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Target {
    Message(u32),
    Wait(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Input {
    Edit(u32),
    Answer(u32),
    Prompt(u32, Option<usize>),
    Screen(u32),
}

pub(super) struct Draft {
    item: Arc<ChatItem>,
    field: Entity<Field>,
    _sub: Subscription,
}

pub(super) enum WaitAction {
    Answer(String),
    Choose(usize),
    Write(Option<usize>, String),
    Press(Press),
    Confirm,
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
                .is_some_and(|item| match &item.entry {
                    ChatEntry::Message { message } => message.editable,
                    _ => same_input(&draft.item, item),
                })
        });
    }

    pub(super) fn wait_input(
        &mut self,
        key: Input,
        item: &Arc<ChatItem>,
        hint: &str,
        cx: &mut Context<Self>,
    ) -> Entity<Field> {
        if let Some(draft) = self.drafts.get(&key)
            && same_input(&draft.item, item)
        {
            return draft.field.clone();
        }
        let secret = matches!(key, Input::Prompt(_, None))
            && matches!(&item.entry, ChatEntry::Wait { wait, .. } if wait.wait.prompt.as_ref().and_then(|p| p.field) == Some(InputField::Secret));
        self.make_draft(key, item.clone(), hint, "", secret, cx)
    }

    fn make_draft(
        &mut self,
        key: Input,
        item: Arc<ChatItem>,
        hint: &str,
        text: &str,
        secret: bool,
        cx: &mut Context<Self>,
    ) -> Entity<Field> {
        let field = field::boxed(hint, secret, !secret, cx);
        if !text.is_empty() {
            field.update(cx, |field, cx| field.set_text_at_end(text, cx));
        }
        let sub = cx.subscribe(&field, move |stream, _, event, cx| match event {
            FieldEvent::Enter => stream.submit(key, cx),
            FieldEvent::Escape if matches!(key, Input::Edit(_)) => {
                stream.drafts.remove(&key);
                cx.notify();
            }
            _ => {}
        });
        self.drafts.insert(
            key,
            Draft {
                item,
                field: field.clone(),
                _sub: sub,
            },
        );
        field
    }

    pub(super) fn submit(&mut self, key: Input, cx: &mut Context<Self>) {
        let Some(draft) = self.drafts.get(&key) else {
            return;
        };
        let field = draft.field.read(cx);
        if field.composing()
            || (matches!(key, Input::Answer(_) | Input::Edit(_)) && field.is_blank())
        {
            return;
        }
        let text = field.text().to_owned();
        let item = draft.item.clone();
        let action = match key {
            Input::Edit(group) => return self.change_message(item, group, Change::Edit(text), cx),
            Input::Answer(_) => WaitAction::Answer(text),
            Input::Prompt(_, option) => WaitAction::Write(option, text),
            Input::Screen(_) => WaitAction::Press(Press::Text(text)),
        };
        self.act_on_wait(item, action, cx);
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
        let key = Input::Edit(group);
        if self.busy.contains(&Target::Message(group)) {
            return Some(meta(tr(cx, "메시지를 바꾸는 중", "Updating message"), cx));
        }
        if let Some(draft) = self.drafts.get(&key) {
            let field = draft.field.clone();
            return Some(
                flow()
                    .w_full()
                    .child(gpui::div().flex_1().min_w(px(130.0)).child(field))
                    .child(
                        btn::button("stream-save-edit", cx)
                            .child(tr(cx, "저장", "Save"))
                            .on_click(cx.listener(move |stream, _, _, cx| stream.submit(key, cx))),
                    )
                    .child(
                        btn::ghost("stream-cancel-edit", cx)
                            .child(ui::say(cx, Str::CommonCancel))
                            .on_click(cx.listener(move |stream, _, _, cx| {
                                stream.drafts.remove(&key);
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<[Stateful<Div>; 2]> {
        let group = self.editable_group(item, cx)?;
        if self.busy.contains(&Target::Message(group))
            || self.drafts.contains_key(&Input::Edit(group))
        {
            return None;
        }
        let edit = item.clone();
        let withdraw = item.clone();
        Some([
            action_button(
                "stream-edit",
                Lucide::Pencil,
                tr(cx, "고치기", "Edit"),
                window,
                cx,
            )
            .on_click(cx.listener(move |stream, _, window, cx| {
                stream.begin_edit(edit.clone(), group, window, cx)
            })),
            action_button(
                "stream-withdraw",
                Lucide::Trash2,
                tr(cx, "거두기", "Withdraw"),
                window,
                cx,
            )
            .on_click(cx.listener(move |stream, _, _, cx| {
                stream.change_message(withdraw.clone(), group, Change::Withdraw, cx)
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
        if self.busy.contains(&Target::Message(group))
            || self
                .current(&item, cx)
                .and_then(|current| self.editable_group(current, cx))
                != Some(group)
        {
            return;
        }
        let hint = tr(cx, "메시지 고치기", "Edit message");
        let field = self.make_draft(
            Input::Edit(group),
            item.clone(),
            hint,
            &message.text,
            false,
            cx,
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
        let key = Target::Message(group);
        if self.busy.contains(&key) {
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
            self.drafts.remove(&Input::Edit(group));
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
                (task, tr(cx, "메시지를 거뒀습니다", "Message withdrawn"))
            }
        };
        self.run(key, task, success, cx);
    }

    pub(super) fn wait_busy(&self, wait: WaitId, cx: &App) -> bool {
        self.data.read(cx).graph().read(cx).is_busy(wait.0)
            || self.busy.contains(&Target::Wait(wait.0))
    }

    pub(super) fn act_on_wait(
        &mut self,
        item: Arc<ChatItem>,
        action: WaitAction,
        cx: &mut Context<Self>,
    ) {
        let ChatEntry::Wait {
            wait, closed: None, ..
        } = &item.entry
        else {
            return;
        };
        let id = wait.wait.id;
        if self.wait_busy(id, cx) || self.data.read(cx).room().is_none_or(|room| room.archived) {
            return;
        }
        if !self
            .current(&item, cx)
            .is_some_and(|current| same_input(&item, current))
        {
            self.show_result(
                Err(tr(
                    cx,
                    "질문이 바뀌었습니다. 새 상태를 확인해 주세요.",
                    "The question changed. Check its new state.",
                )
                .to_owned()),
                cx,
            );
            return;
        }
        let graph = self.data.read(cx).graph().clone();
        let success = match &action {
            WaitAction::Confirm => tr(cx, "확인했습니다", "Confirmed"),
            WaitAction::Answer(_) => tr(cx, "답을 보냈습니다", "Answer sent"),
            _ => tr(cx, "입력을 보냈습니다", "Input sent"),
        };
        let prompt = &wait.wait.prompt;
        let task = graph.update(cx, |store, cx| match action {
            WaitAction::Answer(text) => Some(store.answer_wait(id, text, Vec::new(), cx)),
            WaitAction::Choose(index) => Some(store.choose_option(id, prompt.clone()?, index, cx)),
            WaitAction::Write(index, text) => {
                Some(store.type_text(id, prompt.clone()?, text, index, cx))
            }
            WaitAction::Press(press) => Some(store.press_keys(
                id,
                PressKeys {
                    screen: wait.wait.screen.clone(),
                    press,
                },
                cx,
            )),
            WaitAction::Confirm => Some(store.dismiss_wait(id, cx)),
        });
        if let Some(task) = task {
            self.run(Target::Wait(id.0), task, success, cx);
        }
    }

    fn run(
        &mut self,
        target: Target,
        task: Task<Result<(), String>>,
        success: &'static str,
        cx: &mut Context<Self>,
    ) {
        let graph = self.data.read(cx).graph().clone();
        if let Target::Wait(wait) = target {
            graph.update(cx, |store, cx| store.set_busy(wait, true, cx));
        }
        self.busy.insert(target);
        cx.notify();
        let room = self.room.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Target::Wait(wait) = target {
                cx.update(|cx| graph.update(cx, |store, cx| store.set_busy(wait, false, cx)));
            }
            let _ = this.update(cx, |stream, cx| {
                stream.busy.remove(&target);
                if stream.room == room {
                    let result = result.map(|()| {
                        stream.drafts.retain(|key, _| !key.belongs_to(target));
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

impl Input {
    fn belongs_to(self, target: Target) -> bool {
        match self {
            Input::Edit(id) => target == Target::Message(id),
            Input::Answer(id) | Input::Prompt(id, _) | Input::Screen(id) => {
                target == Target::Wait(id)
            }
        }
    }
}

fn same_input(old: &ChatItem, new: &ChatItem) -> bool {
    matches!((&old.entry, &new.entry), (
        ChatEntry::Wait { wait: old, closed: None, .. },
        ChatEntry::Wait { wait: new, closed: None, .. },
    ) if old.wait.id == new.wait.id && old.wait.prompt == new.wait.prompt
        && old.wait.screen == new.wait.screen && old.wait.answerable == new.wait.answerable
        && old.wait.choices == new.wait.choices && old.wait.text == new.wait.text)
}
