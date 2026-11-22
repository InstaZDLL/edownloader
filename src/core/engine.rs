//! Download orchestration.
//!
//! Two runners, one event stream:
//!
//! * **aria2** (direct files, magnets) — a daemon is started on first use and
//!   each job is polled through `aria2.tellStatus`, which yields exact byte
//!   counts, speed and connections.
//! * **yt-dlp** (media sites) — one child process per job, with a machine
//!   readable `--progress-template` line and a final `EDLPFILE` marker.
//!
//! Everything blocking happens on plain OS threads; the UI thread only reads
//! the event channel. Cancellation is a flag every supervisor honours, plus an
//! RPC call for aria2.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::core::aria2::Aria2;
use crate::core::tools::Tools;
use crate::core::urlx::{self, LinkKind};

/// Emitted by yt-dlp: `status|downloaded|total|estimated|speed|eta|percent`.
const PROGRESS_TEMPLATE: &str = "download:EDLP|%(progress.status)s|%(progress.downloaded_bytes)s|\
%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|\
%(progress.eta)s|%(progress._percent_str)s";
const PROGRESS_PREFIX: &str = "EDLP|";
const FILE_PREFIX: &str = "EDLPFILE|";

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum FormatPreset {
    Best,
    P1080,
    P720,
    Audio,
}

/// Which container the finished file should land in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Container {
    /// Prefer streams the MP4 container can hold (H.264/AV1 + AAC).
    #[default]
    Mp4,
    /// Keep whatever the best codecs produce, which is often WebM.
    Native,
}

impl Container {
    pub const ALL: [Container; 2] = [Container::Mp4, Container::Native];

    pub fn label(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Native => "native",
        }
    }
}

impl FormatPreset {
    pub const ALL: [FormatPreset; 4] = [
        FormatPreset::Best,
        FormatPreset::P1080,
        FormatPreset::P720,
        FormatPreset::Audio,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FormatPreset::Best => "best",
            FormatPreset::P1080 => "1080p",
            FormatPreset::P720 => "720p",
            FormatPreset::Audio => "audio",
        }
    }

    /// Resolution ceiling, if this preset has one.
    pub fn height_cap(self) -> Option<u32> {
        match self {
            FormatPreset::Best | FormatPreset::Audio => None,
            FormatPreset::P1080 => Some(1080),
            FormatPreset::P720 => Some(720),
        }
    }

    /// The `-f` expression.
    ///
    /// `bv*+ba` (best video plus best audio) is what keeps high resolutions
    /// usable: YouTube serves anything above 1080p as video-only DASH, so the
    /// pair must be merged. `b` alone would cap out at a combined format.
    ///
    /// The container is decided by the codecs: VP9/AV1 + Opus merges to WebM,
    /// while H.264/AV1 + AAC merges to MP4. So preferring AAC is what makes
    /// MP4 deterministic — including at 4K, where AV1 is offered in MP4.
    pub fn selector(self, container: Container) -> String {
        let cap = match self.height_cap() {
            Some(height) => format!("[height<={height}]"),
            None => String::new(),
        };
        // Final fallback: whatever quality exists, merged, then combined.
        let tail = if cap.is_empty() {
            "bv*+ba/b".to_string()
        } else {
            format!("bv*{cap}+ba/b{cap}/b")
        };
        match (self, container) {
            (FormatPreset::Audio, Container::Mp4) => "ba[ext=m4a]/ba/b".to_string(),
            (FormatPreset::Audio, Container::Native) => "ba/b".to_string(),
            (_, Container::Native) => tail,
            (_, Container::Mp4) if cap.is_empty() => {
                // Uncapped: take the best MP4 video there is. Above 1080p that
                // means AV1, which is the only MP4-compatible codec YouTube
                // offers at 4K.
                format!("bv*[ext=mp4]{cap}+ba[acodec^=mp4a]/b[ext=mp4]{cap}/bv*{cap}+ba[acodec^=mp4a]/{tail}")
            }
            (_, Container::Mp4) => {
                // Capped: H.264 exists at every resolution up to 1080p and
                // plays on anything, so it wins over the newer codecs.
                format!("bv*[vcodec^=avc1]{cap}+ba[acodec^=mp4a]/bv*[ext=mp4]{cap}+ba[acodec^=mp4a]/b[ext=mp4]{cap}/bv*{cap}+ba[acodec^=mp4a]/{tail}")
            }
        }
    }

    pub fn args(self, container: Container) -> Vec<String> {
        let mut args = vec!["-f".to_string(), self.selector(container)];
        if self == FormatPreset::Audio {
            // `best` keeps the source codec instead of re-encoding, which is
            // the only lossless way to produce an audio-only file.
            args.push("-x".into());
            args.push("--audio-format".into());
            args.push("best".into());
        }
        args
    }
}

#[derive(Clone, Debug)]
pub struct MediaOptions {
    pub preset: FormatPreset,
    pub container: Container,
    pub playlist: bool,
    pub embed_metadata: bool,
}

impl Default for MediaOptions {
    fn default() -> Self {
        MediaOptions {
            preset: FormatPreset::Best,
            container: Container::Mp4,
            playlist: false,
            embed_metadata: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StartRequest {
    pub id: u64,
    pub url: String,
    pub kind: LinkKind,
    pub dest_dir: PathBuf,
    pub out_name: Option<String>,
    pub media: MediaOptions,
    pub cookie_file: Option<PathBuf>,
    pub connections: usize,
}

#[derive(Clone, Debug)]
pub enum Event {
    Progress {
        id: u64,
        downloaded: u64,
        total: Option<u64>,
        speed: u64,
        connections: u64,
    },
    Stage {
        id: u64,
        stage: String,
    },
    Log {
        id: u64,
        line: String,
    },
    Done {
        id: u64,
        path: PathBuf,
        size: u64,
    },
    Failed {
        id: u64,
        message: String,
    },
    /// The aria2 daemon came up (or went away); carries a line for the status bar.
    EngineStatus {
        ok: bool,
        message: String,
    },
}

struct Handle {
    cancel: Arc<AtomicBool>,
    gid: Mutex<Option<String>>,
    paused: AtomicBool,
}

#[derive(Clone)]
struct Options {
    download_dir: PathBuf,
    connections: usize,
    max_concurrent: usize,
}

pub struct Engine {
    tx: Sender<Event>,
    /// Replaced when the tool installer finishes (`set_tools`).
    tools: Mutex<Tools>,
    options: Mutex<Options>,
    daemon: Mutex<Option<Arc<Aria2>>>,
    handles: Mutex<HashMap<u64, Arc<Handle>>>,
}

impl Engine {
    pub fn new(
        tx: Sender<Event>,
        tools: Tools,
        download_dir: PathBuf,
        connections: usize,
    ) -> Arc<Engine> {
        Arc::new(Engine {
            tx,
            tools: Mutex::new(tools),
            options: Mutex::new(Options {
                download_dir,
                connections: connections.clamp(1, 16),
                max_concurrent: 3,
            }),
            daemon: Mutex::new(None),
            handles: Mutex::new(HashMap::new()),
        })
    }

    /// Takes newly installed tools into account for the jobs started from now.
    pub fn set_tools(&self, tools: Tools) {
        if let Ok(mut slot) = self.tools.lock() {
            *slot = tools;
        }
    }

    fn tools(&self) -> Tools {
        self.tools.lock().map(|t| t.clone()).unwrap_or_default()
    }

    pub fn set_options(&self, download_dir: PathBuf, connections: usize, max_concurrent: usize) {
        if let Ok(mut options) = self.options.lock() {
            options.download_dir = download_dir;
            options.connections = connections.clamp(1, 16);
            options.max_concurrent = max_concurrent.max(1);
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn start(self: &Arc<Self>, request: StartRequest) {
        let handle = Arc::new(Handle {
            cancel: Arc::new(AtomicBool::new(false)),
            gid: Mutex::new(None),
            paused: AtomicBool::new(false),
        });
        if let Ok(mut handles) = self.handles.lock() {
            handles.insert(request.id, handle.clone());
        }
        let engine = self.clone();
        std::thread::Builder::new()
            .name(format!("job-{}", request.id))
            .spawn(move || {
                if request.kind == LinkKind::Media {
                    engine.run_ytdlp(request, handle);
                } else {
                    engine.run_aria(request, handle);
                }
            })
            .ok();
    }

    /// Flag the job and, when aria2 owns it, remove it from the daemon.
    pub fn cancel(self: &Arc<Self>, id: u64) {
        let handle = self.handles.lock().ok().and_then(|mut map| map.remove(&id));
        let Some(handle) = handle else { return };
        handle.cancel.store(true, Ordering::SeqCst);
        let gid = handle.gid.lock().ok().and_then(|g| g.clone());
        if let Some(gid) = gid {
            let daemon = self.daemon.lock().ok().and_then(|d| d.clone());
            if let Some(daemon) = daemon {
                std::thread::spawn(move || {
                    let _ = daemon.remove(&gid);
                });
            }
        }
    }

    pub fn pause(&self, id: u64) {
        if let Some(handle) = self.handles.lock().ok().and_then(|m| m.get(&id).cloned()) {
            handle.paused.store(true, Ordering::SeqCst);
            let gid = handle.gid.lock().ok().and_then(|g| g.clone());
            if let (Some(gid), Some(daemon)) =
                (gid, self.daemon.lock().ok().and_then(|d| d.clone()))
            {
                std::thread::spawn(move || {
                    let _ = daemon.pause(&gid);
                });
            }
        }
    }

    pub fn resume(&self, id: u64) {
        if let Some(handle) = self.handles.lock().ok().and_then(|m| m.get(&id).cloned()) {
            handle.paused.store(false, Ordering::SeqCst);
            let gid = handle.gid.lock().ok().and_then(|g| g.clone());
            if let (Some(gid), Some(daemon)) =
                (gid, self.daemon.lock().ok().and_then(|d| d.clone()))
            {
                std::thread::spawn(move || {
                    let _ = daemon.unpause(&gid);
                });
            }
        }
    }

    pub fn forget(&self, id: u64) {
        if let Ok(mut handles) = self.handles.lock() {
            handles.remove(&id);
        }
    }

    /// Kill the daemon; the client child processes end on their own.
    pub fn shutdown(&self) {
        if let Ok(mut daemon) = self.daemon.lock() {
            *daemon = None;
        }
        if let Ok(mut handles) = self.handles.lock() {
            for handle in handles.values() {
                handle.cancel.store(true, Ordering::SeqCst);
            }
            handles.clear();
        }
    }

    fn ensure_daemon(&self) -> Result<Arc<Aria2>, String> {
        let mut slot = self
            .daemon
            .lock()
            .map_err(|_| "the aria2 daemon lock is poisoned".to_string())?;
        if let Some(existing) = slot.as_ref() {
            return Ok(existing.clone());
        }
        let exe = self.tools().aria2()?.to_path_buf();
        let (dir, connections, max_concurrent) = {
            let options = self
                .options
                .lock()
                .map_err(|_| "the options lock is poisoned".to_string())?;
            (
                options.download_dir.clone(),
                options.connections,
                options.max_concurrent,
            )
        };
        if crate::core::paths::ensure_dir(&dir).is_err() {
            // A missing directory is reported by the download itself; aria2
            // can still start with its own default.
        }
        let daemon = Aria2::launch(&exe, &dir, max_concurrent, connections)?;
        let version = daemon.version().unwrap_or_else(|_| "aria2".to_string());
        let daemon = Arc::new(daemon);
        *slot = Some(daemon.clone());
        drop(slot);
        self.emit(Event::EngineStatus {
            ok: true,
            message: format!("aria2 ready ({version})"),
        });
        Ok(daemon)
    }

    fn run_aria(self: Arc<Self>, request: StartRequest, handle: Arc<Handle>) {
        self.emit(Event::Stage {
            id: request.id,
            stage: "starting".into(),
        });
        let daemon = match self.ensure_daemon() {
            Ok(daemon) => daemon,
            Err(error) => {
                // No aria2: a plain http file can still go through yt-dlp's
                // generic extractor, a magnet cannot.
                if request.kind == LinkKind::Direct && self.tools().yt_dlp.is_some() {
                    self.emit(Event::Log {
                        id: request.id,
                        line: format!("{error}; falling back to yt-dlp"),
                    });
                    self.run_ytdlp(request, handle);
                } else {
                    self.emit(Event::Failed {
                        id: request.id,
                        message: error,
                    });
                }
                return;
            }
        };

        let gid = match daemon.add_uri(
            &request.url,
            &request.dest_dir,
            request.out_name.as_deref(),
            request.connections,
        ) {
            Ok(gid) => gid,
            Err(error) => {
                self.emit(Event::Failed {
                    id: request.id,
                    message: error,
                });
                return;
            }
        };
        if let Ok(mut slot) = handle.gid.lock() {
            *slot = Some(gid.clone());
        }
        self.emit(Event::Stage {
            id: request.id,
            stage: "downloading".into(),
        });

        let mut misses = 0u32;
        loop {
            if handle.cancel.load(Ordering::SeqCst) {
                let _ = daemon.remove(&gid);
                return;
            }
            match daemon.tell(&gid) {
                Ok(status) => {
                    misses = 0;
                    if status.is_complete() {
                        let path = status.path.unwrap_or_else(|| request.dest_dir.clone());
                        let size = if status.total > 0 {
                            status.total
                        } else {
                            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
                        };
                        let _ = daemon.remove_download_result(&gid);
                        self.emit(Event::Done {
                            id: request.id,
                            path,
                            size,
                        });
                        return;
                    }
                    if status.is_failed() {
                        let message = status
                            .error
                            .unwrap_or_else(|| "aria2 removed the download".to_string());
                        let _ = daemon.remove_download_result(&gid);
                        self.emit(Event::Failed {
                            id: request.id,
                            message,
                        });
                        return;
                    }
                    let paused = handle.paused.load(Ordering::SeqCst) || status.status == "paused";
                    let speed = if paused { 0 } else { status.speed };
                    let eta = if speed > 0 && status.total > status.completed {
                        Some((status.total - status.completed) / speed)
                    } else {
                        None
                    };
                    self.emit(Event::Progress {
                        id: request.id,
                        downloaded: status.completed,
                        total: (status.total > 0).then_some(status.total),
                        speed,
                        connections: status.connections,
                    });
                    if paused {
                        self.emit(Event::Stage {
                            id: request.id,
                            stage: "paused".into(),
                        });
                    }
                    let _ = eta;
                }
                Err(error) => {
                    misses += 1;
                    if misses >= 5 {
                        self.emit(Event::Failed {
                            id: request.id,
                            message: format!("lost contact with aria2: {error}"),
                        });
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }

    fn build_args(&self, request: &StartRequest) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "--no-warnings".into(),
            "--newline".into(),
            "--no-colors".into(),
            "--progress".into(),
            "--progress-template".into(),
            PROGRESS_TEMPLATE.into(),
            "--print".into(),
            format!("after_move:{FILE_PREFIX}%(filepath)s"),
            "--windows-filenames".into(),
            "--no-mtime".into(),
            "--retries".into(),
            "5".into(),
            "--fragment-retries".into(),
            "5".into(),
            "--socket-timeout".into(),
            "25".into(),
            "-P".into(),
            request.dest_dir.display().to_string(),
            "-o".into(),
            "%(title).150B [%(id)s].%(ext)s".into(),
        ];
        if !request.media.playlist {
            args.push("--no-playlist".into());
        }
        args.extend(request.media.preset.args(request.media.container));
        if request.media.embed_metadata {
            args.push("--embed-metadata".into());
            args.push("--embed-thumbnail".into());
            args.push("--convert-thumbnails".into());
            args.push("jpg".into());
        }
        if let Some(bin_dir) = self.tools().bin_dir.as_ref() {
            args.push("--ffmpeg-location".into());
            args.push(bin_dir.display().to_string());
        }
        // YouTube's signature and `n` challenges are solved in JavaScript.
        // Without an engine yt-dlp silently degrades to storyboard-only
        // responses, which is indistinguishable from a cookie failure.
        if let Some(runtime) = self.tools().js_runtime.as_ref() {
            args.push("--js-runtimes".into());
            args.push(runtime.ytdlp_arg());
        }
        if let Some(cookies) = request.cookie_file.as_ref() {
            args.push("--cookies".into());
            args.push(cookies.display().to_string());
        }
        args.push(request.url.clone());
        args
    }

    fn run_ytdlp(self: Arc<Self>, request: StartRequest, handle: Arc<Handle>) {
        let ytdlp = match self.tools().yt_dlp() {
            Ok(path) => path.to_path_buf(),
            Err(error) => {
                self.emit(Event::Failed {
                    id: request.id,
                    message: error,
                });
                return;
            }
        };
        if let Err(error) = crate::core::paths::ensure_dir(&request.dest_dir) {
            self.emit(Event::Failed {
                id: request.id,
                message: format!("cannot create the download folder: {error}"),
            });
            return;
        }
        let args = self.build_args(&request);
        self.emit(Event::Stage {
            id: request.id,
            stage: "resolving".into(),
        });

        let mut command = Command::new(&ytdlp);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.emit(Event::Failed {
                    id: request.id,
                    message: format!("cannot start yt-dlp: {error}"),
                });
                return;
            }
        };

        let (line_tx, line_rx) = mpsc::channel::<String>();
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let tx = line_tx.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stream);
                for line in reader.split(b'\n') {
                    let Ok(bytes) = line else { break };
                    let text = String::from_utf8_lossy(&bytes).trim_end().to_string();
                    if !text.is_empty() && tx.send(text).is_err() {
                        break;
                    }
                }
            });
        }
        drop(line_tx);

        let started = SystemTime::now();
        let mut final_path: Option<PathBuf> = None;
        let mut errors: Vec<String> = Vec::new();
        let mut stderr_lines: Vec<String> = Vec::new();

        loop {
            // Pipes close when the child exits; that is a hint to stop
            // reading, not the end of the job — the exit status is.
            let mut pipes_closed = false;
            match line_rx.recv_timeout(Duration::from_millis(150)) {
                Ok(line) => {
                    self.consume_line(
                        &request,
                        &line,
                        &mut final_path,
                        &mut errors,
                        &mut stderr_lines,
                    );
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => pipes_closed = true,
            }
            if handle.cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Give the readers a moment to hand over the final lines,
                    // which carry the `EDLPFILE` marker.
                    let deadline = std::time::Instant::now() + Duration::from_millis(300);
                    while std::time::Instant::now() < deadline {
                        match line_rx.recv_timeout(Duration::from_millis(30)) {
                            Ok(line) => self.consume_line(
                                &request,
                                &line,
                                &mut final_path,
                                &mut errors,
                                &mut stderr_lines,
                            ),
                            Err(_) => break,
                        }
                    }
                    if status.success() {
                        let path = final_path
                            .or_else(|| newest_file_after(&request.dest_dir, started))
                            .unwrap_or_else(|| request.dest_dir.clone());
                        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        self.emit(Event::Done {
                            id: request.id,
                            path,
                            size,
                        });
                    } else {
                        let context = FailureContext {
                            used_cookies: request.cookie_file.is_some(),
                            has_js_runtime: self.tools().js_runtime.is_some(),
                        };
                        let message = friendly_error(&stderr_lines, &errors, context)
                            .unwrap_or_else(|| format!("yt-dlp exited with {status}"));
                        self.emit(Event::Failed {
                            id: request.id,
                            message,
                        });
                    }
                    return;
                }
                Ok(None) => {
                    if pipes_closed {
                        // The process is winding down with nothing left to
                        // read; avoid a spin while it exits.
                        std::thread::sleep(Duration::from_millis(40));
                    }
                }
                Err(error) => {
                    self.emit(Event::Failed {
                        id: request.id,
                        message: format!("cannot poll yt-dlp: {error}"),
                    });
                    return;
                }
            }
        }
    }

    fn consume_line(
        &self,
        request: &StartRequest,
        line: &str,
        final_path: &mut Option<PathBuf>,
        errors: &mut Vec<String>,
        stderr_lines: &mut Vec<String>,
    ) {
        if let Some(rest) = line.strip_prefix(FILE_PREFIX) {
            let path = rest.trim();
            if !path.is_empty() {
                *final_path = Some(PathBuf::from(path));
            }
            return;
        }
        if let Some(rest) = line.strip_prefix(PROGRESS_PREFIX) {
            if let Some((downloaded, total, speed)) = parse_progress(rest) {
                self.emit(Event::Progress {
                    id: request.id,
                    downloaded,
                    total,
                    speed,
                    connections: 0,
                });
            }
            return;
        }
        // Everything else is yt-dlp narration. Keep it bounded, and hoist the
        // lines that explain a failure.
        if stderr_lines.len() < 60 {
            stderr_lines.push(line.to_string());
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains("error") {
            errors.push(line.to_string());
            self.emit(Event::Log {
                id: request.id,
                line: line.to_string(),
            });
        } else if lower.contains("[merger]") || lower.contains("[extractaudio]") {
            self.emit(Event::Stage {
                id: request.id,
                stage: "processing".into(),
            });
        } else if lower.contains("destination:") {
            self.emit(Event::Stage {
                id: request.id,
                stage: "downloading".into(),
            });
        }
    }
}

/// `status|downloaded|total|estimated|speed|eta|percent`, `NA` when unknown.
fn parse_progress(rest: &str) -> Option<(u64, Option<u64>, u64)> {
    let fields: Vec<&str> = rest.split('|').collect();
    if fields.len() < 4 {
        return None;
    }
    let number = |value: &str| -> Option<u64> {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("NA") || value == "None" {
            None
        } else {
            value.parse::<u64>().ok().or_else(|| {
                value
                    .parse::<f64>()
                    .ok()
                    .filter(|f| f.is_finite() && *f >= 0.0)
                    .map(|f| f as u64)
            })
        }
    };
    let downloaded = number(fields[1]).unwrap_or(0);
    let total = number(fields[2]).or_else(|| number(fields[3]));
    let speed = number(fields[4]).unwrap_or(0);
    Some((downloaded, total.filter(|t| *t > 0), speed))
}

/// Turn yt-dlp's stderr into one actionable sentence.
/// What the failed job was configured with, for error translation.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FailureContext {
    pub used_cookies: bool,
    pub has_js_runtime: bool,
}

fn friendly_error(stderr: &[String], errors: &[String], context: FailureContext) -> Option<String> {
    let haystack = format!("{}\n{}", stderr.join("\n"), errors.join("\n")).to_ascii_lowercase();
    if haystack.contains("sign in to confirm") || haystack.contains("confirm you") {
        return Some(
            "YouTube wants a signed-in session for this link. Import your cookies (Cookies tab) and retry."
                .into(),
        );
    }
    // Checked before the format error: a missing or failing JavaScript engine
    // is what produces an empty format list on YouTube, and it looks identical
    // to a stale cookie jar.
    if haystack.contains("signature solving failed")
        || haystack.contains("challenge solving failed")
        || haystack.contains("challenge solver script")
    {
        return Some(if context.has_js_runtime {
            "YouTube's JavaScript challenge could not be solved even with the detected runtime — update yt-dlp.".into()
        } else {
            "YouTube needs a JavaScript runtime to unlock its formats. Install Deno or Node.js, then retry.".into()
        });
    }
    // A jar that is present but not really authenticated makes YouTube answer
    // with a stripped player: storyboards only, no streams.
    if haystack.contains("requested format is not available")
        || haystack.contains("requested format not available")
        || haystack.contains("no video formats found")
        || haystack.contains("only images are available")
    {
        return Some(if context.used_cookies {
            "YouTube offered no playable formats: the cookie jar is expired or not signed in. Re-export cookies.txt and retry."
                .into()
        } else {
            "This link has no downloadable formats (live stream, premiere or region lock).".into()
        });
    }
    if haystack.contains("failed to decrypt with dpapi") {
        return Some(
            "The browser refused cookie decryption (Chrome's app-bound encryption). Export cookies.txt from a browser extension and import that instead."
                .into(),
        );
    }
    if haystack.contains("private video") || haystack.contains("is private") {
        return Some(
            "This video is private — the cookie jar must belong to an account that can see it."
                .into(),
        );
    }
    if haystack.contains("video unavailable") || haystack.contains("not available in your country")
    {
        return Some(
            "The site reports this item unavailable, or not available from your region.".into(),
        );
    }
    if haystack.contains("http error 403") {
        return Some(
            "The server refused the request (403). Cookies from that site usually fix it.".into(),
        );
    }
    if haystack.contains("http error 429") || haystack.contains("too many requests") {
        return Some("Rate limited (429). Wait a little, or authenticate with cookies.".into());
    }
    if haystack.contains("members-only") || haystack.contains("join this channel") {
        return Some("Members-only video: the cookie jar must belong to a member account.".into());
    }
    if haystack.contains("this live event will begin") || haystack.contains("premieres in") {
        return Some("This is a scheduled stream that has not started yet.".into());
    }
    if haystack.contains("timed out") || haystack.contains("connection") {
        return Some("The site could not be reached. Check the connection or retry.".into());
    }
    // Prefer the last line that actually starts an error.
    errors
        .iter()
        .rev()
        .find(|line| line.trim_start().to_ascii_lowercase().starts_with("error"))
        .or_else(|| errors.last())
        .map(|line| line.trim().trim_start_matches("ERROR:").trim().to_string())
        .filter(|line| !line.is_empty())
        .or_else(|| stderr.last().map(|l| l.trim().to_string()))
}

fn newest_file_after(dir: &Path, since: SystemTime) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().to_string();
        if name.ends_with(".part") || name.ends_with(".ytdl") || name.ends_with(".aria2") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified + Duration::from_secs(2) < since {
            continue;
        }
        if best.as_ref().is_none_or(|(current, _)| modified > *current) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Name a direct download from its URL when the server offers nothing better.
pub fn direct_out_name(url: &str) -> Option<String> {
    urlx::file_hint(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_build_expected_format_arguments() {
        assert_eq!(FormatPreset::Best.args(Container::Native)[0], "-f");
        assert!(FormatPreset::P1080
            .selector(Container::Native)
            .contains("height<=1080"));
        assert!(FormatPreset::P720
            .selector(Container::Native)
            .contains("height<=720"));
        assert!(FormatPreset::Audio
            .args(Container::Native)
            .contains(&"--audio-format".to_string()));
    }

    #[test]
    fn every_preset_merges_video_with_audio() {
        // Without a `+ba` counterpart YouTube would hand back a silent file
        // above 1080p, where only video-only DASH streams exist.
        for preset in [FormatPreset::Best, FormatPreset::P1080, FormatPreset::P720] {
            for container in Container::ALL {
                let selector = preset.selector(container);
                assert!(
                    selector.contains("+ba") || selector.contains("/b"),
                    "{preset:?}/{container:?}: {selector}"
                );
                assert!(selector.contains("bv*"), "{preset:?}: {selector}");
            }
        }
    }

    #[test]
    fn mp4_mode_asks_for_mp4_video_and_aac_audio() {
        let selector = FormatPreset::Best.selector(Container::Mp4);
        assert!(selector.starts_with("bv*[ext=mp4]"), "{selector}");
        // Capped presets ask for H.264 first; uncapped keeps AV1 for 4K.
        assert!(FormatPreset::P1080
            .selector(Container::Mp4)
            .starts_with("bv*[vcodec^=avc1][height<=1080]"));
        assert!(selector.contains("ba[acodec^=mp4a]"), "{selector}");
        // Still uncapped, so 4K stays reachable through AV1-in-MP4.
        assert!(!selector.contains("height<="), "{selector}");
    }

    #[test]
    fn resolution_caps_survive_the_container_choice() {
        for container in Container::ALL {
            assert!(FormatPreset::P1080
                .selector(container)
                .contains("[height<=1080]"));
            assert!(FormatPreset::P720
                .selector(container)
                .contains("[height<=720]"));
        }
    }

    #[test]
    fn native_mode_leaves_the_codec_choice_alone() {
        assert_eq!(FormatPreset::Best.selector(Container::Native), "bv*+ba/b");
        assert_eq!(
            FormatPreset::P720.selector(Container::Native),
            "bv*[height<=720]+ba/b[height<=720]/b"
        );
    }

    #[test]
    fn audio_mode_prefers_aac_for_mp4() {
        assert!(FormatPreset::Audio
            .selector(Container::Mp4)
            .starts_with("ba[ext=m4a]"));
        assert_eq!(FormatPreset::Audio.selector(Container::Native), "ba/b");
    }

    #[test]
    fn progress_lines_are_parsed() {
        let (down, total, speed) =
            parse_progress("downloading|1024|20971520|NA|1502787.7|14|  0.4%").unwrap();
        assert_eq!(down, 1024);
        assert_eq!(total, Some(20_971_520));
        assert_eq!(speed, 1_502_787);
    }

    #[test]
    fn progress_falls_back_to_the_estimated_total() {
        let (_, total, _) = parse_progress("downloading|10|NA|500|NA|NA|NA").unwrap();
        assert_eq!(total, Some(500));
    }

    #[test]
    fn progress_survives_missing_fields() {
        let (down, total, speed) = parse_progress("downloading|5|NA|NA|NA|NA|NA").unwrap();
        assert_eq!(down, 5);
        assert_eq!(total, None);
        assert_eq!(speed, 0);
        assert!(parse_progress("garbage").is_none());
    }

    #[test]
    fn bot_check_becomes_an_actionable_message() {
        let stderr = vec!["ERROR: [youtube] x: Sign in to confirm you're not a bot.".to_string()];
        let message = friendly_error(&stderr, &stderr, FailureContext::default()).unwrap();
        assert!(message.contains("cookies"));
    }

    #[test]
    fn dpapi_failure_is_explained() {
        let stderr = vec!["ERROR: Failed to decrypt with DPAPI.".to_string()];
        assert!(friendly_error(&stderr, &stderr, FailureContext::default())
            .unwrap()
            .contains("cookies.txt"));
    }

    #[test]
    fn empty_format_list_with_cookies_points_at_the_jar() {
        let stderr =
            vec!["ERROR: [youtube] DujOM1m9JA0: Requested format is not available.".to_string()];
        let message = friendly_error(
            &stderr,
            &stderr,
            FailureContext {
                used_cookies: true,
                has_js_runtime: true,
            },
        )
        .unwrap();
        assert!(message.contains("cookie jar"), "{message}");
        assert!(message.contains("expired"), "{message}");
    }

    #[test]
    fn missing_js_runtime_is_named() {
        let stderr = vec![
            "WARNING: [youtube] x: Signature solving failed: Some formats may be missing."
                .to_string(),
        ];
        let message = friendly_error(&stderr, &stderr, FailureContext::default()).unwrap();
        assert!(message.contains("JavaScript runtime"), "{message}");
        assert!(message.contains("Deno"), "{message}");
    }

    #[test]
    fn failing_js_runtime_points_at_yt_dlp() {
        let stderr = vec!["WARNING: [youtube] x: n challenge solving failed".to_string()];
        let message = friendly_error(
            &stderr,
            &stderr,
            FailureContext {
                used_cookies: false,
                has_js_runtime: true,
            },
        )
        .unwrap();
        assert!(message.contains("update yt-dlp"), "{message}");
    }

    #[test]
    fn empty_format_list_without_cookies_stays_generic() {
        let stderr =
            vec!["ERROR: [youtube] DujOM1m9JA0: Requested format is not available.".to_string()];
        let message = friendly_error(&stderr, &stderr, FailureContext::default()).unwrap();
        assert!(!message.contains("cookie jar"), "{message}");
        assert!(message.contains("no downloadable formats"), "{message}");
    }

    #[test]
    fn generic_errors_are_trimmed_of_the_prefix() {
        let errors = vec!["ERROR: something specific broke".to_string()];
        let message = friendly_error(&errors, &errors, FailureContext::default()).unwrap();
        assert_eq!(message, "something specific broke");
    }

    #[test]
    fn direct_names_come_from_the_url() {
        assert_eq!(
            direct_out_name("https://a.test/files/movie.mkv?sig=1").as_deref(),
            Some("movie.mkv")
        );
        assert_eq!(direct_out_name("https://a.test/"), None);
    }
}
