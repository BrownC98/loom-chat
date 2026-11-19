use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use api::graph::docs::DocRow;
use api::graph::events::{Entry, SwitchCause};
use api::graph::waits::{AnswerWait, ChooseOption, PressKeys, Prompt, TypeText};
use api::graph::{GraphEvent, NewNode};
use api::ids::{NodeId, RepoId, WaitId};
use api::profile::AgentView;
use api::repos::RepoView;
use api::settings::SettingsView;
use gpui::{
    App, AppContext, Context, EventEmitter, Image, ImageFormat, Task, Window, WindowAppearance,
};

use crate::link::{self, Fresh, Graph, LinkState, Snapshot};
use crate::problem::Failure;
use crate::{scale, strings, theme};

const PROMPT_CHANGED: &str = "promptChanged";
const REPOS_EVERY: Duration = Duration::from_secs(5);

pub struct Store {
    base: Option<String>,
    status: String,
    connection: LinkState,
    watch: Option<link::Watch>,
    repo_watch: Option<Task<()>>,
    snapshot: Option<Snapshot>,
    nodes_at: u64,
    repos_at: u64,
    settings: SettingsView,
    settings_key: String,
    unsaved: VecDeque<serde_json::Value>,
    confirmed: SettingsView,
    settings_from: u64,
    notice: Option<Notice>,
    busy: HashSet<u32>,
}

pub struct Notice {
    pub text: String,
}

pub struct BusyChanged;

pub struct OpenAgent(pub NodeId);

impl EventEmitter<GraphEvent> for Store {}
impl EventEmitter<LinkState> for Store {}
impl EventEmitter<BusyChanged> for Store {}
impl EventEmitter<OpenAgent> for Store {}

fn newer(at: &mut u64, order: u64) -> bool {
    let newer = order > *at;
    if newer {
        *at = order;
    }
    newer
}

fn patched<'a>(
    settings: &SettingsView,
    patches: impl IntoIterator<Item = &'a serde_json::Value>,
) -> Option<SettingsView> {
    let mut value = serde_json::to_value(settings).ok()?;
    for patch in patches {
        api::settings::merge(&mut value, patch);
    }
    serde_json::from_value(value).ok()
}

fn apply_settings(settings: &SettingsView, cx: &mut App) {
    let dark = matches!(
        cx.window_appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    scale::set(settings.ui_size);
    cx.set_global(theme::Theme::resolve(settings, dark));
    cx.set_global(strings::Lang::resolve(
        settings.language,
        &sys_locale::get_locale().unwrap_or_default(),
    ));
}

impl Store {
    pub fn new(
        started: Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = SettingsView::default();
        apply_settings(&settings, cx);
        let (base, status, failure, watch) = match started {
            Ok(base) => {
                let (graph, inbox) = async_channel::unbounded();
                let (watch, failure) = match link::watch(&base, graph) {
                    Ok(watch) => (Some(watch), None),
                    Err(error) => (None, Some(error)),
                };
                let status = failure.clone().unwrap_or_else(|| base.clone());
                cx.spawn_in(window, async move |this, cx| {
                    while let Ok(event) = inbox.recv().await {
                        if this
                            .update_in(cx, |store, _, cx| store.apply(event, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
                (Some(base), status, failure, watch)
            }
            Err(error) => (None, error.clone(), Some(error), None),
        };
        let notice = failure.map(|text| Notice { text });
        let connection = if watch.is_some() {
            LinkState::Reconnecting
        } else {
            LinkState::Lost
        };
        let mut store = Self {
            base,
            status,
            connection,
            watch,
            repo_watch: None,
            snapshot: None,
            nodes_at: 0,
            repos_at: 0,
            settings,
            settings_key: String::new(),
            unsaved: VecDeque::new(),
            confirmed: SettingsView::default(),
            settings_from: 0,
            notice,
            busy: HashSet::new(),
        };
        if let Some(base) = store.base.clone() {
            store.watch_repos(base, window.window_handle(), cx);
        }
        store
    }

    fn watch_repos(&mut self, base: String, window: gpui::AnyWindowHandle, cx: &Context<Self>) {
        self.repo_watch = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REPOS_EVERY).await;
                let Ok(visible) = window.update(cx, |_, window, _| window.is_visible()) else {
                    break;
                };
                if !visible {
                    continue;
                }
                let order = link::order();
                let read_base = base.clone();
                let repos = cx
                    .background_spawn(async move { link::read(&read_base, "/api/repos").ok() })
                    .await;
                if this
                    .update(cx, |store, cx| {
                        if let Some(repos) = repos {
                            store.keep_repos(order, repos, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    pub fn base(&self) -> Option<&str> {
        self.base.as_deref()
    }

    pub fn connection(&self) -> LinkState {
        self.connection
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    pub fn settings(&self) -> &SettingsView {
        &self.settings
    }

    pub fn is_busy(&self, wait: u32) -> bool {
        self.busy.contains(&wait)
    }

    pub fn set_busy(&mut self, wait: u32, busy: bool, cx: &mut Context<Self>) {
        let changed = if busy {
            self.busy.insert(wait)
        } else {
            self.busy.remove(&wait)
        };
        if changed {
            cx.emit(BusyChanged);
        }
    }

    pub fn choose_repo(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        if self.settings.repo == Some(repo) {
            return;
        }
        self.save_settings(serde_json::json!({ "repo": repo }), cx);
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    pub fn tell(&mut self, text: String, cx: &mut Context<Self>) {
        self.notice = Some(Notice { text });
        cx.notify();
    }

    pub fn clear_notice(&mut self, cx: &mut Context<Self>) {
        if self.base.is_some() && self.notice.take().is_some() {
            cx.notify();
        }
    }

    pub fn save_settings(&mut self, patch: serde_json::Value, cx: &mut Context<Self>) {
        if let Some(next) = patched(&self.settings, [&patch]) {
            self.show_settings(next, cx);
        }
        if self.base.is_none() {
            return;
        }
        self.unsaved.push_back(patch);
        if self.unsaved.len() == 1 {
            self.send_settings(cx);
        }
    }

    fn send_settings(&mut self, cx: &mut Context<Self>) {
        let (Some(base), Some(patch)) = (self.base.clone(), self.unsaved.front().cloned()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(async move { link::put_settings(&base, &patch) })
                .await;
            let _ = this.update(cx, |store, cx| {
                let Some(patch) = store.unsaved.pop_front() else {
                    return;
                };
                store.settings_from = link::issued();
                match saved {
                    Ok(()) => {
                        if let Some(confirmed) = patched(&store.confirmed, [&patch]) {
                            store.confirmed = confirmed;
                        }
                    }
                    Err(error) => {
                        let text = error.text(*cx.global::<strings::Lang>());
                        store.tell(text, cx);
                        if let Some(next) = patched(&store.confirmed, &store.unsaved) {
                            store.show_settings(next, cx);
                        }
                    }
                }
                if store.unsaved.is_empty() {
                    store.reload(cx)
                } else {
                    store.send_settings(cx)
                }
            });
        })
        .detach();
    }

    fn show_settings(&mut self, settings: SettingsView, cx: &mut Context<Self>) {
        self.take_settings(settings, cx);
        cx.notify();
    }

    fn take_settings(&mut self, settings: SettingsView, cx: &mut Context<Self>) {
        let key = serde_json::to_string(&settings).unwrap_or_default();
        if key != self.settings_key {
            apply_settings(&settings, cx);
            self.settings = settings;
            self.settings_key = key;
        }
    }

    pub fn open_agent(&self, node: NodeId, cx: &mut Context<Self>) {
        cx.emit(OpenAgent(node));
    }

    pub fn reload(&self, _: &mut Context<Self>) {
        if let Some(watch) = &self.watch {
            watch.reload();
        }
    }

    fn apply(&mut self, event: Graph, cx: &mut Context<Self>) {
        match event {
            Graph::Connection(state) => {
                self.connection = state;
                cx.emit(state);
                return;
            }
            Graph::Nodes(fresh) => {
                if !self.refresh(*fresh) {
                    return;
                }
            }
            Graph::Settings { order, result } => match result {
                Ok(settings) => self.keep_settings(order, *settings, cx),
                Err(error) if order > self.settings_from => self.fail(error, cx),
                Err(_) => return,
            },
            Graph::Feed(event) => {
                if let GraphEvent::Recorded { record } = event.as_ref()
                    && matches!(&record.entry, Entry::AccountSwitched { why, .. } if *why != SwitchCause::Manual)
                {
                    self.reload(cx);
                }
                cx.emit(*event);
                return;
            }
            Graph::Failed { order, error } if order > self.nodes_at => {
                self.fail(error, cx);
            }
            _ => return,
        }
        cx.notify();
    }

    fn fail(&mut self, error: Failure, cx: &Context<Self>) {
        let text = error.text(*cx.global::<strings::Lang>());
        let text = if text.is_empty() {
            "connection closed".to_owned()
        } else {
            text
        };
        self.notice = Some(Notice { text });
    }

    fn keep_settings(&mut self, order: u64, settings: SettingsView, cx: &mut Context<Self>) {
        self.status = self.base.clone().unwrap_or_default();
        if self.unsaved.is_empty() && newer(&mut self.settings_from, order) {
            self.confirmed = settings.clone();
            self.take_settings(settings, cx);
        }
    }

    fn refresh(&mut self, fresh: Fresh) -> bool {
        let snapshot = self.snapshot.get_or_insert_with(|| Snapshot {
            nodes: Vec::new(),
            repos: Vec::new(),
        });
        let mut used = false;
        if newer(&mut self.nodes_at, fresh.order) {
            snapshot.nodes = fresh.nodes;
            used = true;
        }
        if let Some(repos) = fresh.repos
            && newer(&mut self.repos_at, fresh.order)
        {
            snapshot.repos = repos;
            used = true;
        }
        if used {
            self.status = self.base.clone().unwrap_or_default();
        }
        used
    }

    fn keep_repos(&mut self, order: u64, repos: Vec<RepoView>, cx: &mut Context<Self>) {
        let Some(snapshot) = &mut self.snapshot else {
            return;
        };
        if !newer(&mut self.repos_at, order)
            || serde_json::to_string(&snapshot.repos).ok() == serde_json::to_string(&repos).ok()
        {
            return;
        }
        snapshot.repos = repos;
        cx.notify();
    }
}

impl Store {
    pub(crate) fn call<T: Send + 'static>(
        &self,
        cx: &App,
        work: impl FnOnce(&str) -> Result<T, Failure> + Send + 'static,
    ) -> Task<Result<T, String>> {
        let Some(base) = self.base.clone() else {
            return Task::ready(Err(self.status.clone()));
        };
        cx.spawn(async move |cx| {
            let done = cx.background_spawn(async move { work(&base) }).await;
            done.map_err(|failure| failure.text(cx.update(|app| *app.global::<strings::Lang>())))
        })
    }

    pub fn agents(&self, cx: &mut Context<Self>) -> Task<Result<Vec<AgentView>, String>> {
        self.call(cx, |base| link::read(base, "/api/agents"))
    }

    pub fn create_node(
        &self,
        body: NewNode,
        cx: &mut Context<Self>,
    ) -> Task<Result<NodeId, String>> {
        self.call(cx, move |base| {
            link::post_within(
                base,
                "/api/nodes",
                serde_json::to_string(&body).ok(),
                link::SLOW_LIMIT,
            )
        })
    }

    pub fn answer_wait(
        &self,
        wait: WaitId,
        text: String,
        images: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let body = AnswerWait {
            text,
            images: images.into_iter().map(Into::into).collect(),
        };
        self.call(cx, move |base| {
            let path = format!("/api/waits/{}/answer", wait.0);
            link::post(base, &path, serde_json::to_string(&body).ok())
        })
    }

    pub fn choose_option(
        &self,
        wait: WaitId,
        prompt: Prompt,
        option: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let body = ChooseOption { prompt, option };
        self.call(cx, move |base| {
            let path = format!("/api/waits/{}/choose", wait.0);
            link::post(base, &path, serde_json::to_string(&body).ok())
        })
    }

    pub fn type_text(
        &self,
        wait: WaitId,
        prompt: Prompt,
        text: String,
        option: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let body = TypeText {
            prompt,
            text,
            option,
        };
        self.call(cx, move |base| {
            let path = format!("/api/waits/{}/type", wait.0);
            link::post(base, &path, serde_json::to_string(&body).ok())
        })
    }

    pub fn press_keys(
        &self,
        wait: WaitId,
        keys: PressKeys,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(base) = self.base.clone() else {
            return Task::ready(Err(self.status.clone()));
        };
        cx.spawn(async move |_, cx| {
            let path = format!("/api/waits/{}/keys", wait.0);
            let body = serde_json::to_string(&keys).ok();
            let done = cx
                .background_spawn(async move { link::post::<()>(&base, &path, body) })
                .await;
            let lang = cx.update(|app| *app.global::<strings::Lang>());
            done.map_err(|failure| match failure.refusal().as_deref() {
                Some(PROMPT_CHANGED) => strings::Str::InboxScreenChanged.text(lang).to_owned(),
                _ => failure.text(lang),
            })
        })
    }

    pub fn wait_image(
        &self,
        wait: WaitId,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Task<Result<Arc<Image>, String>> {
        self.call(cx, move |base| {
            let (mime, bytes) =
                link::image(base, &format!("/api/waits/{}/images/{index}", wait.0))?;
            let format = ImageFormat::from_mime_type(&mime)
                .ok_or_else(|| Failure::Other(format!("image type {mime}")))?;
            Ok(Arc::new(Image::from_bytes(format, bytes)))
        })
    }

    pub fn dismiss_wait(&self, wait: WaitId, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        self.call(cx, move |base| {
            link::delete(base, &format!("/api/waits/{}", wait.0))
        })
    }

    pub fn list_docs(&self, cx: &mut Context<Self>) -> Task<Result<Vec<DocRow>, String>> {
        self.call(cx, |base| link::read(base, "/api/docs"))
    }

    pub fn doc_url(&self, id: &str) -> Result<String, String> {
        match &self.base {
            Some(base) => Ok(format!("{base}/docs/{id}")),
            None => Err(self.status.clone()),
        }
    }
}

impl Store {
    pub fn save_paste(
        &self,
        node: NodeId,
        name: String,
        mime: &'static str,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
    ) -> Task<Result<String, String>> {
        self.call(cx, move |base| {
            link::save_paste(base, Some(node), &name, mime, &bytes)
        })
    }
}
