//! Application state and behaviour.
//!
//! One view owns everything: the task list, the cookie store, the settings and
//! the handle to the download engine. Blocking work happens on engine threads;
//! this type only reacts to events, which keeps rendering free of I/O.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    actions, prelude::*, App, ClipboardItem, Context, FocusHandle, Focusable, KeyBinding,
    ScrollHandle, Window,
};

use crate::core::cookies::{self, CookieJar, Store as CookieStore};
use crate::core::engine::{direct_out_name, Container, Engine, Event, MediaOptions, StartRequest};
use crate::core::human;
use crate::core::paths;
use crate::core::settings::Settings;
use crate::core::setup::{self, Component, SetupEvent};
use crate::core::tools::Tools;
use crate::core::update;
use crate::core::urlx::{self, Link, LinkKind};
use crate::theme::{self, ThemeName};
use crate::ui::text_input::TextInput;

actions!(
    edl,
    [
        SubmitUrl,
        DismissNotice,
        FocusUrl,
        OpenFolder,
        NextTab,
        PrevTab,
        Tab1,
        Tab2,
        Tab3
    ]
);

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tab {
    Downloads,
    Cookies,
    Settings,
}

impl Tab {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Tab::Downloads => "Downloads",
            Tab::Cookies => "Cookies",
            Tab::Settings => "Settings",
        }
    }

    pub(crate) fn all() -> [Tab; 3] {
        [Tab::Downloads, Tab::Cookies, Tab::Settings]
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Tab::Downloads => 0,
            Tab::Cookies => 1,
            Tab::Settings => 2,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tone {
    Ok,
    Warn,
    Err,
    Info,
}

impl Tone {
    pub(crate) fn color(self) -> u32 {
        match self {
            Tone::Ok => crate::theme::palette().accent,
            Tone::Warn => crate::theme::palette().warn,
            Tone::Err => crate::theme::palette().danger,
            Tone::Info => crate::theme::palette().info,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Notice {
    pub(crate) text: String,
    pub(crate) tone: Tone,
}

impl Notice {
    pub(crate) fn ok(text: impl Into<String>) -> Notice {
        Notice {
            text: text.into(),
            tone: Tone::Ok,
        }
    }

    pub(crate) fn warn(text: impl Into<String>) -> Notice {
        Notice {
            text: text.into(),
            tone: Tone::Warn,
        }
    }

    pub(crate) fn err(text: impl Into<String>) -> Notice {
        Notice {
            text: text.into(),
            tone: Tone::Err,
        }
    }

    pub(crate) fn info(text: impl Into<String>) -> Notice {
        Notice {
            text: text.into(),
            tone: Tone::Info,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TaskState {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl TaskState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Running => "running",
            TaskState::Paused => "paused",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
        }
    }

    pub(crate) fn color(self) -> u32 {
        match self {
            TaskState::Queued => crate::theme::palette().text_faint,
            TaskState::Running => crate::theme::palette().accent,
            TaskState::Paused => crate::theme::palette().warn,
            TaskState::Done => crate::theme::palette().accent,
            TaskState::Failed => crate::theme::palette().danger,
            TaskState::Cancelled => crate::theme::palette().text_faint,
        }
    }

    pub(crate) fn is_active(self) -> bool {
        matches!(self, TaskState::Running)
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Done | TaskState::Failed | TaskState::Cancelled
        )
    }
}

pub(crate) struct Task {
    pub(crate) id: u64,
    pub(crate) url: String,
    pub(crate) host: String,
    pub(crate) kind: LinkKind,
    pub(crate) site: Option<&'static str>,
    pub(crate) title: String,
    pub(crate) state: TaskState,
    pub(crate) stage: String,
    pub(crate) downloaded: u64,
    pub(crate) total: Option<u64>,
    pub(crate) speed: u64,
    pub(crate) connections: u64,
    pub(crate) path: Option<PathBuf>,
    pub(crate) error: Option<String>,
    pub(crate) log: String,
}

impl Task {
    pub(crate) fn is_pausable(&self) -> bool {
        self.kind != LinkKind::Media && matches!(self.state, TaskState::Running | TaskState::Paused)
    }

    pub(crate) fn fraction(&self) -> f32 {
        human::bar_fraction(self.downloaded, self.total)
    }

    pub(crate) fn eta(&self) -> Option<u64> {
        let total = self.total?;
        if self.speed == 0 || total <= self.downloaded {
            return None;
        }
        Some((total - self.downloaded) / self.speed)
    }

    /// Second line of a task row: where it came from and how it is going.
    pub(crate) fn subtitle(&self) -> String {
        let source = self.site.unwrap_or(self.host.as_str());
        match self.state {
            TaskState::Queued => format!("{source} · waiting for a slot"),
            TaskState::Done => {
                let size = self
                    .total
                    .or(self
                        .path
                        .as_ref()
                        .and_then(|p| std::fs::metadata(p).ok())
                        .map(|m| m.len()))
                    .map(human::bytes)
                    .unwrap_or_else(|| "--".to_string());
                let name = self
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name.is_empty() {
                    format!("{source} · {size}")
                } else {
                    format!("{source} · {size} · {name}")
                }
            }
            TaskState::Failed => format!(
                "{source} · {}",
                self.error.clone().unwrap_or_else(|| "failed".into())
            ),
            TaskState::Cancelled => format!("{source} · cancelled"),
            TaskState::Paused => {
                let done = human::bytes(self.downloaded);
                match self.total {
                    Some(total) => format!("{source} · {done} of {} · paused", human::bytes(total)),
                    None => format!("{source} · {done} · paused"),
                }
            }
            TaskState::Running => {
                let speed = if self.speed > 0 {
                    format!(" · {}", human::speed(self.speed))
                } else {
                    String::new()
                };
                let size = match self.total {
                    Some(total) => format!(
                        "{} of {}",
                        human::bytes(self.downloaded),
                        human::bytes(total)
                    ),
                    None => human::bytes(self.downloaded),
                };
                let eta = self
                    .eta()
                    .map(|s| format!(" · {} left", human::eta(s)))
                    .unwrap_or_default();
                let stage = if self.total.is_none() && self.downloaded == 0 {
                    format!(" · {}", self.stage)
                } else {
                    String::new()
                };
                format!("{source} · {size}{speed}{eta}{stage}")
            }
        }
    }
}

/// Results from side actions that must not block the UI thread.
pub(crate) enum Side {
    PickedFile(Option<PathBuf>),
    Snapshot(Result<String, String>),
    Setup(SetupEvent),
    UpdateChecked {
        result: Result<Option<update::Found>, String>,
        manual: bool,
    },
}

/// The tool installer, as the banner and Settings show it.
#[derive(Default)]
pub(crate) struct SetupState {
    /// What detection did not find; offered at launch.
    pub(crate) missing: Vec<Component>,
    /// "Not now": the launch banner stays hidden for this session.
    pub(crate) dismissed: bool,
    pub(crate) running: Option<SetupRun>,
}

pub(crate) struct SetupRun {
    pub(crate) count: usize,
    pub(crate) finished: usize,
    pub(crate) current: Option<Component>,
    pub(crate) stage: &'static str,
    pub(crate) done: u64,
    pub(crate) total: Option<u64>,
    pub(crate) errors: Vec<String>,
    pub(crate) installed: Vec<&'static str>,
}

pub(crate) enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    /// Verified installer, run from the banner or when the app closes.
    Ready(update::Update),
    /// A portable copy: only says the version is out.
    Available(String),
    Failed(String),
}

pub(crate) struct RootView {
    /// Whether the desktop was in dark mode when the view was built.
    pub(crate) desktop_is_dark: bool,
    /// Scroll state, kept so the panes can draw their own thumb.
    pub(crate) tasks_scroll: ScrollHandle,
    pub(crate) settings_scroll: ScrollHandle,
    pub(crate) jar_scroll: ScrollHandle,
    pub(crate) tab: Tab,
    pub(crate) url_input: gpui::Entity<TextInput>,
    pub(crate) domain_input: gpui::Entity<TextInput>,
    pub(crate) dir_input: gpui::Entity<TextInput>,
    pub(crate) settings: Settings,
    pub(crate) tools: Tools,
    pub(crate) store: CookieStore,
    pub(crate) cookie_file: Option<PathBuf>,
    pub(crate) tasks: Vec<Task>,
    pub(crate) next_id: u64,
    pub(crate) engine: Arc<Engine>,
    pub(crate) events: Arc<Mutex<Receiver<Event>>>,
    pub(crate) sides: Arc<Mutex<Receiver<Side>>>,
    pub(crate) side_tx: Sender<Side>,
    pub(crate) notice: Option<Notice>,
    pub(crate) selected_jar: Option<String>,
    pub(crate) busy: Option<String>,
    pub(crate) engine_status: Option<String>,
    pub(crate) root_focus: FocusHandle,
    pub(crate) show_details: Option<u64>,
    pub(crate) setup: SetupState,
    pub(crate) update: UpdateState,
    /// The update banner was closed for this session.
    pub(crate) update_dismissed: bool,
}

impl Focusable for RootView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.root_focus.clone()
    }
}

impl RootView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut settings = Settings::load();
        settings.sanitize();
        // Resolve `system` once against the desktop appearance before the first
        // paint, so choosing it later never depends on which window is open.
        let desktop_is_dark = matches!(
            cx.window_appearance(),
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        );
        theme::apply(settings.theme.resolve(desktop_is_dark));
        let tools = Tools::detect();
        let mut store = CookieStore::load();
        if store.active.is_some() && store.active_jar().is_none() {
            store.active = None;
        }
        let cookie_file = store.sync_active_file();
        let selected_jar = store.active.clone();

        let (event_tx, event_rx) = mpsc::channel();
        let (side_tx, side_rx) = mpsc::channel();
        let engine = Engine::new(
            event_tx,
            tools.clone(),
            settings.dir(),
            settings.connections,
        );
        engine.set_options(
            settings.dir(),
            settings.connections,
            settings.max_concurrent,
        );

        let dir_value = settings.download_dir.clone();
        let url_input = cx.new(|cx| TextInput::new("https://…  or  magnet:?xt=…", cx));
        let domain_input = cx.new(|cx| TextInput::new("domain, e.g. youtube.com", cx));
        let dir_input = cx.new(|cx| {
            let mut input = TextInput::new("download folder", cx);
            input.set_content(&dir_value, cx);
            input
        });

        // Any edit in a field re-renders the shell (button states, hints).
        cx.observe(&url_input, |_, _, cx| cx.notify()).detach();
        cx.observe(&domain_input, |_, _, cx| cx.notify()).detach();
        cx.observe(&dir_input, |_, _, cx| cx.notify()).detach();

        let missing = if setup::supported() {
            setup::missing(&tools)
        } else {
            Vec::new()
        };
        let check_updates = settings.check_updates && update::wanted();

        let mut view = RootView {
            setup: SetupState {
                missing,
                ..SetupState::default()
            },
            update: UpdateState::Idle,
            update_dismissed: false,
            tab: Tab::Downloads,
            url_input,
            domain_input,
            dir_input,
            settings,
            desktop_is_dark,
            tasks_scroll: ScrollHandle::new(),
            settings_scroll: ScrollHandle::new(),
            jar_scroll: ScrollHandle::new(),
            tools,
            store,
            cookie_file,
            tasks: Vec::new(),
            next_id: 1,
            engine,
            events: Arc::new(Mutex::new(event_rx)),
            sides: Arc::new(Mutex::new(side_rx)),
            side_tx,
            notice: None,
            selected_jar,
            busy: None,
            engine_status: None,
            root_focus: cx.focus_handle(),
            show_details: None,
        };
        view.start_pump(cx);
        // However the app ends (title bar, Alt+F4, the taskbar), the engine
        // stops and a verified update installs. `shutdown` is idempotent.
        cx.on_app_quit(|this: &mut RootView, _| {
            this.shutdown();
            async {}
        })
        .detach();
        if check_updates {
            view.start_update_check(false);
        }
        view
    }

    // ── Tools and updates ──────────────────────────────────────────────────

    /// Installs `components` in the background; progress arrives as side
    /// events.
    pub(crate) fn install_tools(&mut self, components: Vec<Component>, cx: &mut Context<Self>) {
        if self.setup.running.is_some() || components.is_empty() {
            return;
        }
        self.setup.running = Some(SetupRun {
            count: components.len(),
            finished: 0,
            current: components.first().copied(),
            stage: "starting",
            done: 0,
            total: None,
            errors: Vec::new(),
            installed: Vec::new(),
        });
        let tx = self.side_tx.clone();
        setup::spawn(components, move |event| {
            let _ = tx.send(Side::Setup(event));
        });
        cx.notify();
    }

    pub(crate) fn install_missing_tools(&mut self, cx: &mut Context<Self>) {
        let missing = setup::missing(&self.tools);
        self.install_tools(missing, cx);
    }

    /// yt-dlp follows YouTube's changes week by week: a fresh copy is the
    /// first thing to try when media links start failing.
    pub(crate) fn update_ytdlp(&mut self, cx: &mut Context<Self>) {
        self.install_tools(vec![Component::YtDlp], cx);
    }

    pub(crate) fn dismiss_setup(&mut self, cx: &mut Context<Self>) {
        self.setup.dismissed = true;
        cx.notify();
    }

    fn start_update_check(&mut self, manual: bool) {
        if matches!(self.update, UpdateState::Checking) {
            return;
        }
        self.update = UpdateState::Checking;
        let tx = self.side_tx.clone();
        std::thread::spawn(move || {
            let result = update::check();
            let _ = tx.send(Side::UpdateChecked { result, manual });
        });
    }

    pub(crate) fn check_updates_now(&mut self, cx: &mut Context<Self>) {
        self.update_dismissed = false;
        self.start_update_check(true);
        cx.notify();
    }

    pub(crate) fn toggle_check_updates(&mut self, cx: &mut Context<Self>) {
        self.settings.check_updates = !self.settings.check_updates;
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn dismiss_update(&mut self, cx: &mut Context<Self>) {
        self.update_dismissed = true;
        cx.notify();
    }

    /// Runs the installer with a progress window and quits; the installer
    /// starts the new version when it is done.
    pub(crate) fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Ready(ready) = &self.update else {
            return;
        };
        match update::install(ready, true) {
            Ok(()) => {
                // Already running: nothing left to install on close.
                self.update = UpdateState::Idle;
                self.shutdown();
                cx.quit();
            }
            Err(error) => {
                self.notice = Some(Notice::err(format!("Cannot start the installer: {error}")));
                cx.notify();
            }
        }
    }

    pub(crate) fn open_release_page(&mut self, version: &str, cx: &mut Context<Self>) {
        cx.open_url(&format!("{}{version}", update::RELEASE_PAGE));
    }

    /// Drain engine and side-action channels on the UI thread, forever.
    ///
    /// The future owns its `AsyncApp`, so it can be `'static`: `Context::spawn`
    /// with an async closure cannot express a borrow across awaits in a
    /// `'static` future, and this pump outlives the call that starts it.
    fn start_pump(&self, cx: &mut Context<Self>) {
        let events = self.events.clone();
        let sides = self.sides.clone();
        let weak = cx.entity().downgrade();
        let async_cx = cx.to_async();
        let executor = async_cx.background_executor().clone();
        cx.foreground_executor()
            .spawn(async move {
                loop {
                    executor.timer(Duration::from_millis(120)).await;
                    let engine_batch: Vec<Event> = events
                        .lock()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    let side_batch: Vec<Side> = sides
                        .lock()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    if engine_batch.is_empty() && side_batch.is_empty() {
                        continue;
                    }
                    let result = async_cx.update(|app| {
                        weak.update(app, |view, cx| {
                            view.apply_engine(engine_batch);
                            view.apply_sides(side_batch, cx);
                            view.pump(cx);
                            cx.notify();
                        })
                    });
                    if result.is_err() {
                        break;
                    }
                }
            })
            .detach();
    }

    fn task_mut(&mut self, id: u64) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|task| task.id == id)
    }

    pub(crate) fn active_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.state.is_active()).count()
    }

    pub(crate) fn queued_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.state == TaskState::Queued)
            .count()
    }

    pub(crate) fn total_speed(&self) -> u64 {
        self.tasks.iter().map(|t| t.speed).sum()
    }

    pub(crate) fn done_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.state == TaskState::Done)
            .count()
    }

    fn apply_engine(&mut self, batch: Vec<Event>) {
        for event in batch {
            match event {
                Event::Progress {
                    id,
                    downloaded,
                    total,
                    speed,
                    connections,
                } => {
                    if let Some(task) = self.task_mut(id) {
                        task.downloaded = downloaded;
                        if total.is_some() {
                            task.total = total;
                        }
                        task.speed = speed;
                        task.connections = connections;
                        if task.state == TaskState::Queued {
                            task.state = TaskState::Running;
                        }
                    }
                }
                Event::Stage { id, stage } => {
                    if let Some(task) = self.task_mut(id) {
                        match stage.as_str() {
                            "paused" => {
                                task.state = TaskState::Paused;
                                task.speed = 0;
                            }
                            _ => {
                                if task.state != TaskState::Paused {
                                    task.state = TaskState::Running;
                                }
                                task.stage = stage;
                            }
                        }
                    }
                }
                Event::Log { id, line } => {
                    if let Some(task) = self.task_mut(id) {
                        task.log = line;
                    }
                }
                Event::Done { id, path, size } => {
                    if let Some(task) = self.task_mut(id) {
                        task.state = TaskState::Done;
                        task.speed = 0;
                        if size > 0 {
                            task.total = Some(size);
                            task.downloaded = size;
                        }
                        if let Some(name) = path.file_name() {
                            task.title = name.to_string_lossy().to_string();
                        }
                        task.path = Some(path);
                        task.stage = "done".into();
                    }
                    self.engine.forget(id);
                }
                Event::Failed { id, message } => {
                    if let Some(task) = self.task_mut(id) {
                        task.state = TaskState::Failed;
                        task.speed = 0;
                        task.error = Some(message);
                    }
                    self.engine.forget(id);
                }
                Event::EngineStatus { ok, message } => {
                    self.engine_status = Some(message);
                    if !ok {
                        self.notice = Some(Notice::err("The aria2 engine stopped responding."));
                    }
                }
            }
        }
    }

    fn apply_sides(&mut self, batch: Vec<Side>, cx: &mut Context<Self>) {
        for side in batch {
            match side {
                Side::PickedFile(Some(path)) => {
                    self.busy = None;
                    match std::fs::read_to_string(&path) {
                        Ok(text) => {
                            self.import_cookies(&text, "file", None, cx);
                        }
                        Err(error) => {
                            self.notice =
                                Some(Notice::err(format!("Cannot read that file: {error}")));
                        }
                    }
                }
                Side::PickedFile(None) => self.busy = None,
                Side::Snapshot(Ok(text)) => {
                    self.busy = None;
                    self.import_cookies(&text, "browser", None, cx);
                }
                Side::Snapshot(Err(error)) => {
                    self.busy = None;
                    self.notice = Some(Notice::err(error));
                }
                Side::Setup(event) => self.apply_setup(event),
                Side::UpdateChecked { result, manual } => self.apply_update(result, manual),
            }
        }
    }

    fn apply_setup(&mut self, event: SetupEvent) {
        let Some(run) = self.setup.running.as_mut() else {
            return;
        };
        match event {
            SetupEvent::Progress {
                component,
                stage,
                done,
                total,
            } => {
                run.current = Some(component);
                run.stage = stage;
                run.done = done;
                run.total = total;
            }
            SetupEvent::Installed(component) => {
                run.finished += 1;
                run.installed.push(component.label());
                run.current = None;
                // Usable at once, for the next job the engine starts.
                self.tools = Tools::detect();
                self.engine.set_tools(self.tools.clone());
            }
            SetupEvent::Failed { component, error } => {
                run.finished += 1;
                run.current = None;
                run.errors.push(format!("{}: {error}", component.label()));
            }
            SetupEvent::Finished { failed } => {
                let errors = std::mem::take(&mut run.errors);
                let names = run.installed.join(", ");
                self.setup.running = None;
                self.tools = Tools::detect();
                self.engine.set_tools(self.tools.clone());
                self.setup.missing = setup::missing(&self.tools);
                self.notice = Some(if failed == 0 {
                    Notice::ok(format!("Installed {names}. Ready to download."))
                } else {
                    Notice::err(errors.join(" · "))
                });
            }
        }
    }

    fn apply_update(&mut self, result: Result<Option<update::Found>, String>, manual: bool) {
        self.update = match result {
            Ok(Some(update::Found::Ready(ready))) => {
                if manual {
                    self.notice = Some(Notice::ok(format!(
                        "Version {} is ready: restart to install it.",
                        ready.version
                    )));
                }
                UpdateState::Ready(ready)
            }
            Ok(Some(update::Found::Available(version))) => {
                if manual {
                    self.notice = Some(Notice::info(format!("Version {version} is out.")));
                }
                UpdateState::Available(version)
            }
            Ok(None) => {
                if manual {
                    self.notice = Some(Notice::ok(format!("edownloader {VERSION} is up to date.")));
                }
                UpdateState::UpToDate
            }
            Err(error) => {
                // At launch an offline machine is no news; asked by hand it is.
                if manual {
                    self.notice = Some(Notice::err(format!("Update check failed: {error}")));
                }
                UpdateState::Failed(error)
            }
        };
    }

    /// Start queued work while slots are free. Oldest first.
    fn pump(&mut self, cx: &mut Context<Self>) {
        let max = self.settings.max_concurrent.max(1);
        for _ in 0..64 {
            if self.active_count() >= max {
                break;
            }
            let dest_dir = self.settings.dir();
            let media = MediaOptions {
                preset: self.settings.preset,
                container: self.settings.container,
                playlist: self.settings.playlist,
                embed_metadata: self.settings.embed_metadata,
            };
            let cookie_file = if self.settings.use_cookies {
                self.cookie_file.clone()
            } else {
                None
            };
            let connections = self.settings.connections;

            let Some(task) = self
                .tasks
                .iter_mut()
                .rev()
                .find(|task| task.state == TaskState::Queued)
            else {
                break;
            };
            let request = StartRequest {
                id: task.id,
                url: task.url.clone(),
                kind: task.kind,
                dest_dir,
                out_name: if task.kind == LinkKind::Direct {
                    direct_out_name(&task.url)
                } else {
                    None
                },
                media,
                cookie_file,
                connections,
            };
            task.state = TaskState::Running;
            task.stage = "starting".into();
            task.error = None;
            self.engine.start(request);
        }
        cx.notify();
    }

    pub(crate) fn add_link(&mut self, raw: &str, cx: &mut Context<Self>) {
        match urlx::classify(raw) {
            Ok(link) => {
                self.enqueue(link);
                self.url_input.update(cx, |input, cx| input.clear(cx));
                self.notice = None;
                self.pump(cx);
            }
            Err(message) => {
                self.notice = Some(Notice::err(message));
                cx.notify();
            }
        }
    }

    fn enqueue(&mut self, link: Link) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let title = match link.kind {
            LinkKind::Media => link
                .site
                .map(|site| format!("{site} link"))
                .unwrap_or_else(|| link.host.clone()),
            LinkKind::Torrent => "BitTorrent download".to_string(),
            LinkKind::Direct => {
                urlx::file_hint(&link.url).unwrap_or_else(|| human::short_url(&link.url, 70))
            }
        };
        self.tasks.insert(
            0,
            Task {
                id,
                url: link.url,
                host: link.host,
                kind: link.kind,
                site: link.site,
                title,
                state: TaskState::Queued,
                stage: "queued".into(),
                downloaded: 0,
                total: None,
                speed: 0,
                connections: 0,
                path: None,
                error: None,
                log: String::new(),
            },
        );
        id
    }

    fn persist_settings(&mut self) {
        self.settings.sanitize();
        if let Err(error) = self.settings.save() {
            self.notice = Some(Notice::err(error));
        }
        self.engine.set_options(
            self.settings.dir(),
            self.settings.connections,
            self.settings.max_concurrent,
        );
    }

    fn persist_cookies(&mut self) {
        if let Err(error) = self.store.save() {
            self.notice = Some(Notice::err(error));
            return;
        }
        self.cookie_file = self.store.sync_active_file();
    }

    /// Parse cookie text and store it as a jar, making it active.
    pub(crate) fn import_cookies(
        &mut self,
        text: &str,
        source: &str,
        name: Option<&str>,
        cx: &mut Context<Self>,
    ) -> bool {
        let hint = self.domain_input.read(cx).content().trim().to_string();
        let domain_hint = if hint.is_empty() { None } else { Some(hint) };
        match cookies::parse_auto(text, domain_hint.as_deref()) {
            Ok(list) => {
                let mut jar = CookieJar::build(name.unwrap_or(""), source, list);
                if let Some(explicit) = name {
                    jar.name = explicit.to_string();
                }
                let count = jar.cookies.len();
                let domain = if jar.domain.is_empty() {
                    "unknown domain".to_string()
                } else {
                    jar.domain.clone()
                };
                let auth = jar.youtube();
                let id = jar.id.clone();
                self.store.upsert(jar);
                self.store.set_active(Some(&id));
                self.selected_jar = Some(id);
                self.persist_cookies();
                let suffix = match auth.state {
                    cookies::AuthState::SignedIn => " · YouTube sign-in detected",
                    cookies::AuthState::Partial => " · YouTube sign-in incomplete",
                    _ => "",
                };
                self.notice = Some(Notice::ok(format!(
                    "Imported {count} cookie{} for {domain}{suffix}",
                    if count == 1 { "" } else { "s" }
                )));
                true
            }
            Err(message) => {
                self.notice = Some(Notice::err(message));
                false
            }
        }
    }

    // ── Actions ────────────────────────────────────────────────────────────

    /// Parse the field and queue it, then put the caret back in the field.
    pub(crate) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.url_input.read(cx).content().to_string();
        self.add_link(&raw, cx);
        let handle = self.url_input.read(cx).focus_handle(cx);
        window.focus(&handle);
    }

    pub(crate) fn on_submit(&mut self, _: &SubmitUrl, window: &mut Window, cx: &mut Context<Self>) {
        self.submit(window, cx);
    }

    pub(crate) fn on_dismiss(&mut self, _: &DismissNotice, _: &mut Window, cx: &mut Context<Self>) {
        self.notice = None;
        self.show_details = None;
        cx.notify();
    }

    pub(crate) fn on_focus_url(
        &mut self,
        _: &FocusUrl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tab = Tab::Downloads;
        let handle = self.url_input.read(cx).focus_handle(cx);
        window.focus(&handle);
        cx.notify();
    }

    pub(crate) fn on_open_folder(
        &mut self,
        _: &OpenFolder,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dir = self.settings.dir();
        let _ = paths::ensure_dir(&dir);
        paths::reveal(&dir);
        cx.notify();
    }

    pub(crate) fn on_next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        let next = (self.tab.index() + 1) % 3;
        self.tab = Tab::all()[next];
        cx.notify();
    }

    pub(crate) fn on_tab1(&mut self, _: &Tab1, _: &mut Window, cx: &mut Context<Self>) {
        self.set_tab(Tab::Downloads, cx);
    }

    pub(crate) fn on_tab2(&mut self, _: &Tab2, _: &mut Window, cx: &mut Context<Self>) {
        self.set_tab(Tab::Cookies, cx);
    }

    pub(crate) fn on_tab3(&mut self, _: &Tab3, _: &mut Window, cx: &mut Context<Self>) {
        self.set_tab(Tab::Settings, cx);
    }

    pub(crate) fn on_prev_tab(&mut self, _: &PrevTab, _: &mut Window, cx: &mut Context<Self>) {
        let prev = (self.tab.index() + 2) % 3;
        self.tab = Tab::all()[prev];
        cx.notify();
    }

    pub(crate) fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    pub(crate) fn paste_and_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let cleaned = text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim();
            self.add_link(cleaned, cx);
        } else {
            self.notice = Some(Notice::warn("The clipboard is empty."));
            cx.notify();
        }
        let handle = self.url_input.read(cx).focus_handle(cx);
        window.focus(&handle);
    }

    pub(crate) fn cancel_task(&mut self, id: u64, cx: &mut Context<Self>) {
        self.engine.cancel(id);
        let terminal = self
            .task_mut(id)
            .map(|task| task.state.is_terminal())
            .unwrap_or(false);
        if terminal {
            self.tasks.retain(|task| task.id != id);
        } else if let Some(task) = self.task_mut(id) {
            task.state = TaskState::Cancelled;
            task.speed = 0;
        }
        self.pump(cx);
        cx.notify();
    }

    pub(crate) fn toggle_pause(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(task) = self.task_mut(id) {
            match task.state {
                TaskState::Paused => {
                    task.state = TaskState::Running;
                    self.engine.resume(id);
                }
                TaskState::Running => {
                    task.state = TaskState::Paused;
                    task.speed = 0;
                    self.engine.pause(id);
                }
                _ => {}
            }
        }
        cx.notify();
    }

    pub(crate) fn retry_task(&mut self, id: u64, cx: &mut Context<Self>) {
        self.engine.forget(id);
        if let Some(task) = self.task_mut(id) {
            task.state = TaskState::Queued;
            task.error = None;
            task.stage = "queued".into();
            task.downloaded = 0;
            task.total = None;
            task.speed = 0;
            task.path = None;
        }
        self.pump(cx);
        cx.notify();
    }

    pub(crate) fn clear_finished(&mut self, cx: &mut Context<Self>) {
        let keep: Vec<u64> = self
            .tasks
            .iter()
            .filter(|t| t.state.is_terminal())
            .map(|t| t.id)
            .collect();
        for id in keep {
            self.engine.forget(id);
        }
        self.tasks.retain(|t| !t.state.is_terminal());
        cx.notify();
    }

    pub(crate) fn open_task(&mut self, id: u64, cx: &mut Context<Self>) {
        let target = self
            .task_mut(id)
            .and_then(|t| t.path.clone())
            .and_then(|p| p.parent().map(|parent| parent.to_path_buf()))
            .unwrap_or_else(|| self.settings.dir());
        paths::reveal(&target);
        cx.notify();
    }

    pub(crate) fn copy_url(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(url) = self.task_mut(id).map(|t| t.url.clone()) {
            cx.write_to_clipboard(ClipboardItem::new_string(url));
            self.notice = Some(Notice::info("Link copied."));
        }
        cx.notify();
    }

    // ── Cookies ────────────────────────────────────────────────────────────

    pub(crate) fn paste_cookies(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.notice = Some(Notice::warn("The clipboard is empty."));
            cx.notify();
            return;
        };
        self.import_cookies(&text, "clipboard", None, cx);
        cx.notify();
    }

    pub(crate) fn pick_cookie_file(&mut self, cx: &mut Context<Self>) {
        let tx = self.side_tx.clone();
        self.busy = Some("Waiting for the file picker…".into());
        cx.notify();
        std::thread::spawn(move || {
            let picked = paths::pick_open_file(
                "Import cookies",
                "Cookie files (*.txt;*.json)|*.txt;*.json|All files (*.*)|*.*",
            );
            let _ = tx.send(Side::PickedFile(picked));
        });
    }

    pub(crate) fn snapshot_browser(&mut self, cx: &mut Context<Self>) {
        let Some(ytdlp) = self.tools.yt_dlp.clone() else {
            self.notice = Some(Notice::err(
                "yt-dlp is required to read cookies out of a browser.",
            ));
            cx.notify();
            return;
        };
        let browser = self.settings.browser.clone();
        let out = paths::app_data_dir().join("browser-cookies.txt");
        let _ = std::fs::remove_file(&out);
        let tx = self.side_tx.clone();
        self.busy = Some(format!("Reading cookies from {browser}…"));
        self.notice = None;
        cx.notify();
        std::thread::spawn(move || {
            let result = export_browser_cookies(&ytdlp, &browser, &out);
            let _ = tx.send(Side::Snapshot(result));
        });
    }

    pub(crate) fn set_active_jar(&mut self, id: &str, cx: &mut Context<Self>) {
        self.store.set_active(Some(id));
        self.selected_jar = Some(id.to_string());
        self.persist_cookies();
        if let Some(jar) = self.store.active_jar() {
            self.notice = Some(Notice::ok(format!("Downloads now use “{}”.", jar.name)));
        }
        cx.notify();
    }

    pub(crate) fn remove_jar(&mut self, id: &str, cx: &mut Context<Self>) {
        self.store.remove(id);
        if self.selected_jar.as_deref() == Some(id) {
            self.selected_jar = self.store.active.clone();
        }
        self.persist_cookies();
        self.notice = Some(Notice::info("Cookie jar removed."));
        cx.notify();
    }

    pub(crate) fn export_jar(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(jar) = self.store.jars.iter().find(|jar| jar.id == id) else {
            return;
        };
        let dir = self.settings.dir();
        if paths::ensure_dir(&dir).is_err() {
            self.notice = Some(Notice::err("Cannot create the download folder."));
            cx.notify();
            return;
        }
        let domain = if jar.domain.is_empty() {
            "cookies".to_string()
        } else {
            jar.domain.replace('.', "_")
        };
        let path = dir.join(format!("{domain}-cookies.txt"));
        match std::fs::write(&path, jar.netscape()) {
            Ok(()) => {
                self.notice = Some(Notice::ok(format!("Exported to {}", path.display())));
                paths::reveal(&dir);
            }
            Err(error) => {
                self.notice = Some(Notice::err(format!("Cannot write the export: {error}")))
            }
        }
        cx.notify();
    }

    pub(crate) fn test_jar(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(jar) = self.store.jars.iter().find(|jar| jar.id == id) else {
            return;
        };
        let report = jar.youtube();
        let message = match report.state {
            cookies::AuthState::SignedIn => format!(
                "Signed in. {} YouTube cookies, {} expired.",
                report.total, report.expired
            ),
            cookies::AuthState::Partial => format!(
                "Incomplete sign-in — missing {}.",
                report.missing.join(", ")
            ),
            cookies::AuthState::NoAuth => {
                "No YouTube authentication cookies in this jar.".to_string()
            }
            cookies::AuthState::NotYouTube => "This jar has no youtube.com cookies.".to_string(),
        };
        let tone = match report.state {
            cookies::AuthState::SignedIn if report.expired == 0 => Tone::Ok,
            cookies::AuthState::SignedIn => Tone::Warn,
            _ => Tone::Err,
        };
        self.notice = Some(Notice {
            text: message,
            tone,
        });
        cx.notify();
    }

    // ── Settings ───────────────────────────────────────────────────────────

    pub(crate) fn set_connections(&mut self, value: i32, cx: &mut Context<Self>) {
        let next = (self.settings.connections as i32 + value).clamp(1, 16) as usize;
        self.settings.connections = next;
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn set_concurrency(&mut self, value: i32, cx: &mut Context<Self>) {
        let next = (self.settings.max_concurrent as i32 + value).clamp(1, 8) as usize;
        self.settings.max_concurrent = next;
        self.persist_settings();
        self.pump(cx);
        cx.notify();
    }

    pub(crate) fn set_preset(
        &mut self,
        preset: crate::core::engine::FormatPreset,
        cx: &mut Context<Self>,
    ) {
        self.settings.preset = preset;
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn set_theme(&mut self, choice: ThemeName, cx: &mut Context<Self>) {
        self.settings.theme = choice;
        theme::apply(choice.resolve(self.desktop_is_dark));
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn set_container(&mut self, container: Container, cx: &mut Context<Self>) {
        self.settings.container = container;
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn toggle_playlist(&mut self, cx: &mut Context<Self>) {
        self.settings.playlist = !self.settings.playlist;
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn toggle_metadata(&mut self, cx: &mut Context<Self>) {
        self.settings.embed_metadata = !self.settings.embed_metadata;
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn toggle_cookies(&mut self, cx: &mut Context<Self>) {
        self.settings.use_cookies = !self.settings.use_cookies;
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn set_browser(&mut self, browser: &str, cx: &mut Context<Self>) {
        self.settings.browser = browser.to_string();
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn apply_dir(&mut self, cx: &mut Context<Self>) {
        let value = self.dir_input.read(cx).content().trim().to_string();
        if value.is_empty() {
            self.settings.download_dir = paths::default_download_dir().display().to_string();
            let default = self.settings.download_dir.clone();
            self.dir_input
                .update(cx, |input, cx| input.set_content(default, cx));
        } else {
            self.settings.download_dir = value;
        }
        let dir = self.settings.dir();
        if let Err(error) = paths::ensure_dir(&dir) {
            self.notice = Some(Notice::err(format!("Cannot use that folder: {error}")));
        } else {
            self.notice = Some(Notice::ok(format!("Downloads go to {}", dir.display())));
        }
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn reset_dir(&mut self, cx: &mut Context<Self>) {
        self.settings.download_dir = paths::default_download_dir().display().to_string();
        let default = self.settings.download_dir.clone();
        self.dir_input
            .update(cx, |input, cx| input.set_content(default, cx));
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn redetect_tools(&mut self, cx: &mut Context<Self>) {
        self.tools = Tools::detect();
        self.engine.set_tools(self.tools.clone());
        self.setup.missing = setup::missing(&self.tools);
        let found = self.tools.ready_count();
        let total = crate::core::tools::TOOL_COUNT;
        self.notice = Some(if found == total {
            Notice::ok("Every tool is available.")
        } else {
            Notice::warn(format!("{found} of {total} tools found."))
        });
        cx.notify();
    }

    pub(crate) fn shutdown(&mut self) {
        self.engine.shutdown();
        // A verified update installs silently as the app closes: the next
        // launch is the new version.
        if let UpdateState::Ready(ready) = std::mem::replace(&mut self.update, UpdateState::Idle) {
            let _ = update::install(&ready, false);
        }
    }
}

/// Ask yt-dlp to read the browser's cookie store and persist it as a jar.
///
/// yt-dlp exports the jar to `--cookies` on exit even when extraction fails,
/// so the exit status is not what decides success — the file is.
fn export_browser_cookies(
    ytdlp: &std::path::Path,
    browser: &str,
    out: &std::path::Path,
) -> Result<String, String> {
    use std::process::{Command, Stdio};

    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut command = Command::new(ytdlp);
    command
        .arg("--cookies-from-browser")
        .arg(browser)
        .arg("--cookies")
        .arg(out)
        .arg("--simulate")
        .arg("--no-warnings")
        .arg("--skip-download")
        .arg("https://www.youtube.com/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command
        .output()
        .map_err(|error| format!("cannot run yt-dlp: {error}"))?;

    if let Ok(text) = std::fs::read_to_string(out) {
        if text.contains('\t') {
            return Ok(text);
        }
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr
        .lines()
        .find(|line| line.to_ascii_lowercase().contains("error"))
        .unwrap_or("")
        .trim()
        .trim_start_matches("ERROR:")
        .trim();
    if line.to_ascii_lowercase().contains("dpapi") {
        return Err(
            "This browser protects its cookies with app-bound encryption, so nothing can be read out of it. Export cookies.txt with a browser extension and use “Paste” instead."
                .into(),
        );
    }
    if line.is_empty() {
        Err(format!(
            "No cookies could be read from {browser}. Is it installed, and are you signed in?"
        ))
    } else {
        Err(line.to_string())
    }
}

/// Key bindings for the shell.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", SubmitUrl, None),
        KeyBinding::new("secondary-enter", SubmitUrl, None),
        KeyBinding::new("escape", DismissNotice, None),
        KeyBinding::new("secondary-l", FocusUrl, None),
        KeyBinding::new("secondary-o", OpenFolder, None),
        KeyBinding::new("secondary-1", Tab1, None),
        KeyBinding::new("secondary-2", Tab2, None),
        KeyBinding::new("secondary-3", Tab3, None),
        KeyBinding::new("secondary-tab", NextTab, None),
        KeyBinding::new("secondary-shift-tab", PrevTab, None),
    ]);
}
