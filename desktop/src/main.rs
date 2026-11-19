#![cfg_attr(windows, windows_subsystem = "windows")]

mod canvas;
mod daemon;
mod float;
mod fonts;
mod frame;
mod link;
#[cfg(not(any(windows, target_os = "macos")))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod problem;
mod program;
mod scale;
mod settings;
mod store;
mod strings;
mod theme;
mod views;
mod vt;
#[cfg(windows)]
mod windows;

#[cfg(not(any(windows, target_os = "macos")))]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use windows as platform;

use gpui::{App, AppContext};

fn main() -> std::process::ExitCode {
    if std::env::args_os().nth(1).is_some() {
        return unsafe { loom::run_desktop_cli() };
    }
    program::remember();
    let started = daemon::start();
    gpui_platform::application()
        .with_assets(views::icons::Assets)
        .run(move |cx: &mut App| {
            fonts::register(cx);
            settings::notify::identify(cx);
            cx.open_window(platform::window_options(cx), |window, cx| {
                let store = cx.new(|cx| store::Store::new(started, window, cx));
                settings::notify::watch(&store, window, cx);
                let inputs = cx.new(|_| views::inputs::Inputs::default());
                let shelf = cx.new(|_| views::pictures::Shelf::default());
                cx.new(|cx| views::shell::Shell::new(store, inputs, shelf, window, cx))
            })
            .expect("could not open the window");
            cx.activate(true);
        });
    std::process::ExitCode::SUCCESS
}
