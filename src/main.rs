// Release builds are GUI applications: without this Windows attaches a
// console window to the process, which is the stray shell that used to open
// alongside the app. Debug builds keep the console for panic output.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! edownloader — a utilitarian link downloader.
//!
//! Paste a link: direct files are handed to aria2 (multi-connection, resumable,
//! magnets included), media sites to yt-dlp. Cookie jars can be imported and
//! attached, which is what YouTube needs for anything signed-in.

// First, so `t!` and `tn!` are in scope in every module below.
#[macro_use]
mod i18n;
mod app;
mod core;
mod theme;
mod ui;
mod views;

use gpui::{
    prelude::*, px, size, App, Application, Bounds, Focusable, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};

fn main() {
    Application::new().run(|cx: &mut App| {
        app::bind_keys(cx);
        ui::text_input::bind_keys(cx);

        let bounds = Bounds::centered(None, size(px(1120.), px(720.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(860.), px(540.))),
            titlebar: Some(TitlebarOptions {
                title: Some("edownloader".into()),
                // The caption is replaced by the app's own 34px bar.
                appears_transparent: true,
                traffic_light_position: None,
            }),
            // Without this the whole surface renders transparent over the desktop.
            window_background: WindowBackgroundAppearance::Opaque,
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            let view = cx.new(app::RootView::new);
            let input = view.read(cx).url_input.clone();
            let handle = input.read(cx).focus_handle(cx);
            window.focus(&handle);
            view
        })
        .expect("the main window could not be created");

        // One window: closing it any way other than the title bar's button
        // (Alt+F4, the taskbar) ends the app too, through the quit hooks.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.activate(true);
    });
}
