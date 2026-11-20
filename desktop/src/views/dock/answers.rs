use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use api::graph::waits::{
    InputField, Press, PressKeys, Prompt, ScreenWait, TermKey, WaitKind, WaitTone,
};
use api::ids::{NodeId, WaitId};
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Div, Entity, ExternalPaths, Image, InteractiveElement,
    IntoElement, MouseButton, ObjectFit, ParentElement, Stateful, StatefulInteractiveElement,
    Styled, StyledImage, Subscription, Task, Window, div, img,
};

use super::blocks;
use super::body::{self, Target};
use super::entry::{self, Choice, Look};
use super::questions::key_label;
use super::ui::{self, GAP, SMALL};
use crate::scale::fixed as px;
use crate::store::{BusyChanged, Store};
use crate::strings::Lang;
use crate::strings::Str;
use crate::theme::mix;
use crate::views::field::{self, Field, FieldEvent};
use crate::views::icons::{Lucide, icon};
use crate::views::pictures::{self, Pictures, PicturesEvent};
use crate::views::tooltip::tip;
use crate::views::viewer::Viewer;

const SHOT_HEIGHT: f32 = 44.0;
const SHOT_WIDTH: f32 = 64.0;
const SCREEN_KEYS: [(&str, Press); 8] = [
    ("Enter", Press::Key(TermKey::Enter)),
    ("Esc", Press::Key(TermKey::Escape)),
    ("\u{2190}", Press::Key(TermKey::Left)),
    ("\u{2192}", Press::Key(TermKey::Right)),
    ("\u{2191}", Press::Key(TermKey::Up)),
    ("\u{2193}", Press::Key(TermKey::Down)),
    ("y", Press::Char('y')),
    ("n", Press::Char('n')),
];

struct Draft {
    field: Entity<Field>,
    secret: bool,
    _sub: Subscription,
}

struct Attach {
    node: NodeId,
    pictures: Entity<Pictures>,
    _sub: Subscription,
}

type Done = Box<dyn FnOnce(&mut App)>;

enum Caption<'a> {
    Label(Str),
    Text(&'a str),
}

enum Text {
    Typed(String),
    Field(Entity<Field>),
}

impl Text {
    fn read(self, cx: &App) -> String {
        match self {
            Self::Typed(text) => text,
            Self::Field(field) => field.read(cx).text().to_owned(),
        }
    }
}

fn cleared(field: Entity<Field>) -> Done {
    Box::new(move |cx| field.update(cx, |field, cx| field.clear(cx)))
}

pub(crate) struct Answers {
    store: Entity<Store>,
    fields: HashMap<(u32, Slot), Draft>,
    shown: HashMap<u32, TerminalInput>,
    notes: HashMap<u32, String>,
    images: HashMap<(u32, usize), Option<Arc<Image>>>,
    attached: HashMap<u32, Attach>,
    waiting: HashMap<u32, (Text, Option<Done>)>,
    viewer: Entity<Viewer>,
    zoomed: Option<u32>,
    _subs: Vec<Subscription>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Slot {
    Answer,
    Option(usize),
    Field,
    Screen,
}

#[derive(PartialEq, Eq)]
enum TerminalInput {
    Prompt(Prompt),
    Screen(Vec<String>),
}

pub(crate) fn in_alert_strip(wait: &ScreenWait) -> bool {
    matches!(
        wait.wait.kind,
        WaitKind::Question
            | WaitKind::Done
            | WaitKind::Reply
            | WaitKind::Approval
            | WaitKind::Trust
            | WaitKind::Auth
            | WaitKind::Input
    )
}

impl Answers {
    pub(crate) fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        let mut answers = Self {
            store: store.clone(),
            fields: HashMap::new(),
            shown: HashMap::new(),
            notes: HashMap::new(),
            images: HashMap::new(),
            attached: HashMap::new(),
            waiting: HashMap::new(),
            viewer: cx.new(Viewer::new),
            zoomed: None,
            _subs: Vec::new(),
        };
        answers
            ._subs
            .push(cx.observe(&store, |answers, _, cx| answers.sync(cx)));
        answers
            ._subs
            .push(cx.subscribe(&store, |_, _, _: &BusyChanged, cx| cx.notify()));
        answers.sync(cx);
        answers
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let live: HashSet<_> = self
            .store
            .read(cx)
            .snapshot()
            .into_iter()
            .flat_map(|snapshot| &snapshot.nodes)
            .flat_map(|node| &node.waits)
            .map(|wait| wait.wait.id.0)
            .collect();
        self.shown.retain(|id, _| live.contains(id));
        self.fields.retain(|(id, _), _| live.contains(id));
        self.notes.retain(|id, _| live.contains(id));
        self.attached.retain(|id, _| live.contains(id));
        self.waiting.retain(|id, _| {
            if live.contains(id) {
                true
            } else {
                self.store
                    .update(cx, |store, cx| store.set_busy(*id, false, cx));
                false
            }
        });
        cx.notify();
    }

    pub(crate) fn prepare(&mut self, node: NodeId, wait: &ScreenWait, cx: &mut Context<Self>) {
        self.ensure_pictures(node, wait, cx);
        self.ensure_fields(wait, cx);
        self.load_images(wait, cx);
    }

    pub(crate) fn retain_images(&mut self, keep: impl Fn(u32) -> bool) {
        self.images.retain(|(id, _), _| keep(*id));
        self.zoomed = self.zoomed.filter(|&id| keep(id));
    }

    pub(crate) fn is_zoomed(&self, cx: &App) -> bool {
        self.zoomed.is_some() && self.viewer.read(cx).is_open()
    }

    pub(crate) fn focus(&self, id: WaitId, window: &mut Window, cx: &mut App) {
        if let Some(draft) = self.fields.get(&(id.0, Slot::Answer)) {
            let focus = draft.field.read(cx).handle();
            window.focus(&focus, cx);
        }
    }

    fn press(&mut self, wait: u32, keys: PressKeys, cx: &mut Context<Self>) {
        if self.store.read(cx).is_busy(wait) {
            return;
        }
        let task = self
            .store
            .update(cx, |store, cx| store.press_keys(WaitId(wait), keys, cx));
        self.run(wait, None, task, cx);
    }

    pub(crate) fn load_images(&mut self, wait: &ScreenWait, cx: &mut Context<Self>) {
        let id = wait.wait.id;
        for index in 0..wait.wait.images.len() {
            let key = (id.0, index);
            let std::collections::hash_map::Entry::Vacant(entry) = self.images.entry(key) else {
                continue;
            };
            entry.insert(None);
            let task = self
                .store
                .update(cx, |store, cx| store.wait_image(id, index, cx));
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |answers, cx| {
                    if let Some(saved) = answers.images.get_mut(&key) {
                        match result {
                            Ok(image) => *saved = Some(image),
                            Err(error) => {
                                answers.notes.insert(id.0, error);
                            }
                        }
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    fn ensure_pictures(&mut self, node: NodeId, wait: &ScreenWait, cx: &mut Context<Self>) {
        let id = wait.wait.id.0;
        if !wait.wait.answerable {
            self.attached.remove(&id);
            if self.waiting.remove(&id).is_some() {
                self.store
                    .update(cx, |store, cx| store.set_busy(id, false, cx));
            }
            return;
        }
        if !self.attached.contains_key(&id) {
            let store = self.store.clone();
            let pictures = cx.new(|_| Pictures::new(store, node));
            let sub =
                cx.subscribe(
                    &pictures,
                    move |answers, _, event: &PicturesEvent, cx| match event {
                        PicturesEvent::Settled => answers.settled(id, cx),
                        PicturesEvent::Refused(text) => {
                            answers.notes.insert(id, text.clone());
                            cx.notify();
                        }
                        PicturesEvent::Other(name) => {
                            let text = ui::say_with(cx, Str::InboxNotImage, &[("name", name)]);
                            answers.notes.insert(id, text);
                            cx.notify();
                        }
                    },
                );
            self.attached.insert(
                id,
                Attach {
                    node,
                    pictures,
                    _sub: sub,
                },
            );
        }
    }

    fn settled(&mut self, wait: u32, cx: &mut Context<Self>) {
        if let Some((text, done)) = self.waiting.remove(&wait) {
            self.send(wait, text, done, cx);
        }
    }

    fn ensure_fields(&mut self, wait: &ScreenWait, cx: &mut Context<Self>) {
        let id = wait.wait.id.0;
        if wait.wait.answerable {
            let done = matches!(wait.tone, WaitTone::Done);
            let hint = if done {
                Str::InboxFollowUpPlaceholder
            } else {
                Str::InboxAnswerPlaceholder
            };
            self.make_field(id, Slot::Answer, Caption::Label(hint), false, true, cx);
            self.hook_files(id, cx);
        }
        self.ensure_terminal_input(wait);
        let Some(prompt) = wait.wait.prompt.as_ref() else {
            if matches!(self.shown.get(&id), Some(TerminalInput::Screen(_))) {
                self.make_field(
                    id,
                    Slot::Screen,
                    Caption::Label(Str::InboxTextLabel),
                    false,
                    true,
                    cx,
                );
            }
            return;
        };
        for (index, option) in prompt.options.iter().enumerate() {
            if option.writes {
                self.make_field(
                    id,
                    Slot::Option(index),
                    Caption::Text(&option.label),
                    false,
                    false,
                    cx,
                );
            }
        }
        if let Some(kind) = prompt.field {
            let secret = kind == InputField::Secret;
            let hint = if secret {
                Str::InboxSecretLabel
            } else {
                Str::InboxTextLabel
            };
            self.make_field(id, Slot::Field, Caption::Label(hint), secret, false, cx);
        }
    }

    fn ensure_terminal_input(&mut self, wait: &ScreenWait) {
        let id = wait.wait.id.0;
        let input = if let Some(prompt) = wait.wait.prompt.as_ref() {
            Some(TerminalInput::Prompt(prompt.clone()))
        } else if !wait.wait.answerable && !wait.wait.screen.is_empty() {
            Some(TerminalInput::Screen(wait.wait.screen.clone()))
        } else {
            None
        };
        if self.shown.get(&id) != input.as_ref() {
            self.fields
                .retain(|(wait, slot), _| *wait != id || *slot == Slot::Answer);
            if let Some(input) = input {
                self.shown.insert(id, input);
            } else {
                self.shown.remove(&id);
            }
        }
    }

    fn hook_files(&mut self, id: u32, cx: &mut Context<Self>) {
        let Some(attach) = self.attached.get(&id) else {
            return;
        };
        let pictures = attach.pictures.clone();
        if let Some(draft) = self.fields.get(&(id, Slot::Answer)) {
            draft.field.update(cx, |field, _| {
                field.on_files(move |entries, _, cx| {
                    pictures.update(cx, |pictures, cx| pictures.take_clipboard(entries, cx));
                });
            });
        }
    }

    fn make_field(
        &mut self,
        id: u32,
        slot: Slot,
        hint: Caption,
        secret: bool,
        multiline: bool,
        cx: &mut Context<Self>,
    ) {
        if self
            .fields
            .get(&(id, slot))
            .is_some_and(|draft| draft.secret == secret)
        {
            return;
        }
        let field = match hint {
            Caption::Label(label) => field::boxed_label(label, secret, multiline, cx),
            Caption::Text(text) => field::boxed(text, secret, multiline, cx),
        };
        let sub = cx.subscribe(&field, move |answers, source, event: &FieldEvent, cx| {
            if matches!(event, FieldEvent::Enter)
                && answers
                    .fields
                    .get(&(id, slot))
                    .is_some_and(|draft| draft.field.entity_id() == source.entity_id())
            {
                answers.submit(id, slot, cx);
            }
        });
        self.fields.insert(
            (id, slot),
            Draft {
                field,
                secret,
                _sub: sub,
            },
        );
    }

    fn zoom(&mut self, wait: u32, image: Arc<Image>, window: &mut Window, cx: &mut Context<Self>) {
        self.zoomed = Some(wait);
        self.viewer
            .update(cx, |viewer, cx| viewer.open(image, window, cx));
    }

    fn follow(&mut self, wait: u32, target: Target, cx: &mut Context<Self>) {
        let task = body::open(&self.store, target, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |answers, cx| {
                match result {
                    Ok(()) => answers.notes.remove(&wait),
                    Err(error) => answers.notes.insert(wait, error),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn run(
        &mut self,
        wait: u32,
        done: Option<Done>,
        task: Task<Result<(), String>>,
        cx: &mut Context<Self>,
    ) {
        self.store
            .update(cx, |store, cx| store.set_busy(wait, true, cx));
        self.notes.remove(&wait);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |answers, cx| {
                answers
                    .store
                    .update(cx, |store, cx| store.set_busy(wait, false, cx));
                match result {
                    Ok(()) => {
                        if let Some(done) = done {
                            done(cx);
                        }
                    }
                    Err(error) => {
                        answers.notes.insert(wait, error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit(&mut self, wait: u32, slot: Slot, cx: &mut Context<Self>) {
        if self.store.read(cx).is_busy(wait) {
            return;
        }
        let Some(field) = self
            .fields
            .get(&(wait, slot))
            .map(|draft| draft.field.clone())
        else {
            return;
        };
        if field.read(cx).composing() {
            return;
        }
        let text = field.read(cx).text().to_owned();
        let id = WaitId(wait);
        let store = self.store.clone();
        let task = match slot {
            Slot::Answer => {
                let bare = self
                    .attached
                    .get(&wait)
                    .is_none_or(|a| a.pictures.read(cx).is_empty());
                if field.read(cx).is_blank() && bare {
                    return;
                }
                self.send(wait, Text::Field(field.clone()), Some(cleared(field)), cx);
                return;
            }
            Slot::Option(_) | Slot::Field => {
                let Some(TerminalInput::Prompt(prompt)) = self.shown.get(&wait) else {
                    return;
                };
                let option = match slot {
                    Slot::Option(index) => Some(index),
                    _ => None,
                };
                let prompt = prompt.clone();
                store.update(cx, |store, cx| {
                    store.type_text(id, prompt, text, option, cx)
                })
            }
            Slot::Screen => {
                let Some(TerminalInput::Screen(screen)) = self.shown.get(&wait) else {
                    return;
                };
                let keys = PressKeys {
                    screen: screen.clone(),
                    press: Press::Text(text),
                };
                store.update(cx, |store, cx| store.press_keys(id, keys, cx))
            }
        };
        self.run(wait, Some(cleared(field)), task, cx);
    }

    fn send(&mut self, wait: u32, text: Text, done: Option<Done>, cx: &mut Context<Self>) {
        let pictures = self.attached.get(&wait).map(|a| {
            a.pictures
                .update(cx, |pictures, cx| pictures.retarget(a.node, cx));
            a.pictures.clone()
        });
        if pictures.as_ref().is_some_and(|p| p.read(cx).busy()) {
            self.store
                .update(cx, |store, cx| store.set_busy(wait, true, cx));
            self.waiting.insert(wait, (text, done));
            return;
        }
        let text = text.read(cx);
        let saved = pictures
            .as_ref()
            .map(|p| p.read(cx).saved())
            .unwrap_or_default();
        if text.trim().is_empty() && saved.is_empty() {
            let note = ui::say(cx, Str::ComposerNothingSaved).to_owned();
            self.notes.insert(wait, note);
            self.store
                .update(cx, |store, cx| store.set_busy(wait, false, cx));
            cx.notify();
            return;
        }
        let (ids, paths): (Vec<u32>, Vec<String>) = saved.into_iter().unzip();
        let task = self.store.update(cx, |store, cx| {
            store.answer_wait(WaitId(wait), text, paths, cx)
        });
        let done: Done = Box::new(move |cx| {
            if let Some(done) = done {
                done(cx);
            }
            if let Some(pictures) = pictures {
                pictures.update(cx, |pictures, cx| pictures.forget(&ids, cx));
            }
        });
        self.run(wait, Some(done), task, cx);
    }

    fn pick(&mut self, wait: u32, option: usize, cx: &mut Context<Self>) {
        if self.store.read(cx).is_busy(wait) {
            return;
        }
        let Some(TerminalInput::Prompt(prompt)) = self.shown.get(&wait) else {
            return;
        };
        let prompt = prompt.clone();
        let id = WaitId(wait);
        let task = self
            .store
            .update(cx, |store, cx| store.choose_option(id, prompt, option, cx));
        self.run(wait, None, task, cx);
    }

    pub(crate) fn choose_nth(
        &mut self,
        wait: &ScreenWait,
        nth: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let id = wait.wait.id.0;
        if let Some(prompt) = &wait.wait.prompt {
            if prompt.options.get(nth).is_none_or(|option| option.writes) {
                return false;
            }
            self.ensure_terminal_input(wait);
            self.pick(id, nth, cx);
            return true;
        }
        let answerable = wait.wait.answerable && matches!(wait.tone, WaitTone::Ask);
        let Some(choice) = wait.wait.choices.get(nth).filter(|_| answerable) else {
            return false;
        };
        self.reply(id, choice.key.to_string(), cx);
        true
    }

    fn confirm(&mut self, id: WaitId, cx: &mut Context<Self>) {
        if self.store.read(cx).is_busy(id.0) {
            return;
        }
        let task = self
            .store
            .update(cx, |store, cx| store.dismiss_wait(id, cx));
        self.run(id.0, None, task, cx);
    }

    fn reply(&mut self, wait: u32, text: String, cx: &mut Context<Self>) {
        if self.store.read(cx).is_busy(wait) {
            return;
        }
        self.send(wait, Text::Typed(text), None, cx);
    }
}

fn scaled(
    answers: &Entity<Answers>,
    id: u32,
    slot: Slot,
    tone: WaitTone,
    z: f32,
    cx: &mut App,
) -> Option<Entity<Field>> {
    let field = answers.read(cx).fields.get(&(id, slot))?.field.clone();
    let color = entry::tone_of(tone, &ui::colors(cx));
    field.update(cx, |field, cx| {
        field.set_scale(z, cx);
        field.set_tone(color, cx);
    });
    Some(field)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardState {
    Alert,
    Inline,
    ReadOnly,
}

impl CardState {
    fn hint(self, index: usize) -> Option<String> {
        (self == Self::Alert && index < 9).then(|| format!("Alt+{}", index + 1))
    }
}

pub(crate) fn content(
    answers: &Entity<Answers>,
    wait: &ScreenWait,
    state: CardState,
    z: f32,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let c = ui::colors(cx);
    let id = wait.wait.id.0;
    let answers_state = answers.read(cx);
    let error = answers_state.notes.get(&id).cloned();
    let viewer = (answers_state.zoomed == Some(id) && answers_state.viewer.read(cx).is_open())
        .then(|| answers_state.viewer.clone());
    wait_content(answers, wait, state, z, window, cx)
        .children(error.map(|text| div().text_size(px(SMALL * z)).text_color(c.err).child(text)))
        .children(viewer)
}

fn wait_content(
    answers: &Entity<Answers>,
    wait: &ScreenWait,
    state: CardState,
    z: f32,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let c = ui::colors(cx);
    let id = wait.wait.id.0;
    let done = matches!(wait.tone, WaitTone::Done);
    let group = format!("wait-{id}");
    let mut body = div()
        .group(group.clone())
        .relative()
        .flex()
        .flex_col()
        .gap(px(GAP * z));
    let shots: Vec<(usize, Arc<Image>)> = (0..wait.wait.images.len())
        .filter_map(|index| {
            let image = answers.read(cx).images.get(&(id, index))?.clone()?;
            Some((index, image))
        })
        .collect();
    if !shots.is_empty() {
        body = body.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.0 * z))
                .pt(px(4.0 * z))
                .children(shots.into_iter().map(|(index, image)| {
                    let owner = answers.clone();
                    let opened = image.clone();
                    let name = wait.wait.images[index].clone();
                    div()
                        .id(("shot", id as usize * 256 + index % 256))
                        .cursor_pointer()
                        .tooltip(tip(ui::say_with(
                            cx,
                            Str::InboxZoomImage,
                            &[("name", &name)],
                        )))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| {
                            let opened = opened.clone();
                            owner.update(cx, |answers, cx| answers.zoom(id, opened, window, cx))
                        })
                        .child(
                            img(image)
                                .w(px(SHOT_WIDTH * z))
                                .h(px(SHOT_HEIGHT * z))
                                .object_fit(ObjectFit::Cover)
                                .rounded(px(6.0 * z))
                                .border_1()
                                .border_color(c.line_strong)
                                .bg(c.panel),
                        )
                })),
        );
    }
    if let Some(instruction) = &wait.wait.instruction {
        body = body.child(
            div()
                .text_size(px(SMALL * z))
                .text_color(c.ink_2)
                .child(ui::say_with(
                    cx,
                    Str::InboxInstruction,
                    &[("text", instruction)],
                )),
        );
    }
    if let Some(prompt) = &wait.wait.prompt {
        let text = if prompt.body.is_empty() {
            prompt.summary.clone()
        } else {
            prompt.body.join("\n")
        };
        body = body.child(entry::prompt_box(crate::views::prose::prose(text), z, &c));
        if state != CardState::ReadOnly {
            body = body.child(prompt_actions(answers, wait, prompt, state, z, cx));
        }
    } else if !wait.wait.text.is_empty() || !wait.wait.blocks.is_empty() || wait.wait.doc.is_some()
    {
        let owner = answers.clone();
        body = body.child(
            div()
                .text_size(px(ui::TEXT * z))
                .child(blocks::render_selectable(
                    ("wait-text", id as usize),
                    &wait.wait.blocks,
                    &wait.wait.text,
                    wait.wait.doc.as_ref(),
                    entry::tone_of(wait.tone, &c),
                    ui::TEXT * z,
                    window,
                    cx,
                    move |target, cx| {
                        owner.update(cx, |answers, cx| answers.follow(id, target, cx))
                    },
                )),
        );
    } else if state == CardState::ReadOnly && !wait.summary.trim().is_empty() {
        body = body.child(crate::views::prose::prose(wait.summary.clone()));
    }
    if state == CardState::ReadOnly {
        return body;
    }
    if wait.wait.answerable && !done && !wait.wait.choices.is_empty() {
        body = body.child(div().flex().flex_wrap().gap(px(GAP * z)).children(
            wait.wait.choices.iter().enumerate().map(|(index, choice)| {
                let key = choice.key;
                let owner = answers.clone();
                let look = if choice.recommended {
                    Look::Recommended
                } else {
                    Look::Plain
                };
                let button = div()
                    .id(("choice", id as usize * 256 + key as usize % 256))
                    .cursor_pointer()
                    .hover({
                        let hover = c.surface_hover;
                        move |style| style.bg(hover)
                    });
                entry::choice_face(
                    button,
                    Choice {
                        key: key.to_string(),
                        hint: state.hint(index),
                        label: &choice.label,
                        recommended: choice.recommended,
                        look,
                        tone: wait.tone,
                    },
                    z,
                    cx,
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    owner.update(cx, |answers, cx| answers.reply(id, key.to_string(), cx))
                })
            }),
        ));
    }
    if done {
        let owner = answers.clone();
        let busy = answers.read(cx).store.read(cx).is_busy(id);
        body = body.child(
            div().flex().child(
                crate::views::widgets::btn::button("alert-confirm", cx)
                    .child(ui::say(cx, Str::InboxConfirm))
                    .when(busy, |button| {
                        crate::views::widgets::btn::inert(button).opacity(0.55)
                    })
                    .when(!busy, |button| {
                        button.on_click(move |_, _, cx| {
                            owner.update(cx, |answers, cx| answers.confirm(WaitId(id), cx))
                        })
                    }),
            ),
        );
    }
    body.children(screen_keys(answers, wait, z, cx)).children(
        scaled(answers, id, Slot::Screen, wait.tone, z, cx)
            .map(|field| send_row(answers, id, Slot::Screen, wait.tone, &field, z, cx)),
    )
}

fn act(
    id: (&'static str, usize),
    label: &str,
    recommended: bool,
    z: f32,
    cx: &App,
) -> Stateful<Div> {
    let c = ui::colors(cx);
    let color = entry::tone_of(WaitTone::Ask, &c);
    let button = crate::views::ui::button_at(id, z, cx)
        .flex_none()
        .whitespace_nowrap()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    let button = if recommended {
        button.border_color(color).bg(mix(color, 0.1, c.surface))
    } else {
        button
    };
    button.child(label.to_owned())
}

fn screen_keys(answers: &Entity<Answers>, wait: &ScreenWait, z: f32, cx: &App) -> Option<Div> {
    if wait.wait.prompt.is_some() || wait.wait.screen.is_empty() {
        return None;
    }
    let c = ui::colors(cx);
    let id = wait.wait.id.0;
    let keys = SCREEN_KEYS
        .iter()
        .enumerate()
        .map(|(index, (label, press))| {
            let (owner, screen, press) = (answers.clone(), wait.wait.screen.clone(), press.clone());
            act(
                ("screen-key", id as usize * 16 + index),
                label,
                false,
                z,
                cx,
            )
            .font_family(crate::fonts::TERMINAL_FAMILY)
            .on_click(move |_, _, cx| {
                let keys = PressKeys {
                    screen: screen.clone(),
                    press: press.clone(),
                };
                owner.update(cx, |answers, cx| answers.press(id, keys, cx))
            })
        });
    let rows = wait.wait.screen.iter().enumerate().map(|(index, row)| {
        let line = div().child(row.clone());
        let Some(option) = option_of(row) else {
            return line.into_any_element();
        };
        let (owner, screen) = (answers.clone(), wait.wait.screen.clone());
        let hover = c.surface_hover;
        line.id(("screen-option", id as usize * 64 + index))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                let keys = PressKeys {
                    screen: screen.clone(),
                    press: Press::Char(option),
                };
                owner.update(cx, |answers, cx| answers.press(id, keys, cx))
            })
            .into_any_element()
    });
    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(GAP * z))
            .child(
                entry::prompt_box(div().flex().flex_col().children(rows), z, &c)
                    .whitespace_nowrap()
                    .overflow_hidden(),
            )
            .child(div().flex().flex_wrap().gap(px(GAP * z)).children(keys)),
    )
}

fn option_of(row: &str) -> Option<char> {
    let row = row.trim_start();
    let row = row
        .strip_prefix(['\u{276f}', '\u{203a}', '>'])
        .map_or(row, str::trim_start);
    let mut chars = row.chars();
    let digit = chars.next().filter(|c| matches!(c, '1'..='9'))?;
    (chars.next() == Some('.') && chars.next().is_some_and(char::is_whitespace)).then_some(digit)
}

pub(crate) fn composer(
    answers: &Entity<Answers>,
    wait: &ScreenWait,
    name: &str,
    z: f32,
    cx: &mut App,
) -> Option<Stateful<Div>> {
    if !wait.wait.answerable {
        return None;
    }
    let id = wait.wait.id.0;
    let c = ui::colors(cx);
    let ink = entry::tone_of(wait.tone, &c);
    let label = match (wait.tone, cx.global::<Lang>()) {
        (WaitTone::Ask, Lang::Ko) => format!("{name}에게 답"),
        (WaitTone::Ask, Lang::En) => format!("Reply to {name}"),
        (WaitTone::Done, Lang::Ko) => format!("{name}에게 이어서 맡기기"),
        (WaitTone::Done, Lang::En) => format!("Follow up with {name}"),
    };
    let field = scaled(answers, id, Slot::Answer, wait.tone, z, cx)?;
    let pictures = answers.read(cx).attached.get(&id)?.pictures.clone();
    let owner = answers.clone();
    let chips = pictures::chips(
        &pictures,
        move |picture, window, cx| {
            owner.update(cx, |answers, cx| answers.zoom(id, picture, window, cx))
        },
        "answer-chip",
        id,
        z,
        cx,
    );
    let clip = pictures.clone();
    let attach = crate::views::widgets::btn::ghost(("answer-attach", id as usize), cx)
        .aria_label(ui::say(cx, Str::InboxAttachImage))
        .tooltip(tip(ui::say(cx, Str::InboxAttachImage).into()))
        .flex_none()
        .child(icon(Lucide::Paperclip, px(14.0 * z), ink))
        .on_click(move |_, _, cx| clip.update(cx, |pictures, cx| pictures.pick_files(cx)));
    Some(
        div()
            .id(("answer-composer", id as usize))
            .flex()
            .flex_col()
            .gap(px(4.0 * z))
            .children(chips)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0 * z))
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(220.0 * z))
                            .truncate()
                            .rounded(px(5.0 * z))
                            .px(px(8.0 * z))
                            .py(px(4.0 * z))
                            .bg(mix(ink, 0.12, c.surface))
                            .child(label),
                    )
                    .child(attach)
                    .child(div().flex_1().min_w_0().child(send_row(
                        answers,
                        id,
                        Slot::Answer,
                        wait.tone,
                        &field,
                        z,
                        cx,
                    ))),
            )
            .on_drop(move |paths: &ExternalPaths, _, cx| {
                pictures.update(cx, |pictures, cx| {
                    pictures.take_paths(paths.paths().to_vec(), cx)
                });
            }),
    )
}

fn send_row(
    answers: &Entity<Answers>,
    id: u32,
    slot: Slot,
    tone: WaitTone,
    field: &Entity<Field>,
    z: f32,
    cx: &mut App,
) -> Div {
    let c = ui::colors(cx);
    let owner = answers.clone();
    let slot_id = match slot {
        Slot::Answer => 0,
        Slot::Option(index) => index + 1,
        Slot::Field => 255,
        Slot::Screen => 254,
    };
    let empty = {
        let state = answers.read(cx);
        let bare = state
            .attached
            .get(&id)
            .is_none_or(|a| a.pictures.read(cx).is_empty());
        slot == Slot::Answer && field.read(cx).is_blank() && bare
    };
    let busy = answers.read(cx).store.read(cx).is_busy(id);
    let off = empty || (slot != Slot::Answer && busy);
    let color = entry::tone_of(tone, &c);
    let mut send = div()
        .id(("send", id as usize * 256 + slot_id))
        .flex_none()
        .size(px(28.0 * z))
        .rounded(px(6.0 * z))
        .flex()
        .items_center()
        .justify_center()
        .bg(color)
        .child(icon(
            Lucide::ArrowUp,
            px(14.0 * z),
            entry::tone_ink(tone, &c),
        ));
    if slot != Slot::Answer {
        send = send.tooltip(tip(ui::say(cx, Str::InboxTypeSend).to_owned()));
    }
    let send = if off {
        send.opacity(0.45)
    } else {
        send.cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| owner.update(cx, |answers, cx| answers.submit(id, slot, cx)))
    };
    div()
        .flex()
        .items_end()
        .gap(px(GAP * z))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(div().flex_1().min_w_0().child(field.clone()))
        .child(send)
}

fn prompt_actions(
    answers: &Entity<Answers>,
    wait: &ScreenWait,
    prompt: &Prompt,
    state: CardState,
    z: f32,
    cx: &mut App,
) -> AnyElement {
    let (id, tone) = (wait.wait.id.0, wait.tone);
    let c = ui::colors(cx);
    let busy = answers.read(cx).store.read(cx).is_busy(id);
    let mut body = div().flex().flex_col().gap(px(GAP * z));
    let options =
        div()
            .flex()
            .flex_col()
            .gap(px(GAP * z))
            .children(prompt.options.iter().enumerate().map(|(index, option)| {
                let label = key_label(&option.key, index);
                if option.writes {
                    return entry::choice_face(
                        div(),
                        Choice {
                            key: label,
                            hint: None,
                            label: &option.label,
                            recommended: false,
                            look: Look::Plain,
                            tone,
                        },
                        z,
                        cx,
                    )
                    .into_any_element();
                }
                let owner = answers.clone();
                let button = div()
                    .id(("option", id as usize * 256 + index))
                    .cursor_pointer()
                    .when(busy, |button| button.opacity(0.6))
                    .hover({
                        let hover = c.surface_hover;
                        move |style| style.bg(hover)
                    });
                entry::choice_face(
                    button,
                    Choice {
                        key: label,
                        hint: state.hint(index),
                        label: &option.label,
                        recommended: index == 0,
                        look: if index == 0 {
                            Look::Recommended
                        } else {
                            Look::Plain
                        },
                        tone,
                    },
                    z,
                    cx,
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    owner.update(cx, |answers, cx| answers.pick(id, index, cx))
                })
                .into_any_element()
            }));
    if !prompt.options.is_empty() {
        body = body.child(options);
    }
    for (index, option) in prompt.options.iter().enumerate() {
        let field = option
            .writes
            .then(|| scaled(answers, id, Slot::Option(index), tone, z, cx))
            .flatten();
        if let Some(field) = field {
            body = body.child(send_row(
                answers,
                id,
                Slot::Option(index),
                tone,
                &field,
                z,
                cx,
            ));
        }
    }
    let field = scaled(answers, id, Slot::Field, tone, z, cx);
    if let Some(field) = field {
        body = body.child(send_row(answers, id, Slot::Field, tone, &field, z, cx));
    }
    body.into_any_element()
}
