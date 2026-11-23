//! Pinterest: pins, boards and board sections.
//!
//! yt-dlp only knows Pinterest's videos: an image pin, which is most of them,
//! fails with "No video formats found", and so does a board as soon as it
//! holds one. So Pinterest is listed here, through the JSON API the Pinterest
//! website itself calls (`/resource/<Name>Resource/get/`), anonymously for
//! public pins and boards, with the user's Pinterest cookies when the active
//! cookie jar has some (secret boards). gallery.rs fetches the files.
//!
//! A board lands in a folder named after it; every file name ends with the
//! start of the file's name on Pinterest's CDN, which the same picture keeps
//! whatever board or account shows it.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::core::gallery::{
    self, cdn_key, extension, jitter, pause, str_at, url_path, Job, Listed, Listing, Media, Source,
    Stop,
};
use crate::core::urlx;

const ORIGIN: &str = "https://www.pinterest.com";
pub(crate) const REFERER: &str = "https://www.pinterest.com/";
const PAGE_SIZE: u32 = 25;
/// A board of this many pins is far past anything real; the bound only stops
/// a feed that never ends.
const MAX_MEDIA: usize = 50_000;
/// First path segments that are Pinterest pages, not user names.
const RESERVED: &[&str] = &[
    "_",
    "about",
    "business",
    "categories",
    "create",
    "explore",
    "homefeed",
    "ideas",
    "login",
    "news_hub",
    "pin",
    "pin-builder",
    "resource",
    "search",
    "settings",
    "shopping",
    "signup",
    "today",
    "topics",
    "url_shortener",
    "videos",
];

/// True for Pinterest's hosts: `pinterest.com`, the country sites
/// (`pinterest.fr`, `pinterest.co.uk`, `fr.pinterest.com`…) and `pin.it`.
pub fn is_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host == "pin.it" {
        return true;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let Some(at) = labels.iter().position(|label| *label == "pinterest") else {
        return false;
    };
    let suffix = &labels[at + 1..];
    !suffix.is_empty()
        && suffix.len() <= 2
        && suffix.iter().all(|label| {
            (2..=3).contains(&label.len()) && label.bytes().all(|b| b.is_ascii_lowercase())
        })
}

/// True when a job for `url` belongs to this runner.
pub fn handles(url: &str) -> bool {
    is_host(&urlx::host_of(url))
}

/// What a Pinterest link points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Pin(String),
    Board {
        user: String,
        board: String,
    },
    Section {
        user: String,
        board: String,
        section: String,
    },
}

fn plain(segment: &str) -> bool {
    !segment.is_empty()
        && segment.chars().count() <= 200
        && !segment.starts_with('_')
        && !segment.contains("..")
        && segment
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
}

/// Reads a pinterest.* link (not `pin.it`: that one is resolved first).
pub fn target(url: &str) -> Result<Target, String> {
    let segments = gallery::path_segments(url);
    let unsupported = || -> String { t!("pinterest.unsupported").into() };
    let first = segments.first().ok_or_else(unsupported)?;
    if first == "pin" {
        // `/pin/<id>/`, or `/pin/<words>--<id>/` as some shares write it.
        let raw = segments.get(1).ok_or_else(unsupported)?;
        let id = raw.rsplit("--").next().unwrap_or(raw);
        let valid = !id.is_empty()
            && id.len() <= 80
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        return if valid {
            Ok(Target::Pin(id.to_string()))
        } else {
            Err(unsupported())
        };
    }
    if RESERVED.contains(&first.to_ascii_lowercase().as_str()) || !plain(first) {
        return Err(unsupported());
    }
    match &segments[1..] {
        [board] if plain(board) => Ok(Target::Board {
            user: first.clone(),
            board: board.clone(),
        }),
        [board, section] if plain(board) && plain(section) => Ok(Target::Section {
            user: first.clone(),
            board: board.clone(),
            section: section.clone(),
        }),
        // A profile (`/user/`, `/user/_saved/`), or deeper than a section.
        _ => Err(unsupported()),
    }
}

/// The best video a `video_list` offers: an MP4 (the `V_720P` entry is
/// Pinterest's original upload whatever its name says, otherwise the widest),
/// or else an HLS playlist.
fn pick_video(list: &Value) -> Option<Source> {
    let map = list.as_object()?;
    let url_of = |entry: &Value| entry.get("url").and_then(Value::as_str).map(str::to_string);
    let is_mp4 = |entry: &&Value| {
        url_of(entry).is_some_and(|u| url_path(&u).to_ascii_lowercase().ends_with(".mp4"))
    };
    let width = |entry: &&Value| entry.get("width").and_then(Value::as_u64).unwrap_or(0);
    if let Some(mp4) = map
        .get("V_720P")
        .filter(is_mp4)
        .or_else(|| map.values().filter(is_mp4).max_by_key(width))
    {
        return url_of(mp4).map(Source::File);
    }
    let is_hls = |entry: &&Value| {
        url_of(entry).is_some_and(|u| url_path(&u).to_ascii_lowercase().ends_with(".m3u8"))
    };
    ["V_HLSV4", "V_HLSV3_WEB", "V_HLSV3_MOBILE"]
        .iter()
        .filter_map(|key| map.get(*key))
        .find(|entry| is_hls(entry))
        .or_else(|| map.values().find(|entry| is_hls(entry)))
        .and_then(url_of)
        .map(Source::Hls)
}

fn best_image(images: &Value) -> Option<Source> {
    ["originals", "orig", "1200x", "736x"]
        .iter()
        .find_map(|size| str_at(images, &format!("/{size}/url")))
        .map(|url| Source::File(url.to_string()))
}

/// Every file of one pin, in the order the pin shows them.
pub fn media_of(pin: &Value) -> Vec<Media> {
    let mut sources: Vec<Source> = Vec::new();
    // Story (idea) pins: pages of blocks, each a picture or a video.
    if let Some(pages) = pin
        .pointer("/story_pin_data/pages")
        .and_then(Value::as_array)
    {
        for block in pages
            .iter()
            .filter_map(|page| page.get("blocks").and_then(Value::as_array))
            .flatten()
        {
            if let Some(found) = block.pointer("/video/video_list").and_then(pick_video) {
                sources.push(found);
            } else if let Some(image) = block.pointer("/image/images").and_then(best_image) {
                sources.push(image);
            }
        }
    }
    // Carousels: several pictures in one pin.
    if sources.is_empty() {
        if let Some(slots) = pin
            .pointer("/carousel_data/carousel_slots")
            .and_then(Value::as_array)
        {
            sources.extend(
                slots
                    .iter()
                    .filter_map(|slot| slot.get("images").and_then(best_image)),
            );
        }
    }
    if sources.is_empty() {
        if let Some(found) = pin.pointer("/videos/video_list").and_then(pick_video) {
            sources.push(found);
        } else if let Some(image) = pin.get("images").and_then(best_image) {
            sources.push(image);
        }
    }

    let title = str_at(pin, "/title")
        .or_else(|| str_at(pin, "/grid_title"))
        .unwrap_or_default();
    let mut seen = HashSet::new();
    sources
        .into_iter()
        .filter(|source| {
            let url = source.url();
            url.starts_with("https://") && seen.insert(url.to_string())
        })
        .map(|source| {
            let url = source.url();
            let key = cdn_key(url);
            let ext = match &source {
                Source::File(url) => extension(url).unwrap_or_else(|| "jpg".into()),
                _ => "mp4".to_string(),
            };
            Media::new(source, title, &key, &ext)
        })
        .collect()
}

fn resource_url(name: &str, source: &str, options: &Value, bookmark: Option<&str>) -> String {
    let mut options = options.clone();
    if let Some(mark) = bookmark {
        options["bookmarks"] = json!([mark]);
    }
    let data = json!({ "options": options, "context": {} }).to_string();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "{ORIGIN}/resource/{name}Resource/get/?source_url={}&data={}&_={stamp}",
        gallery::encode(source),
        gallery::encode(&data)
    )
}

/// The data and next-page bookmark of an API answer, or the HTTP status of
/// what went wrong (0 when the answer is not one the app understands).
fn unwrap_response(body: &Value) -> Result<(Value, Option<String>), u16> {
    let response = body
        .get("resource_response")
        .filter(|r| r.is_object())
        .ok_or(0u16)?;
    let error = response.get("error").filter(|e| !e.is_null());
    let failed = response
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status != "success");
    if error.is_some() || failed {
        let status = error
            .and_then(|e| e.get("http_status"))
            .or_else(|| response.get("http_status"))
            .and_then(Value::as_u64);
        return Err(status.and_then(|s| u16::try_from(s).ok()).unwrap_or(0));
    }
    let bookmark = response
        .get("bookmark")
        .and_then(Value::as_str)
        .filter(|mark| !mark.is_empty() && *mark != "-end-")
        .map(str::to_string);
    Ok((
        response.get("data").cloned().unwrap_or(Value::Null),
        bookmark,
    ))
}

fn status_message(status: u16) -> String {
    match status {
        401 | 403 => t!("pinterest.denied").into(),
        404 => t!("pinterest.not_found").into(),
        429 => t!("gallery.rate_limited", site = "Pinterest"),
        0 => t!("gallery.changed", site = "Pinterest"),
        other => format!("Pinterest answered HTTP {other}"),
    }
}

struct Api {
    agent: ureq::Agent,
    cookie: Option<String>,
    cancel: Arc<AtomicBool>,
}

impl Api {
    /// One API call, retried as `gallery::get` does, and once more on a rate
    /// limit or a refusal that comes inside an answer (5 s later).
    fn get(
        &self,
        name: &str,
        source: &str,
        handler: &str,
        options: &Value,
        bookmark: Option<&str>,
    ) -> Result<(Value, Option<String>), Stop> {
        const LIMITED: [u64; 3] = [15, 30, 60];
        let (mut limited, mut refused) = (0usize, false);
        loop {
            let url = resource_url(name, source, options, bookmark);
            let mut headers = vec![
                ("accept", "application/json, text/javascript, */*, q=0.01"),
                ("accept-language", "en-US,en;q=0.9"),
                ("referer", REFERER),
                ("x-requested-with", "XMLHttpRequest"),
                ("x-pinterest-appstate", "active"),
                ("x-pinterest-source-url", source),
                ("x-pinterest-pws-handler", handler),
            ];
            if let Some(cookie) = &self.cookie {
                headers.push(("cookie", cookie));
            }
            let page = gallery::get(&self.agent, &url, &headers, &self.cancel)?;
            let answer = match serde_json::from_str::<Value>(&page.body) {
                Ok(body) => unwrap_response(&body),
                Err(_) if page.status != 200 => Err(page.status),
                Err(_) => Err(0),
            };
            let wait = match answer {
                Ok(found) => return Ok(found),
                // The limit can also come inside a 200 answer.
                Err(429) if limited < LIMITED.len() => {
                    limited += 1;
                    LIMITED[limited - 1]
                }
                // A burst of requests can meet a 403 for a moment: one more
                // try before calling the board private.
                Err(403) if !refused => {
                    refused = true;
                    5
                }
                Err(code) => return Err(Stop::Failed(status_message(code))),
            };
            if !pause(&self.cancel, Duration::from_secs(wait)) {
                return Err(Stop::Cancelled);
            }
        }
    }

    /// Every page of a feed, the media of each pin collected on the way.
    fn feed(
        &self,
        name: &str,
        source: &str,
        handler: &str,
        options: Value,
        found: &mut Vec<Media>,
        on_found: &mut dyn FnMut(usize),
    ) -> Result<(), Stop> {
        let mut seen: HashSet<String> = found.iter().map(|m| m.source.url().to_string()).collect();
        let mut bookmark: Option<String> = None;
        for page in 0.. {
            if page > 0 && !pause(&self.cancel, Duration::from_millis(350) + jitter(400)) {
                return Err(Stop::Cancelled);
            }
            let (data, next) = self.get(name, source, handler, &options, bookmark.as_deref())?;
            let pins = data.as_array().cloned().unwrap_or_default();
            for pin in &pins {
                if pin
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t != "pin")
                {
                    continue;
                }
                for media in media_of(pin) {
                    if seen.insert(media.source.url().to_string()) {
                        found.push(media);
                    }
                }
            }
            on_found(found.len());
            match next {
                Some(mark) if !pins.is_empty() && found.len() < MAX_MEDIA => bookmark = Some(mark),
                _ => break,
            }
        }
        Ok(())
    }
}

fn board_info(api: &Api, user: &str, board: &str) -> Result<(String, String), Stop> {
    let source = format!("/{user}/{board}/");
    let (data, _) = api.get(
        "Board",
        &source,
        "www/[username]/[slug].js",
        &json!({ "username": user, "slug": board, "field_set_key": "detailed" }),
        None,
    )?;
    let id = str_at(&data, "/id").ok_or_else(|| t!("pinterest.not_found").to_string())?;
    let name = str_at(&data, "/name").unwrap_or(board);
    Ok((id.to_string(), name.to_string()))
}

fn list_target(
    api: &Api,
    target: &Target,
    on_found: &mut dyn FnMut(usize),
) -> Result<Listing, Stop> {
    match target {
        Target::Pin(id) => {
            let (data, _) = api.get(
                "Pin",
                &format!("/pin/{id}/"),
                "www/pin/[id].js",
                &json!({ "id": id, "field_set_key": "detailed" }),
                None,
            )?;
            let media = media_of(&data);
            if media.is_empty() {
                return Err(gallery::no_media());
            }
            let title = str_at(&data, "/title")
                .or_else(|| str_at(&data, "/grid_title"))
                .map(str::to_string)
                .unwrap_or_else(|| format!("Pinterest {id}"));
            Ok(Listing::post(title, media))
        }
        Target::Board { user, board } => {
            let (board_id, name) = board_info(api, user, board)?;
            let source = format!("/{user}/{board}/");
            let mut media = Vec::new();
            api.feed(
                "BoardFeed",
                &source,
                "www/[username]/[slug].js",
                json!({
                    "board_id": board_id,
                    "board_url": source,
                    "currentFilter": -1,
                    "field_set_key": "react_grid_pin",
                    "filter_section_pins": false,
                    "sort": "default",
                    "layout": "default",
                    "page_size": PAGE_SIZE,
                    "redux_normalize_feed": true,
                }),
                &mut media,
                on_found,
            )?;
            if media.is_empty() {
                return Err(Stop::Failed(t!("pinterest.empty").into()));
            }
            Ok(Listing {
                title: name.clone(),
                folder: Some(name),
                media,
            })
        }
        Target::Section {
            user,
            board,
            section,
        } => {
            let (board_id, board_name) = board_info(api, user, board)?;
            let board_source = format!("/{user}/{board}/");
            let wanted = section.to_lowercase();
            let mut bookmark: Option<String> = None;
            let mut found: Option<(String, String)> = None;
            for _ in 0..40 {
                let (data, next) = api.get(
                    "BoardSections",
                    &board_source,
                    "www/[username]/[slug].js",
                    &json!({ "board_id": board_id, "redux_normalize_feed": true }),
                    bookmark.as_deref(),
                )?;
                found = data.as_array().into_iter().flatten().find_map(|s| {
                    let slug = str_at(s, "/slug")?;
                    (slug.to_lowercase() == wanted).then(|| {
                        let title = str_at(s, "/title").unwrap_or(slug);
                        (
                            str_at(s, "/id").unwrap_or_default().to_string(),
                            title.to_string(),
                        )
                    })
                });
                match (&found, next) {
                    (Some(_), _) | (None, None) => break,
                    (None, Some(mark)) => bookmark = Some(mark),
                }
            }
            let (section_id, title) = found
                .filter(|(id, _)| !id.is_empty())
                .ok_or_else(|| t!("pinterest.not_found").to_string())?;
            let mut media = Vec::new();
            api.feed(
                "BoardSectionPins",
                &format!("/{user}/{board}/{section}/"),
                "www/[username]/[slug]/[section_slug].js",
                json!({
                    "section_id": section_id,
                    "page_size": PAGE_SIZE,
                    "redux_normalize_feed": true,
                }),
                &mut media,
                on_found,
            )?;
            if media.is_empty() {
                return Err(Stop::Failed(t!("pinterest.empty").into()));
            }
            let folder = format!("{board_name} - {title}");
            Ok(Listing {
                title: folder.clone(),
                folder: Some(folder),
                media,
            })
        }
    }
}

/// `pin.it/…` → the pin's or board's address on pinterest.com (shares add
/// `/sent/?invite_code=…`, which `target` ignores).
fn expand_short_link(agent: &ureq::Agent, url: &str, cancel: &AtomicBool) -> Result<String, Stop> {
    let landed = gallery::follow(agent, url, cancel)?;
    if !handles(&landed) || urlx::host_of(&landed) == "pin.it" {
        return Err(Stop::Failed(t!("pinterest.unsupported").into()));
    }
    // A dead short link lands on the home page (`/?show_error=true`).
    if gallery::path_segments(&landed).is_empty() {
        return Err(Stop::Failed(t!("pinterest.not_found").into()));
    }
    Ok(landed)
}

pub(crate) fn list(
    job: &Job<'_>,
    agent: &ureq::Agent,
    on_found: &mut dyn FnMut(usize),
) -> Result<Listed, Stop> {
    let mut url = job.url.to_string();
    if urlx::host_of(&url) == "pin.it" {
        url = expand_short_link(agent, &url, &job.cancel)?;
    }
    let target = target(&url)?;
    let api = Api {
        agent: agent.clone(),
        cookie: gallery::cookie_header(job.cookie_file, "www.pinterest.com"),
        cancel: job.cancel.clone(),
    };
    if job.cancel.load(Ordering::SeqCst) {
        return Err(Stop::Cancelled);
    }
    list_target(&api, &target, on_found).map(Listed::Files)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinterest_hosts_are_told_from_lookalikes() {
        for host in [
            "pinterest.com",
            "www.pinterest.com",
            "fr.pinterest.com",
            "pinterest.fr",
            "pinterest.co.uk",
            "pinterest.com.au",
            "pin.it",
        ] {
            assert!(is_host(host), "{host}");
        }
        for host in [
            "notpinterest.com",
            "pinterest.com.evil.example",
            "pinterest",
            "pinimg.com",
            "i.pinimg.com",
        ] {
            assert!(!is_host(host), "{host}");
        }
    }

    #[test]
    fn links_name_pins_boards_and_sections() {
        assert_eq!(
            target("https://www.pinterest.com/pin/2885187256207927/").unwrap(),
            Target::Pin("2885187256207927".into())
        );
        assert_eq!(
            target("https://pinterest.com/pin/dive-into-serenity--2885187256207927?utm=x").unwrap(),
            Target::Pin("2885187256207927".into())
        );
        assert_eq!(
            target("https://www.pinterest.com/pin/319333429840453019/sent/?invite_code=5cf5&sfo=1")
                .unwrap(),
            Target::Pin("319333429840453019".into())
        );
        assert_eq!(
            target("https://fr.pinterest.com/christineueb/nails/").unwrap(),
            Target::Board {
                user: "christineueb".into(),
                board: "nails".into()
            }
        );
        assert_eq!(
            target("https://www.pinterest.com/christineueb/fitness/legs/").unwrap(),
            Target::Section {
                user: "christineueb".into(),
                board: "fitness".into(),
                section: "legs".into()
            }
        );
        assert_eq!(
            target("https://www.pinterest.com/user/caf%C3%A9-d%C3%A9co/").unwrap(),
            Target::Board {
                user: "user".into(),
                board: "café-déco".into()
            }
        );
        for url in [
            "https://www.pinterest.com/",
            "https://www.pinterest.com/christineueb/",
            "https://www.pinterest.com/christineueb/_saved/",
            "https://www.pinterest.com/search/pins/?q=nails",
            "https://www.pinterest.com/ideas/nails/123/",
            "https://www.pinterest.com/a/b/c/d/",
        ] {
            assert!(target(url).is_err(), "{url}");
        }
    }

    #[test]
    fn video_pins_take_the_mp4_over_the_stream() {
        let pin = json!({"title": "Dive into serenity", "videos": {"video_list": {
            "V_HLSV4": {"url": "https://v1.pinimg.com/videos/iht/hls/ec/e1/c5/ece1c59e8cc370b6.m3u8", "width": 1080},
            "V_720P": {"url": "https://v1.pinimg.com/videos/iht/720p/ec/e1/c5/ece1c59e8cc370b6.mp4", "width": 1080}
        }}, "images": {"orig": {"url": "https://i.pinimg.com/originals/c0/0a/2e/c00a2e5b706c.jpg"}}});
        let media = media_of(&pin);
        assert_eq!(media.len(), 1);
        assert!(media[0].source.url().ends_with("ece1c59e8cc370b6.mp4"));
        assert!(matches!(media[0].source, Source::File(_)));
        assert_eq!(media[0].ext, "mp4");
        assert_eq!(media[0].stem, "Dive into serenity [ece1c59e8cc3]");
    }

    #[test]
    fn stream_only_videos_go_through_hls() {
        let pin = json!({"videos": {"video_list": {
            "V_HLSV3_MOBILE": {"url": "https://v1.pinimg.com/videos/mc/hls/ab/cd/abcdef0123456789.m3u8"}
        }}});
        let media = media_of(&pin);
        assert_eq!(media.len(), 1);
        assert!(matches!(media[0].source, Source::Hls(_)));
        assert_eq!(media[0].ext, "mp4");
        assert_eq!(media[0].stem, "[abcdef012345]");
    }

    #[test]
    fn image_pins_take_the_original() {
        let pin = json!({"grid_title": "  Blue\n nails  ", "videos": null, "story_pin_data": null,
            "images": {"736x": {"url": "https://i.pinimg.com/736x/14/4a/6c/144a6c798cef05888b.jpg"},
                       "orig": {"url": "https://i.pinimg.com/originals/14/4a/6c/144a6c798cef05888b.PNG"}}});
        let media = media_of(&pin);
        assert_eq!(media.len(), 1);
        assert!(media[0].source.url().contains("/originals/"));
        assert_eq!(media[0].ext, "png");
        assert_eq!(media[0].stem, "Blue nails [144a6c798cef]");
    }

    #[test]
    fn story_pins_give_every_page() {
        let pin = json!({"title": "Leg day", "story_pin_data": {"pages": [
            {"blocks": [{"type": "story_pin_image_block", "image": {"images": {
                "originals": {"url": "https://i.pinimg.com/originals/46/40/98/464098d104fe6ac8.jpg"}}}}]},
            {"blocks": [{"type": "story_pin_text_block"}, {"video": {"video_list": {
                "V_EXP4": {"url": "https://v1.pinimg.com/videos/mc/expMp4/aa/bb/aabbccddeeff0011_t1.mp4", "width": 360},
                "V_EXP7": {"url": "https://v1.pinimg.com/videos/mc/expMp4/aa/bb/ffeeddccbbaa0011_t4.mp4", "width": 720}}}}]}
        ]}, "images": {"orig": {"url": "https://i.pinimg.com/originals/cover.jpg"}}});
        let media = media_of(&pin);
        assert_eq!(media.len(), 2);
        assert!(media[0].source.url().ends_with("464098d104fe6ac8.jpg"));
        assert!(media[1].source.url().ends_with("ffeeddccbbaa0011_t4.mp4"));
    }

    #[test]
    fn carousels_give_every_slot() {
        let pin = json!({"carousel_data": {"carousel_slots": [
            {"images": {"orig": {"url": "https://i.pinimg.com/originals/aa/aaaa.jpg"}}},
            {"images": {"736x": {"url": "https://i.pinimg.com/736x/bb/bbbb.jpg"}}}
        ]}});
        assert_eq!(media_of(&pin).len(), 2);
    }

    #[test]
    fn titles_cannot_escape_the_folder() {
        let pin = json!({"title": "../../evil: a/b?", "images": {"orig": {"url": "https://i.pinimg.com/originals/aa/0123456789abcdef.jpg"}}});
        let stem = &media_of(&pin)[0].stem;
        assert!(
            !stem.contains('/') && !stem.contains('\\') && !stem.contains(':'),
            "{stem}"
        );
        assert!(stem.ends_with("[0123456789ab]"), "{stem}");
    }

    #[test]
    fn answers_unwrap_to_data_and_bookmark() {
        let (data, next) = unwrap_response(&json!({"resource_response": {
            "status": "success", "data": [1, 2], "bookmark": "Y2Jx"}}))
        .unwrap();
        assert_eq!(data, json!([1, 2]));
        assert_eq!(next.as_deref(), Some("Y2Jx"));
        let (_, end) = unwrap_response(&json!({"resource_response": {
            "status": "success", "data": [], "bookmark": "-end-"}}))
        .unwrap();
        assert!(end.is_none());
        let missing = unwrap_response(&json!({"resource_response": {"error": {
            "status": "failure", "http_status": 404, "message": "Board not found."}}}))
        .unwrap_err();
        assert_eq!(missing, 404);
        assert_eq!(status_message(missing), t!("pinterest.not_found"));
        assert!(unwrap_response(&json!({"nothing": true})).is_err());
    }

    #[test]
    fn requests_carry_the_options_and_bookmark() {
        let url = resource_url(
            "BoardFeed",
            "/a/b/",
            &json!({"board_id": "1"}),
            Some("Y2Jx"),
        );
        assert!(url.starts_with(
            "https://www.pinterest.com/resource/BoardFeedResource/get/?source_url=%2Fa%2Fb%2F&data="
        ));
        let data = url
            .split("&data=")
            .nth(1)
            .unwrap()
            .split("&_=")
            .next()
            .unwrap();
        let decoded: Value = serde_json::from_str(&urlx::percent_decode(data)).unwrap();
        assert_eq!(decoded["options"]["board_id"], "1");
        assert_eq!(decoded["options"]["bookmarks"][0], "Y2Jx");
        assert_eq!(decoded["context"], json!({}));
    }
}

/// Against Pinterest itself: `cargo test -- --ignored pinterest_for_real --nocapture`.
#[cfg(test)]
mod live {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fetch(url: &str, dest: &Path) -> (Vec<String>, Option<PathBuf>) {
        let (events, path, _) = gallery::fetch_for_real(url, dest);
        (events, path)
    }

    #[test]
    #[ignore]
    fn pinterest_for_real() {
        let root = std::env::temp_dir().join(format!("edl-pinterest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for url in [
            "https://www.pinterest.com/pin/2885187256207927/",
            "https://fr.pinterest.com/pin/2885187252297068/",
            "https://www.pinterest.com/christineueb/fitness/legs/",
        ] {
            let (events, path) = fetch(url, &root);
            let last = events.last().cloned().unwrap_or_default();
            println!(
                "{url}\n  {}",
                events
                    .iter()
                    .filter(|e| !e.starts_with("items"))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n  ")
            );
            assert!(last.starts_with("done"), "{url}: {last}");
            let path = path.unwrap();
            println!("  -> {}", path.display());
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).unwrap().flatten() {
                    println!(
                        "     {} ({} B)",
                        entry.file_name().to_string_lossy(),
                        entry.metadata().unwrap().len()
                    );
                }
            }
        }
        // A second run finds everything in place and fetches nothing.
        let (events, _) = fetch(
            "https://www.pinterest.com/christineueb/fitness/legs/",
            &root,
        );
        println!("again: {}", events.join(" | "));
        assert!(
            events.last().unwrap().starts_with("done 0 B")
                || events.last().unwrap().starts_with("done")
        );
        println!("files in {}", root.display());
    }
}

#[cfg(test)]
mod live_pages {
    use super::*;

    /// A board of a few hundred pins comes in pages of 25, through bookmarks.
    #[test]
    #[ignore]
    fn pinterest_board_pages_for_real() {
        let api = Api {
            agent: gallery::agent(),
            cookie: None,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let mut pages = 0;
        let target = target("https://www.pinterest.com/christineueb/nails/").unwrap();
        let listing = list_target(&api, &target, &mut |_| pages += 1).unwrap();
        let videos = listing.media.iter().filter(|m| m.ext == "mp4").count();
        println!(
            "{}: {} files in {pages} pages, {videos} videos",
            listing.title,
            listing.media.len()
        );
        assert!(pages > 10, "{pages} pages");
        assert!(listing.media.len() > 300, "{}", listing.media.len());

        let missing =
            super::target("https://www.pinterest.com/christineueb/no-such-board-edl/").unwrap();
        match list_target(&api, &missing, &mut |_| {}) {
            Err(Stop::Failed(message)) => println!("missing board: {message}"),
            other => panic!(
                "missing board: {other:?}",
                other = other.map(|l| l.media.len())
            ),
        }
    }
}

#[cfg(test)]
mod live_short_links {
    use super::*;

    /// `pin.it` links, to a pin and to a board, as shared from the app.
    #[test]
    #[ignore]
    fn pinterest_short_links_for_real() {
        let api = Api {
            agent: gallery::agent(),
            cookie: None,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        // Dead: Pinterest sends it to its home page.
        assert_eq!(
            match expand_short_link(&api.agent, "https://pin.it/2JXU6KR", &api.cancel) {
                Err(Stop::Failed(message)) => message,
                other => panic!("{other:?}"),
            },
            t!("pinterest.not_found").to_string()
        );
        for short in ["https://pin.it/1nNZTyo", "https://pin.it/6JNp3j8zd"] {
            let long = expand_short_link(&api.agent, short, &api.cancel).unwrap();
            let target = super::target(&long).unwrap();
            let listing = match list_target(&api, &target, &mut |_| {}) {
                Ok(listing) => listing,
                Err(stop) => panic!("{short}: {stop:?}"),
            };
            println!(
                "{short} -> {long}\n  {target:?}: {} · {} files, {} videos",
                listing.title,
                listing.media.len(),
                listing.media.iter().filter(|m| m.ext == "mp4").count()
            );
            assert!(!listing.media.is_empty());
        }
    }
}
