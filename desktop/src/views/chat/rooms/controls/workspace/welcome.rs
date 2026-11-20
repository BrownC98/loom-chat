use api::graph::{NewNode, Place};
use api::ids::{AgentId, NodeId};
use api::profile::AgentView;
use api::repos::{PlaceView, RepoView};
use api::start::{ModelChoice, Start};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, ClickEvent, Context, Div, ElementId, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, Render, RenderOnce, Role, StatefulInteractiveElement, Styled,
    Task, WeakEntity, Window, div,
};

use crate::scale::px;
use crate::store::Store;
use crate::strings::{Lang, Str};
use crate::views::chat::rooms::Rooms;
use crate::views::chat::words;
use crate::views::field::{self, Field, FieldEvent};
use crate::views::icons::{Lucide, icon};
use crate::views::new_node::{agent_place, usable};
use crate::views::prose::prose;
use crate::views::shell::OpenSettings;
use crate::views::ui::{SMALL, TEXT, colors, open_url, say};
use crate::views::widgets::btn;
use crate::views::workspace::pop;

use super::super::{below, option};
use super::scratch;

const POPULAR: [(&str, &str); 3] = [
    ("claude", "Anthropic"),
    ("codex", "OpenAI"),
    ("cursor", "Anysphere"),
];
const WIDTH: f32 = 540.0;
const MENU_WIDTH: f32 = 244.0;

#[derive(IntoElement)]
pub(super) struct Slot {
    store: Entity<Store>,
    rooms: WeakEntity<Rooms>,
}

impl Slot {
    pub(super) fn new(store: Entity<Store>, rooms: WeakEntity<Rooms>) -> Self {
        Self { store, rooms }
    }
}

impl RenderOnce for Slot {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { store, rooms } = self;
        window.use_keyed_state("chat-welcome-composer", cx, |_, cx| {
            Welcome::new(store, rooms, cx)
        })
    }
}

pub(super) struct Welcome {
    store: Entity<Store>,
    rooms: WeakEntity<Rooms>,
    field: Entity<Field>,
    picked: Option<AgentId>,
    folder: Option<RepoView>,
    menu: Option<Point<Pixels>>,
    busy: bool,
    focus_on_tools: bool,
    error: Option<String>,
}

impl Welcome {
    fn new(store: Entity<Store>, rooms: WeakEntity<Rooms>, cx: &mut Context<Self>) -> Self {
        let field = field::boxed("", false, true, cx);
        cx.subscribe(&field, |this, _, event: &FieldEvent, cx| match event {
            FieldEvent::Enter => this.send(cx),
            FieldEvent::Changed => {
                this.error = None;
                cx.notify();
            }
            FieldEvent::Escape => {
                this.menu = None;
                cx.notify();
            }
            FieldEvent::Step(_) => {}
        })
        .detach();
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        store.update(cx, |store, cx| store.detect_agents(cx));
        Self {
            store,
            rooms,
            field,
            picked: None,
            folder: None,
            menu: None,
            busy: false,
            focus_on_tools: true,
            error: None,
        }
    }

    pub(super) fn set_folder(
        &mut self,
        repo: RepoView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.folder = Some(repo);
        self.error = None;
        window.focus(&self.field.read(cx).handle(), cx);
        cx.notify();
    }

    fn installed<'a>(&self, cx: &'a App) -> Vec<&'a AgentView> {
        let Some(Ok(agents)) = self.store.read(cx).detected() else {
            return Vec::new();
        };
        let at = self
            .folder
            .as_ref()
            .map_or(PlaceView::Pc, |repo| repo.place.clone());
        let at = agent_place(&at, agents);
        agents.iter().filter(|agent| usable(agent, &at)).collect()
    }

    fn active<'a>(&self, installed: &[&'a AgentView]) -> Option<&'a AgentView> {
        installed
            .iter()
            .find(|agent| self.picked.as_ref() == Some(&agent.id))
            .or_else(|| {
                POPULAR
                    .iter()
                    .find_map(|(id, _)| installed.iter().find(|agent| agent.id.0 == *id))
            })
            .or(installed.first())
            .copied()
    }

    fn place(&self, cx: &mut Context<Self>) -> Task<Result<Place, String>> {
        if let Some(repo) = &self.folder {
            return Task::ready(Ok(Place {
                repo: repo.id,
                worktree: repo.path.clone(),
            }));
        }
        let made = scratch(&self.store, cx);
        cx.foreground_executor().spawn(async move {
            made.await.map(|repo| Place {
                repo: repo.id,
                worktree: repo.path,
            })
        })
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let brief = self.field.read(cx).text().to_owned();
        if self.busy || brief.trim().is_empty() {
            return;
        }
        let installed = self.installed(cx);
        let Some(agent) = self.active(&installed).map(|agent| agent.id.clone()) else {
            return;
        };
        self.busy = true;
        self.error = None;
        self.menu = None;
        self.field
            .update(cx, |field, cx| field.set_locked(true, cx));
        let place = self.place(cx);
        let store = self.store.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let created = match place.await {
                Ok(place) => {
                    let body = NewNode {
                        start: Start::Agent {
                            agent,
                            brief,
                            choice: ModelChoice::default(),
                        },
                        place,
                    };
                    let task =
                        cx.update(|cx| store.update(cx, |store, cx| store.create_node(body, cx)));
                    task.await
                }
                Err(error) => Err(error),
            };
            if let Ok(node) = created.as_ref() {
                cx.update(|cx| {
                    store.update(cx, |store, cx| {
                        store.reload(cx);
                        store.open_agent(*node, cx);
                    });
                });
            }
            let _ = this.update(cx, |this, cx| this.finish(created, cx));
        })
        .detach();
    }

    fn finish(&mut self, created: Result<NodeId, String>, cx: &mut Context<Self>) {
        self.busy = false;
        self.field
            .update(cx, |field, cx| field.set_locked(false, cx));
        match created {
            Ok(_) => {
                self.field.update(cx, |field, cx| field.clear(cx));
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn composer(&self, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        let lang = *cx.global::<Lang>();
        let installed = self.installed(cx);
        let active = self.active(&installed);
        let field = self.field.read(cx);
        let ready = active.is_some() && !self.busy && !field.is_blank();
        let target = match (active, lang) {
            (Some(agent), Lang::Ko) => format!("{}에게", agent.label),
            (Some(agent), Lang::En) => format!("To {}", agent.label),
            (None, _) if self.store.read(cx).detecting() => {
                words(cx, "AI 도구를 찾습니다…", "Finding AI tools…").to_owned()
            }
            (None, _) => words(
                cx,
                "이 위치에서 쓸 도구 없음",
                "No AI tools for this location",
            )
            .to_owned(),
        };
        let pill = btn::button("chat-welcome-agent", cx)
            .flex_none()
            .h(px(30.0))
            .px(px(8.0))
            .rounded(px(8.0))
            .font_weight(FontWeight::SEMIBOLD)
            .aria_label(words(cx, "AI 도구 고르기", "Choose an AI tool"))
            .child(target)
            .child(icon(Lucide::ChevronDown, px(12.0), c.ink_2));
        let pill = if active.is_some() && !self.busy {
            pill.on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.menu = Some(below(event));
                cx.notify();
            }))
        } else {
            btn::inert(pill)
        };
        let placeholder = words(
            cx,
            "예: 할 일 목록 앱을 만들어 주세요",
            "For example: Make a to-do list app",
        );
        let input = div()
            .id("chat-welcome-message")
            .track_focus(&field.handle().tab_stop(false))
            .role(Role::MultilineTextInput)
            .aria_label(placeholder)
            .aria_value(field.text().to_owned())
            .relative()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .id("chat-welcome-scroll")
                    .max_h(px(140.0))
                    .overflow_y_scroll()
                    .child(self.field.clone()),
            )
            .children(field.text().is_empty().then(|| {
                div()
                    .absolute()
                    .top(px(5.0))
                    .left(px(8.0))
                    .right(px(8.0))
                    .flex()
                    .gap(px(8.0))
                    .text_color(c.ink_3)
                    .child(div().flex_1().min_w_0().truncate().child(placeholder))
                    .child(div().flex_none().text_size(px(11.0)).child(words(
                        cx,
                        "Enter 보내기 · Shift+Enter 줄 바꿈",
                        "Enter to send · Shift+Enter for a new line",
                    )))
            }));
        let send = btn::filled("chat-welcome-send", c.accent, c.accent_ink, cx)
            .role(Role::Button)
            .aria_label(words(cx, "보내기", "Send"))
            .aria_keyshortcuts("Enter")
            .flex_none()
            .h(px(30.0))
            .w(px(30.0))
            .p_0()
            .rounded(px(8.0))
            .justify_center()
            .child(icon(Lucide::ArrowUp, px(15.0), c.accent_ink))
            .when(ready, |button| {
                button.on_click(cx.listener(|this, _, _, cx| this.send(cx)))
            })
            .when(!ready, |button| btn::inert(button).opacity(0.4));
        let bypass = active.filter(|agent| {
            agent.can_bypass
                && !self
                    .store
                    .read(cx)
                    .settings()
                    .bypass_off
                    .contains(&agent.id)
        });
        let folder = self.folder.as_ref().map(|repo| repo.name.clone());
        let place_note = match (&folder, lang) {
            (Some(name), Lang::Ko) => format!("에이전트가 {name} 폴더 안에서 일합니다."),
            (Some(name), Lang::En) => format!("The agent works in the {name} folder."),
            (None, _) => words(
                cx,
                "폴더를 열지 않으면 빈 작업 폴더에서 시작합니다.",
                "Without a folder, the agent starts in an empty work folder.",
            )
            .to_owned(),
        };
        let chip = btn::button("chat-welcome-folder", cx)
            .flex_none()
            .h(px(26.0))
            .px(px(10.0))
            .rounded_full()
            .text_size(px(SMALL))
            .child(icon(Lucide::Folder, px(13.0), c.ink_2))
            .child(
                div()
                    .max_w(px(200.0))
                    .truncate()
                    .child(folder.unwrap_or_else(|| {
                        words(cx, "프로젝트 폴더 열기", "Open project folder").to_owned()
                    })),
            );
        let chip = if self.busy {
            btn::inert(chip).opacity(0.6)
        } else {
            chip.on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                let welcome = cx.weak_entity();
                let at = below(event);
                let _ = this.rooms.update(cx, |rooms, cx| {
                    rooms.pick_welcome_folder(welcome, at, window, cx);
                });
            }))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(28.0))
            .child(heading(
                words(
                    cx,
                    "첫 에이전트와 대화를 시작합니다",
                    "Start a chat with your first agent",
                ),
                words(
                    cx,
                    "에이전트는 맡긴 일을 하는 AI입니다. 아래 칸에 맡길 일을 쓰고 Enter를 누릅니다.",
                    "An agent is an AI that does the work you give it. Write the work below and press Enter.",
                ),
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(6.0))
                    .p(px(8.0))
                    .rounded(px(14.0))
                    .border_1()
                    .border_color(c.accent)
                    .bg(c.panel)
                    .child(pill)
                    .child(input)
                    .child(send),
            )
            .children(bypass.map(|_| {
                div().text_size(px(SMALL)).child(prose(say(cx, Str::NewNodeBypass)))
            }))
            .when(self.busy, |column| {
                column.child(
                    div()
                        .text_size(px(SMALL))
                        .child(words(cx, "에이전트를 띄웁니다…", "Starting the agent…")),
                )
            })
            .children(self.error_line(cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(SMALL))
                    .child(div().flex_1().min_w_0().child(prose(place_note)))
                    .child(chip),
            )
    }

    fn menu(&self, at: Point<Pixels>, window: &Window, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        let installed = self.installed(cx);
        let active = self.active(&installed).map(|agent| agent.id.clone());
        let rows = installed.iter().map(|agent| {
            let id = agent.id.clone();
            let here = active.as_ref() == Some(&agent.id);
            option(
                ElementId::Name(format!("chat-welcome-agent-{}", agent.id.0).into()),
                Lucide::Terminal,
                &agent.label,
                cx,
            )
            .when(here, |row| {
                row.bg(c.accent_wash)
                    .child(icon(Lucide::Check, px(13.0), c.ink))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.picked = Some(id.clone());
                this.menu = None;
                window.focus(&this.field.read(cx).handle(), cx);
                cx.notify();
            }))
        });
        let list = div()
            .id("chat-welcome-agents")
            .role(Role::Dialog)
            .aria_label(words(cx, "AI 도구 고르기", "Choose an AI tool"))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .w(px(MENU_WIDTH))
            .max_h(px(320.0))
            .overflow_y_scroll()
            .p(px(8.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(c.line_strong)
            .bg(c.surface)
            .shadow_lg()
            .children(rows);
        let outside = cx.listener(|this, _, _, cx| {
            this.menu = None;
            cx.notify();
        });
        pop::overlay(at, list, outside, window)
    }

    fn missing(&self, agents: &[AgentView], cx: &Context<Self>) -> Div {
        let c = colors(cx);
        let popular: Vec<_> = POPULAR
            .iter()
            .filter_map(|(id, vendor)| {
                agents
                    .iter()
                    .find(|agent| agent.id.0 == *id)
                    .map(|agent| (agent, *vendor))
            })
            .collect();
        let others = agents.len() - popular.len();
        let rows = popular
            .into_iter()
            .enumerate()
            .map(|(index, (agent, vendor))| {
                let url = agent.homepage.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .min_h(px(42.0))
                    .when(index > 0, |row| row.border_t_1())
                    .border_color(c.line)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c.ink)
                            .child(agent.label.clone()),
                    )
                    .child(div().text_size(px(SMALL)).child(vendor))
                    .child(div().flex_1())
                    .child(
                        btn::button(
                            ElementId::Name(format!("chat-install-{}", agent.id.0).into()),
                            cx,
                        )
                        .text_size(px(SMALL))
                        .child(words(cx, "설치 안내 열기", "Open install guide"))
                        .child(icon(Lucide::ExternalLink, px(12.0), c.ink_2))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.error = open_url(&url).err();
                            cx.notify();
                        })),
                    )
            });
        let lang = *cx.global::<Lang>();
        let more = match lang {
            Lang::Ko => format!("다른 도구 {others}개 보기"),
            Lang::En => format!("See {others} other tools"),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(28.0))
            .child(heading(
                words(
                    cx,
                    "먼저 AI 도구를 하나 설치합니다",
                    "Install an AI tool first",
                ),
                words(
                    cx,
                    "loom은 이 컴퓨터에 설치된 AI 도구로 에이전트를 띄웁니다. 지금은 찾은 도구가 없습니다.",
                    "loom starts agents with the AI tools on this computer. It found no tools.",
                ),
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(c.line_strong)
                    .bg(c.surface)
                    .children(rows),
            )
            .children(self.error_line(cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(SMALL))
                    .child(self.retry_button(cx))
                    .child(words(
                        cx,
                        "설치를 마친 뒤 누릅니다.",
                        "Click after you install a tool.",
                    ))
                    .child(div().flex_1())
                    .when(others > 0, |line| {
                        line.child(btn::ghost("chat-other-tools", cx).child(more).on_click(
                            |_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx),
                        ))
                    }),
            )
    }

    fn failed(&self, error: &str, cx: &Context<Self>) -> Div {
        let c = colors(cx);
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(heading(
                words(
                    cx,
                    "AI 도구 목록을 읽지 못했습니다",
                    "Could not read the AI tool list",
                ),
                words(
                    cx,
                    "loom 서비스 연결을 확인한 뒤 다시 찾습니다.",
                    "Check the loom service connection, then search again.",
                ),
                cx,
            ))
            .child(
                div()
                    .text_size(px(SMALL))
                    .text_color(c.err)
                    .child(error.to_owned()),
            )
            .child(div().flex().child(self.retry_button(cx)))
    }

    fn retry_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let c = colors(cx);
        let searching = self.store.read(cx).detecting();
        let button = btn::filled("chat-tools-retry", c.accent, c.accent_ink, cx)
            .child(icon(Lucide::RotateCw, px(13.0), c.accent_ink))
            .child(if searching {
                words(cx, "찾는 중…", "Searching…")
            } else {
                words(cx, "다시 찾기", "Search again")
            });
        if searching {
            btn::inert(button).opacity(0.6)
        } else {
            button.on_click(cx.listener(|this, _, _, cx| {
                this.focus_on_tools = true;
                this.store.update(cx, |store, cx| store.detect_agents(cx));
            }))
        }
    }

    fn error_line(&self, cx: &App) -> Option<Div> {
        let c = colors(cx);
        self.error.as_ref().map(|error| {
            div()
                .text_size(px(SMALL))
                .text_color(c.err)
                .child(error.clone())
        })
    }
}

impl Render for Welcome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_on_tools && !self.installed(cx).is_empty() {
            self.focus_on_tools = false;
            window.focus(&self.field.read(cx).handle(), cx);
        }
        let c = colors(cx);
        let store = self.store.read(cx);
        let body = match store.detected() {
            Some(Err(error)) => self.failed(error, cx),
            Some(Ok(agents)) if store.no_tools() => self.missing(agents, cx),
            _ => self.composer(cx),
        };
        let menu = self.menu.map(|at| self.menu(at, window, cx));
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .pb(px(64.0))
            .text_size(px(TEXT))
            .text_color(c.ink_2)
            .child(
                div()
                    .id("chat-welcome")
                    .w_full()
                    .max_w(px(WIDTH))
                    .child(body),
            )
            .children(menu)
    }
}

fn heading(title: &str, intro: &str, cx: &App) -> Div {
    let c = colors(cx);
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(18.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c.ink)
                .child(prose(title.to_owned())),
        )
        .child(div().text_size(px(TEXT)).child(prose(intro.to_owned())))
}
