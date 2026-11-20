use std::rc::Rc;
use std::time::Duration;

use api::profile::Provider;
use api::settings::accounts::{LoginId, LoginState, LoginTarget};
use api::settings::usage::{Limits, UsageProblem};
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, FontWeight, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window, div,
};

use super::parts::{modal, small};
use crate::link;
use crate::problem::Failure;
use crate::scale::px;
use crate::strings::{Lang, Str};
use crate::views::dock::ui::{SMALL, TEXT, colors, say, say_with};
use crate::views::terminal::{self, TerminalView};
use crate::views::usage::{accounts_changed, provider_label, slug};

const POLL: Duration = Duration::from_secs(1);
const TRIES: u32 = 5;
const WIDTH: f32 = 640.0;
const TERMINAL_H: f32 = 320.0;
pub const CURSOR_USAGE: &str = "/api/usage/cursor";

#[derive(Clone)]
pub struct Request {
    path: String,
    body: Option<String>,
    title: String,
    fresh: bool,
}

impl Request {
    pub fn account(
        provider: Provider,
        distro: Option<&str>,
        target: LoginTarget,
        name: &str,
        cx: &App,
    ) -> Self {
        let label = provider_label(provider, cx);
        let title = match &target {
            LoginTarget::New => say_with(cx, Str::AccountsAddTitle, &[("provider", label)]),
            LoginTarget::Account(_) => say_with(cx, Str::AccountsReloginTitle, &[("name", name)]),
            LoginTarget::System => {
                say_with(cx, Str::AccountsSystemLoginTitle, &[("provider", label)])
            }
        };
        Self {
            path: format!(
                "/api/accounts/{}{}",
                slug(provider),
                link::query(&[("distro", distro)])
            ),
            body: serde_json::to_string(&target).ok(),
            title,
            fresh: matches!(target, LoginTarget::New),
        }
    }

    pub fn cursor(distro: Option<&str>, label: &str, cx: &App) -> Self {
        Self {
            path: format!("{CURSOR_USAGE}/login{}", link::query(&[("distro", distro)])),
            body: None,
            title: say_with(cx, Str::AccountsSystemLoginTitle, &[("provider", label)]),
            fresh: false,
        }
    }
}

pub fn needed(limits: &Limits) -> bool {
    matches!(
        limits,
        Limits::NeedsLogin
            | Limits::Failed {
                problem: UsageProblem::TokenExpired(_)
            }
    )
}

#[derive(Clone, Copy)]
pub enum Finished {
    Succeeded,
    Cancelled,
}

enum Stage {
    Starting(Option<LoginId>),
    Open {
        id: LoginId,
        terminal: Entity<TerminalView>,
        error: Option<String>,
    },
    Failed(Option<LoginId>, String),
    Closed,
}

impl Stage {
    fn id(&self) -> Option<LoginId> {
        match self {
            Self::Open { id, .. } => Some(*id),
            Self::Starting(id) | Self::Failed(id, _) => *id,
            Self::Closed => None,
        }
    }
}

pub struct Dialog {
    base: String,
    request: Request,
    stage: Stage,
    focus: FocusHandle,
    restore: Option<FocusHandle>,
}

impl EventEmitter<Finished> for Dialog {}

fn remove(base: &str, id: LoginId) -> Result<(), Failure> {
    match link::delete_within(base, &format!("/api/logins/{id}"), link::SLOW_LIMIT) {
        Err(Failure::Refused { status: 404, .. }) => Ok(()),
        result => result,
    }
}

impl Dialog {
    pub fn open(base: String, request: Request, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx: &mut Context<Self>| {
            let mut dialog = Self {
                base,
                request,
                stage: Stage::Starting(None),
                focus: cx.focus_handle(),
                restore: window.focused(cx),
            };
            cx.on_release(|dialog, cx| dialog.remove_open(cx)).detach();
            dialog.start(window, cx);
            dialog
        })
    }

    pub fn is_closed(&self) -> bool {
        matches!(self.stage, Stage::Closed)
    }

    fn remove_open(&self, cx: &App) {
        if let Some(id) = self.stage.id() {
            let base = self.base.clone();
            cx.background_spawn(async move {
                let _ = remove(&base, id);
            })
            .detach();
        }
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prev = self.stage.id();
        self.stage = Stage::Starting(prev);
        window.focus(&self.focus, cx);
        let (base, path, body) = (
            self.base.clone(),
            self.request.path.clone(),
            self.request.body.clone(),
        );
        cx.spawn_in(window, async move |this, cx| {
            let closed = if let Some(id) = prev {
                let base = base.clone();
                cx.background_spawn(async move { remove(&base, id) }).await
            } else {
                Ok(())
            };
            if !this
                .update(cx, |dialog, _| matches!(dialog.stage, Stage::Starting(_)))
                .unwrap_or(false)
            {
                return;
            }
            let started = match closed {
                Ok(()) => {
                    let base = base.clone();
                    cx.background_spawn(async move { link::post::<LoginId>(&base, &path, body) })
                        .await
                        .map_err(|error| (None, error))
                }
                Err(error) => Err((prev, error)),
            };
            let started_id = started.as_ref().ok().copied();
            let accepted = this
                .update_in(cx, |dialog, window, cx| {
                    if !matches!(dialog.stage, Stage::Starting(_)) {
                        return false;
                    }
                    match started {
                        Ok(id) => {
                            let terminal = terminal::open_login(&dialog.base, id.0, window, cx);
                            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
                            dialog.stage = Stage::Open {
                                id,
                                terminal,
                                error: None,
                            };
                            dialog.watch(id, window, cx);
                        }
                        Err((id, error)) => {
                            dialog.stage = Stage::Failed(id, error.text(*cx.global::<Lang>()))
                        }
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !accepted && let Some(id) = started_id {
                let _ = cx.background_spawn(async move { remove(&base, id) }).await;
            }
        })
        .detach();
        cx.notify();
    }

    fn watch(&self, id: LoginId, window: &mut Window, cx: &mut Context<Self>) {
        let base = self.base.clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut errors = 0;
            loop {
                cx.background_executor().timer(POLL).await;
                let open = this.update(cx, |dialog, _| dialog.stage.id() == Some(id)).unwrap_or(false);
                if !open {
                    break;
                }
                let base = base.clone();
                let state = cx
                    .background_spawn(async move {
                        link::read::<LoginState>(&base, &format!("/api/logins/{id}"))
                    })
                    .await;
                if matches!(state, Ok(LoginState::Running)) {
                    errors = 0;
                    continue;
                }
                if matches!(state, Err(ref error) if !matches!(error, Failure::Refused { status: 404, .. })) {
                    errors += 1;
                    if errors < TRIES {
                        continue;
                    }
                }
                let _ = this.update_in(cx, |dialog, window, cx| {
                    let Stage::Open { error, id: active, .. } = &mut dialog.stage else {
                        return;
                    };
                    if *active != id {
                        return;
                    }
                    if matches!(state, Ok(LoginState::Succeeded)) {
                        dialog.finish(Finished::Succeeded, window, cx);
                        return;
                    }
                    *error = Some(match state {
                        Err(error) => error.text(*cx.global::<Lang>()),
                        _ => match cx.global::<Lang>() {
                            Lang::Ko => "로그인을 완료하지 못했습니다.",
                            Lang::En => "Could not complete sign-in.",
                        }.to_owned(),
                    });
                    window.focus(&dialog.focus, cx);
                    cx.notify();
                });
                break;
            }
        }).detach();
    }

    fn finish(&mut self, outcome: Finished, window: &mut Window, cx: &mut Context<Self>) {
        self.remove_open(cx);
        self.stage = Stage::Closed;
        if let Some(restore) = self.restore.take() {
            window.focus(&restore, cx);
        }
        if matches!(outcome, Finished::Succeeded) {
            accounts_changed(cx);
        }
        cx.emit(outcome);
        cx.notify();
    }
}

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (view, error) = match &self.stage {
            Stage::Closed => return div().into_any_element(),
            Stage::Starting(_) => (None, None),
            Stage::Open {
                terminal, error, ..
            } => (Some(terminal.clone()), error.clone()),
            Stage::Failed(_, error) => (None, Some(error.clone())),
        };
        let c = colors(cx);
        let close = small("login-close", cx)
            .border_color(gpui::transparent_black())
            .bg(c.panel)
            .child(say(
                cx,
                if error.is_some() {
                    Str::CommonClose
                } else {
                    Str::CommonCancel
                },
            ))
            .on_click(
                cx.listener(|dialog, _, window, cx| dialog.finish(Finished::Cancelled, window, cx)),
            );
        let foot = match error {
            Some(error) => div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().flex_1().text_color(c.err).child(error))
                .child(
                    small("login-retry", cx)
                        .child(say(cx, Str::UpdateRetry))
                        .on_click(cx.listener(|dialog, _, window, cx| {
                            if matches!(
                                dialog.stage,
                                Stage::Open { error: Some(_), .. } | Stage::Failed(..)
                            ) {
                                dialog.start(window, cx);
                            }
                        })),
                ),
            None => div().text_color(c.ink_3).child(say(
                cx,
                if view.is_none() {
                    Str::StatusBarLoading
                } else if self.request.fresh {
                    Str::AccountsLoginHint
                } else {
                    Str::AccountsReloginHint
                },
            )),
        };
        let content = div()
            .w(px(WIDTH))
            .max_w_full()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(c.line_strong)
            .bg(c.panel)
            .text_size(px(TEXT))
            .text_color(c.ink)
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(15.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.request.title.clone()),
                    )
                    .child(close),
            )
            .children(view.map(|view| {
                div()
                    .w_full()
                    .h(px(TERMINAL_H))
                    .rounded(px(6.0))
                    .overflow_hidden()
                    .child(view)
            }))
            .child(foot.text_size(px(SMALL)));
        let this = cx.entity().downgrade();
        modal(
            &self.focus,
            content,
            Rc::new(move |window, cx| {
                let _ = this.update(cx, |dialog, cx| {
                    dialog.finish(Finished::Cancelled, window, cx)
                });
            }),
            window,
            cx,
        )
    }
}
