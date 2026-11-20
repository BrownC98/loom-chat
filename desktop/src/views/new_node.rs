use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Render, StatefulInteractiveElement, Styled, Task,
    Window, div,
};

use api::graph::errors::LaunchError;
use api::graph::{NewNode, Place};
use api::ids::AgentId;
use api::profile::{AgentView, Profile, Provider};
use api::repos::PlaceView;
use api::settings::accounts::{AccountView, PlaceUsage};
use api::start::{ModelChoice, Start};

use crate::fonts;
use crate::link;
use crate::scale::px;
use crate::settings::{login, want_agents};
use crate::store::Store;
use crate::strings::{Lang, Str};
use crate::theme::Theme;
use crate::views::field::{self, Field, FieldEvent, Form};
use crate::views::icons::{Lucide, agent_mark, icon};
use crate::views::scrollbar::Bar;
use crate::views::ui::{button, open_url};
use crate::views::usage::{self, ACCOUNTS};
use crate::views::worktree::Closed;

const WIDTH: f32 = 340.0;
const LIST: f32 = 264.0;

#[derive(Clone)]
struct Choice {
    agent: Option<usize>,
    installed: bool,
}

pub struct NewNodeView {
    store: Entity<Store>,
    place: Place,
    branch: String,
    at: PlaceView,
    recent: Option<AgentId>,
    agents: Option<Arc<Vec<AgentView>>>,
    accounts: Option<Vec<AccountView>>,
    cursor: Option<Vec<PlaceUsage>>,
    load_accounts: Option<Task<()>>,
    scanning: bool,
    field: Entity<Field>,
    picked: Option<Option<AgentId>>,
    error: Option<String>,
    sign_in: Option<(AgentId, login::Request)>,
    login_dialog: Option<Entity<login::Dialog>>,
    busy: bool,
    reveal: bool,
    scroll: Bar,
}

impl EventEmitter<Closed> for NewNodeView {}

fn blocked(agent: &AgentView, at: &PlaceView) -> bool {
    matches!(at, PlaceView::Ssh { .. }) && agent.serves
}

fn usable(agent: &AgentView, at: &PlaceView) -> bool {
    !blocked(agent, at)
        && agent
            .places
            .iter()
            .find(|p| p.place == *at)
            .is_some_and(|p| p.path.is_some())
}

fn short_place(at: &PlaceView, lang: Lang) -> String {
    match at {
        PlaceView::Pc => Str::PlacePcShort.text(lang).to_owned(),
        PlaceView::Wsl { distro } => Str::PlaceWslDistro.with(lang, &[("distro", distro)]),
        PlaceView::Ssh { host } => Str::PlaceSshHost.with(lang, &[("host", host)]),
    }
}

fn tag_place(at: &PlaceView, lang: Lang) -> String {
    match at {
        PlaceView::Pc => Str::PlacePcShort.text(lang).to_owned(),
        PlaceView::Wsl { .. } => Str::PlaceWsl.text(lang).to_owned(),
        PlaceView::Ssh { host } => host.clone(),
    }
}

impl NewNodeView {
    pub(crate) fn new(
        store: Entity<Store>,
        place: Place,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lang = *cx.global::<Lang>();
        let field = field::form(Str::NewNodeSearch.text(lang), false, false, Form::Plain, cx);
        cx.subscribe(&field, |this, _, event: &FieldEvent, cx| match event {
            FieldEvent::Changed => cx.notify(),
            FieldEvent::Step(step) => this.step(*step, cx),
            FieldEvent::Enter => {
                let active = this.active(cx);
                if let Some(choice) = active {
                    this.launch(this.key(&choice), cx);
                }
            }
            FieldEvent::Escape => cx.emit(Closed),
        })
        .detach();
        let handle = field.read(cx).handle();
        window.focus(&handle, cx);
        let snapshot = store.read(cx).snapshot();
        let repo = snapshot.and_then(|s| s.repos.iter().find(|r| r.id == place.repo));
        let at = repo.map_or(PlaceView::Pc, |r| r.place.clone());
        let recent = snapshot.and_then(|s| {
            s.nodes
                .iter()
                .filter(|n| n.parent.is_none())
                .max_by_key(|n| n.id.0)
                .and_then(|n| n.agent.clone())
        });
        let mut view = Self {
            store,
            place,
            branch,
            at,
            recent,
            agents: None,
            accounts: None,
            cursor: None,
            load_accounts: None,
            scanning: false,
            field,
            picked: None,
            error: None,
            sign_in: None,
            login_dialog: None,
            busy: false,
            reveal: false,
            scroll: Bar::new(),
        };
        view.scan(cx);
        view
    }

    fn scan(&mut self, cx: &mut Context<Self>) {
        if self.scanning || self.busy || self.login_dialog.is_some() {
            return;
        }
        self.scanning = true;
        self.error = None;
        let agents = self.store.update(cx, |store, cx| store.agents(cx));
        let accounts = self
            .store
            .read(cx)
            .call(cx, |base| link::read(base, ACCOUNTS));
        let cursor = self
            .store
            .read(cx)
            .call(cx, |base| link::read(base, login::CURSOR_USAGE));
        self.load_accounts = Some(cx.spawn(async move |this, cx| {
            let (accounts, cursor) = (accounts.await, cursor.await);
            let _ = this.update(cx, |this, cx| {
                this.accounts = accounts.ok();
                this.cursor = cursor.ok();
                cx.notify();
            });
        }));
        cx.spawn(async move |this, cx| {
            let agents = agents.await;
            let _ = this.update(cx, |this, cx| {
                this.scanning = false;
                match agents {
                    Ok(agents) => this.agents = Some(agents),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn place_view(&self) -> PlaceView {
        let places = self
            .agents
            .as_ref()
            .and_then(|a| a.first())
            .map(|a| a.places.iter().map(|p| p.place.clone()).collect::<Vec<_>>())
            .unwrap_or_default();
        if matches!(self.at, PlaceView::Ssh { .. }) || places.contains(&self.at) {
            self.at.clone()
        } else {
            PlaceView::Pc
        }
    }

    fn choices(&self) -> Vec<Choice> {
        let Some(agents) = &self.agents else {
            return Vec::new();
        };
        let at = self.place_view();
        let mut all = Vec::new();
        for installed in [true, false] {
            all.extend(
                agents
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| usable(a, &at) == installed)
                    .map(|(i, _)| Choice {
                        agent: Some(i),
                        installed,
                    }),
            );
            if installed {
                all.push(Choice {
                    agent: None,
                    installed: true,
                });
            }
        }
        all
    }

    fn key(&self, choice: &Choice) -> Option<AgentId> {
        self.agent(choice).map(|agent| agent.id.clone())
    }

    fn agent(&self, choice: &Choice) -> Option<&AgentView> {
        self.agents.as_ref()?.get(choice.agent?)
    }

    fn label(&self, choice: &Choice, lang: Lang) -> String {
        self.agent(choice).map_or_else(
            || Str::NewNodeShell.text(lang).to_owned(),
            |agent| agent.label.clone(),
        )
    }

    fn shown(&self, lang: Lang, cx: &gpui::App) -> Vec<Choice> {
        let needle = self.field.read(cx).text().trim().to_lowercase();
        self.choices()
            .into_iter()
            .filter(|c| self.label(c, lang).to_lowercase().contains(&needle))
            .collect()
    }

    fn active_in(&self, shown: &[Choice]) -> Option<Choice> {
        let selectable = || shown.iter().filter(|choice| choice.installed);
        if let Some(choice) = self
            .picked
            .as_ref()
            .and_then(|wanted| selectable().find(|choice| self.key(choice) == *wanted))
        {
            return Some(choice.clone());
        }
        let installed = || selectable().filter(|choice| choice.agent.is_some());
        installed()
            .find(|choice| self.key(choice) == self.recent)
            .or_else(|| installed().next())
            .cloned()
    }

    fn active(&self, cx: &gpui::App) -> Option<Choice> {
        let lang = *cx.global::<Lang>();
        self.active_in(&self.shown(lang, cx))
    }

    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        let lang = *cx.global::<Lang>();
        let shown = self.shown(lang, cx);
        let selectable: Vec<&Choice> = shown.iter().filter(|c| c.installed).collect();
        if selectable.is_empty() {
            return;
        }
        let at = self
            .active_in(&shown)
            .and_then(|a| selectable.iter().position(|c| self.key(c) == self.key(&a)));
        let start = at.map_or(if step > 0 { -1 } else { 0 }, |i| i as isize);
        let next = selectable[(start + step).rem_euclid(selectable.len() as isize) as usize];
        self.pick(self.key(next), cx);
    }

    fn pick(&mut self, agent: Option<AgentId>, cx: &mut Context<Self>) {
        if self.busy || self.login_dialog.is_some() {
            return;
        }
        self.picked = Some(agent);
        self.error = None;
        self.sign_in = None;
        self.reveal = true;
        cx.notify();
    }

    fn account(&self, provider: Provider, distro: Option<&str>, cx: &App) -> Option<&AccountView> {
        let own: Vec<_> = self
            .accounts
            .as_ref()?
            .iter()
            .filter(|a| a.provider == provider && a.distro.as_deref() == distro)
            .collect();
        let selected = self.store.read(cx).settings().account(provider, distro);
        Some(own[usage::active_index(&own, selected)?])
    }

    fn account_line(&self, agent: &AgentView, multi: bool, cx: &App) -> Option<String> {
        let lang = *cx.global::<Lang>();
        let distro = match &self.at {
            PlaceView::Pc => None,
            PlaceView::Wsl { distro } => Some(distro.as_str()),
            PlaceView::Ssh { host } => {
                return agent
                    .provider
                    .map(|_| Str::NewNodeAccountSsh.with(lang, &[("host", host)]));
            }
        };
        let (name, limits) = if agent.profile == Profile::Cursor {
            let entry = self
                .cursor
                .as_ref()?
                .iter()
                .find(|entry| entry.distro.as_deref() == distro)?;
            let name = entry
                .usage
                .email
                .clone()
                .unwrap_or_else(|| usage::unnamed(true, cx));
            (name, &entry.usage.limits)
        } else {
            let account = self.account(agent.provider?, distro, cx)?;
            (usage::account_name(account, cx), &account.usage.limits)
        };
        let usage = usage::problem(limits, cx).unwrap_or_else(|| {
            usage::inline_marks(limits, cx)
                .into_iter()
                .map(|(label, used)| format!("{label} {used}%"))
                .collect::<Vec<_>>()
                .join(" · ")
        });
        let place = short_place(&self.at, lang);
        let key = if multi {
            Str::NewNodeAccountAt
        } else {
            Str::NewNodeAccount
        };
        let line = key.with(
            lang,
            &[("name", &name), ("place", &place), ("usage", &usage)],
        );
        Some(line.trim_end_matches(" · ").to_owned())
    }

    fn sign_in_request(
        &self,
        id: &AgentId,
        distro: Option<&str>,
        cx: &App,
    ) -> Option<login::Request> {
        if matches!(self.at, PlaceView::Ssh { .. }) {
            return None;
        }
        let agent = self.agents.as_ref()?.iter().find(|agent| &agent.id == id)?;
        (agent.profile == Profile::Cursor).then(|| login::Request::cursor(distro, &agent.label, cx))
    }

    fn open_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(base), Some((agent, request))) = (
            self.store.read(cx).base().map(str::to_owned),
            self.sign_in.clone(),
        ) else {
            return;
        };
        if self.login_dialog.is_some() {
            return;
        }
        let dialog = login::Dialog::open(base, request, window, cx);
        cx.subscribe(&dialog, move |this, _, outcome, cx| {
            this.login_dialog = None;
            if matches!(outcome, login::Finished::Succeeded) {
                this.launch(Some(agent.clone()), cx);
            }
            cx.notify();
        })
        .detach();
        self.login_dialog = Some(dialog);
        cx.notify();
    }

    fn launch(&mut self, agent: Option<AgentId>, cx: &mut Context<Self>) {
        if self.busy || self.scanning || self.login_dialog.is_some() {
            return;
        }
        self.picked = Some(agent.clone());
        self.busy = true;
        self.error = None;
        self.sign_in = None;
        let start = match agent.as_ref() {
            Some(agent) => Start::Agent {
                agent: agent.clone(),
                brief: String::new(),
                choice: ModelChoice::default(),
            },
            None => Start::Command {
                line: String::new(),
            },
        };
        let body = NewNode {
            start,
            place: self.place.clone(),
        };
        let store = self.store.clone();
        let task = store.update(cx, |store, cx| store.create_node(body, cx));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let created = task.await;
            let _ = this.update(cx, |this, cx| match created {
                Ok(id) => {
                    this.store.update(cx, |store, cx| store.open_agent(id, cx));
                    cx.emit(Closed);
                }
                Err(error) => {
                    let lang = *cx.global::<Lang>();
                    if let (Some(agent), Some(LaunchError::NotLoggedIn { distro, .. })) =
                        (agent, error.launch_error())
                        && let Some(request) = this.sign_in_request(&agent, distro.as_deref(), cx)
                    {
                        this.sign_in = Some((agent, request));
                        this.error = Some(Str::UsageLoginRequired.text(lang).to_owned());
                    } else {
                        this.error = Some(error.text(lang));
                    }
                    this.busy = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Render for NewNodeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.global::<Theme>().colors;
        let lang = *cx.global::<Lang>();
        let at = self.place_view();
        let multi = self
            .agents
            .as_ref()
            .and_then(|a| a.first())
            .is_some_and(|a| a.places.len() > 1)
            || !matches!(at, PlaceView::Pc);
        let shown = self.shown(lang, cx);
        let active = self.active_in(&shown);
        let agent = active.as_ref().and_then(|choice| self.agent(choice));
        let account = agent.and_then(|agent| self.account_line(agent, multi, cx));
        let bypass = |agent: &AgentView| {
            agent.can_bypass
                && !self
                    .store
                    .read(cx)
                    .settings()
                    .bypass_off
                    .contains(&agent.id)
        };
        let show_bypass = agent.is_some_and(bypass);
        let rescan = self
            .agents
            .as_ref()
            .is_none_or(|agents| !agents.iter().any(|agent| usable(agent, &at)));
        let count = |installed: bool| {
            shown
                .iter()
                .filter(|c| c.installed == installed && c.agent.is_some())
                .count()
        };
        let heading = |text: String| {
            div()
                .px(px(8.0))
                .pt(px(6.0))
                .pb(px(2.0))
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.ink_3)
                .child(text)
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut scroll_to = 0;
        if shown.is_empty() {
            let text = if self.scanning {
                Str::StatusBarLoading.text(lang)
            } else {
                match lang {
                    Lang::Ko => "일치하는 AI 도구가 없습니다.",
                    Lang::En => "No matching AI tools.",
                }
            };
            rows.push(heading(text.to_owned()).into_any_element());
        }
        for installed in [true, false] {
            let group: Vec<&Choice> = shown.iter().filter(|c| c.installed == installed).collect();
            if group.is_empty() {
                continue;
            }
            let n = count(installed).to_string();
            let title = match (installed, multi) {
                (true, true) => Str::NewNodeInstalledAt
                    .with(lang, &[("n", &n), ("place", &short_place(&at, lang))]),
                (true, false) => Str::NewNodeInstalled.with(lang, &[("n", &n)]),
                (false, _) => Str::NewNodeMissing.with(lang, &[("n", &count(false).to_string())]),
            };
            if count(installed) > 0 {
                rows.push(heading(title).into_any_element());
            }
            for choice in group {
                let key = self.key(choice);
                let selected = active
                    .as_ref()
                    .is_some_and(|a| choice.installed && self.key(a) == key);
                if selected {
                    scroll_to = rows.len();
                }
                let recent = choice.installed && key.is_some() && self.recent == key;
                let row_agent = self.agent(choice);
                let blocked = row_agent.is_some_and(|agent| blocked(agent, &at));
                let tag = if !choice.installed {
                    Some(if blocked {
                        Str::NewNodeServerOnSsh.text(lang).to_owned()
                    } else if multi
                        && row_agent.is_some_and(|a| a.places.iter().any(|p| p.path.is_some()))
                    {
                        Str::PlaceMissingAt.with(lang, &[("place", &tag_place(&at, lang))])
                    } else {
                        Str::NewNodeNotInstalled.text(lang).to_owned()
                    })
                } else if choice.agent.is_none() {
                    Some(Str::NewNodeTerminalOnly.text(lang).to_owned())
                } else {
                    None
                };
                let picked = key.clone();
                let installed = choice.installed;
                let must_select = installed && !selected && row_agent.is_some_and(bypass);
                let homepage = row_agent.map(|agent| agent.homepage.clone());
                let label = self.label(choice, lang);
                let ink = if choice.installed {
                    colors.ink
                } else {
                    colors.ink_3
                };
                rows.push(
                    div()
                        .id(("choice", rows.len()))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(8.0))
                        .py(px(5.0))
                        .rounded(px(5.0))
                        .text_size(px(13.0))
                        .line_height(px(19.5))
                        .text_color(ink)
                        .when(!blocked, |row| {
                            row.cursor_pointer()
                                .hover(move |style| style.bg(colors.surface_hover))
                        })
                        .when(selected, |row| row.bg(colors.accent_wash))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                if must_select {
                                    this.pick(picked.clone(), cx);
                                } else if installed {
                                    this.launch(picked.clone(), cx);
                                } else if let Some(url) = homepage.as_deref().filter(|_| !blocked)
                                    && open_url(url).is_err()
                                {
                                    this.error = Some(
                                        match lang {
                                            Lang::Ko => "설치 페이지를 열 수 없습니다.",
                                            Lang::En => "Could not open the installation page.",
                                        }
                                        .to_owned(),
                                    );
                                    cx.notify();
                                }
                                cx.stop_propagation();
                            }),
                        )
                        .child(agent_mark(
                            key.as_ref().map(|id| id.0.as_str()),
                            &label,
                            px(16.0),
                            colors.ink_2,
                        ))
                        .child(label)
                        .when(recent, |row| {
                            row.child(
                                div()
                                    .px(px(6.0))
                                    .rounded(px(9.0))
                                    .bg(colors.accent_wash)
                                    .text_size(px(11.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(colors.accent)
                                    .child(Str::NewNodeRecent.text(lang)),
                            )
                        })
                        .children(tag.map(|tag| {
                            div()
                                .ml_auto()
                                .text_size(px(11.0))
                                .text_color(colors.ink_3)
                                .child(tag)
                        }))
                        .into_any_element(),
                );
            }
        }
        if self.reveal {
            self.reveal = false;
            self.scroll
                .handle()
                .scroll_to_item(scroll_to.saturating_sub(1));
        }
        let close = div()
            .id("close")
            .size(px(26.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |style| style.bg(colors.surface_hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                    cx.emit(Closed);
                    cx.stop_propagation();
                }),
            )
            .child(icon(Lucide::X, px(14.0), colors.ink_2));
        div()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(14.0))
            .rounded(px(10.0))
            .bg(colors.surface)
            .shadow_lg()
            .occlude()
            .text_size(px(13.0))
            .text_color(colors.ink)
            .child(
                div()
                    .flex()
                    .items_center()
                    .my(px(-4.0))
                    .mr(px(-6.0))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .min_w_0()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(match lang {
                                Lang::Ko => "새 에이전트",
                                Lang::En => "New agent",
                            })
                            .child(
                                div()
                                    .ml(px(6.0))
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(12.0))
                                    .child(fonts::latin_mono(
                                        &self.branch,
                                        colors.ink_3,
                                        FontWeight::MEDIUM,
                                    )),
                            ),
                    )
                    .child(div().ml_auto().flex_none().child(close)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.ink_2)
                            .child(match lang {
                                Lang::Ko => "AI 도구",
                                Lang::En => "AI tool",
                            }),
                    )
                    .child(self.field.clone()),
            )
            .child(
                self.scroll
                    .on_overflow(
                        div()
                            .id("choices")
                            .max_h(px(LIST))
                            .p(px(4.0))
                            .overflow_y_scroll()
                            .track_scroll(self.scroll.handle())
                            .flex()
                            .flex_col()
                            .children(rows),
                    )
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(colors.line_strong)
                    .bg(colors.bg)
                    .overflow_hidden(),
            )
            .children((rescan && !self.scanning).then(|| {
                div().flex().child(
                    button("node-rescan", cx)
                        .child(Str::SettingsRescan.text(lang))
                        .on_click(cx.listener(|this, _, _, cx| this.scan(cx))),
                )
            }))
            .children(account.map(|line| {
                div()
                    .text_size(px(12.0))
                    .text_color(colors.ink_2)
                    .child(line)
            }))
            .children(show_bypass.then(|| {
                div()
                    .id("node-bypass")
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(colors.accent)
                    .hover(|style| style.underline())
                    .on_click(cx.listener(|_, _, window, cx| {
                        cx.emit(Closed);
                        window.defer(cx, |window, cx| {
                            want_agents(cx);
                            window.dispatch_action(Box::new(crate::views::shell::OpenSettings), cx);
                        });
                    }))
                    .child(Str::NewNodeBypass.text(lang))
            }))
            .children(match (&at, self.busy) {
                (PlaceView::Wsl { distro }, true) => Some(
                    div()
                        .text_size(px(12.0))
                        .text_color(colors.ink_3)
                        .child(Str::PlaceStarting.with(lang, &[("distro", distro)])),
                ),
                _ => None,
            })
            .children(self.error.clone().map(|error| {
                div()
                    .text_size(px(12.0))
                    .text_color(colors.err)
                    .child(error)
            }))
            .children(self.sign_in.as_ref().map(|_| {
                div().flex().child(
                    button("node-login", cx)
                        .child(match lang {
                            Lang::Ko => "로그인하기",
                            Lang::En => "Sign in",
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.open_login(window, cx))),
                )
            }))
            .children(self.login_dialog.clone())
    }
}
