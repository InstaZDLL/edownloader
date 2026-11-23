//! Rendering for the shell, the task list, the cookie manager and settings.
//!
//! Layout is dense on purpose: one line of chrome, one command row, then a
//! list of rows that each carry their own state, progress and actions.

use gpui::{
    canvas, div, fill, point, prelude::*, px, rgb, size, Bounds, Div, FontWeight, IntoElement,
    MouseButton, PathBuilder, Pixels, Render, ScrollHandle, SharedString, Stateful, Window,
};

use crate::app::{RootView, Tab, Task, TaskState, UpdateState};
use crate::core::cookies::AuthState;
use crate::core::engine::{Container, FormatPreset};
use crate::core::human;
use crate::core::urlx::LinkKind;
use crate::theme;
use crate::theme::ThemeName;
use crate::ui::widgets as w;

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme::palette().bg))
            .font_family(theme::FONT_UI)
            .text_size(px(theme::TEXT_BASE))
            .text_color(rgb(theme::palette().text))
            .key_context("Root")
            .track_focus(&self.root_focus)
            .on_action(cx.listener(Self::on_submit))
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_focus_url))
            .on_action(cx.listener(Self::on_open_folder))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_tab1))
            .on_action(cx.listener(Self::on_tab2))
            .on_action(cx.listener(Self::on_tab3))
            .child(self.titlebar(window, cx))
            .child(self.tabbar(cx))
            .children(self.setup_banner(cx))
            .children(self.update_banner(cx))
            .child(self.body(cx))
            .child(self.statusbar(cx))
            // Last, so its band sits over everything else.
            .children(crate::ui::frame::resize_edges(window))
    }
}

/// GPUI reserves scrollbar space but paints nothing, so each scroll pane gets a
/// thin thumb of its own. `fill` keeps it a single quad, cheap to draw.
fn scroll_thumb(handle: &ScrollHandle) -> Div {
    let handle = handle.clone();
    div().w(px(7.)).h_full().flex_shrink_0().child(
        canvas(
            |_, _, _| (),
            move |bounds: Bounds<Pixels>, _, window, _| {
                let max = f32::from(handle.max_offset().height);
                let viewport = f32::from(bounds.size.height);
                if max <= 0.5 || viewport <= 0.0 {
                    return;
                }
                let track = viewport + max;
                let thumb = (viewport * viewport / track).clamp(28.0, viewport);
                let travel = viewport - thumb;
                let scrolled = -f32::from(handle.offset().y);
                let progress = (scrolled / max).clamp(0.0, 1.0);
                let width = 4.0;
                window.paint_quad(fill(
                    Bounds::new(
                        point(
                            bounds.origin.x + px((7.0 - width) / 2.0),
                            bounds.origin.y + px(travel * progress),
                        ),
                        size(px(width), px(thumb)),
                    ),
                    rgb(theme::palette().border_strong),
                ));
            },
        )
        .w_full()
        .h_full(),
    )
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowGlyph {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// A press on the title bar: drag the window, or (double click) maximize and
/// restore it, as a system title bar does.
fn drag_title(event: &gpui::MouseDownEvent, window: &mut Window) {
    if event.click_count == 2 {
        toggle_maximized(window);
    } else {
        window.start_window_move();
    }
}

/// Maximize, or back to the previous size. GPUI's `zoom_window` only ever
/// maximizes on Windows (and the native caption-button hit area never fires
/// under GPUI 0.2.2), so the restore half asks Windows directly.
fn toggle_maximized(window: &mut Window) {
    #[cfg(windows)]
    if window.is_maximized() {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindowAsync, SW_RESTORE};

        let raw = HasWindowHandle::window_handle(window).map(|handle| handle.as_raw());
        if let Ok(RawWindowHandle::Win32(handle)) = raw {
            // SAFETY: the handle belongs to this live window, and
            // ShowWindowAsync only posts a request to its thread.
            unsafe {
                let _ = ShowWindowAsync(HWND(handle.hwnd.get() as *mut _), SW_RESTORE);
            }
        }
        return;
    }
    window.zoom_window();
}

/// Window controls are drawn as vectors rather than text. The characters that
/// used to stand in for them (`□`, `✕`, `—`) are missing from Segoe UI at this
/// size and rendered as an unknown-character box.
fn window_button(glyph: WindowGlyph, danger: bool) -> Div {
    let hover = if danger {
        theme::palette().danger_soft
    } else {
        theme::palette().hover
    };
    div()
        .flex()
        .items_center()
        .justify_center()
        .size(px(28.))
        .rounded(px(3.))
        .hover(move |style| style.bg(rgb(hover)).cursor_pointer())
        .child(window_glyph(glyph))
}

fn window_glyph(glyph: WindowGlyph) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<Pixels>, _, window, _| {
            let colour = rgb(theme::palette().text);
            let centre = bounds.center();
            let mut line = |from: (f32, f32), to: (f32, f32)| {
                let mut path = PathBuilder::stroke(px(1.));
                path.move_to(point(centre.x + px(from.0), centre.y + px(from.1)));
                path.line_to(point(centre.x + px(to.0), centre.y + px(to.1)));
                if let Ok(path) = path.build() {
                    window.paint_path(path, colour);
                }
            };
            match glyph {
                WindowGlyph::Minimize => line((-4., 0.), (4., 0.)),
                WindowGlyph::Close => {
                    line((-3.5, -3.5), (3.5, 3.5));
                    line((3.5, -3.5), (-3.5, 3.5));
                }
                WindowGlyph::Maximize => {
                    line((-4., -4.), (4., -4.));
                    line((4., -4.), (4., 4.));
                    line((4., 4.), (-4., 4.));
                    line((-4., 4.), (-4., -4.));
                }
                WindowGlyph::Restore => {
                    // Back window peeking out top-right, front window in front.
                    line((-1., -4.), (4., -4.));
                    line((4., -4.), (4., 1.));
                    line((-4., -1.), (1., -1.));
                    line((1., -1.), (1., 4.));
                    line((1., 4.), (-4., 4.));
                    line((-4., 4.), (-4., -1.));
                }
            }
        },
    )
    .size(px(12.))
}

fn tab_button(label: &'static str, active: bool) -> Div {
    let base = div()
        .flex()
        .items_center()
        .h(px(28.))
        .px(px(12.))
        .rounded(px(3.))
        .text_size(px(13.));
    if active {
        base.bg(rgb(theme::palette().elevated))
            .text_color(rgb(theme::palette().text))
            .border_1()
            .border_color(rgb(theme::palette().border_strong))
            .child(label)
    } else {
        base.text_color(rgb(theme::palette().text_dim))
            .hover(|style| {
                style
                    .bg(rgb(theme::palette().hover))
                    .text_color(rgb(theme::palette().text))
                    .cursor_pointer()
            })
            .child(label)
    }
}

fn toggle(on: bool) -> Div {
    let track = div()
        .flex()
        .items_center()
        .w(px(30.))
        .h(px(16.))
        .px(px(2.))
        .rounded_full()
        .border_1()
        .bg(rgb(if on {
            theme::palette().accent_dim
        } else {
            theme::palette().elevated
        }))
        .border_color(rgb(if on {
            theme::palette().accent
        } else {
            theme::palette().border
        }));
    let track = if on {
        track.justify_end()
    } else {
        track.justify_start()
    };
    track.child(div().size(px(10.)).rounded_full().bg(rgb(if on {
        theme::palette().accent_hover
    } else {
        theme::palette().text_faint
    })))
}

/// A full-width strip under the tabs, on the soft shade of its tone.
fn banner(soft: u32) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .px(px(12.))
        .py(px(7.))
        .bg(rgb(soft))
        .border_b_1()
        .border_color(rgb(theme::palette().border))
}

fn vsep() -> Div {
    div().w(px(1.)).h(px(16.)).bg(rgb(theme::palette().border))
}

fn kind_badge(kind: LinkKind) -> Div {
    let (fg, bg) = match kind {
        LinkKind::Media => (theme::palette().accent_hover, theme::palette().accent_soft),
        LinkKind::Direct => (theme::palette().info, theme::palette().info_soft),
        LinkKind::Torrent => (theme::palette().warn, theme::palette().warn_soft),
    };
    div()
        .flex()
        .items_center()
        .justify_center()
        .w(px(58.))
        .h(px(20.))
        .rounded(px(3.))
        .bg(rgb(bg))
        .font_family(theme::FONT_MONO)
        .text_size(px(11.))
        .text_color(rgb(fg))
        .flex_shrink_0()
        .child(kind.badge())
}

impl RootView {
    fn titlebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(34.))
            .pl(px(10.))
            .pr(px(6.))
            .gap(px(8.))
            .bg(rgb(theme::palette().titlebar))
            .border_b_1()
            .border_color(rgb(theme::palette().border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .on_mouse_down(MouseButton::Left, |event, window, _| {
                        drag_title(event, window)
                    })
                    .on_mouse_down(MouseButton::Right, |event, window, _| {
                        window.show_window_menu(event.position)
                    })
                    .child(
                        div()
                            .size(px(10.))
                            .rounded(px(2.))
                            .bg(rgb(theme::palette().accent)),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("edownloader"),
                    )
                    .child(w::faint(crate::app::VERSION)),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .on_mouse_down(MouseButton::Left, |event, window, _| {
                        drag_title(event, window)
                    })
                    .on_mouse_down(MouseButton::Right, |event, window, _| {
                        window.show_window_menu(event.position)
                    }),
            )
            .child(
                window_button(WindowGlyph::Minimize, false)
                    .on_mouse_up(MouseButton::Left, |_, window, _| window.minimize_window()),
            )
            .child(
                window_button(
                    if window.is_maximized() {
                        WindowGlyph::Restore
                    } else {
                        WindowGlyph::Maximize
                    },
                    false,
                )
                .on_mouse_up(MouseButton::Left, |_, window, _| toggle_maximized(window)),
            )
            .child(window_button(WindowGlyph::Close, true).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    |this: &mut RootView,
                     _: &gpui::MouseUpEvent,
                     _: &mut Window,
                     cx: &mut Context<RootView>| {
                        this.shutdown();
                        cx.quit();
                    },
                ),
            ))
    }

    fn tabbar(&mut self, cx: &mut Context<Self>) -> Div {
        let mut tabs: Vec<Div> = Vec::new();
        for tab in Tab::all() {
            tabs.push(tab_button(tab.label(), self.tab == tab).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    move |this: &mut RootView,
                          _: &gpui::MouseUpEvent,
                          _: &mut Window,
                          cx: &mut Context<RootView>| {
                        this.set_tab(tab, cx);
                    },
                ),
            ));
        }

        let (jar_label, jar_fg) = match self.store.active_jar() {
            Some(jar) => {
                let auth = jar.youtube();
                let tone = match auth.state {
                    AuthState::SignedIn => theme::palette().accent,
                    AuthState::Partial => theme::palette().warn,
                    _ => theme::palette().text_faint,
                };
                (format!("{} · {}", jar.name, auth.state.label()), tone)
            }
            None => (
                t!("titlebar.no_cookies").to_string(),
                theme::palette().text_faint,
            ),
        };

        let tools_ready = self.tools.ready_count();
        let tools_total = crate::core::tools::TOOL_COUNT;
        let tools_fg = if tools_ready == tools_total {
            theme::palette().text_dim
        } else {
            theme::palette().warn
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(38.))
            .px(px(10.))
            .gap(px(4.))
            .bg(rgb(theme::palette().panel))
            .border_b_1()
            .border_color(rgb(theme::palette().border))
            .children(tabs)
            .child(w::spacer())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .h(px(20.))
                    .px(px(7.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(rgb(theme::palette().border))
                    .bg(rgb(theme::palette().elevated))
                    .text_size(px(11.))
                    .text_color(rgb(jar_fg))
                    .truncate()
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(
                            |this: &mut RootView,
                             _: &gpui::MouseUpEvent,
                             _: &mut Window,
                             cx: &mut Context<RootView>| {
                                this.set_tab(Tab::Cookies, cx);
                            },
                        ),
                    )
                    .child(jar_label),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .h(px(20.))
                    .px(px(7.))
                    .rounded(px(3.))
                    .text_size(px(11.))
                    .text_color(rgb(tools_fg))
                    .font_family(theme::FONT_MONO)
                    .child(t!(
                        "titlebar.tools",
                        ready = tools_ready,
                        total = tools_total
                    )),
            )
    }

    // ── Banners ────────────────────────────────────────────────────────────

    /// Tools: what is missing at launch with a one-click install, then the
    /// installation's progress.
    fn setup_banner(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        if let Some(run) = self.setup.running.as_ref() {
            let step = (run.finished + 1).min(run.count);
            let name = run.current.map(|c| c.label()).unwrap_or(t!("setup.tools"));
            let amount = match (run.stage, run.total) {
                ("downloading", Some(total)) => format!(
                    " · {}",
                    t!(
                        "task.of",
                        done = human::bytes(run.done),
                        total = human::bytes(total)
                    )
                ),
                ("downloading", None) => format!(" · {}", human::bytes(run.done)),
                _ => String::new(),
            };
            let fraction = match (run.stage, run.total) {
                ("downloading", Some(_)) => human::bar_fraction(run.done, run.total),
                ("extracting", _) => 1.0,
                _ => 0.0,
            };
            return Some(
                banner(theme::palette().info_soft)
                    .child(w::dot(theme::palette().info))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_SM))
                                    .truncate()
                                    .child(format!(
                                        "{}{amount}",
                                        t!(
                                            "setup.installing",
                                            name = name,
                                            step = step,
                                            count = run.count,
                                            stage = crate::app::stage_label(run.stage)
                                        )
                                    )),
                            )
                            .child(w::progress(fraction, theme::palette().info)),
                    ),
            );
        }
        if self.setup.dismissed || (self.setup.missing.is_empty() && self.setup.system.is_empty()) {
            return None;
        }
        let missing = &self.setup.missing;
        // Tools only the distribution provides (aria2 on Linux): named, with
        // how to get them, but nothing to click.
        let system_note = (!self.setup.system.is_empty()).then(|| {
            let names: Vec<&str> = self.setup.system.iter().map(|c| c.label()).collect();
            t!("setup.system_package", names = names.join(", "))
        });
        let installable = !missing.is_empty();
        let names: Vec<&str> = missing.iter().map(|c| c.label()).collect();
        let mut details: Vec<String> = missing
            .iter()
            .map(|c| format!("{} {} — {}", c.label(), c.approx_size(), c.purpose()))
            .collect();
        if installable {
            details.push(t!("setup.origin").to_string());
        }
        let essential = missing
            .iter()
            .any(|c| matches!(c, crate::core::setup::Component::YtDlp));
        let headline = if !installable {
            system_note.clone().unwrap_or_default()
        } else if essential {
            t!("setup.needs", names = names.join(", "))
        } else {
            tn!("setup.not_installed", names.len(), names = names.join(", "))
        };
        if installable {
            details.extend(system_note);
        }
        Some(
            banner(theme::palette().warn_soft)
                .child(w::dot(theme::palette().warn))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .flex_1()
                        .min_w(px(0.))
                        .child(
                            div()
                                .text_size(px(theme::TEXT_SM))
                                .truncate()
                                .child(headline),
                        )
                        .when(!details.is_empty(), |this| {
                            this.child(w::faint(details.join(" · ")).truncate())
                        }),
                )
                .when(installable, |this| {
                    this.child(w::primary_button(t!("button.install")).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(
                            |this: &mut RootView,
                             _: &gpui::MouseUpEvent,
                             _: &mut Window,
                             cx: &mut Context<RootView>| {
                                this.install_missing_tools(cx);
                            },
                        ),
                    ))
                })
                .child(w::ghost_button(t!("button.not_now")).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(
                        |this: &mut RootView,
                         _: &gpui::MouseUpEvent,
                         _: &mut Window,
                         cx: &mut Context<RootView>| {
                            this.dismiss_setup(cx);
                        },
                    ),
                )),
        )
    }

    /// A new version of the app: ready to install, or (portable copy) out.
    fn update_banner(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        if self.update_dismissed {
            return None;
        }
        let (text, version, ready) = match &self.update {
            UpdateState::Ready(ready) => (
                t!("update.ready_banner", version = ready.version),
                ready.version.clone(),
                true,
            ),
            UpdateState::Available(version) => (
                t!("update.out_banner", version = version),
                version.clone(),
                false,
            ),
            _ => return None,
        };
        let notes_version = version.clone();
        let row = banner(theme::palette().accent_soft)
            .child(w::dot(theme::palette().accent))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(px(theme::TEXT_SM))
                    .truncate()
                    .child(text),
            );
        let row = if ready {
            row.child(w::primary_button(t!("button.restart_update")).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    |this: &mut RootView,
                     _: &gpui::MouseUpEvent,
                     _: &mut Window,
                     cx: &mut Context<RootView>| {
                        this.restart_to_update(cx);
                    },
                ),
            ))
        } else {
            row
        };
        Some(
            row.child(
                (if ready {
                    w::ghost_button(t!("button.whats_new"))
                } else {
                    w::primary_button(t!("button.download"))
                })
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(
                        move |this: &mut RootView,
                              _: &gpui::MouseUpEvent,
                              _: &mut Window,
                              cx: &mut Context<RootView>| {
                            this.open_release_page(&notes_version, cx);
                        },
                    ),
                ),
            )
            .child(w::ghost_button(t!("button.later")).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    |this: &mut RootView,
                     _: &gpui::MouseUpEvent,
                     _: &mut Window,
                     cx: &mut Context<RootView>| {
                        this.dismiss_update(cx);
                    },
                ),
            )),
        )
    }

    fn body(&mut self, cx: &mut Context<Self>) -> Div {
        let container = div().flex().flex_col().flex_1().min_h(px(0.));
        match self.tab {
            Tab::Downloads => container.child(self.downloads_pane(cx)),
            Tab::Cookies => container.child(self.cookies_pane(cx)),
            Tab::Settings => container.child(self.settings_pane(cx)),
        }
    }

    // ── Downloads ──────────────────────────────────────────────────────────

    fn downloads_pane(&mut self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .child(self.command_bar(cx))
            .child(self.task_list(cx))
    }

    fn command_bar(&mut self, cx: &mut Context<Self>) -> Div {
        let mut presets: Vec<Div> = Vec::new();
        for preset in FormatPreset::ALL {
            let selected = self.settings.preset == preset;
            presets.push(w::segment(preset.label(), selected).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    move |this: &mut RootView,
                          _: &gpui::MouseUpEvent,
                          _: &mut Window,
                          cx: &mut Context<RootView>| {
                        this.set_preset(preset, cx);
                    },
                ),
            ));
        }

        let playlist_on = self.settings.playlist;
        let metadata_on = self.settings.embed_metadata;
        let cookies_on = self.settings.use_cookies && self.cookie_file.is_some();
        let folder = human::short_path(&self.settings.dir(), 46);

        div()
            .flex()
            .flex_col()
            .gap(px(7.))
            .px(px(10.))
            .py(px(8.))
            .bg(rgb(theme::palette().panel))
            .border_b_1()
            .border_color(rgb(theme::palette().border))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .child(div().flex_1().child(self.url_input.clone()))
                    .child(w::primary_button(t!("button.download")).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(
                            |this: &mut RootView,
                             _: &gpui::MouseUpEvent,
                             window: &mut Window,
                             cx: &mut Context<RootView>| {
                                this.submit(window, cx);
                            },
                        ),
                    ))
                    .child(w::button(t!("button.paste_go")).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(
                            |this: &mut RootView,
                             _: &gpui::MouseUpEvent,
                             window: &mut Window,
                             cx: &mut Context<RootView>| {
                                this.paste_and_add(window, cx);
                            },
                        ),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .child(w::section(t!("section.format")))
                    .children(presets)
                    .child(vsep())
                    .child(toggle_chip(t!("chip.playlist"), playlist_on).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(
                            |this: &mut RootView,
                             _: &gpui::MouseUpEvent,
                             _: &mut Window,
                             cx: &mut Context<RootView>| {
                                this.toggle_playlist(cx);
                            },
                        ),
                    ))
                    .child(toggle_chip(t!("chip.metadata"), metadata_on).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(
                            |this: &mut RootView,
                             _: &gpui::MouseUpEvent,
                             _: &mut Window,
                             cx: &mut Context<RootView>| {
                                this.toggle_metadata(cx);
                            },
                        ),
                    ))
                    .child(vsep())
                    .child(toggle_chip(t!("chip.cookies"), cookies_on).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(
                            |this: &mut RootView,
                             _: &gpui::MouseUpEvent,
                             _: &mut Window,
                             cx: &mut Context<RootView>| {
                                this.toggle_cookies(cx);
                            },
                        ),
                    ))
                    .child(w::spacer())
                    .child(
                        w::mono(folder, theme::palette().text_faint)
                            .min_w(px(0.))
                            .truncate(),
                    )
                    .child(w::ghost_button(t!("button.open_folder")).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(
                            |this: &mut RootView,
                             _: &gpui::MouseUpEvent,
                             _: &mut Window,
                             cx: &mut Context<RootView>| {
                                let dir = this.settings.dir();
                                let _ = crate::core::paths::ensure_dir(&dir);
                                crate::core::paths::reveal(&dir);
                                cx.notify();
                            },
                        ),
                    )),
            )
    }

    fn task_list(&mut self, cx: &mut Context<Self>) -> Div {
        if self.tasks.is_empty() {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .items_center()
                .justify_center()
                .gap(px(7.))
                .child(
                    div()
                        .text_size(px(theme::TEXT_LG))
                        .text_color(rgb(theme::palette().text_dim))
                        .child(t!("empty.title")),
                )
                .child(w::faint(t!("empty.routing")))
                .child(w::faint(t!("empty.cookies")))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(5.))
                        .pt(px(6.))
                        .child(w::keycap("Enter"))
                        .child(w::faint(t!("keys.queue")))
                        .child(w::keycap("Ctrl+V"))
                        .child(w::faint(t!("keys.paste")))
                        .child(w::keycap("Ctrl+O"))
                        .child(w::faint(t!("keys.folder")))
                        .child(w::keycap("Ctrl+1..3"))
                        .child(w::faint(t!("keys.tabs"))),
                );
        }

        let mut rows: Vec<Div> = Vec::new();
        for task in self.tasks.iter() {
            rows.push(self.task_row(task, cx));
        }
        let finished = self.tasks.iter().filter(|t| t.state.is_terminal()).count();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.))
                    .py(px(5.))
                    .border_b_1()
                    .border_color(rgb(theme::palette().border))
                    .child(w::section(tn!("queue.title", self.tasks.len())))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .when(finished > 0, |this| {
                                this.child(
                                    w::ghost_button(t!("queue.clear", n = finished)).on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                            this.clear_finished(cx);
                                        }),
                                    ),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h(px(0.))
                            .id("task-list")
                            .track_scroll(&self.tasks_scroll)
                            .overflow_y_scroll()
                            .children(rows),
                    )
                    .child(scroll_thumb(&self.tasks_scroll)),
            )
    }

    fn task_row(&self, task: &Task, cx: &mut Context<Self>) -> Div {
        let id = task.id;
        let mut actions: Vec<Div> = Vec::new();

        if task.is_pausable() {
            let label = if task.state == TaskState::Paused {
                t!("button.resume")
            } else {
                t!("button.pause")
            };
            actions.push(w::ghost_button(label).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    move |this: &mut RootView,
                          _: &gpui::MouseUpEvent,
                          _: &mut Window,
                          cx: &mut Context<RootView>| {
                        this.toggle_pause(id, cx);
                    },
                ),
            ));
        }
        match task.state {
            TaskState::Done | TaskState::Failed | TaskState::Cancelled => {
                actions.push(w::ghost_button(t!("button.retry")).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(
                        move |this: &mut RootView,
                              _: &gpui::MouseUpEvent,
                              _: &mut Window,
                              cx: &mut Context<RootView>| {
                            this.retry_task(id, cx);
                        },
                    ),
                ));
                actions.push(w::ghost_button(t!("button.open")).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(
                        move |this: &mut RootView,
                              _: &gpui::MouseUpEvent,
                              _: &mut Window,
                              cx: &mut Context<RootView>| {
                            this.open_task(id, cx);
                        },
                    ),
                ));
                actions.push(w::ghost_button(t!("button.copy")).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(
                        move |this: &mut RootView,
                              _: &gpui::MouseUpEvent,
                              _: &mut Window,
                              cx: &mut Context<RootView>| {
                            this.copy_url(id, cx);
                        },
                    ),
                ));
            }
            _ => {
                actions.push(w::ghost_button(t!("button.cancel")).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(
                        move |this: &mut RootView,
                              _: &gpui::MouseUpEvent,
                              _: &mut Window,
                              cx: &mut Context<RootView>| {
                            this.cancel_task(id, cx);
                        },
                    ),
                ));
            }
        }

        let figure = match task.state {
            TaskState::Running | TaskState::Paused | TaskState::Done => {
                match human::percent(task.downloaded, task.total) {
                    Some(fraction) => format!("{:>5.1}%", fraction * 100.0),
                    None => human::bytes(task.downloaded),
                }
            }
            _ => "--".to_string(),
        };

        let bar_color = match task.state {
            TaskState::Queued => theme::palette().border_strong,
            TaskState::Cancelled => theme::palette().text_faint,
            other => other.color(),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(10.))
            .py(px(8.))
            .border_b_1()
            .border_color(rgb(theme::palette().border))
            .hover(|style| style.bg(rgb(theme::palette().panel)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(kind_badge(task.kind))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(rgb(match task.state {
                                        TaskState::Queued => theme::palette().text_dim,
                                        TaskState::Failed => theme::palette().danger,
                                        _ => theme::palette().text,
                                    }))
                                    .truncate()
                                    .child(task.title.clone()),
                            )
                            .child({
                                let subtitle = w::mono(
                                    task.subtitle(),
                                    match task.state {
                                        TaskState::Failed => theme::palette().danger,
                                        TaskState::Queued | TaskState::Cancelled => {
                                            theme::palette().text_faint
                                        }
                                        _ => theme::palette().text_dim,
                                    },
                                );
                                // A failure is the one subtitle that must be
                                // readable in full, so it wraps instead of
                                // collapsing to an ellipsis.
                                if task.state == TaskState::Failed {
                                    subtitle
                                } else {
                                    subtitle.truncate()
                                }
                            }),
                    )
                    .child(
                        w::chip(
                            task.state.label(),
                            task.state.color(),
                            match task.state {
                                TaskState::Running | TaskState::Done => {
                                    theme::palette().accent_soft
                                }
                                TaskState::Failed => theme::palette().danger_soft,
                                TaskState::Paused => theme::palette().warn_soft,
                                _ => theme::palette().elevated,
                            },
                        )
                        .flex_shrink_0(),
                    )
                    .child(
                        div()
                            .w(px(54.))
                            .flex()
                            .justify_end()
                            .flex_shrink_0()
                            .child(w::mono(figure, theme::palette().text_dim)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .flex_shrink_0()
                            .children(actions),
                    ),
            )
            .child(w::progress(task.fraction(), bar_color))
    }

    // ── Cookies ────────────────────────────────────────────────────────────

    fn cookies_pane(&mut self, cx: &mut Context<Self>) -> Div {
        let mut jar_rows: Vec<Div> = Vec::new();
        if self.store.jars.is_empty() {
            jar_rows.push(
                div()
                    .px(px(8.))
                    .py(px(10.))
                    .child(w::faint(t!("cookies.no_jars"))),
            );
        }
        for jar in self.store.jars.iter() {
            let id = jar.id.clone();
            let selected = self.selected_jar.as_deref() == Some(jar.id.as_str());
            let active = self.store.active.as_deref() == Some(jar.id.as_str());
            let auth = jar.youtube();
            let tone = match auth.state {
                AuthState::SignedIn => theme::palette().accent,
                AuthState::Partial => theme::palette().warn,
                _ => theme::palette().text_faint,
            };
            let summary = tn!(
                "cookies.summary",
                jar.cookies.len(),
                domain = if jar.domain.is_empty() {
                    t!("cookies.unknown")
                } else {
                    jar.domain.as_str()
                }
            );

            let row = div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(3.))
                .border_1()
                .border_color(rgb(if active {
                    theme::palette().accent_dim
                } else if selected {
                    theme::palette().border_strong
                } else {
                    theme::palette().border
                }))
                .bg(rgb(if active {
                    theme::palette().accent_soft
                } else if selected {
                    theme::palette().elevated
                } else {
                    theme::palette().panel
                }))
                .hover(|style| style.bg(rgb(theme::palette().hover)).cursor_pointer())
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(
                        move |this: &mut RootView,
                              _: &gpui::MouseUpEvent,
                              _: &mut Window,
                              cx: &mut Context<RootView>| {
                            this.selected_jar = Some(id.clone());
                            cx.notify();
                        },
                    ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .child(w::dot(tone))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_size(px(13.))
                                .text_color(rgb(theme::palette().text))
                                .truncate()
                                .child(jar.name.clone()),
                        )
                        .when(active, |this| {
                            this.child(w::chip(
                                t!("chip.active"),
                                theme::palette().accent,
                                theme::palette().accent_soft,
                            ))
                        }),
                )
                .child(w::mono(summary, theme::palette().text_faint).truncate());
            jar_rows.push(row);
        }

        let busy = self.busy.clone();

        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .w(px(330.))
                    .flex_shrink_0()
                    .px(px(10.))
                    .py(px(9.))
                    .border_r_1()
                    .border_color(rgb(theme::palette().border))
                    .bg(rgb(theme::palette().panel))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(w::section(t!("cookies.jars", n = self.store.jars.len())))
                            .child(w::faint(t!("cookies.stored"))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .id("jar-list")
                            .overflow_y_scroll()
                            .children(jar_rows),
                    )
                    .child(w::divider())
                    .child(w::section(t!("section.import")))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(w::faint(t!("cookies.domain_hint")))
                            .child(self.domain_input.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .child(
                                w::primary_button(t!("button.paste_cookies")).on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                        this.paste_cookies(cx);
                                    }),
                                ),
                            )
                            .child(
                                w::button(t!("button.from_file")).on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                        this.pick_cookie_file(cx);
                                    }),
                                ),
                            ),
                    )
                    .child(
                        div().flex().items_center().gap(px(5.)).child(
                            w::button(t!("button.from_browser", browser = self.settings.browser)).on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                    this.snapshot_browser(cx);
                                }),
                            ),
                        ),
                    )
                    .when(busy.is_some(), |this| {
                        this.child(w::mono(busy.unwrap_or_default(), theme::palette().warn))
                    })
                    .child(w::faint(t!("cookies.accepts"))),
            )
            .child(self.cookie_detail(cx))
    }

    fn cookie_detail(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let Some(id) = self.selected_jar.clone() else {
            return div()
                .id("jar-empty")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.))
                .child(w::empty_state(
                    t!("cookies.none_selected"),
                    vec![
                        t!("cookies.none_line1"),
                        t!("cookies.none_line2"),
                        t!("cookies.none_line3"),
                    ],
                ));
        };

        let Some(jar) = self.store.jars.iter().find(|jar| jar.id == id) else {
            self.selected_jar = None;
            return div().id("jar-missing").flex().flex_1();
        };

        let auth = jar.youtube();
        let now = human::now_epoch();
        let expired = jar.expired(now);
        let active = self.store.active.as_deref() == Some(id.as_str());
        let name = jar.name.clone();
        let source = crate::app::source_label(&jar.source).to_string();
        let total = jar.cookies.len();
        let domain = jar.domain.clone();
        let netscape = jar.netscape();
        let id_use = id.clone();
        let id_export = id.clone();
        let id_test = id.clone();
        let id_delete = id.clone();

        let mut present: Vec<Div> = Vec::new();
        for cookie_name in auth.present.iter() {
            present.push(w::chip(
                cookie_name.clone(),
                theme::palette().accent_hover,
                theme::palette().accent_soft,
            ));
        }
        let mut missing: Vec<Div> = Vec::new();
        for cookie_name in auth.missing.iter() {
            missing.push(w::chip(
                *cookie_name,
                theme::palette().text_faint,
                theme::palette().elevated,
            ));
        }

        let (auth_fg, auth_bg) = match auth.state {
            AuthState::SignedIn => (theme::palette().accent_hover, theme::palette().accent_soft),
            AuthState::Partial => (theme::palette().warn, theme::palette().warn_soft),
            AuthState::NoAuth => (theme::palette().danger, theme::palette().danger_soft),
            AuthState::NotYouTube => (theme::palette().text_dim, theme::palette().elevated),
        };
        let auth_label = match auth.state {
            AuthState::NotYouTube => t!("auth.not_youtube_jar").to_string(),
            other => other.label().to_string(),
        };

        let mut domain_chips: Vec<Div> = Vec::new();
        for (domain_name, count) in jar.domains().into_iter().take(6) {
            domain_chips.push(w::chip(
                format!("{domain_name} ({count})"),
                theme::palette().text_dim,
                theme::palette().elevated,
            ));
        }

        // Cookie names and lifetimes only: values are secrets and never render.
        let mut table: Vec<Div> = Vec::new();
        table.push(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(6.))
                .py(px(3.))
                .border_b_1()
                .border_color(rgb(theme::palette().border))
                .child(div().w(px(190.)).child(w::section(t!("cookies.col.name"))))
                .child(div().flex_1().child(w::section(t!("cookies.col.domain"))))
                .child(
                    div()
                        .w(px(80.))
                        .child(w::section(t!("cookies.col.expires"))),
                )
                .child(
                    div()
                        .w(px(28.))
                        .child(w::section(t!("cookies.col.http_only"))),
                ),
        );
        for cookie in jar.cookies.iter().take(200) {
            table.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(6.))
                    .py(px(2.))
                    .hover(|style| style.bg(rgb(theme::palette().panel)))
                    .child(
                        div()
                            .w(px(190.))
                            .min_w(px(0.))
                            .child(w::mono(cookie.name.clone(), theme::palette().text).truncate()),
                    )
                    .child(div().flex_1().min_w(px(0.)).child(
                        w::mono(cookie.domain.clone(), theme::palette().text_dim).truncate(),
                    ))
                    .child(div().w(px(80.)).child(w::mono(
                        human::expiry_label(cookie.expires, now),
                        if cookie.expires > 0 && cookie.expires < now {
                            theme::palette().danger
                        } else {
                            theme::palette().text_faint
                        },
                    )))
                    .child(div().w(px(28.)).child(w::mono(
                        if cookie.http_only { "http" } else { "—" },
                        theme::palette().text_faint,
                    ))),
            );
        }

        div()
            .id("jar-detail-pane")
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(9.))
                    .flex_1()
                    .min_h(px(0.))
                    .p(px(12.))
                    .id("jar-detail")
                    .track_scroll(&self.jar_scroll)
                    .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_LG))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .truncate()
                                    .child(name),
                            )
                            .child(w::mono(
                                tn!("cookies.meta", total, source = source, domain = domain),
                                theme::palette().text_faint,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .flex_shrink_0()
                            .when(!active, |this| {
                                this.child(
                                    w::primary_button(t!("button.use_jar")).on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(move |this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                            this.set_active_jar(&id_use, cx);
                                        }),
                                    ),
                                )
                            })
                            .when(active, |this| {
                                this.child(w::chip(t!("chip.active_downloads"), theme::palette().accent, theme::palette().accent_soft))
                            })
                            .child(
                                w::button(t!("button.export")).on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                        this.export_jar(&id_export, cx);
                                    }),
                                ),
                            )
                            .child(
                                w::ghost_button(t!("button.test")).on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                        this.test_jar(&id_test, cx);
                                    }),
                                ),
                            )
                            .child(
                                w::danger_button(t!("button.delete")).on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                        this.remove_jar(&id_delete, cx);
                                    }),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(10.))
                    .rounded(px(3.))
                    .bg(rgb(theme::palette().panel))
                    .border_1()
                    .border_color(rgb(theme::palette().border))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(w::section(t!("section.youtube")))
                            .child(w::chip(auth_label, auth_fg, auth_bg)),
                    )
                    .when(auth.state != AuthState::NotYouTube, |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(4.))
                                .child(w::faint(t!("cookies.present")))
                                .children(present)
                                .when(auth.present.is_empty(), |this| {
                                    this.child(w::mono(t!("cookies.present_none"), theme::palette().danger))
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(4.))
                                .child(w::faint(t!("cookies.missing")))
                                .children(missing)
                                .when(auth.missing.is_empty(), |this| {
                                    this.child(w::mono(t!("cookies.missing_none"), theme::palette().accent))
                                }),
                        )
                        .when(expired > 0, |this| {
                            this.child(w::mono(
                                tn!("cookies.expired", expired),
                                theme::palette().warn,
                            ))
                        })
                        .child(w::faint(t!("cookies.signin_rule")))
                        .when(auth.state == AuthState::SignedIn, |this| {
                            this.child(w::faint(t!("cookies.names_ok")))
                        })
                    })
                    .when(auth.state == AuthState::NotYouTube, |this| {
                        // A jar for another site is perfectly valid; saying
                        // "missing SID" here would read as a failure.
                        this.child(w::faint(t!("cookies.other_site", domain = domain)))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .child(w::section(t!("section.domains")))
                    .children(domain_chips),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(rgb(theme::palette().border))
                    .bg(rgb(theme::palette().panel))
                    .id("cookie-table")
                    .children(table),
            )
            .when(total > 200, |this| {
                this.child(w::faint(t!("cookies.showing", total = total)))
            })
            .child(w::faint(t!("cookies.netscape_size", bytes = netscape.len()))),
        )
        .child(scroll_thumb(&self.jar_scroll))
    }

    // ── Settings ───────────────────────────────────────────────────────────

    fn settings_pane(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut presets: Vec<Div> = Vec::new();
        for preset in FormatPreset::ALL {
            let selected = self.settings.preset == preset;
            presets.push(w::segment(preset.label(), selected).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    move |this: &mut RootView,
                          _: &gpui::MouseUpEvent,
                          _: &mut Window,
                          cx: &mut Context<RootView>| {
                        this.set_preset(preset, cx);
                    },
                ),
            ));
        }

        let mut containers: Vec<Div> = Vec::new();
        for container in Container::ALL {
            let selected = self.settings.container == container;
            containers.push(w::segment_small(container.label(), selected).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    move |this: &mut RootView,
                          _: &gpui::MouseUpEvent,
                          _: &mut Window,
                          cx: &mut Context<RootView>| {
                        this.set_container(container, cx);
                    },
                ),
            ));
        }

        let mut browsers: Vec<Div> = Vec::new();
        for browser in ["chrome", "edge", "brave", "firefox", "vivaldi"] {
            let selected = self.settings.browser == browser;
            browsers.push(w::segment_small(browser, selected).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    move |this: &mut RootView,
                          _: &gpui::MouseUpEvent,
                          _: &mut Window,
                          cx: &mut Context<RootView>| {
                        this.set_browser(browser, cx);
                    },
                ),
            ));
        }

        let concurrency = self.settings.max_concurrent;
        let connections = self.settings.connections;
        let playlist = self.settings.playlist;
        let metadata = self.settings.embed_metadata;
        let use_cookies = self.settings.use_cookies;

        let tool_row = |name: &str, path: Option<std::path::PathBuf>| -> Div {
            let found = path.is_some();
            let text = path
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| t!("tools.not_installed").to_string());
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(10.))
                .py(px(4.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .flex_1()
                        .min_w(px(0.))
                        .child(
                            div()
                                .text_size(px(theme::TEXT_BASE))
                                .text_color(rgb(theme::palette().text))
                                .child(name.to_string()),
                        )
                        .child(w::mono(text, theme::palette().text_faint).truncate()),
                )
                .child(w::chip(
                    if found {
                        t!("chip.ready")
                    } else {
                        t!("chip.missing")
                    },
                    if found {
                        theme::palette().accent_hover
                    } else {
                        theme::palette().danger
                    },
                    if found {
                        theme::palette().accent_soft
                    } else {
                        theme::palette().danger_soft
                    },
                ))
        };

        // Install / refresh, where this build can install tools at all.
        let mut tool_actions: Vec<Div> = Vec::new();
        if crate::core::setup::supported() && self.setup.running.is_none() {
            if !self.setup.missing.is_empty() {
                tool_actions.push(w::button(t!("button.install_missing")).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(
                        |this: &mut RootView,
                         _: &gpui::MouseUpEvent,
                         _: &mut Window,
                         cx: &mut Context<RootView>| {
                            this.install_missing_tools(cx);
                        },
                    ),
                ));
            }
            tool_actions.push(w::ghost_button(t!("button.update_ytdlp")).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    |this: &mut RootView,
                     _: &gpui::MouseUpEvent,
                     _: &mut Window,
                     cx: &mut Context<RootView>| {
                        this.update_ytdlp(cx);
                    },
                ),
            ));
        }

        let check_updates = self.settings.check_updates;
        let update_status = match &self.update {
            UpdateState::Idle => t!("update.status.idle").to_string(),
            UpdateState::Checking => t!("update.status.checking").to_string(),
            UpdateState::UpToDate => t!("update.status.up_to_date").to_string(),
            UpdateState::Ready(ready) => t!("update.status.ready", version = ready.version),
            UpdateState::Available(version) => t!("update.status.out", version = version),
            UpdateState::Failed(error) => t!("update.status.failed", error = error),
        };

        let yt = self.tools.yt_dlp.clone();
        let aria = self.tools.aria2.clone();
        let ff = self.tools.ffmpeg.clone();
        let js_runtime = self.tools.js_runtime.clone();
        let app_data = crate::core::paths::app_data_dir().display().to_string();

        // Two columns keep the whole panel inside a 720px window.
        let mut themes: Vec<Div> = Vec::new();
        for choice in ThemeName::ALL {
            let selected = self.settings.theme == choice;
            themes.push(w::segment_small(choice.label(), selected).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    move |this: &mut RootView,
                          _: &gpui::MouseUpEvent,
                          _: &mut Window,
                          cx: &mut Context<RootView>| {
                        this.set_theme(choice, cx);
                    },
                ),
            ));
        }
        let mut languages: Vec<Div> = Vec::new();
        let choices = std::iter::once(("system", t!("settings.language_system")))
            .chain(crate::i18n::Lang::ALL.map(|lang| (lang.code(), lang.native_name())));
        for (code, name) in choices {
            let selected = self.settings.language == code;
            languages.push(w::segment_small(name, selected).on_mouse_up(
                MouseButton::Left,
                cx.listener(
                    move |this: &mut RootView,
                          _: &gpui::MouseUpEvent,
                          _: &mut Window,
                          cx: &mut Context<RootView>| {
                        this.set_language(code, cx);
                    },
                ),
            ));
        }
        let mut swatches: Vec<Div> = Vec::new();
        for colour in [
            theme::palette().bg,
            theme::palette().panel,
            theme::palette().accent,
            theme::palette().info,
            theme::palette().warn,
            theme::palette().danger,
            theme::palette().text,
        ] {
            swatches.push(
                div()
                    .size(px(14.))
                    .rounded(px(2.))
                    .bg(rgb(colour))
                    .border_1()
                    .border_color(rgb(theme::palette().border_strong)),
            );
        }

        let left = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .flex_1()
            .min_w(px(0.))
            .child(
                div()
                    .flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(w::section(t!("section.appearance")))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(3.))
                            .flex_shrink_0()
                            .children(swatches),
                    ),
            )
            .child(w::setting_stack(
                t!("settings.language"),
                Some(t!("settings.language_hint")),
                languages,
            ))
            .child(w::setting_row(
                t!("settings.theme"),
                Some(t!("settings.theme_hint")),
                div().flex().items_center().gap(px(3.)).children(themes),
            ))
            .child(w::section(t!("section.downloads")))
            .child(w::setting_row(
                t!("settings.save_to"),
                Some(t!("settings.save_to_hint")),
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .w(px(300.))
                    .child(self.dir_input.clone())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .child(w::button(t!("button.apply")).on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                    this.apply_dir(cx);
                                }),
                            ))
                            .child(w::ghost_button(t!("button.default")).on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                    this.reset_dir(cx);
                                }),
                            ))
                            .child(w::ghost_button(t!("button.open")).on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                    let dir = this.settings.dir();
                                    let _ = crate::core::paths::ensure_dir(&dir);
                                    crate::core::paths::reveal(&dir);
                                    cx.notify();
                                }),
                            )),
                    ),
            ))
            .child(w::setting_row(
                t!("settings.concurrency"),
                Some(t!("settings.concurrency_hint")),
                stepper(concurrency, cx, |this, delta, cx| this.set_concurrency(delta, cx)),
            ))
            .child(w::setting_row(
                t!("settings.connections"),
                Some(t!("settings.connections_hint")),
                stepper(connections, cx, |this, delta, cx| this.set_connections(delta, cx)),
            ))
            .child(w::divider())
            .child(w::section(t!("section.media")))
            .child(w::setting_row(
                t!("settings.format"),
                Some(t!("settings.format_hint")),
                div().flex().items_center().gap(px(4.)).children(presets),
            ))
            .child(w::setting_row(
                t!("settings.container"),
                Some(t!("settings.container_hint")),
                div().flex().items_center().gap(px(4.)).children(containers),
            ))
            .child(w::setting_row(
                t!("settings.playlists"),
                Some(t!("settings.playlists_hint")),
                toggle(playlist).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                        this.toggle_playlist(cx);
                    }),
                ),
            ))
            .child(w::setting_row(
                t!("settings.metadata"),
                Some(t!("settings.metadata_hint")),
                toggle(metadata).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                        this.toggle_metadata(cx);
                    }),
                ),
            ));

        let right = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .flex_1()
            .min_w(px(0.))
            .child(w::section(t!("section.cookies")))
            .child(w::setting_row(
                t!("settings.send_jar"),
                Some(t!("settings.send_jar_hint")),
                toggle(use_cookies).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                        this.toggle_cookies(cx);
                    }),
                ),
            ))
            .child(w::setting_stack(
                t!("settings.browser"),
                Some(t!("settings.browser_hint")),
                browsers,
            ))
            .child(w::divider())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(w::section(t!("section.tools")).flex_shrink_0())
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .justify_end()
                            .items_center()
                            .gap(px(4.))
                            .min_w(px(0.))
                            .children(tool_actions)
                            .child(w::ghost_button(t!("button.redetect")).on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                    this.redetect_tools(cx);
                                }),
                            )),
                    ),
            )
            .child(tool_row("yt-dlp", yt))
            .child(tool_row("aria2c", aria))
            .child(tool_row("ffmpeg", ff))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .py(px(4.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_BASE))
                                    .text_color(rgb(theme::palette().text))
                                    .child(t!("tools.js_runtime")),
                            )
                            .child(
                                w::mono(
                                    match js_runtime.as_ref() {
                                        Some(runtime) => {
                                            format!("{} — {}", runtime.name, runtime.path.display())
                                        }
                                        None => t!("tools.js_missing").to_string(),
                                    },
                                    theme::palette().text_faint,
                                )
                                .truncate(),
                            ),
                    )
                    .child(w::chip(
                        if js_runtime.is_some() {
                            t!("chip.ready")
                        } else {
                            t!("chip.missing")
                        },
                        if js_runtime.is_some() {
                            theme::palette().accent_hover
                        } else {
                            theme::palette().warn
                        },
                        if js_runtime.is_some() {
                            theme::palette().accent_soft
                        } else {
                            theme::palette().warn_soft
                        },
                    )),
            )
            .child(w::faint(if js_runtime.is_some() {
                t!("tools.js_ready_hint")
            } else {
                t!("tools.js_missing_hint")
            }))
            .child(w::faint(t!("tools.location_hint")))
            .child(w::divider())
            .child(w::section(t!("section.updates")))
            .child(w::setting_row(
                t!("settings.check_updates"),
                Some(t!("settings.check_updates_hint")),
                toggle(check_updates).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                        this.toggle_check_updates(cx);
                    }),
                ),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(w::mono(update_status, theme::palette().text_faint).truncate())
                    .child(w::ghost_button(t!("button.check_now")).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                            this.check_updates_now(cx);
                        }),
                    )),
            )
            .child(w::divider())
            .child(w::section(t!("section.about")))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(w::mono(
                        format!("edownloader {} · MIT", crate::app::VERSION),
                        theme::palette().text_dim,
                    ))
                    .child(
                        w::mono(app_data, theme::palette().text_faint)
                            .truncate(),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .pt(px(2.))
                            .child(w::ghost_button(t!("button.open_data")).on_mouse_up(
                                MouseButton::Left,
                                |_: &gpui::MouseUpEvent, _: &mut Window, _: &mut gpui::App| {
                                    crate::core::paths::reveal(&crate::core::paths::app_data_dir());
                                },
                            ))
                            .child(w::ghost_button(t!("button.open_downloads")).on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                    let dir = this.settings.dir();
                                    let _ = crate::core::paths::ensure_dir(&dir);
                                    crate::core::paths::reveal(&dir);
                                    cx.notify();
                                }),
                            )),
                    ),
            );

        div()
            .id("settings-pane")
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .id("settings-scroll")
                    .flex()
                    .flex_row()
                    // Without `min_w(0)` the row grows to its content's
                    // widest line and the right column leaves the window.
                    .min_w(px(0.))
                    // Without `items_start` the two columns are stretched to the
                    // viewport height, so their content has nothing to overflow
                    // and the pane cannot scroll.
                    .items_start()
                    .gap(px(18.))
                    .p(px(12.))
                    .pb(px(20.))
                    .flex_1()
                    .min_h(px(0.))
                    .track_scroll(&self.settings_scroll)
                    .overflow_y_scroll()
                    .child(left)
                    .child(right),
            )
            .child(scroll_thumb(&self.settings_scroll))
    }

    fn statusbar(&mut self, cx: &mut Context<Self>) -> Div {
        let active = self.active_count();
        let queued = self.queued_count();
        let done = self.done_count();
        let speed = self.total_speed();

        let summary = tn!(
            "status.summary",
            self.tasks.len(),
            active = active,
            queued = queued,
            done = done
        );

        let right = if speed > 0 {
            t!("status.total", speed = human::speed(speed))
        } else {
            self.engine_status
                .clone()
                .unwrap_or_else(|| t!("status.idle").to_string())
        };

        let notice = self.notice.clone();

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(theme::STATUS_H))
            .px(px(10.))
            .gap(px(10.))
            .bg(rgb(theme::palette().titlebar))
            .border_t_1()
            .border_color(rgb(theme::palette().border))
            .child(w::mono(summary, theme::palette().text_faint))
            .child(div().flex_1().min_w(px(0.)).child(match notice {
                Some(notice) => {
                    let tone = notice.tone;
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(w::dot(tone.color()))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_size(px(theme::TEXT_SM))
                                .text_color(rgb(tone.color()))
                                .truncate()
                                .child(notice.text),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(rgb(theme::palette().text_faint))
                                .hover(|style| style.text_color(rgb(theme::palette().text)).cursor_pointer())
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this: &mut RootView, _: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<RootView>| {
                                        this.notice = None;
                                        cx.notify();
                                    }),
                                )
                                .child(t!("button.dismiss")),
                        )
                }
                None => div(),
            }))
            .child(w::mono(right, theme::palette().text_faint))
    }
}

fn toggle_chip(label: &'static str, on: bool) -> Div {
    let base = div()
        .flex()
        .items_center()
        .h(px(22.))
        .px(px(8.))
        .rounded(px(3.))
        .border_1()
        .text_size(px(11.))
        .hover(|style| style.cursor_pointer());
    if on {
        base.bg(rgb(theme::palette().accent_soft))
            .border_color(rgb(theme::palette().accent_dim))
            .text_color(rgb(theme::palette().accent_hover))
            .child(t!("chip.on", label = label))
    } else {
        base.bg(rgb(theme::palette().elevated))
            .border_color(rgb(theme::palette().border))
            .text_color(rgb(theme::palette().text_faint))
            .child(t!("chip.off", label = label))
    }
}

/// `− value +` with a monospace readout.
fn stepper(
    value: usize,
    cx: &mut Context<RootView>,
    change: fn(&mut RootView, i32, &mut Context<RootView>),
) -> Div {
    let minus = w::ghost_button("−").on_mouse_up(
        MouseButton::Left,
        cx.listener(
            move |this: &mut RootView,
                  _: &gpui::MouseUpEvent,
                  _: &mut Window,
                  cx: &mut Context<RootView>| {
                change(this, -1, cx);
            },
        ),
    );
    let plus = w::ghost_button("+").on_mouse_up(
        MouseButton::Left,
        cx.listener(
            move |this: &mut RootView,
                  _: &gpui::MouseUpEvent,
                  _: &mut Window,
                  cx: &mut Context<RootView>| {
                change(this, 1, cx);
            },
        ),
    );
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .child(minus)
        .child(
            div()
                .w(px(26.))
                .flex()
                .justify_center()
                .child(w::mono(format!("{value}"), theme::palette().text)),
        )
        .child(plus)
}

/// Keep `SharedString` in scope for the view helpers' inferred types.
#[allow(dead_code)]
fn _assert_shared(string: SharedString) -> SharedString {
    string
}
