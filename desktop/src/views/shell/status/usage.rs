use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use api::graph::GraphEvent;
use api::graph::events::{Entry, SwitchCause};
use api::ids::{AccountId, RepoId};
use api::profile::{Profile, Provider};
use api::repos::PlaceView;
use api::settings::accounts::{AccountView, PlaceUsage};
use api::settings::usage::Limits;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Div, Entity, FocusHandle, FontFeatures,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Window, canvas, div,
};
use serde::de::DeserializeOwned;

use crate::fonts;
use crate::link::{self, LinkState, VERSION};
use crate::scale::px;
use crate::settings::{keep_focus, login, modal, settings_patch, want_about};
use crate::store::{AgentsScanned, Store};
use crate::strings::{Lang, Str};
use crate::theme::{Colors, Theme};
use crate::views::dock::ui::{say, say_with};
use crate::views::icons::{Lucide, agent_mark, icon};
use crate::views::tooltip::tip;
use crate::views::usage::{
    self, ACCOUNTS, AccountsChanged, PROVIDERS, REFRESH, Switching, account_name, active_index,
};

const NAME_W: f32 = 220.0;
const METER_W: f32 = 36.0;
const MARK: f32 = 14.0;

mod popover;

pub struct Usage {
    store: Entity<Store>,
    pub(super) repo: Option<RepoId>,
    accounts: Option<Vec<AccountView>>,
    cursor: Option<Vec<PlaceUsage>>,
    error: Option<String>,
    catalog_error: Option<String>,
    scanning: bool,
    pending: u8,
    ticket: u64,
    open: bool,
    expanded: Option<Provider>,
    switching: Option<Switching>,
    login: Option<Entity<login::Dialog>>,
    focus: FocusHandle,
    dialog_focus: FocusHandle,
    restore: Option<FocusHandle>,
    anchor: Rc<Cell<Bounds<Pixels>>>,
}

impl Usage {
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        cx.observe_global::<AccountsChanged>(|this, cx| this.refresh(cx))
            .detach();
        cx.subscribe(&store, |this, _, _: &AgentsScanned, cx| {
            this.catalog_error = None;
            this.refresh(cx);
        })
        .detach();
        cx.subscribe(&store, |this, _, event: &GraphEvent, cx| {
            this.feed(event, cx)
        })
        .detach();
        cx.subscribe(&store, |this, _, state: &LinkState, cx| {
            if *state == LinkState::Connected {
                if this.store.read(cx).agent_catalog().is_none() {
                    this.scan(cx);
                }
                if this.accounts.is_none() || this.error.is_some() {
                    this.refresh(cx);
                }
            }
        })
        .detach();
        let mut usage = Self {
            store,
            repo: None,
            accounts: None,
            cursor: None,
            error: None,
            catalog_error: None,
            scanning: false,
            pending: 0,
            ticket: 0,
            open: false,
            expanded: None,
            switching: None,
            login: None,
            focus: cx.focus_handle(),
            dialog_focus: cx.focus_handle(),
            restore: None,
            anchor: Rc::default(),
        };
        usage.scan(cx);
        cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(REFRESH).await;
            }
        })
        .detach();
        usage
    }

    fn base(&self, cx: &App) -> Option<String> {
        self.store.read(cx).base().map(str::to_owned)
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.store.read(cx).base().is_none() {
            return;
        }
        self.ticket += 1;
        self.pending = 2;
        self.error = None;
        self.read(ACCOUNTS, |this, value| this.accounts = Some(value), cx);
        self.read(
            login::CURSOR_USAGE,
            |this, value| this.cursor = Some(value),
            cx,
        );
        cx.notify();
    }

    fn read<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &'static str,
        keep: fn(&mut Self, T),
        cx: &mut Context<Self>,
    ) {
        let task = self
            .store
            .read(cx)
            .call(cx, move |base| link::read(base, path));
        let ticket = self.ticket;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.ticket != ticket {
                    return;
                }
                this.pending -= 1;
                match result {
                    Ok(value) => keep(this, value),
                    Err(error) => {
                        this.error.get_or_insert(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn scan(&mut self, cx: &mut Context<Self>) {
        if self.scanning || self.store.read(cx).base().is_none() {
            return;
        }
        self.scanning = true;
        self.catalog_error = None;
        let scan = self.store.update(cx, |store, cx| store.agents(cx));
        cx.spawn(async move |this, cx| {
            let result = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.catalog_error = result.err();
                this.scanning = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn installed(&self, profile: Profile, distro: Option<&str>, cx: &App) -> bool {
        self.store
            .read(cx)
            .agent_catalog()
            .into_iter()
            .flatten()
            .any(|agent| {
                agent.profile == profile
                    && agent.places.iter().any(|at| {
                        at.path.is_some()
                            && match &at.place {
                                PlaceView::Pc => distro.is_none(),
                                PlaceView::Wsl { distro: name } => distro == Some(name.as_str()),
                                PlaceView::Ssh { .. } => false,
                            }
                    })
            })
    }

    fn feed(&mut self, event: &GraphEvent, cx: &mut Context<Self>) {
        let GraphEvent::Recorded { record } = event else {
            return;
        };
        if matches!(&record.entry, Entry::AccountSwitched { why, .. } if *why != SwitchCause::Manual)
        {
            self.refresh(cx);
        }
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(window, cx);
            return;
        }
        if self.pending == 0 {
            self.refresh(cx);
        }
        if self.store.read(cx).agent_catalog().is_none() {
            self.scan(cx);
        }
        self.restore = window.focused(cx);
        window.focus(&self.focus, cx);
        self.open = true;
        cx.notify();
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        if self.base(cx).is_none() {
            self.store.update(cx, |store, cx| store.reload(cx));
        } else {
            if self.pending == 0 {
                self.refresh(cx);
            }
            self.scan(cx);
        }
    }

    fn open_login(&mut self, request: login::Request, window: &mut Window, cx: &mut Context<Self>) {
        let Some(base) = self.base(cx) else {
            return;
        };
        self.close(window, cx);
        let dialog = login::Dialog::open(base, request, window, cx);
        cx.subscribe(&dialog, |this, _, _: &login::Finished, cx| {
            this.login = None;
            cx.notify();
        })
        .detach();
        self.login = Some(dialog);
        cx.notify();
    }

    fn give_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.restore.take() {
            window.focus(&handle, cx);
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        if self.switching.is_none() {
            self.give_back(window, cx);
        }
        cx.notify();
    }

    fn end_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switching = None;
        self.give_back(window, cx);
        cx.notify();
    }

    fn distro(&self, cx: &App) -> Option<String> {
        let store = self.store.read(cx);
        let snapshot = store.snapshot()?;
        let saved = store.settings().repo;
        let repo = self
            .repo
            .and_then(|id| snapshot.repos.iter().find(|r| r.id == id))
            .or_else(|| snapshot.repos.iter().find(|r| Some(r.id) == saved))
            .or_else(|| snapshot.repos.first())?;
        let PlaceView::Wsl { distro } = &repo.place else {
            return None;
        };
        let known = self
            .accounts
            .as_ref()
            .is_some_and(|all| all.iter().any(|a| a.distro.as_ref() == Some(distro)));
        known.then(|| distro.clone())
    }

    fn own<'a>(&'a self, provider: Provider, distro: Option<&str>) -> Vec<&'a AccountView> {
        self.accounts
            .iter()
            .flatten()
            .filter(|a| a.provider == provider && a.distro.as_deref() == distro)
            .collect()
    }

    fn active<'a>(
        &'a self,
        provider: Provider,
        distro: Option<&str>,
        cx: &App,
    ) -> Option<&'a AccountView> {
        let own = self.own(provider, distro);
        let chosen = self.store.read(cx).settings().account(provider, distro);
        active_index(&own, chosen).map(|index| own[index])
    }

    fn choose(
        &mut self,
        provider: Provider,
        distro: Option<String>,
        to: Option<AccountId>,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(base) = self.base(cx) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let (found, id) = (distro.clone(), to.clone());
            let nodes = cx
                .background_spawn(async move {
                    usage::movable_nodes(&base, provider, found.as_deref(), id.as_ref())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if nodes.is_empty() {
                    this.save_account(provider, distro.as_deref(), to, cx);
                    return;
                }
                this.open = false;
                this.switching = Some(Switching {
                    provider,
                    distro,
                    to,
                    name,
                    nodes,
                    busy: false,
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn account_patch(
        &self,
        provider: Provider,
        distro: Option<&str>,
        to: Option<AccountId>,
        cx: &App,
    ) -> Option<serde_json::Value> {
        let before = self.store.read(cx).settings();
        let mut after = before.clone();
        after.set_account(provider, distro, to);
        settings_patch(before, &after)
    }

    fn save_account(
        &mut self,
        provider: Provider,
        distro: Option<&str>,
        to: Option<AccountId>,
        cx: &mut Context<Self>,
    ) {
        if let Some(patch) = self.account_patch(provider, distro, to, cx) {
            self.store
                .update(cx, |store, cx| store.save_settings(patch, cx));
        }
    }

    fn move_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(base) = self.base(cx) else {
            return;
        };
        let Some(switching) = &mut self.switching else {
            return;
        };
        switching.busy = true;
        let (provider, distro, to) = (
            switching.provider,
            switching.distro.clone(),
            switching.to.clone(),
        );
        cx.notify();
        let patch = self.account_patch(provider, distro.as_deref(), to, cx);
        cx.spawn_in(window, async move |this, cx| {
            let done = cx
                .background_spawn(async move {
                    usage::switch_all(&base, patch.as_ref(), provider, distro.as_deref())
                })
                .await;
            let lang = cx.update(|_, cx| *cx.global::<Lang>()).unwrap_or(Lang::En);
            let _ = this.update_in(cx, |this, window, cx| {
                this.end_switch(window, cx);
                this.store.update(cx, |store, cx| {
                    store.reload(cx);
                    if let Err(error) = &done {
                        store.tell(error.text(lang), cx);
                    }
                });
                this.refresh(cx);
            });
        })
        .detach();
    }
}

fn text(label: impl Into<SharedString>) -> Div {
    div().flex_none().whitespace_nowrap().child(label.into())
}

fn install_label(cx: &App) -> &'static str {
    match cx.global::<Lang>() {
        Lang::Ko => "에이전트 설치",
        Lang::En => "Install an agent",
    }
}

fn name_cell(name: String) -> Div {
    div().flex_none().max_w(px(NAME_W)).truncate().child(name)
}

fn inline_marks(limits: &Limits, c: &Colors, cx: &App) -> Vec<Div> {
    usage::inline_marks(limits, cx)
        .into_iter()
        .map(|(label, used)| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(5.0))
                .font_features(FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
                .child(div().w(px(METER_W)).flex().child(usage::meter(used, c)))
                .child(text(format!("{label} {used}%")))
        })
        .collect()
}

fn segment(name: String, limits: &Limits, c: &Colors, cx: &App) -> Vec<Div> {
    let mut parts = vec![name_cell(name)];
    parts.extend(inline_marks(limits, c, cx));
    if matches!(limits, Limits::Failed { .. }) {
        parts.push(text(say(cx, Str::StatusBarUnavailable)).text_color(c.err));
    }
    parts
}

fn provider_mark(provider: Provider, c: &Colors) -> AnyElement {
    let (id, label) = match provider {
        Provider::Claude => ("claude", "Claude"),
        Provider::Codex => ("codex", "Codex"),
    };
    agent_mark(Some(id), label, px(MARK), c.ink_2)
}

fn cursor_mark(c: &Colors) -> AnyElement {
    agent_mark(Some("cursor"), "Cursor", px(MARK), c.ink_2)
}

fn chevron(up: bool, c: &Colors) -> gpui::Svg {
    icon(
        if up {
            Lucide::ChevronUp
        } else {
            Lucide::ChevronDown
        },
        px(MARK),
        c.ink_3,
    )
}

fn separator(c: &Colors) -> Div {
    div()
        .w(px(1.0))
        .h(px(14.0))
        .mx(px(2.0))
        .flex_none()
        .bg(c.line)
}

impl Render for Usage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.global::<Theme>().colors;
        let distro = self.distro(cx);
        let place = distro.as_deref();
        let this = cx.entity();

        let signed = PROVIDERS.into_iter().filter_map(|provider| {
            let account = self.active(provider, place, cx)?;
            Some((
                match provider {
                    Provider::Claude => Profile::Claude,
                    Provider::Codex => Profile::Codex,
                },
                provider_mark(provider, &c),
                account_name(account, cx),
                &account.usage.limits,
            ))
        });
        let cursor = self
            .cursor
            .iter()
            .flatten()
            .find(|entry| entry.distro.as_deref() == place)
            .map(|entry| {
                (
                    Profile::Cursor,
                    cursor_mark(&c),
                    entry
                        .usage
                        .email
                        .clone()
                        .unwrap_or_else(|| usage::unnamed(true, cx)),
                    &entry.usage.limits,
                )
            });
        let mut summary: Vec<AnyElement> = Vec::new();
        let mut needs_login = false;
        for (profile, mark, name, limits) in signed.chain(cursor) {
            if login::needed(limits) || usage::cli_missing(limits) {
                needs_login |= login::needed(limits) && self.installed(profile, place, cx);
                continue;
            }
            if !summary.is_empty() {
                summary.push(separator(&c).into_any_element());
            }
            summary.push(mark);
            summary.extend(
                segment(name, limits, &c, cx)
                    .into_iter()
                    .map(IntoElement::into_any_element),
            );
        }
        let store = self.store.read(cx);
        let no_agents = store.no_tools();
        let label = if no_agents
            && store
                .snapshot()
                .is_some_and(|snapshot| snapshot.nodes.is_empty())
        {
            Some(match cx.global::<Lang>() {
                Lang::Ko => "설치된 AI 도구 없음",
                Lang::En => "No AI tools installed",
            })
        } else if no_agents {
            Some(install_label(cx))
        } else if needs_login {
            Some(say(cx, Str::StatusBarConnectAccount))
        } else if summary.is_empty() {
            Some(say(
                cx,
                if self.pending > 0 || self.scanning {
                    Str::StatusBarLoading
                } else {
                    Str::StatusBarUnavailable
                },
            ))
        } else {
            None
        };
        if no_agents {
            summary.clear();
        }
        if let Some(label) = label {
            if !summary.is_empty() {
                summary.insert(0, separator(&c).into_any_element());
            }
            summary.insert(0, text(label).into_any_element());
        }

        let open = self.open;
        let mut button = div()
            .id("usage-button")
            .flex_none()
            .h_full()
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(10.0))
            .overflow_hidden()
            .cursor_pointer()
            .hover(move |style| style.bg(c.surface_hover).text_color(c.ink))
            .tooltip(tip(say_with(
                cx,
                Str::StatusBarButtonLabel,
                &[(
                    "summary",
                    label.unwrap_or_else(|| say(cx, Str::StatusBarUsage)),
                )],
            )))
            .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx)))
            .children(summary);
        if open {
            button = button.bg(c.accent_wash).text_color(c.ink);
        }
        if self.pending > 0 {
            button = button.opacity(0.7);
        }

        let refresh = div()
            .id("usage-refresh")
            .w(px(26.0))
            .h_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .when(self.pending > 0, |style| style.opacity(0.5))
            .hover(move |style| style.bg(c.surface_hover))
            .tooltip(tip(say(cx, Str::StatusBarRefresh).to_owned()))
            .on_click(cx.listener(|this, _, _, cx| this.retry(cx)))
            .child(icon(Lucide::RotateCw, px(MARK), c.ink_2));

        let mut host = div()
            .relative()
            .h_full()
            .min_w_0()
            .flex()
            .items_center()
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .child(button)
                    .child(
                        canvas(
                            {
                                let anchor = self.anchor.clone();
                                move |bounds, _, _| anchor.set(bounds)
                            },
                            |_, _, _, _| (),
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    ),
            )
            .child(refresh);
        if open {
            host = host.child(self.backdrop(window, cx)).child(self.popover(
                distro.as_deref(),
                &c,
                window,
                cx,
            ));
        }
        if let Some(switching) = self.switching.clone() {
            keep_focus(&self.dialog_focus, window, cx);
            let (keep, all, cancel) = (this.clone(), this.clone(), this);
            let (provider, distro, to) = (
                switching.provider,
                switching.distro.clone(),
                switching.to.clone(),
            );
            let content = usage::switch_content(
                &switching,
                move |window, cx| {
                    let (distro, to) = (distro.clone(), to.clone());
                    keep.update(cx, |this, cx| {
                        this.end_switch(window, cx);
                        this.save_account(provider, distro.as_deref(), to, cx);
                    });
                },
                move |window, cx| all.update(cx, |this, cx| this.move_all(window, cx)),
                cx,
            );
            host = host.child(modal(
                &self.dialog_focus,
                content,
                Rc::new(move |window, cx| {
                    cancel.update(cx, |this, cx| this.end_switch(window, cx))
                }),
                window,
                cx,
            ));
        }
        host.children(self.login.clone())
    }
}

pub fn version(cx: &App) -> Stateful<Div> {
    let c = cx.global::<Theme>().colors;
    text(format!("v{VERSION}"))
        .id("status-about")
        .h_full()
        .flex()
        .items_center()
        .px(px(10.0))
        .font_family(fonts::TERMINAL_FAMILY)
        .cursor_pointer()
        .hover(move |style| style.bg(c.surface_hover).text_color(c.ink))
        .tooltip(tip(say(cx, Str::SettingsTabAbout).to_owned()))
        .on_click(|_, window, cx| {
            want_about(cx);
            window.dispatch_action(Box::new(crate::views::shell::OpenSettings), cx);
        })
}
