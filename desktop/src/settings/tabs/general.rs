use std::collections::BTreeMap;

use api::repos::Locations;
use api::settings::{FloatSize, Language, Theme as ThemeSetting};
use gpui::{
    App, AppContext, Div, ElementId, Entity, ParentElement, PathPromptOptions, SharedString,
    Stateful, StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window, div,
};

use crate::link;
use crate::problem::Failure;
use crate::scale::px;
use crate::settings::notify;
use crate::settings::widgets::{Label, Text, TextEvent, button, field_reset, segmented, switch};
use crate::settings::{Ctx, Dialog, Section};
use crate::strings::{Lang, Str};
use crate::views::dock::ui::{GAP, SMALL, colors, say, say_with};
use crate::views::icons::{Lucide, icon};

const PC: &str = "";

enum DirError {
    NotAbsolute(String),
    Picker,
}

#[derive(Default)]
struct Places {
    started: bool,
    distros: Vec<String>,
    defaults: BTreeMap<String, String>,
    errors: BTreeMap<String, DirError>,
}

#[derive(Default)]
pub struct State {
    places: Option<Entity<Places>>,
    fields: BTreeMap<String, Entity<Text>>,
    subs: Vec<Subscription>,
}

fn absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

fn load(places: Entity<Places>, base: String, dialog: WeakEntity<Dialog>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let found = cx
            .background_spawn(async move {
                let distros = link::read::<Locations>(&base, "/api/locations")?.distros;
                let defaults = distros
                    .iter()
                    .filter_map(|name| {
                        let view = link::distro(&base, name).ok()?;
                        Some((name.clone(), view.workspace))
                    })
                    .collect();
                Ok::<_, Failure>((distros, defaults))
            })
            .await;
        places.update(cx, |places, cx| {
            if let Ok((distros, defaults)) = found {
                places.distros = distros;
                places.defaults = defaults;
            }
            cx.notify();
        });
        let _ = dialog.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

fn field(
    state: &mut State,
    key: &str,
    places: &Entity<Places>,
    ctx: &Ctx,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Text> {
    if let Some(found) = state.fields.get(key) {
        return found.clone();
    }
    let text = cx.new(|cx| Text::mono("", window, cx).keep_draft());
    let writer = ctx.writer.clone();
    let dialog = ctx.dialog.clone();
    let places = places.clone();
    let owner = key.to_owned();
    state
        .subs
        .push(cx.subscribe(&text, move |_, event: &TextEvent, cx| {
            let TextEvent::Commit(draft) = event;
            let value = draft.trim().to_owned();
            let valid = absolute(&value);
            places.update(cx, |places, _| {
                if valid {
                    places.errors.remove(&owner);
                } else {
                    places
                        .errors
                        .insert(owner.clone(), DirError::NotAbsolute(value.clone()));
                }
            });
            if valid {
                let owner = owner.clone();
                writer.edit(cx, move |settings| {
                    if owner == PC {
                        settings.workspace_dir = Some(value);
                    } else {
                        settings.wsl_workspace_dirs.insert(owner, value);
                    }
                });
            }
            let _ = dialog.update(cx, |_, cx| cx.notify());
        }));
    state.fields.insert(key.to_owned(), text.clone());
    text
}

fn folder_picker(
    section: &mut Section,
    text: &Entity<Text>,
    places: &Entity<Places>,
    ctx: &Ctx,
    cx: &App,
) -> Stateful<Div> {
    let label = match cx.global::<Lang>() {
        Lang::Ko => "폴더 고르기…",
        Lang::En => "Choose folder…",
    };
    let (text, places, dialog, writer) = (
        text.clone(),
        places.clone(),
        ctx.dialog.clone(),
        ctx.writer.clone(),
    );
    button("pick-workspace-folder", cx)
        .flex_none()
        .child(icon(Lucide::Folder, px(14.0), colors(cx).ink_2))
        .child(section.text(label, cx))
        .on_click(move |_, _, cx| {
            let chosen = cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: None,
            });
            let (text, places, dialog, writer) =
                (text.clone(), places.clone(), dialog.clone(), writer.clone());
            cx.spawn(async move |cx| {
                let path = match chosen.await {
                    Ok(Ok(Some(paths))) => paths
                        .into_iter()
                        .next()
                        .and_then(|path| path.into_os_string().into_string().ok()),
                    Ok(Ok(None)) => return,
                    _ => None,
                };
                cx.update(|cx| {
                    if let Some(path) = path {
                        places.update(cx, |places, _| places.errors.remove(PC));
                        text.update(cx, |text, cx| text.reset(&path, cx));
                        writer.edit(cx, move |settings| settings.workspace_dir = Some(path));
                    } else {
                        places.update(cx, |places, _| {
                            places.errors.insert(PC.to_owned(), DirError::Picker);
                        });
                    }
                    let _ = dialog.update(cx, |_, cx| cx.notify());
                });
            })
            .detach();
        })
}

struct Dir<'a> {
    label: String,
    owner: &'a str,
    saved: String,
    note: Option<Str>,
    text: Entity<Text>,
    reset: Option<Stateful<Div>>,
}

fn dir_section(
    dir: Dir,
    places: &Entity<Places>,
    ctx: &Ctx,
    window: &Window,
    cx: &mut App,
) -> Section {
    dir.text
        .update(cx, |text, cx| text.show(&dir.saved, window, cx));
    let c = colors(cx);
    let mut section = Section::new(Some(&dir.label), ctx).beside(dir.reset);
    let picker = (dir.owner == PC).then(|| folder_picker(&mut section, &dir.text, places, ctx, cx));
    section = section.child(
        div()
            .flex()
            .items_center()
            .gap(px(GAP))
            .child(div().flex_1().min_w_0().child(dir.text))
            .children(picker),
    );
    if let Some(error) = places.read(cx).errors.get(dir.owner) {
        let message = match error {
            DirError::NotAbsolute(value) => {
                say_with(cx, Str::ProblemRepoNotAbsolute, &[("value", value)])
            }
            DirError::Picker => match cx.global::<Lang>() {
                Lang::Ko => "폴더를 고르지 못했습니다. 경로를 직접 입력해 주세요.",
                Lang::En => "Could not choose a folder. Enter the path directly.",
            }
            .to_owned(),
        };
        section.child(div().text_size(px(SMALL)).text_color(c.err).child(message))
    } else if let Some(note) = dir.note {
        let hint = section.text(say(cx, note), cx);
        section.child(div().text_size(px(SMALL)).text_color(c.ink_3).child(hint))
    } else {
        section
    }
}

pub fn sections(state: &mut State, ctx: &Ctx, window: &mut Window, cx: &mut App) -> Vec<Section> {
    let mut out = vec![
        language_section(ctx, cx),
        theme_section(ctx, cx),
        size_section(ctx, cx),
    ];
    if notify::SUPPORTED {
        out.push(notify_section(ctx, cx));
    }

    let places = match &state.places {
        Some(places) => places.clone(),
        None => {
            let places = cx.new(|_| Places::default());
            state.places = Some(places.clone());
            places
        }
    };
    if let Some(base) = ctx.store.read(cx).base().map(str::to_owned)
        && !places.read(cx).started
    {
        places.update(cx, |places, _| places.started = true);
        load(places.clone(), base, ctx.dialog.clone(), cx);
    }

    let snapshot = places.read(cx);
    let distros = snapshot.distros.clone();
    let defaults = snapshot.defaults.clone();
    let many = !distros.is_empty();

    let pc_label = if many {
        let place = say(cx, Str::PlacePc);
        say_with(cx, Str::WorkspaceFolderAt, &[("place", place)])
    } else {
        match cx.global::<Lang>() {
            Lang::Ko => "작업 폴더",
            Lang::En => say(cx, Str::WorkspaceFolder),
        }
        .to_owned()
    };
    let pc_text = field(state, PC, &places, ctx, window, cx);
    let pc = Dir {
        label: pc_label,
        owner: PC,
        saved: ctx.view.workspace_dir.clone().unwrap_or_default(),
        note: (!many).then_some(Str::WorkspaceHint),
        text: pc_text,
        reset: field_reset("reset-workspace-dir", &["workspaceDir"], ctx, cx),
    };
    out.push(dir_section(pc, &places, ctx, window, cx));
    for (index, distro) in distros.iter().enumerate() {
        let place = say_with(cx, Str::PlaceWslDistro, &[("distro", distro)]);
        let label = say_with(cx, Str::WorkspaceFolderAt, &[("place", &place)]);
        let saved = ctx
            .view
            .wsl_workspace_dirs
            .get(distro)
            .or(defaults.get(distro))
            .cloned()
            .unwrap_or_default();
        let text = field(state, distro, &places, ctx, window, cx);
        let note = (index + 1 == distros.len()).then_some(Str::WorkspaceHintPlaces);
        let dir = Dir {
            label,
            owner: distro,
            saved,
            note,
            text,
            reset: field_reset(
                ElementId::NamedInteger("reset-wsl-dir".into(), index as u64),
                &["wslWorkspaceDirs", distro],
                ctx,
                cx,
            ),
        };
        out.push(dir_section(dir, &places, ctx, window, cx));
    }

    out
}

fn notify_section(ctx: &Ctx, cx: &App) -> Section {
    let c = colors(cx);
    let on = ctx.view.os_notify;
    let writer = ctx.writer.clone();
    let mut notify = Section::new(Some(say(cx, Str::OsNotifyHeading)), ctx);
    let label = notify.text(say(cx, Str::OsNotifyLabel), cx);
    let row = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(px(SMALL))
        .text_color(c.ink_2)
        .child(switch(
            "os-notify",
            on,
            label,
            move |value, _, cx| writer.edit(cx, |settings| settings.os_notify = value),
            cx,
        ));
    notify
        .beside(field_reset("reset-os-notify", &["osNotify"], ctx, cx))
        .child(row)
}

fn language_section(ctx: &Ctx, cx: &App) -> Section {
    let mut section = Section::new(Some(say(cx, Str::SettingsLanguage)), ctx);
    let items = [
        (Language::System, Str::CommonSystem),
        (Language::En, Str::LanguageNameEn),
        (Language::Ko, Str::LanguageNameKo),
    ]
    .into_iter()
    .map(|(value, label)| {
        let label = say(cx, label);
        section.text(label, cx);
        (value, SharedString::from(label))
    })
    .collect();
    let writer = ctx.writer.clone();
    section
        .beside(field_reset("reset-language", &["language"], ctx, cx))
        .child(segmented(
            "language",
            items,
            ctx.view.language,
            move |value, _, cx| writer.edit(cx, |settings| settings.language = value),
            cx,
        ))
}

fn size_section(ctx: &Ctx, cx: &App) -> Section {
    let mut section = Section::new(Some(say(cx, Str::FloatSizeUiSize)), ctx);
    let items = [
        (FloatSize::Small, Str::FloatSizeSmall),
        (FloatSize::Normal, Str::FloatSizeNormal),
        (FloatSize::Large, Str::FloatSizeLarge),
        (FloatSize::Huge, Str::FloatSizeHuge),
    ]
    .into_iter()
    .map(|(value, label)| {
        let label = say(cx, label);
        section.text(label, cx);
        (value, SharedString::from(label))
    })
    .collect();
    let writer = ctx.writer.clone();
    let hint = section.text(say(cx, Str::FloatSizeUiSizeHint), cx);
    section
        .beside(field_reset("reset-ui-size", &["uiSize"], ctx, cx))
        .child(segmented(
            "display-ui-size",
            items,
            ctx.view.ui_size,
            move |value, _, cx| writer.edit(cx, |settings| settings.ui_size = value),
            cx,
        ))
        .child(
            div()
                .text_size(px(SMALL))
                .text_color(colors(cx).ink_3)
                .child(hint),
        )
}

fn theme_section(ctx: &Ctx, cx: &mut App) -> Section {
    let c = colors(cx);
    let mut section = Section::new(Some(say(cx, Str::ThemeHeading)), ctx);
    let items: Vec<(ThemeSetting, Label)> = [
        (ThemeSetting::System, Str::CommonSystem, Lucide::Monitor),
        (ThemeSetting::Light, Str::ThemeLight, Lucide::Sun),
        (ThemeSetting::Dark, Str::ThemeDark, Lucide::Moon),
    ]
    .into_iter()
    .map(|(value, label, glyph)| {
        let _ = section.text(say(cx, label), cx);
        (
            value,
            Label::with_icon(SharedString::from(say(cx, label)), glyph),
        )
    })
    .collect();
    let writer = ctx.writer.clone();
    let row = div().flex().items_center().gap(px(GAP)).child(segmented(
        "theme",
        items,
        ctx.view.theme,
        move |value, _, cx| writer.edit(cx, |settings| settings.theme = value),
        cx,
    ));
    let hint = section.text(say(cx, Str::ThemeSystemHint), cx);
    section
        .beside(field_reset("reset-theme", &["theme"], ctx, cx))
        .child(row)
        .child(div().text_size(px(SMALL)).text_color(c.ink_3).child(hint))
}
