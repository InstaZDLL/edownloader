//! The sites edownloader reads itself: Pinterest, X, Instagram, TikTok and
//! Bilibili.
//!
//! yt-dlp is the right tool for a video, but it has gaps on these sites:
//! pictures (an X or Instagram photo post, a TikTok slideshow, most of
//! Pinterest), boards of many pins, and Bilibili's pages, which answer it
//! with HTTP 412. Each site module turns a link into a [`Listing`], the files
//! to fetch, and this module fetches them, a few at a time: plain files as
//! they are, HLS streams and separate video and audio streams (DASH) through
//! ffmpeg.
//!
//! When a link holds nothing yt-dlp would miss (a video tweet, a TikTok
//! video, an Instagram reel), the module answers [`Listed::Delegate`] and the
//! engine hands the job to yt-dlp, whose format presets and merges then
//! apply as for any other site.
//!
//! Every file name ends with `[key]`, an identifier the site gives the file,
//! so running the same link again only fetches what is new, and a retry
//! after a partial failure only fetches what is missing.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use ureq::ResponseExt;

use crate::core::cookies;
use crate::core::engine::{Event, MediaOptions};
use crate::core::urlx;
use crate::core::{bilibili, instagram, pinterest, tiktok, twitter};

/// The websites answer browsers; a current desktop Chrome is what they see
/// most.
pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
/// Pages and API answers larger than this are not what the app asked for.
pub(crate) const MAX_TEXT: u64 = 32 * 1024 * 1024;
/// Files fetched at once. The CDNs are fast; the APIs are what rate-limits,
/// and they are only called while listing.
const WORKERS: usize = 3;

/// A site with a runner of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    Pinterest,
    X,
    Instagram,
    TikTok,
    Bilibili,
}

impl Site {
    /// The site whose runner takes `url`, if any: only the link shapes the
    /// module understands, everything else on these sites stays with yt-dlp.
    pub fn of(url: &str) -> Option<Site> {
        if pinterest::handles(url) {
            Some(Site::Pinterest)
        } else if twitter::handles(url) {
            Some(Site::X)
        } else if instagram::handles(url) {
            Some(Site::Instagram)
        } else if tiktok::handles(url) {
            Some(Site::TikTok)
        } else if bilibili::handles(url) {
            Some(Site::Bilibili)
        } else {
            None
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Site::Pinterest => "Pinterest",
            Site::X => "X",
            Site::Instagram => "Instagram",
            Site::TikTok => "TikTok",
            Site::Bilibili => "Bilibili",
        }
    }

    /// The `Referer` the site's file servers expect, when they check it.
    fn referer(self) -> Option<&'static str> {
        match self {
            Site::Pinterest => Some(pinterest::REFERER),
            Site::Bilibili => Some(bilibili::REFERER),
            Site::TikTok => Some(tiktok::REFERER),
            Site::X | Site::Instagram => None,
        }
    }

    fn list(
        self,
        job: &Job<'_>,
        agent: &ureq::Agent,
        on_found: &mut dyn FnMut(usize),
    ) -> Result<Listed, Stop> {
        match self {
            Site::Pinterest => pinterest::list(job, agent, on_found),
            Site::X => twitter::list(job, agent),
            Site::Instagram => instagram::list(job),
            Site::TikTok => tiktok::list(job, agent),
            Site::Bilibili => bilibili::list(job, agent, on_found),
        }
    }
}

/// Where a file comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Fetched as is.
    File(String),
    /// An HLS playlist, saved through ffmpeg.
    Hls(String),
    /// A video stream and an audio stream, fetched then muxed by ffmpeg.
    Dash { video: String, audio: String },
}

impl Source {
    /// The address that identifies the file (the video's, for a pair).
    pub fn url(&self) -> &str {
        match self {
            Source::File(url) | Source::Hls(url) => url,
            Source::Dash { video, .. } => video,
        }
    }
}

/// One file to fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Media {
    pub source: Source,
    /// File name without extension, ending with `[key]`.
    pub stem: String,
    pub key: String,
    pub ext: String,
}

impl Media {
    /// A file named `title [key].ext` (`[key].ext` without a title).
    pub fn new(source: Source, title: &str, key: &str, ext: &str) -> Media {
        let key: String = key
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .take(40)
            .collect();
        let title = clean_title(title, 80);
        let stem = if title.is_empty() {
            format!("[{key}]")
        } else {
            urlx::sanitize_filename(&format!("{title} [{key}]"))
        };
        Media {
            source,
            stem,
            key,
            ext: ext.to_ascii_lowercase(),
        }
    }

    /// `key.ext`: what tells a file already on disk.
    fn tag(&self) -> String {
        format!("{}.{}", self.key, self.ext)
    }
}

/// What a link resolved to: the files, and the folder they go in (none for a
/// post with a single file).
pub struct Listing {
    pub title: String,
    pub folder: Option<String>,
    pub media: Vec<Media>,
}

impl Listing {
    /// One post: a folder named after it when it holds several files.
    pub fn post(title: String, media: Vec<Media>) -> Listing {
        let folder = (media.len() > 1).then(|| clean_title(&title, 80));
        Listing {
            title,
            folder,
            media,
        }
    }
}

pub enum Listed {
    Files(Listing),
    /// Nothing yt-dlp would miss: the job goes to yt-dlp.
    Delegate,
}

/// Why a step ended without a result: a message for the user, or a cancel.
#[derive(Debug)]
pub enum Stop {
    Failed(String),
    Cancelled,
}

impl From<String> for Stop {
    fn from(message: String) -> Self {
        Stop::Failed(message)
    }
}

/// A job, from the link to the files.
pub struct Job<'a> {
    pub id: u64,
    pub url: &'a str,
    pub dest_dir: &'a Path,
    pub cookie_file: Option<&'a Path>,
    pub ffmpeg: Option<&'a Path>,
    pub yt_dlp: Option<&'a Path>,
    pub media: &'a MediaOptions,
    pub cancel: Arc<AtomicBool>,
}

impl Job<'_> {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// How a job ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Done, failed or cancelled: the events said which.
    Finished,
    /// yt-dlp should take the job.
    Delegate,
}

/// Whitespace collapsed, at most `max` characters.
pub(crate) fn clean_title(text: &str, max: usize) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    line.chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// `title 1`, `title 2`… for the files of one post, so they sort in the
/// order the post shows them.
pub(crate) fn numbered(title: &str, index: usize, count: usize) -> String {
    if count <= 1 {
        return title.to_string();
    }
    let width = count.to_string().len();
    format!(
        "{} {:0width$}",
        clean_title(title, 72),
        index + 1,
        width = width
    )
}

pub(crate) fn str_at<'a>(value: &'a Value, path: &str) -> Option<&'a str> {
    value
        .pointer(path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub(crate) fn url_path(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// The path segments of a link, decoded.
pub(crate) fn path_segments(url: &str) -> Vec<String> {
    let after = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path = after
        .split_once('/')
        .map(|(_, path)| path)
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(urlx::percent_decode)
        .collect()
}

/// The value of `name` in a link's query string, decoded.
pub(crate) fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == name).then(|| urlx::percent_decode(value))
    })
}

/// The extension of the file a link points at, lowercase.
pub(crate) fn extension(url: &str) -> Option<String> {
    let name = url_path(url).rsplit('/').next()?;
    let (_, ext) = name.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    (!ext.is_empty() && ext.len() <= 5 && ext.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then_some(ext)
}

/// The start of the CDN file name (`144a6c798cef05888b…jpg` → `144a6c798cef`).
pub(crate) fn cdn_key(url: &str) -> String {
    let name = url_path(url).rsplit('/').next().unwrap_or("");
    let stem = name.split('.').next().unwrap_or(name);
    let key: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(12)
        .collect();
    if key.is_empty() {
        "file".into()
    } else {
        key
    }
}

/// `%`-encoding for a query value: everything but RFC 3986's unreserved set.
pub(crate) fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Sleeps in short steps; false when the job was cancelled meanwhile.
pub(crate) fn pause(cancel: &AtomicBool, wait: Duration) -> bool {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100).min(end - Instant::now()));
    }
    !cancel.load(Ordering::SeqCst)
}

/// 0..span milliseconds, from the clock: spacing, not secrecy.
pub(crate) fn jitter(span: u64) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(nanos % span.max(1))
}

pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .build()
        .into()
}

/// The body of an answer, as text.
pub(crate) fn read_text(response: ureq::http::Response<ureq::Body>) -> Result<String, Stop> {
    response
        .into_body()
        .into_with_config()
        .limit(MAX_TEXT)
        .read_to_string()
        .map_err(|error| Stop::Failed(format!("cannot read the answer: {error}")))
}

/// An answer: its status, the address it came from after redirects, and
/// its body.
pub(crate) struct Page {
    pub status: u16,
    pub url: String,
    pub body: String,
}

/// A GET, retried on rate limits (after the site's `Retry-After`, or 15, 30,
/// 60 s) and on server or network errors (1, 3, 6 s). Any other status is
/// the caller's to read.
pub(crate) fn get(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(&str, &str)],
    cancel: &AtomicBool,
) -> Result<Page, Stop> {
    const LIMITED: [u64; 3] = [15, 30, 60];
    const BROKEN: [u64; 3] = [1, 3, 6];
    let (mut limited, mut broken) = (0usize, 0usize);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(Stop::Cancelled);
        }
        let mut request = agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let wait = match request.call() {
            Ok(response) => {
                let status = response.status().as_u16();
                if status == 429 && limited < LIMITED.len() {
                    let asked = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .filter(|s| (1..=120).contains(s));
                    limited += 1;
                    asked.unwrap_or(LIMITED[limited - 1])
                } else if status >= 500 && broken < BROKEN.len() {
                    broken += 1;
                    BROKEN[broken - 1]
                } else {
                    let landed = response.get_uri().to_string();
                    return Ok(Page {
                        status,
                        url: landed,
                        body: read_text(response)?,
                    });
                }
            }
            Err(_) if broken < BROKEN.len() => {
                broken += 1;
                BROKEN[broken - 1]
            }
            Err(error) => {
                return Err(Stop::Failed(format!(
                    "cannot reach {}: {error}",
                    urlx::host_of(url)
                )));
            }
        };
        if !pause(cancel, Duration::from_secs(wait)) {
            return Err(Stop::Cancelled);
        }
    }
}

/// Where a short link (`pin.it`, `b23.tv`, `vm.tiktok.com`…) leads.
pub(crate) fn follow(agent: &ureq::Agent, url: &str, cancel: &AtomicBool) -> Result<String, Stop> {
    Ok(get(agent, url, &[], cancel)?.url)
}

/// The `Cookie` header for `host`, from the cookie file the job was given,
/// when it holds any.
pub(crate) fn cookie_header(file: Option<&Path>, host: &str) -> Option<String> {
    let content = std::fs::read_to_string(file?).ok()?;
    let jar = cookies::parse_netscape(&content).ok()?;
    let pairs: Vec<String> = jar
        .iter()
        .filter(|c| cookies::domain_matches(&c.domain, host))
        .filter(|c| !c.name.is_empty() && !c.value.contains(['\r', '\n', ';']))
        .map(|c| format!("{}={}", c.name, c.value))
        .collect();
    (!pairs.is_empty()).then(|| pairs.join("; "))
}

/// The value of one cookie in a `Cookie` header.
pub(crate) fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}

/// The messages every site shares.
pub(crate) fn not_found(site: Site) -> Stop {
    Stop::Failed(t!("gallery.not_found", site = site.name()))
}

pub(crate) fn private(site: Site) -> Stop {
    Stop::Failed(t!("gallery.private", site = site.name()))
}

pub(crate) fn rate_limited(site: Site) -> Stop {
    Stop::Failed(t!("gallery.rate_limited", site = site.name()))
}

pub(crate) fn changed(site: Site) -> Stop {
    Stop::Failed(t!("gallery.changed", site = site.name()))
}

pub(crate) fn no_media() -> Stop {
    Stop::Failed(t!("gallery.no_media").into())
}

/// `key.ext` of a finished file whose name ends with `[key].ext`; none for
/// a `.part`.
fn file_tag(name: &str) -> Option<String> {
    if name.ends_with(".part") || name.contains(".part.") {
        return None;
    }
    let (stem, ext) = name.rsplit_once('.')?;
    let (open, close) = (stem.rfind('[')?, stem.rfind(']')?);
    (open < close && close + 1 == stem.len())
        .then(|| format!("{}.{}", &stem[open + 1..close], ext.to_ascii_lowercase()))
}

fn present_tags(dir: &Path) -> HashSet<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| file_tag(&entry.file_name().to_string_lossy()))
        .collect()
}

/// `name.jpg` → `name.jpg.part`; with an extension, `name.part.mp4`, which
/// tells ffmpeg the container.
fn part_path(dest: &Path, ext: Option<&str>) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    match ext {
        Some(ext) => {
            let stem = name.strip_suffix(&format!(".{ext}")).unwrap_or(&name);
            dest.with_file_name(format!("{stem}.part.{ext}"))
        }
        None => dest.with_file_name(format!("{name}.part")),
    }
}

/// Byte counters shared by the workers: what came in, and what the files
/// being fetched announced.
struct Counters {
    bytes: AtomicU64,
    expected: AtomicU64,
}

/// Streams `url` into `path`.
fn stream(
    agent: &ureq::Agent,
    url: &str,
    referer: Option<&str>,
    path: &Path,
    cancel: &AtomicBool,
    counters: &Counters,
) -> Result<u64, Stop> {
    let mut request = agent.get(url);
    if let Some(referer) = referer {
        request = request.header("referer", referer);
    }
    let response = request
        .call()
        .map_err(|error| format!("cannot reach {}: {error}", urlx::host_of(url)))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(Stop::Failed(format!(
            "{} answered HTTP {status}",
            urlx::host_of(url)
        )));
    }
    if let Some(length) = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    {
        counters.expected.fetch_add(length, Ordering::Relaxed);
    }
    let result = (|| -> Result<u64, Stop> {
        let mut reader = response.into_body().into_reader();
        let mut file = std::fs::File::create(path)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        let mut buffer = vec![0u8; 128 * 1024];
        let mut done = 0u64;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(Stop::Cancelled);
            }
            let read = reader
                .read(&mut buffer)
                .map_err(|error| format!("download interrupted: {error}"))?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
            done += read as u64;
            counters.bytes.fetch_add(read as u64, Ordering::Relaxed);
        }
        file.flush()
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        Ok(done)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

/// Streams one file to `dest`, through `dest.part`.
fn fetch_file(
    agent: &ureq::Agent,
    url: &str,
    referer: Option<&str>,
    dest: &Path,
    cancel: &AtomicBool,
    counters: &Counters,
) -> Result<u64, Stop> {
    let part = part_path(dest, None);
    let size = stream(agent, url, referer, &part, cancel, counters)?;
    std::fs::rename(&part, dest).map_err(|error| {
        let _ = std::fs::remove_file(&part);
        format!("cannot write {}: {error}", dest.display())
    })?;
    Ok(size)
}

/// Runs ffmpeg to completion, or kills it on cancel.
fn run_ffmpeg(ffmpeg: &Path, args: &[std::ffi::OsString], cancel: &AtomicBool) -> Result<(), Stop> {
    let mut command = Command::new(ffmpeg);
    command
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start ffmpeg: {error}"))?;
    let stderr = child.stderr.take();
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    });
    let status = loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Stop::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(error) => return Err(Stop::Failed(format!("ffmpeg: {error}"))),
        }
    };
    let errors = errors.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let last = errors.lines().last().unwrap_or("ffmpeg failed").trim();
        Err(Stop::Failed(format!("ffmpeg: {last}")))
    }
}

/// Saves an HLS stream without re-encoding it.
fn fetch_hls(
    ffmpeg: Option<&Path>,
    url: &str,
    referer: Option<&str>,
    dest: &Path,
    cancel: &AtomicBool,
    counters: &Counters,
) -> Result<u64, Stop> {
    let ffmpeg = ffmpeg.ok_or_else(|| t!("gallery.no_ffmpeg").to_string())?;
    let part = part_path(dest, Some("mp4"));
    let mut args: Vec<std::ffi::OsString> = vec!["-user_agent".into(), USER_AGENT.into()];
    if let Some(referer) = referer {
        args.push("-headers".into());
        args.push(format!("Referer: {referer}\r\n").into());
    }
    args.extend(["-i".into(), url.into()]);
    args.extend(["-c", "copy", "-bsf:a", "aac_adtstoasc"].map(Into::into));
    args.push(part.clone().into());
    if let Err(stop) = run_ffmpeg(ffmpeg, &args, cancel) {
        let _ = std::fs::remove_file(&part);
        return Err(stop);
    }
    std::fs::rename(&part, dest)
        .map_err(|error| format!("cannot write {}: {error}", dest.display()))?;
    let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    counters.bytes.fetch_add(size, Ordering::Relaxed);
    Ok(size)
}

/// Fetches a video stream and an audio stream, then muxes them into `dest`
/// without re-encoding.
#[allow(clippy::too_many_arguments)]
fn fetch_dash(
    agent: &ureq::Agent,
    ffmpeg: Option<&Path>,
    video: &str,
    audio: &str,
    referer: Option<&str>,
    dest: &Path,
    cancel: &AtomicBool,
    counters: &Counters,
) -> Result<u64, Stop> {
    let ffmpeg = ffmpeg.ok_or_else(|| t!("gallery.no_ffmpeg").to_string())?;
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let video_part = dest.with_file_name(format!("{name}.video.part"));
    let audio_part = dest.with_file_name(format!("{name}.audio.part"));
    let part = part_path(dest, Some("mp4"));
    let result = (|| -> Result<u64, Stop> {
        stream(agent, video, referer, &video_part, cancel, counters)?;
        stream(agent, audio, referer, &audio_part, cancel, counters)?;
        let mut args: Vec<std::ffi::OsString> = vec!["-i".into(), video_part.clone().into()];
        args.extend(["-i".into(), audio_part.clone().into()]);
        args.extend(
            [
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c",
                "copy",
                "-movflags",
                "+faststart",
            ]
            .map(Into::into),
        );
        args.push(part.clone().into());
        run_ffmpeg(ffmpeg, &args, cancel)?;
        std::fs::rename(&part, dest)
            .map_err(|error| format!("cannot write {}: {error}", dest.display()))?;
        Ok(std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0))
    })();
    let _ = std::fs::remove_file(&video_part);
    let _ = std::fs::remove_file(&audio_part);
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

pub fn run(site: Site, job: &Job<'_>, emit: &dyn Fn(Event)) -> Outcome {
    match run_inner(site, job, emit) {
        Ok(outcome) => outcome,
        Err(Stop::Cancelled) => Outcome::Finished,
        Err(Stop::Failed(message)) => {
            emit(Event::Failed {
                id: job.id,
                message,
            });
            Outcome::Finished
        }
    }
}

fn run_inner(site: Site, job: &Job<'_>, emit: &dyn Fn(Event)) -> Result<Outcome, Stop> {
    let id = job.id;
    emit(Event::Stage {
        id,
        stage: "resolving".into(),
    });
    let agent = agent();
    let mut listing_shown = false;
    let listing = match site.list(job, &agent, &mut |found| {
        if !listing_shown {
            listing_shown = true;
            emit(Event::Stage {
                id,
                stage: "listing".into(),
            });
        }
        emit(Event::Items {
            id,
            done: 0,
            total: found as u64,
        })
    })? {
        Listed::Files(listing) => listing,
        Listed::Delegate => return Ok(Outcome::Delegate),
    };
    emit(Event::Title {
        id,
        title: listing.title.clone(),
    });
    if listing.media.is_empty() {
        return Err(no_media());
    }

    let dir = match &listing.folder {
        Some(folder) => job.dest_dir.join(urlx::sanitize_filename(folder)),
        None => job.dest_dir.to_path_buf(),
    };
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let present = present_tags(&dir);
    let total = listing.media.len();
    let queue: Vec<&Media> = listing
        .media
        .iter()
        .filter(|m| !present.contains(&m.tag()))
        .collect();
    let skipped = total - queue.len();

    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(skipped);
    let counters = Counters {
        bytes: AtomicU64::new(0),
        expected: AtomicU64::new(0),
    };
    let running = AtomicUsize::new(WORKERS.min(queue.len()));
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let last_file: Mutex<Option<PathBuf>> = Mutex::new(None);
    let referer = site.referer();
    emit(Event::Stage {
        id,
        stage: "downloading".into(),
    });
    // A file count only means something for several files; for one, the
    // row shows bytes, and a percentage once the size is known.
    let counted = total > 1;
    emit(Event::Items {
        id,
        done: skipped as u64,
        total: if counted { total as u64 } else { 0 },
    });

    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(queue.len()) {
            scope.spawn(|| {
                loop {
                    if job.cancelled() {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(media) = queue.get(index) else { break };
                    let dest = dir.join(format!("{}.{}", media.stem, media.ext));
                    let result = match &media.source {
                        Source::File(url) => {
                            fetch_file(&agent, url, referer, &dest, &job.cancel, &counters)
                        }
                        Source::Hls(url) => {
                            fetch_hls(job.ffmpeg, url, referer, &dest, &job.cancel, &counters)
                        }
                        Source::Dash { video, audio } => fetch_dash(
                            &agent,
                            job.ffmpeg,
                            video,
                            audio,
                            referer,
                            &dest,
                            &job.cancel,
                            &counters,
                        ),
                    };
                    match result {
                        Ok(_) => {
                            if let Ok(mut last) = last_file.lock() {
                                *last = Some(dest);
                            }
                        }
                        Err(Stop::Cancelled) => break,
                        Err(Stop::Failed(message)) => {
                            if let Ok(mut list) = failures.lock() {
                                list.push(message);
                            }
                        }
                    }
                    done.fetch_add(1, Ordering::SeqCst);
                }
                running.fetch_sub(1, Ordering::SeqCst);
            });
        }
        // Progress, a few times a second, while the workers run.
        let mut last_bytes = 0u64;
        let mut last_tick = Instant::now();
        let mut speed = 0u64;
        while running.load(Ordering::SeqCst) > 0 {
            std::thread::sleep(Duration::from_millis(400));
            let now_bytes = counters.bytes.load(Ordering::Relaxed);
            let elapsed = last_tick.elapsed().as_secs_f64();
            if elapsed >= 1.0 {
                speed = (now_bytes.saturating_sub(last_bytes) as f64 / elapsed) as u64;
                last_bytes = now_bytes;
                last_tick = Instant::now();
            }
            let expected = counters.expected.load(Ordering::Relaxed);
            emit(Event::Progress {
                id,
                downloaded: now_bytes,
                total: (!counted && expected >= now_bytes && expected > 0).then_some(expected),
                speed,
                connections: running.load(Ordering::SeqCst) as u64,
            });
            if counted {
                emit(Event::Items {
                    id,
                    done: done.load(Ordering::SeqCst) as u64,
                    total: total as u64,
                });
            }
        }
    });

    if job.cancelled() {
        return Err(Stop::Cancelled);
    }
    let failures = failures.into_inner().unwrap_or_default();
    if let Some(first) = failures.first() {
        let message = if failures.len() == total {
            first.clone()
        } else {
            tn!(
                "gallery.partial",
                failures.len(),
                total = total,
                error = first
            )
        };
        return Err(Stop::Failed(message));
    }
    let tags: HashSet<String> = listing.media.iter().map(Media::tag).collect();
    let path = match listing.folder {
        Some(_) => dir,
        None => last_file
            .into_inner()
            .ok()
            .flatten()
            .or_else(|| {
                // Already there from an earlier run: point at that file.
                std::fs::read_dir(&dir)
                    .ok()?
                    .flatten()
                    .map(|e| e.path())
                    .find(|p| {
                        p.file_name()
                            .and_then(|n| file_tag(&n.to_string_lossy()))
                            .is_some_and(|tag| tags.contains(&tag))
                    })
            })
            .unwrap_or(dir),
    };
    // What the link amounts to on disk, files from earlier runs included.
    let size = if path.is_file() {
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
    } else {
        std::fs::read_dir(&path)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                file_tag(&entry.file_name().to_string_lossy())
                    .is_some_and(|tag| tags.contains(&tag))
            })
            .filter_map(|entry| entry.metadata().ok())
            .filter(|meta| meta.is_file())
            .map(|meta| meta.len())
            .sum()
    };
    emit(Event::Done { id, path, size });
    Ok(Outcome::Finished)
}

/// Runs a job against the real site, with the tools found on this machine
/// and default options: the events in short, the path `Done` gave, and how
/// the job ended.
#[cfg(test)]
pub(crate) fn fetch_for_real(url: &str, dest: &Path) -> (Vec<String>, Option<PathBuf>, Outcome) {
    fetch_with(url, dest, &MediaOptions::default())
}

#[cfg(test)]
pub(crate) fn fetch_with(
    url: &str,
    dest: &Path,
    options: &MediaOptions,
) -> (Vec<String>, Option<PathBuf>, Outcome) {
    let tools = crate::core::tools::Tools::detect();
    let site = Site::of(url).unwrap_or_else(|| panic!("no runner for {url}"));
    let events = Mutex::new(Vec::new());
    let done = Mutex::new(None);
    let job = Job {
        id: 1,
        url,
        dest_dir: dest,
        cookie_file: None,
        ffmpeg: tools.ffmpeg.as_deref(),
        yt_dlp: tools.yt_dlp.as_deref(),
        media: options,
        cancel: Arc::new(AtomicBool::new(false)),
    };
    let outcome = run(site, &job, &|event| match event {
        Event::Done { path, size, .. } => {
            events.lock().unwrap().push(format!("done {size} B"));
            *done.lock().unwrap() = Some(path);
        }
        Event::Failed { message, .. } => events.lock().unwrap().push(format!("failed: {message}")),
        Event::Title { title, .. } => events.lock().unwrap().push(format!("title {title}")),
        Event::Items { done, total, .. } => {
            events.lock().unwrap().push(format!("items {done}/{total}"))
        }
        _ => {}
    });
    (
        events.into_inner().unwrap(),
        done.into_inner().unwrap(),
        outcome,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_files_are_recognised_by_key_and_extension() {
        let dir = std::env::temp_dir().join(format!("edl-gallery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Blue nails [144a6c798cef].jpg"), b"x").unwrap();
        std::fs::write(dir.join("[aaaaaaaaaaaa].jpg.part"), b"x").unwrap();
        std::fs::write(dir.join("clip [BV1x].mp4.video.part"), b"x").unwrap();
        std::fs::write(dir.join("notes [x] old.txt"), b"x").unwrap();
        let tags = present_tags(&dir);
        assert!(tags.contains("144a6c798cef.jpg"));
        assert_eq!(tags.len(), 1, "{tags:?}");
        std::fs::remove_dir_all(&dir).unwrap();
        // The same post as audio is another file than as video.
        assert_eq!(file_tag("Song [BV1x].m4a").as_deref(), Some("BV1x.m4a"));
        assert_eq!(file_tag("Song [BV1x].MP4").as_deref(), Some("BV1x.mp4"));
    }

    #[test]
    fn part_files_keep_the_container_for_ffmpeg() {
        let dest = Path::new("x").join("clip [abc].mp4");
        assert_eq!(
            part_path(&dest, Some("mp4")),
            Path::new("x").join("clip [abc].part.mp4")
        );
        assert_eq!(
            part_path(&dest, None),
            Path::new("x").join("clip [abc].mp4.part")
        );
    }

    #[test]
    fn names_end_with_the_key_and_stay_in_the_folder() {
        let media = Media::new(
            Source::File("u".into()),
            "  Blue\n nails ",
            "144a6c798cef",
            "JPG",
        );
        assert_eq!(media.stem, "Blue nails [144a6c798cef]");
        assert_eq!(media.ext, "jpg");
        let bare = Media::new(Source::File("u".into()), "", "abc", "mp4");
        assert_eq!(bare.stem, "[abc]");
        let evil = Media::new(
            Source::File("u".into()),
            "../../evil: a/b?",
            "k/../x",
            "jpg",
        );
        assert!(
            !evil.stem.contains('/') && !evil.stem.contains('\\') && !evil.stem.contains(':'),
            "{}",
            evil.stem
        );
        assert!(evil.stem.ends_with("[kx]"), "{}", evil.stem);
    }

    #[test]
    fn only_the_site_cookies_are_sent() {
        let file =
            std::env::temp_dir().join(format!("edl-gallery-cookies-{}.txt", std::process::id()));
        std::fs::write(
            &file,
            "# Netscape HTTP Cookie File\n\
             .pinterest.com\tTRUE\t/\tTRUE\t0\t_pinterest_sess\tabc\n\
             .youtube.com\tTRUE\t/\tTRUE\t0\tSID\tsecret\n",
        )
        .unwrap();
        assert_eq!(
            cookie_header(Some(&file), "www.pinterest.com").as_deref(),
            Some("_pinterest_sess=abc")
        );
        assert!(cookie_header(Some(&file), "x.com").is_none());
        std::fs::remove_file(&file).unwrap();
        assert!(cookie_header(None, "x.com").is_none());
        assert_eq!(cookie_value("a=1; ct0=xyz; b=", "ct0"), Some("xyz"));
        assert_eq!(cookie_value("a=1; b=", "b"), None);
    }

    #[test]
    fn links_split_into_segments_and_parameters() {
        assert_eq!(
            path_segments("https://www.bilibili.com/video/BV1bK411W797/?p=3&x=1#t"),
            vec!["video", "BV1bK411W797"]
        );
        assert_eq!(
            query_param("https://www.bilibili.com/video/BV1/?p=3&x=1", "p").as_deref(),
            Some("3")
        );
        assert!(query_param("https://a/b?pp=3", "p").is_none());
        assert_eq!(extension("https://a/b/c.JPEG?x=1").as_deref(), Some("jpeg"));
        assert_eq!(
            cdn_key("https://i.pinimg.com/originals/14/4a/144a6c798cef05888b.jpg"),
            "144a6c798cef"
        );
    }
}
