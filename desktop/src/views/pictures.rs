use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::ids::NodeId;
use gpui::{
    App, AppContext, ClipboardEntry, Context, Div, Entity, EventEmitter, Image, ImageFormat,
    InteractiveElement, IntoElement, MouseButton, ObjectFit, ParentElement, PathPromptOptions,
    Role, StatefulInteractiveElement, Styled, StyledImage, Window, div, img,
};

use super::dock::ui;
use super::icons::{Lucide, icon};
use super::tooltip::tip;
use super::widgets::btn;
use crate::scale::fixed as px;
use crate::store::Store;
use crate::strings::Str;

pub const MEGABYTE: u64 = 1024 * 1024;
pub const LIMIT_MEGABYTES: u64 = 32;
const THUMB: f32 = 34.0;

#[derive(Clone)]
enum Upload {
    Local,
    Busy,
    Saved(String),
    Failed,
}

struct Attached {
    id: u32,
    file: Attachment,
    state: Upload,
    node: Option<NodeId>,
}

#[derive(Clone)]
pub struct Attachment {
    pub name: String,
    content: Content,
}

#[derive(Clone)]
enum Content {
    Image(Arc<Image>),
    File(Arc<[u8]>),
}

impl Attachment {
    pub fn payload(&self) -> (&'static str, &[u8]) {
        match &self.content {
            Content::Image(image) => (image.format.mime_type(), &image.bytes),
            Content::File(bytes) => ("application/octet-stream", bytes),
        }
    }
}

struct Destination {
    store: Entity<Store>,
    node: NodeId,
}

pub enum PicturesEvent {
    Settled,
    Refused(String),
    Other(String),
}

enum Unreadable {
    Folder(String),
    Big(String, u64),
    NotFile(String),
    Io(String),
}

pub struct Pictures {
    destination: Option<Destination>,
    list: Vec<Attached>,
    next: u32,
    reading: usize,
    locked: bool,
}

impl EventEmitter<PicturesEvent> for Pictures {}

fn image_format(name: &str) -> Option<ImageFormat> {
    let extension = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        _ => return None,
    })
}

fn read_file(path: &Path) -> Result<(String, Vec<u8>), Unreadable> {
    let name = path
        .file_name()
        .map_or_else(|| "file".to_owned(), |n| n.to_string_lossy().into_owned());
    let meta = std::fs::metadata(path).map_err(|e| Unreadable::Io(e.to_string()))?;
    if meta.is_dir() {
        return Err(Unreadable::Folder(name));
    }
    if !meta.is_file() {
        return Err(Unreadable::NotFile(name));
    }
    let limit = LIMIT_MEGABYTES * MEGABYTE;
    if meta.len() > limit {
        return Err(Unreadable::Big(name, meta.len()));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|e| Unreadable::Io(e.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(Unreadable::Big(name, bytes.len() as u64));
    }
    Ok((name, bytes))
}

pub fn too_big(name: &str, bytes: u64, cx: &App) -> String {
    ui::say_with(
        cx,
        Str::TerminalTooBig,
        &[
            ("name", name),
            ("size", &bytes.div_ceil(MEGABYTE).to_string()),
            ("limit", &LIMIT_MEGABYTES.to_string()),
        ],
    )
}

impl Pictures {
    pub fn new(store: Entity<Store>, node: NodeId) -> Self {
        Self {
            destination: Some(Destination { store, node }),
            ..Self::draft()
        }
    }

    pub fn draft() -> Self {
        Self {
            destination: None,
            list: Vec::new(),
            next: 0,
            reading: 0,
            locked: false,
        }
    }

    pub fn set_locked(&mut self, locked: bool, cx: &mut Context<Self>) {
        if self.locked != locked {
            self.locked = locked;
            cx.notify();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn retarget(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let stale: Vec<u32> = self
            .list
            .iter()
            .filter(|image| matches!(image.state, Upload::Saved(_)) && image.node != Some(node))
            .map(|image| image.id)
            .collect();
        for id in stale {
            self.upload(id, node, cx);
        }
    }

    pub fn busy(&self) -> bool {
        self.reading > 0
            || self
                .list
                .iter()
                .any(|image| matches!(image.state, Upload::Busy))
    }

    pub fn attachments(&self) -> Vec<Attachment> {
        self.list
            .iter()
            .filter(|item| matches!(item.state, Upload::Local))
            .map(|item| item.file.clone())
            .collect()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.list.clear();
        cx.notify();
    }

    pub fn saved(&self) -> Vec<(u32, String)> {
        self.list
            .iter()
            .filter_map(|image| match &image.state {
                Upload::Saved(path) => Some((image.id, path.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn forget(&mut self, ids: &[u32], cx: &mut Context<Self>) {
        self.list.retain(|image| !ids.contains(&image.id));
        cx.notify();
    }

    pub fn take_clipboard(&mut self, entries: Vec<ClipboardEntry>, cx: &mut Context<Self>) {
        if self.locked {
            return;
        }
        for entry in entries {
            match entry {
                ClipboardEntry::Image(image) => {
                    let name = format!("image.{}", image.format.extension());
                    self.attach(name, image.format, image.bytes, cx);
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    self.take_paths(paths.paths().to_vec(), cx);
                }
                ClipboardEntry::String(_) => {}
            }
        }
    }

    pub fn take_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.locked {
            return;
        }
        for path in paths {
            let read = cx.background_spawn(async move { read_file(&path) });
            self.reading += 1;
            cx.notify();
            cx.spawn(async move |this, cx| {
                let read = read.await;
                let _ = this.update(cx, |pictures, cx| {
                    pictures.reading -= 1;
                    match read {
                        Ok((name, bytes)) => match image_format(&name) {
                            Some(format) => pictures.attach(name, format, bytes, cx),
                            None if pictures.destination.is_none() => pictures.add(
                                Attachment {
                                    name,
                                    content: Content::File(bytes.into()),
                                },
                                cx,
                            ),
                            None => cx.emit(PicturesEvent::Other(name)),
                        },
                        Err(error) => {
                            let text = match error {
                                Unreadable::Folder(name) => {
                                    ui::say_with(cx, Str::TerminalFolder, &[("name", &name)])
                                }
                                Unreadable::Big(name, bytes) => too_big(&name, bytes, cx),
                                Unreadable::NotFile(name) => {
                                    format!("{name}은 일반 파일이 아닙니다.")
                                }
                                Unreadable::Io(text) => text,
                            };
                            cx.emit(PicturesEvent::Refused(text));
                        }
                    }
                    cx.notify();
                    cx.emit(PicturesEvent::Settled);
                });
            })
            .detach();
        }
    }

    pub fn pick_files(&mut self, cx: &mut Context<Self>) {
        if self.locked {
            return;
        }
        self.reading += 1;
        cx.notify();
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let chosen = chosen.await;
            let _ = this.update(cx, |pictures, cx| {
                pictures.reading -= 1;
                match chosen {
                    Ok(Ok(Some(paths))) => pictures.take_paths(paths, cx),
                    Ok(Err(error)) => cx.emit(PicturesEvent::Refused(error.to_string())),
                    _ => {}
                }
                cx.notify();
                cx.emit(PicturesEvent::Settled);
            });
        })
        .detach();
    }

    fn attach(
        &mut self,
        name: String,
        format: ImageFormat,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        if bytes.len() as u64 > LIMIT_MEGABYTES * MEGABYTE {
            let text = too_big(&name, bytes.len() as u64, cx);
            cx.emit(PicturesEvent::Refused(text));
            return;
        }
        self.add(
            Attachment {
                name,
                content: Content::Image(Arc::new(Image::from_bytes(format, bytes))),
            },
            cx,
        );
    }

    fn add(&mut self, file: Attachment, cx: &mut Context<Self>) {
        let id = self.next;
        self.next += 1;
        self.list.push(Attached {
            id,
            file,
            state: Upload::Local,
            node: None,
        });
        if let Some(node) = self.destination.as_ref().map(|at| at.node) {
            self.upload(id, node, cx);
        } else {
            cx.notify();
        }
    }

    fn upload(&mut self, id: u32, node: NodeId, cx: &mut Context<Self>) {
        let Some(destination) = &self.destination else {
            return;
        };
        let Some(image) = self.list.iter_mut().find(|image| image.id == id) else {
            return;
        };
        image.state = Upload::Busy;
        image.node = Some(node);
        let (mime, bytes) = image.file.payload();
        let task = destination.store.update(cx, |store, cx| {
            store.save_paste(node, image.file.name.clone(), mime, bytes.to_vec(), cx)
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = task.await;
            let _ = this.update(cx, |pictures, cx| pictures.finished(id, saved, cx));
        })
        .detach();
    }

    fn finished(&mut self, id: u32, saved: Result<String, String>, cx: &mut Context<Self>) {
        let Some(image) = self.list.iter_mut().find(|i| i.id == id) else {
            return;
        };
        image.state = match &saved {
            Ok(path) => Upload::Saved(path.clone()),
            Err(_) => Upload::Failed,
        };
        if let Err(error) = saved {
            cx.emit(PicturesEvent::Refused(error));
        }
        cx.notify();
        cx.emit(PicturesEvent::Settled);
    }

    fn remove(&mut self, id: u32, cx: &mut Context<Self>) {
        if self.locked {
            return;
        }
        self.list.retain(|image| image.id != id);
        cx.notify();
    }
}

pub fn chips(
    pictures: &Entity<Pictures>,
    zoom: impl Fn(Arc<Image>, &mut Window, &mut App) + Clone + 'static,
    id: &'static str,
    scope: u32,
    z: f32,
    cx: &App,
) -> Option<Div> {
    let c = ui::colors(cx);
    let list = &pictures.read(cx).list;
    if list.is_empty() {
        return None;
    }
    let chips: Vec<Div> = list
        .iter()
        .map(|image| {
            let attached = image.id;
            let state = match image.state {
                Upload::Busy => Some(Str::ComposerUploading),
                Upload::Failed => Some(Str::ComposerFailed),
                Upload::Local | Upload::Saved(_) => None,
            };
            let name = &image.file.name;
            let owner = pictures.clone();
            let caption = match &image.file.content {
                Content::Image(picture) => {
                    let picture = picture.clone();
                    let open = zoom.clone();
                    let label = ui::say_with(cx, Str::InboxZoomImage, &[("name", name)]);
                    btn::tabbable(
                        div().id((id, ((scope as usize) << 20) | attached as usize)),
                        cx,
                    )
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .cursor_pointer()
                    .tooltip(tip(label))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        img(picture.clone())
                            .size(px(THUMB * z))
                            .rounded(px(4.0 * z))
                            .object_fit(ObjectFit::Cover),
                    )
                    .on_click(move |_, window, cx| open(picture.clone(), window, cx))
                    .into_any_element()
                }
                Content::File(_) => div()
                    .flex()
                    .items_center()
                    .gap(px(4.0 * z))
                    .child(icon(Lucide::FileText, px(14.0 * z), c.ink_2))
                    .child(div().max_w(px(200.0 * z)).truncate().child(name.clone()))
                    .into_any_element(),
            };
            let label = match image.file.content {
                Content::Image(_) => format!("{} · {name}", ui::say(cx, Str::InboxRemoveImage)),
                Content::File(_) => format!("{name} 빼기"),
            };
            let removable = !pictures.read(cx).locked && !matches!(image.state, Upload::Busy);
            div()
                .flex()
                .items_center()
                .gap(px(4.0 * z))
                .p(px(3.0 * z))
                .rounded(px(6.0 * z))
                .border_1()
                .border_color(if matches!(image.state, Upload::Failed) {
                    c.err
                } else {
                    c.line
                })
                .bg(c.surface)
                .text_size(px(12.0 * z))
                .text_color(c.ink_3)
                .child(caption)
                .children(state.map(|s| ui::say(cx, s)))
                .children(removable.then(|| {
                    btn::tabbable(
                        div().id((id, ((scope as usize) << 20) | (1 << 19) | attached as usize)),
                        cx,
                    )
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .size(px(20.0 * z))
                    .rounded(px(4.0 * z))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |s| s.bg(c.surface_hover).text_color(c.ink))
                    .tooltip(tip(label))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        owner.update(cx, |pictures, cx| pictures.remove(attached, cx))
                    })
                    .child(icon(Lucide::X, px(12.0 * z), c.ink_2))
                }))
        })
        .collect();
    Some(div().flex().flex_wrap().gap(px(6.0 * z)).children(chips))
}
