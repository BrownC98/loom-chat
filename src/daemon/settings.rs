use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, State};
use axum::response::{Html, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use futures_util::future::join_all;
use portable_pty::CommandBuilder;
use serde::Deserialize;
use tokio::sync::watch;

use api::settings::accounts::{AccountView, LoginId, LoginState, LoginTarget, PlaceUsage};

use super::nodes::{PtyInput, Typed, serve_pty};
use super::origin::{self, ViaRemote};
use super::ssh_hosts::{self, Reach};
use super::{App, internal};
use crate::accounts::Site;
use crate::distro::Host;
use crate::node_host::{Exit, NodeHost, STOP_WAIT};
use crate::place::{self, Machine};
use crate::profile::{self, Accounted};
use crate::settings::PatchProblem;
use crate::worktree;
use crate::wsl::{self, Inside};
use api::graph::errors::{LaunchError, Problem};
use api::ids::AccountId;
use api::profile::{AgentView, Profile, Provider, VersionMismatch};
use api::repos::RepoProblem;
use api::settings::SettingsView;

pub struct Logins {
    host: NodeHost<LoginId>,
    sessions: Mutex<HashMap<LoginId, LoginProcess>>,
    next: AtomicU32,
}

#[derive(PartialEq, Eq)]
struct LoginKey {
    command: &'static str,
    distro: Option<String>,
    account: Option<AccountId>,
}

impl LoginKey {
    fn new(site: &Site, command: &'static str, account: Option<&AccountId>) -> Self {
        Self {
            command,
            distro: site.distro().map(str::to_owned),
            account: account.map(|id| AccountId(id.0.to_ascii_lowercase())),
        }
    }
}

struct LoginProcess {
    key: LoginKey,
    exit: watch::Sender<Option<Exit>>,
}

fn login_stopped(exit: Option<Exit>) -> bool {
    matches!(exit, Some(Exit::Command(_) | Exit::Setup(_)))
}

impl Logins {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self {
            host: NodeHost::new(dir),
            sessions: Mutex::default(),
            next: AtomicU32::default(),
        }
    }

    fn reserve(&self, key: LoginKey) -> Result<(LoginId, watch::Sender<Option<Exit>>), Problem> {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions
            .values()
            .any(|process| process.key == key && !login_stopped(*process.exit.borrow()))
        {
            return Err(internal(
                "Wait for the previous login for this account to stop, then try again.",
            ));
        }
        let id = LoginId(self.next.fetch_add(1, Ordering::Relaxed));
        let (exit, _) = watch::channel(None);
        sessions.insert(
            id,
            LoginProcess {
                key,
                exit: exit.clone(),
            },
        );
        Ok((id, exit))
    }
}

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/", get(index))
        .route("/api/agents", get(agents))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/settings/default", get(default_settings))
        .route("/api/accounts", get(accounts))
        .route("/api/usage/{profile}", get(place_usage))
        .route("/api/usage/{profile}/login", post(login_single))
        .route("/api/accounts/{provider}", post(login))
        .route("/api/accounts/{provider}/logout", post(logout))
        .route("/api/accounts/{provider}/{id}", delete(remove_account))
        .route("/api/logins/{id}", get(login_state).delete(close_login))
        .route(
            "/api/logins/{id}/pty",
            get(login_pty).layer(axum::middleware::from_fn(origin::from_page)),
        )
}

async fn agents(State(app): State<Arc<App>>) -> Json<Vec<AgentView>> {
    let (sites, ssh) = tokio::join!(app.sites(), app.ssh_insides());
    let insides: Vec<_> = sites.iter().filter_map(Site::inside).chain(&ssh).collect();
    let warnings: Vec<_> = std::iter::once(profile::version_warnings(&app.versions))
        .chain(join_all(insides.iter().map(|inside| inside_version_warnings(inside))).await)
        .collect();
    let mut agents = profile::agents(&insides);
    agents.sort_by_cached_key(|a| a.label.to_lowercase());
    for agent in &mut agents {
        let profile = agent.profile;
        for (place, warnings) in agent.places.iter_mut().zip(&warnings) {
            place.version_warning = warnings.get(&profile).cloned();
        }
    }
    Json(agents)
}

async fn inside_version_warnings(inside: &Inside) -> HashMap<Profile, VersionMismatch> {
    let reads = profile::pinned().into_iter().filter_map(|(profile, cmd)| {
        let program = inside.find(&[cmd.to_owned()])?;
        let run = inside.run(&[program, "--version"], &[] as &[(&str, &str)]);
        Some(async move {
            let ran = tokio::task::spawn_blocking(move || run.output(crate::usage::PROBE_TIMEOUT))
                .await
                .ok()?
                .ok()?;
            profile::version_warning(profile, &ran.stdout).map(|w| (profile, w))
        })
    });
    join_all(reads).await.into_iter().flatten().collect()
}

pub(super) async fn index(State(app): State<Arc<App>>) -> Result<Html<String>, Problem> {
    let settings = app.settings.get().map_err(internal)?;
    let page = super::Assets::get("index.html")
        .ok_or_else(|| internal("web/dist has no index.html. Run `npm run build`."))?;
    Ok(Html(with_settings(
        &String::from_utf8_lossy(&page.data),
        &settings,
    )))
}

fn with_settings(page: &str, settings: &SettingsView) -> String {
    let theme = serde_json::to_string(&settings.theme).expect("Theme은 늘 직렬화된다");
    let language = serde_json::to_string(&settings.language).expect("Language는 늘 직렬화된다");
    page.replacen(
        "<html",
        &format!("<html data-theme={theme} data-language={language}"),
        1,
    )
}

async fn get_settings(State(app): State<Arc<App>>) -> Result<Json<SettingsView>, Problem> {
    Ok(Json(shown(app.settings.get().map_err(internal)?)))
}

async fn default_settings() -> Json<SettingsView> {
    Json(shown(SettingsView::default()))
}

pub(super) fn shown(mut view: SettingsView) -> SettingsView {
    if view.workspace_dir.is_none() {
        view.workspace_dir = worktree::default_workspace()
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
    }
    view
}

async fn put_settings(
    State(app): State<Arc<App>>,
    Json(patch): Json<serde_json::Value>,
) -> Result<Json<()>, Problem> {
    absolute_dirs(&patch)?;
    match app.settings.patch(&patch) {
        Ok(_) => {}
        Err(PatchProblem::Db(e)) => return Err(internal(e)),
        Err(PatchProblem::Shape(e)) => return Err(internal(e)),
    }
    app.graph.publish_view_changed();
    Ok(Json(()))
}

pub(super) fn absolute_dirs(patch: &serde_json::Value) -> Result<(), Problem> {
    let wsl_dirs = patch
        .get("wslWorkspaceDirs")
        .and_then(|dirs| dirs.as_object())
        .into_iter()
        .flat_map(|dirs| dirs.values());
    let dirs = patch.get("workspaceDir").into_iter().chain(wsl_dirs);
    if let Some(dir) = dirs
        .filter_map(|dir| dir.as_str())
        .find(|dir| !std::path::Path::new(dir).is_absolute())
    {
        return Err(Problem::Repo(RepoProblem::NotAbsolute(dir.to_owned())));
    }
    Ok(())
}

async fn accounts(State(app): State<Arc<App>>) -> Result<Json<Vec<AccountView>>, Problem> {
    let sites: Vec<_> = app.sites().await.into_iter().map(Arc::new).collect();
    let mut rows = Vec::new();
    for site in &sites {
        for accounted in profile::with_accounts() {
            let ids = site.accounts().list(accounted.cli).map_err(internal)?;
            rows.extend(
                std::iter::once(None)
                    .chain(ids.into_iter().map(Some))
                    .map(|id| (site.clone(), accounted, id)),
            );
        }
    }
    let probes = rows
        .iter()
        .map(|(site, a, id)| app.usage.read(site, a.profile, a.meter, id.clone()));
    let usages = join_all(probes).await;
    let views = rows
        .into_iter()
        .zip(usages)
        .map(|((site, a, id), usage)| AccountView {
            provider: a.cli.provider,
            distro: site.distro().map(str::to_owned),
            id,
            usage,
        })
        .collect();
    Ok(Json(views))
}

async fn place_usage(
    State(app): State<Arc<App>>,
    Path(profile): Path<Profile>,
) -> Json<Vec<PlaceUsage>> {
    let Some(meter) = profile::meter(profile) else {
        return Json(Vec::new());
    };
    let sites: Vec<_> = app.sites().await.into_iter().map(Arc::new).collect();
    let probes = sites.iter().map(|site| async {
        PlaceUsage {
            distro: site.distro().map(str::to_owned),
            usage: app.usage.read(site, profile, meter, None).await,
        }
    });
    Json(join_all(probes).await)
}

fn accounted(provider: Provider) -> Result<Accounted, Problem> {
    profile::of_provider(provider).ok_or_else(|| internal("no agent adapter has these accounts"))
}

#[derive(Deserialize)]
struct At {
    distro: Option<String>,
}

async fn remove_account(
    State(app): State<Arc<App>>,
    Path((provider, id)): Path<(Provider, AccountId)>,
    Query(at): Query<At>,
) -> Result<Json<()>, Problem> {
    let cli = accounted(provider)?.cli;
    let site = app.site_of(at.distro.as_deref()).await?;
    site.accounts().remove(cli, &id).map_err(internal)?;
    Ok(Json(()))
}

async fn login(
    State(app): State<Arc<App>>,
    Path(provider): Path<Provider>,
    Query(at): Query<At>,
    Json(target): Json<LoginTarget>,
) -> Result<Json<LoginId>, Problem> {
    let cli = accounted(provider)?.cli;
    let site = Arc::new(app.site_of(at.distro.as_deref()).await?);
    site.login(cli, None)?;
    let (id, created) = match target {
        LoginTarget::System => (None, false),
        LoginTarget::Account(id) => (Some(id), false),
        LoginTarget::New => (Some(site.accounts().create(cli).map_err(internal)?), true),
    };
    if let Some(id) = &id
        && site.accounts().config_dir(cli, Some(id)).is_none()
    {
        return Err(internal("the account folder is gone"));
    }
    let key = LoginKey::new(&site, cli.command, id.as_ref());
    let discard = {
        let site = site.clone();
        let id = id.clone();
        move || {
            if created && let Some(id) = &id {
                let _ = site.accounts().remove(cli, id);
            }
        }
    };
    start_login(app, key, || site.login(cli, id.as_ref()), discard).map(Json)
}

async fn login_single(
    State(app): State<Arc<App>>,
    Path(profile): Path<Profile>,
    Query(at): Query<At>,
) -> Result<Json<LoginId>, Problem> {
    let (label, command) = profile::single_login_cli(profile).ok_or(Problem::NoLogin)?;
    let site = app.site_of(at.distro.as_deref()).await?;
    let key = LoginKey::new(&site, command, None);
    start_login(app, key, || site.login_installed(label, command), || {}).map(Json)
}

fn start_login(
    app: Arc<App>,
    key: LoginKey,
    build: impl FnOnce() -> Result<CommandBuilder, LaunchError>,
    discard: impl Fn() + Clone + Send + 'static,
) -> Result<LoginId, Problem> {
    let (login, exit_state) = app.logins.reserve(key).inspect_err(|_| discard())?;
    let on_exit = {
        let app = app.clone();
        let discard = discard.clone();
        move |exit| {
            if exit != Exit::Command(0) {
                discard();
            }
            exit_state.send_replace(Some(exit));
            app.graph.publish_view_changed();
        }
    };
    build()
        .map_err(Problem::from)
        .and_then(|cmd| {
            app.logins
                .host
                .spawn_while_linked(login, &cmd, on_exit)
                .map_err(internal)
        })
        .map(|()| login)
        .inspect_err(|_| {
            discard();
            app.logins.sessions.lock().unwrap().remove(&login);
        })
}

async fn logout(
    State(app): State<Arc<App>>,
    Path(provider): Path<Provider>,
    Query(at): Query<At>,
) -> Result<Json<()>, Problem> {
    let cli = accounted(provider)?.cli;
    let mut cmd = app.site_of(at.distro.as_deref()).await?.logout(cli)?;
    crate::timed::hide_console(&mut cmd);
    let out = tokio::task::spawn_blocking(move || cmd.stdin(std::process::Stdio::null()).output())
        .await
        .map_err(internal)?
        .map_err(internal)?;
    if !out.status.success() {
        return Err(internal(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(Json(()))
}

impl App {
    pub(super) fn pc_site(&self) -> Site {
        Site::pc(self.accounts_root.clone())
    }

    pub(super) async fn sites(&self) -> Vec<Site> {
        let running = wsl::running().await;
        let mut found: BTreeMap<String, wsl::WslPath> = BTreeMap::new();
        for (_, path) in self.repos.list().unwrap_or_default() {
            match place::of(&path) {
                Machine::Wsl(at) if running.contains(&at.distro) => {
                    found.entry(at.distro.clone()).or_insert(at);
                }
                Machine::Pc | Machine::Wsl(_) | Machine::Ssh(_) => {}
            }
        }
        let commands = profile::catalog_commands();
        let read = found.values().map(|at| async {
            let distro = self.host(Host::Distro(at.distro.clone()))?;
            wsl::inside(at, distro, &commands, &self.shim, &[]).await
        });
        let mut sites = vec![self.pc_site()];
        for inside in join_all(read).await {
            match inside {
                Ok(inside) => sites.push(Site::wsl(inside)),
                Err(e) => crate::log::logln!("loom: WSL 위치를 읽지 못했다: {e}"),
            }
        }
        sites
    }

    async fn ssh_insides(&self) -> Vec<Inside> {
        let mut found: BTreeMap<String, place::SshPath> = BTreeMap::new();
        for (_, path) in self.repos.list().unwrap_or_default() {
            if let Machine::Ssh(at) = place::of(&path)
                && let Some(Reach::Reached(_)) = self.ssh_reach(&at.host)
            {
                found.entry(ssh_hosts::key(&at.host)).or_insert(at);
            }
        }
        let commands = profile::catalog_commands();
        let read = found.values().map(|at| async {
            let (distro, remote) = self.ssh_host(at).await?;
            wsl::inside_ssh(at.url(""), &at.path, distro, &remote, &commands, Vec::new()).await
        });
        let mut insides = Vec::new();
        for inside in join_all(read).await {
            match inside {
                Ok(inside) => insides.push(inside),
                Err(e) => crate::log::logln!("loom: SSH 위치를 읽지 못했다: {e}"),
            }
        }
        insides
    }

    pub(super) async fn site_of(&self, distro: Option<&str>) -> Result<Site, Problem> {
        let Some(distro) = distro else {
            return Ok(self.pc_site());
        };
        let repos = self.repos.list().map_err(internal)?;
        let path = repos
            .iter()
            .map(|(_, path)| path)
            .find(|path| matches!(place::of(path), Machine::Wsl(at) if at.distro == distro))
            .ok_or_else(|| internal(format!("no repository in WSL · {distro}")))?;
        self.site_at(path).await
    }

    pub(super) async fn site_at(&self, cwd: &std::path::Path) -> Result<Site, Problem> {
        Ok(match place::of(cwd) {
            Machine::Wsl(at) => {
                let distro = self
                    .host(Host::Distro(at.distro.clone()))
                    .map_err(LaunchError::Wsl)?;
                let commands: Vec<_> = profile::login_commands().collect();
                let inside = wsl::inside(&at, distro, &commands, &self.shim, &[]);
                Site::wsl(inside.await.map_err(LaunchError::Wsl)?)
            }
            Machine::Pc => self.pc_site(),
            Machine::Ssh(at) => {
                let (distro, remote) = self.ssh_host(&at).await.map_err(LaunchError::Wsl)?;
                let commands: Vec<_> = profile::with_accounts().map(|a| a.cli.command).collect();
                let inside =
                    wsl::inside_ssh(at.url(""), &at.path, distro, &remote, &commands, Vec::new());
                Site::wsl(inside.await.map_err(LaunchError::Wsl)?)
            }
        })
    }
}

async fn login_state(
    State(app): State<Arc<App>>,
    Path(id): Path<u32>,
) -> Result<Json<LoginState>, Problem> {
    let sessions = app.logins.sessions.lock().unwrap();
    let exit = *sessions
        .get(&LoginId(id))
        .ok_or(Problem::NoLogin)?
        .exit
        .borrow();
    Ok(Json(match exit {
        None => LoginState::Running,
        Some(Exit::Command(0)) => LoginState::Succeeded,
        Some(_) => LoginState::Failed,
    }))
}

async fn close_login(
    State(app): State<Arc<App>>,
    Path(id): Path<u32>,
) -> Result<Json<()>, Problem> {
    let id = LoginId(id);
    let exit = app
        .logins
        .sessions
        .lock()
        .unwrap()
        .get(&id)
        .map(|process| process.exit.subscribe());
    app.logins.host.kill(id);
    let Some(mut exit) = exit else {
        return Ok(Json(()));
    };
    let confirmed = tokio::time::timeout(STOP_WAIT, exit.wait_for(Option::is_some))
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|exit| login_stopped(*exit));
    if !confirmed {
        return Err(internal("could not confirm that the login process stopped"));
    }
    app.logins.sessions.lock().unwrap().remove(&id);
    Ok(Json(()))
}

async fn login_pty(
    ws: WebSocketUpgrade,
    Path(id): Path<u32>,
    State(app): State<Arc<App>>,
    via: Option<Extension<ViaRemote>>,
) -> Response {
    serve_pty(
        ws,
        via,
        app,
        |app| &app.logins.host,
        PtyInput {
            typed: |_, _, _, _| Typed::Write,
            composing: |_, _, _| {},
        },
        LoginId(id),
    )
}
