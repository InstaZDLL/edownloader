//! Bilibili: videos, and every part of a multi-part video.
//!
//! Bilibili's pages answer yt-dlp with HTTP 412 from many connections (its
//! risk control), while its JSON API answers anyone who carries the
//! `buvid3` cookie a first visit sets. So videos are read here: the video's
//! details (`x/web-interface/view`), then the streams of each part
//! (`x/player/playurl`), which come as separate video and audio, muxed by
//! ffmpeg. The quality presets apply: the height cap, MP4-friendly codecs
//! first in MP4 mode, and the audio stream alone for Audio.
//!
//! Without an account Bilibili serves up to 480p; with cookies from a
//! browser signed in to Bilibili, up to what the account may watch.

use serde_json::Value;

use crate::core::engine::{Container, FormatPreset};
use crate::core::gallery::{
    self, extension, jitter, pause, str_at, Job, Listed, Listing, Media, Site, Source, Stop,
};
use crate::core::urlx;

pub(crate) const REFERER: &str = "https://www.bilibili.com/";
const API: &str = "https://api.bilibili.com";

/// A video's id, in either of its spellings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VideoId {
    Bv(String),
    Av(u64),
}

impl VideoId {
    fn query(&self) -> String {
        match self {
            VideoId::Bv(bv) => format!("bvid={bv}"),
            VideoId::Av(av) => format!("aid={av}"),
        }
    }
}

/// The video of a `bilibili.com/video/<BV…|av…>` link.
pub fn video_id(url: &str) -> Option<VideoId> {
    let host = urlx::host_of(url);
    if host != "bilibili.com" && host != "m.bilibili.com" {
        return None;
    }
    let segments = gallery::path_segments(url);
    let [first, id, ..] = segments.as_slice() else {
        return None;
    };
    if first != "video" {
        return None;
    }
    if id.len() == 12
        && id[..2].eq_ignore_ascii_case("bv")
        && id.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Some(VideoId::Bv(format!("BV{}", &id[2..])));
    }
    id.strip_prefix("av")
        .or_else(|| id.strip_prefix("AV"))
        .and_then(|n| n.parse::<u64>().ok())
        .map(VideoId::Av)
}

/// True when a job for `url` belongs to this runner: a video, or a `b23.tv`
/// share link. Series, live rooms and the international site stay with
/// yt-dlp.
pub fn handles(url: &str) -> bool {
    video_id(url).is_some() || urlx::host_of(url) == "b23.tv"
}

/// The data of an API answer, or what to tell the user.
fn unwrap(body: &str) -> Result<Value, Stop> {
    let answer: Value = serde_json::from_str(body).map_err(|_| gallery::changed(Site::Bilibili))?;
    match answer.get("code").and_then(Value::as_i64) {
        Some(0) => Ok(answer.get("data").cloned().unwrap_or(Value::Null)),
        Some(-404 | 62002 | 62004 | 62012) => Err(gallery::not_found(Site::Bilibili)),
        Some(-403 | -10403 | 87007 | 87008) => Err(gallery::private(Site::Bilibili)),
        Some(-412 | -509 | -799 | -352) => Err(gallery::rate_limited(Site::Bilibili)),
        Some(code) => Err(Stop::Failed(format!(
            "Bilibili: {} ({code})",
            str_at(&answer, "/message").unwrap_or("error")
        ))),
        None => Err(gallery::changed(Site::Bilibili)),
    }
}

struct Api<'a> {
    agent: &'a ureq::Agent,
    cookie: String,
    job: &'a Job<'a>,
}

impl Api<'_> {
    fn get(&self, path: &str) -> Result<Value, Stop> {
        let page = gallery::get(
            self.agent,
            &format!("{API}{path}"),
            &[
                ("accept", "application/json"),
                ("referer", REFERER),
                ("origin", "https://www.bilibili.com"),
                ("cookie", &self.cookie),
            ],
            &self.job.cancel,
        )?;
        match page.status {
            412 => Err(gallery::rate_limited(Site::Bilibili)),
            404 => Err(gallery::not_found(Site::Bilibili)),
            _ => unwrap(&page.body),
        }
    }
}

/// The user's Bilibili cookies, and a `buvid3` when they lack one: the API
/// refuses requests that carry none.
fn session_cookie(job: &Job<'_>, agent: &ureq::Agent) -> String {
    let mut cookie =
        gallery::cookie_header(job.cookie_file, "www.bilibili.com").unwrap_or_default();
    if gallery::cookie_value(&cookie, "buvid3").is_none() {
        let fresh = gallery::get(
            agent,
            &format!("{API}/x/frontend/finger/spi"),
            &[("referer", REFERER)],
            &job.cancel,
        )
        .ok()
        .and_then(|page| unwrap(&page.body).ok());
        for (name, key) in [("buvid3", "/b_3"), ("buvid4", "/b_4")] {
            if let Some(value) = fresh.as_ref().and_then(|data| str_at(data, key)) {
                if !cookie.is_empty() {
                    cookie.push_str("; ");
                }
                cookie.push_str(&format!("{name}={value}"));
            }
        }
    }
    cookie
}

fn stream_url(stream: &Value) -> Option<String> {
    str_at(stream, "/baseUrl")
        .or_else(|| str_at(stream, "/base_url"))
        .filter(|u| u.starts_with("http"))
        .map(|u| u.replacen("http://", "https://", 1))
}

/// Codec order: H.264 plays everywhere, which is what the MP4 mode is for;
/// otherwise the newer codecs first, which look better at the same size.
fn codec_rank(stream: &Value, container: Container) -> u8 {
    let codec = stream.get("codecid").and_then(Value::as_u64).unwrap_or(0);
    let order: [u64; 3] = match container {
        Container::Mp4 => [7, 13, 12],
        Container::Native => [13, 12, 7],
    };
    order.iter().position(|c| *c == codec).unwrap_or(3) as u8
}

/// The video stream the preset asks for: the tallest within the cap (the
/// smallest there is when none fits), in the preferred codec.
fn pick_video(videos: &[Value], cap: Option<u32>, container: Container) -> Option<&Value> {
    let height = |v: &Value| v.get("height").and_then(Value::as_u64).unwrap_or(0);
    let fits = |v: &&Value| cap.is_none_or(|cap| height(v) <= u64::from(cap));
    let target = videos
        .iter()
        .filter(fits)
        .map(height)
        .max()
        .or_else(|| videos.iter().map(height).min())?;
    videos
        .iter()
        .filter(|v| height(v) == target && stream_url(v).is_some())
        .min_by_key(|v| {
            (
                codec_rank(v, container),
                u64::MAX - v.get("bandwidth").and_then(Value::as_u64).unwrap_or(0),
            )
        })
}

fn pick_audio(audios: &[Value]) -> Option<&Value> {
    audios
        .iter()
        .filter(|a| stream_url(a).is_some())
        .max_by_key(|a| a.get("bandwidth").and_then(Value::as_u64).unwrap_or(0))
}

/// The file one part becomes, from its `playurl` answer.
fn source_of(
    play: &Value,
    options: &crate::core::engine::MediaOptions,
) -> Option<(Source, String)> {
    let empty = Vec::new();
    let videos = play
        .pointer("/dash/video")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let audios = play
        .pointer("/dash/audio")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let audio = pick_audio(audios).and_then(stream_url);
    if options.preset == FormatPreset::Audio {
        return audio.map(|url| (Source::File(url), "m4a".into()));
    }
    if let Some(video) =
        pick_video(videos, options.preset.height_cap(), options.container).and_then(stream_url)
    {
        return Some(match audio {
            Some(audio) => (Source::Dash { video, audio }, "mp4".into()),
            // A silent video.
            None => (Source::File(video), "mp4".into()),
        });
    }
    // Old videos may only come whole, in a single segment.
    match play
        .get("durl")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    {
        Some([only]) => {
            let url = str_at(only, "/url")?.replacen("http://", "https://", 1);
            let ext = extension(&url).unwrap_or_else(|| "flv".into());
            Some((Source::File(url), ext))
        }
        _ => None,
    }
}

/// One part of a video: its number (from 1), `cid` and name.
struct Part {
    number: u64,
    cid: u64,
    name: String,
}

fn parts_of(view: &Value) -> Vec<Part> {
    view.get("pages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|page| {
            Some(Part {
                number: page.get("page").and_then(Value::as_u64)?,
                cid: page.get("cid").and_then(Value::as_u64)?,
                name: str_at(page, "/part").unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// The parts a link asks for: the one its `?p=` names, all of them when the
/// playlist option is on, else the first.
fn chosen(parts: Vec<Part>, p: Option<u64>, playlist: bool) -> Result<Vec<Part>, Stop> {
    match p {
        Some(n) => {
            let part = parts
                .into_iter()
                .find(|part| part.number == n)
                .ok_or_else(|| gallery::not_found(Site::Bilibili))?;
            Ok(vec![part])
        }
        None if playlist => Ok(parts),
        None => Ok(parts.into_iter().take(1).collect()),
    }
}

pub(crate) fn list(
    job: &Job<'_>,
    agent: &ureq::Agent,
    on_found: &mut dyn FnMut(usize),
) -> Result<Listed, Stop> {
    let mut url = job.url.to_string();
    if urlx::host_of(&url) == "b23.tv" {
        url = gallery::follow(agent, &url, &job.cancel)?;
    }
    let Some(id) = video_id(&url) else {
        return Ok(Listed::Delegate);
    };
    let api = Api {
        agent,
        cookie: session_cookie(job, agent),
        job,
    };
    let view = api.get(&format!("/x/web-interface/view?{}", id.query()))?;
    let bvid = str_at(&view, "/bvid")
        .map(str::to_string)
        .ok_or_else(|| gallery::changed(Site::Bilibili))?;
    let title = str_at(&view, "/title").unwrap_or(&bvid).to_string();
    let parts = parts_of(&view);
    let several = parts.len() > 1;
    let p = gallery::query_param(&url, "p").and_then(|p| p.parse::<u64>().ok());
    let parts = chosen(parts, p, job.media.playlist)?;
    if parts.is_empty() {
        return Err(gallery::no_media());
    }

    let mut media = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0
            && !pause(
                &job.cancel,
                std::time::Duration::from_millis(300) + jitter(300),
            )
        {
            return Err(Stop::Cancelled);
        }
        let play = api.get(&format!(
            "/x/player/playurl?bvid={bvid}&cid={}&qn=127&fnval=4048&fnver=0&fourk=1",
            part.cid
        ))?;
        let (source, ext) =
            source_of(&play, job.media).ok_or_else(|| gallery::changed(Site::Bilibili))?;
        let (name, key) = if !several {
            (title.clone(), bvid.clone())
        } else if parts.len() > 1 {
            (
                format!("P{:02} {}", part.number, part.name),
                format!("{bvid}_p{}", part.number),
            )
        } else {
            (
                format!("{title} - P{} {}", part.number, part.name),
                format!("{bvid}_p{}", part.number),
            )
        };
        media.push(Media::new(source, &name, &key, &ext));
        if parts.len() > 1 {
            on_found(media.len());
        } else {
            // One file: the row shows its name.
            return Ok(Listed::Files(Listing {
                title: name,
                folder: None,
                media,
            }));
        }
    }
    Ok(Listed::Files(Listing {
        folder: Some(gallery::clean_title(&title, 80)),
        title,
        media,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::MediaOptions;
    use serde_json::json;

    #[test]
    fn video_links_are_recognised() {
        assert_eq!(
            video_id("https://www.bilibili.com/video/BV1bK411W797/?p=3"),
            Some(VideoId::Bv("BV1bK411W797".into()))
        );
        assert_eq!(
            video_id("https://m.bilibili.com/video/bv1bK411W797"),
            Some(VideoId::Bv("BV1bK411W797".into()))
        );
        assert_eq!(
            video_id("https://www.bilibili.com/video/av498159642"),
            Some(VideoId::Av(498159642))
        );
        assert!(handles("https://b23.tv/abc123"));
        for url in [
            "https://www.bilibili.com/bangumi/play/ep123",
            "https://space.bilibili.com/123",
            "https://www.bilibili.tv/en/video/123",
            "https://live.bilibili.com/123",
            "https://www.bilibili.com/video/BV1/",
        ] {
            assert!(!handles(url), "{url}");
        }
    }

    fn streams() -> Value {
        json!({"dash": {
        "video": [
            {"id": 64, "codecid": 12, "height": 720, "bandwidth": 900, "baseUrl": "https://up/720-hevc.m4s"},
            {"id": 64, "codecid": 7, "height": 720, "bandwidth": 1000, "baseUrl": "https://up/720-avc.m4s"},
            {"id": 80, "codecid": 13, "height": 1080, "bandwidth": 1500, "base_url": "http://up/1080-av1.m4s"},
            {"id": 80, "codecid": 7, "height": 1080, "bandwidth": 2000, "baseUrl": "https://up/1080-avc.m4s"}
        ],
        "audio": [
            {"id": 30216, "bandwidth": 67000, "baseUrl": "https://up/a-low.m4s"},
            {"id": 30280, "bandwidth": 179000, "baseUrl": "https://up/a-high.m4s"}
        ]}})
    }

    #[test]
    fn presets_pick_the_streams() {
        let play = streams();
        let best = MediaOptions::default();
        assert_eq!(
            source_of(&play, &best),
            Some((
                Source::Dash {
                    video: "https://up/1080-avc.m4s".into(),
                    audio: "https://up/a-high.m4s".into()
                },
                "mp4".into()
            ))
        );
        let native = MediaOptions {
            container: Container::Native,
            ..MediaOptions::default()
        };
        assert!(matches!(source_of(&play, &native),
            Some((Source::Dash { video, .. }, _)) if video == "https://up/1080-av1.m4s"));
        let capped = MediaOptions {
            preset: FormatPreset::P720,
            ..MediaOptions::default()
        };
        assert!(matches!(source_of(&play, &capped),
            Some((Source::Dash { video, .. }, _)) if video == "https://up/720-avc.m4s"));
        let audio = MediaOptions {
            preset: FormatPreset::Audio,
            ..MediaOptions::default()
        };
        assert_eq!(
            source_of(&play, &audio),
            Some((Source::File("https://up/a-high.m4s".into()), "m4a".into()))
        );
        let old = json!({"durl": [{"url": "http://up/whole.flv?x=1"}]});
        assert_eq!(
            source_of(&old, &best),
            Some((
                Source::File("https://up/whole.flv?x=1".into()),
                "flv".into()
            ))
        );
    }

    #[test]
    fn parts_follow_the_link_and_the_playlist_option() {
        let view = json!({"pages": [
            {"page": 1, "cid": 11, "part": "One"},
            {"page": 2, "cid": 22, "part": "Two"},
            {"page": 3, "cid": 33, "part": "Three"}]});
        let numbers = |parts: Vec<Part>| parts.iter().map(|p| p.number).collect::<Vec<_>>();
        assert_eq!(numbers(chosen(parts_of(&view), None, false).unwrap()), [1]);
        assert_eq!(
            numbers(chosen(parts_of(&view), None, true).unwrap()),
            [1, 2, 3]
        );
        assert_eq!(
            numbers(chosen(parts_of(&view), Some(2), false).unwrap()),
            [2]
        );
        assert!(chosen(parts_of(&view), Some(9), false).is_err());
    }

    #[test]
    fn api_errors_become_messages() {
        assert!(unwrap(r#"{"code":0,"data":{"a":1}}"#).is_ok());
        for (body, expected) in [
            (
                r#"{"code":-404,"message":"啥都木有"}"#,
                t!("gallery.not_found", site = "Bilibili"),
            ),
            (
                r#"{"code":-412,"message":"请求被拦截"}"#,
                t!("gallery.rate_limited", site = "Bilibili"),
            ),
            (
                r#"{"code":-403,"message":"访问权限不足"}"#,
                t!("gallery.private", site = "Bilibili"),
            ),
        ] {
            match unwrap(body) {
                Err(Stop::Failed(message)) => assert_eq!(message, expected, "{body}"),
                other => panic!("{body}: {other:?}"),
            }
        }
        assert!(unwrap("<html>").is_err());
    }
}

/// Against Bilibili itself: `cargo test -- --ignored bilibili_for_real --nocapture`.
#[cfg(test)]
mod live {
    use crate::core::engine::{FormatPreset, MediaOptions};
    use crate::core::gallery::{self, Outcome};

    #[test]
    #[ignore]
    fn bilibili_for_real() {
        let root = std::env::temp_dir().join(format!("edl-bilibili-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // The second part of a 23-part video, as video then as sound.
        let url = "https://www.bilibili.com/video/BV1bK411W797/?p=2";
        let (events, path, outcome) = gallery::fetch_for_real(url, &root);
        println!("{events:?} -> {path:?}");
        assert_eq!(outcome, Outcome::Finished);
        assert!(events.last().unwrap().starts_with("done"), "{events:?}");
        assert!(path
            .unwrap()
            .to_string_lossy()
            .ends_with("[BV1bK411W797_p2].mp4"));
        let audio = MediaOptions {
            preset: FormatPreset::Audio,
            ..MediaOptions::default()
        };
        let (events, path, _) = gallery::fetch_with(url, &root, &audio);
        println!("{events:?} -> {path:?}");
        assert!(path
            .unwrap()
            .to_string_lossy()
            .ends_with("[BV1bK411W797_p2].m4a"));
        // Gone.
        let (events, _, _) =
            gallery::fetch_for_real("https://www.bilibili.com/video/BV1GJ411x7h7", &root);
        println!("{events:?}");
        assert!(events.last().unwrap().starts_with("failed"), "{events:?}");
    }
}
