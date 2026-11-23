//! Instagram: the photos of a post.
//!
//! yt-dlp saves the videos of a post, and lists its photos without saving
//! them ("There is no video in this post"). Instagram only answers visitors
//! whose connection looks like a browser's, down to the TLS handshake, which
//! yt-dlp imitates and the app's own HTTP client cannot. So yt-dlp reads the
//! post here (`-J`), with the user's cookies when there are some, and the
//! photos it lists are fetched from Instagram's CDN, which asks nothing. A
//! post whose files are all videos, and every reel, goes to yt-dlp to
//! download, with the quality settings.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::core::engine::FormatPreset;
use crate::core::gallery::{self, extension, str_at, Job, Listed, Listing, Media, Source, Stop};
use crate::core::urlx;

fn is_host(host: &str) -> bool {
    host == "instagram.com" || host.ends_with(".instagram.com")
}

/// The shortcode of a post link: `/p/<code>/`, `/<user>/p/<code>/`,
/// `/tv/<code>/`. Reels, stories and profiles stay with yt-dlp.
pub fn shortcode(url: &str) -> Option<String> {
    if !is_host(&urlx::host_of(url)) {
        return None;
    }
    let segments = gallery::path_segments(url);
    let at = match segments.as_slice() {
        [first, ..] if first == "p" || first == "tv" => 1,
        [_, second, ..] if second == "p" => 2,
        _ => return None,
    };
    let code = segments.get(at)?;
    (!code.is_empty()
        && code.len() <= 60
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
    .then(|| code.clone())
}

pub fn handles(url: &str) -> bool {
    shortcode(url).is_some()
}

/// A video's file: the highest of the formats Instagram serves whole (the
/// `dash-` ones are separate video and audio).
fn progressive(formats: &[Value]) -> Option<String> {
    formats
        .iter()
        .filter(|f| {
            !str_at(f, "/format_id").is_some_and(|id| id.starts_with("dash"))
                && str_at(f, "/url").is_some_and(|u| u.starts_with("https://"))
        })
        .max_by_key(|f| f.get("height").and_then(Value::as_u64).unwrap_or(0))
        .and_then(|f| str_at(f, "/url"))
        .map(str::to_string)
}

/// The files of the post yt-dlp read; `None` when yt-dlp should download it
/// itself (videos only, or a video it cannot hand over whole).
fn listing_of(info: &Value) -> Option<Listing> {
    let entries: Vec<&Value> = match info.get("entries").and_then(Value::as_array) {
        Some(entries) if info.get("_type").and_then(Value::as_str) == Some("playlist") => {
            entries.iter().collect()
        }
        _ => vec![info],
    };
    let mut files: Vec<(Source, String, String)> = Vec::new();
    let mut photos = 0;
    for entry in entries {
        let key = str_at(entry, "/id")?.to_string();
        let formats = entry
            .get("formats")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if formats.is_empty() {
            // A photo: yt-dlp lists its sizes smallest first.
            let url = entry
                .get("thumbnails")
                .and_then(Value::as_array)?
                .iter()
                .rev()
                .find_map(|t| str_at(t, "/url").filter(|u| u.starts_with("https://")))?;
            let ext = extension(url).unwrap_or_else(|| "jpg".into());
            photos += 1;
            files.push((Source::File(url.to_string()), key, ext));
        } else {
            files.push((Source::File(progressive(&formats)?), key, "mp4".into()));
        }
    }
    if photos == 0 {
        return None;
    }
    let user = str_at(info, "/channel")
        .or_else(|| str_at(info, "/uploader_id"))
        .unwrap_or("instagram");
    let caption = str_at(info, "/description")
        .and_then(|d| d.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(|line| gallery::clean_title(line, 70));
    let title = match caption {
        Some(caption) => format!("@{user} - {caption}"),
        None => format!("@{user}"),
    };
    let count = files.len();
    let media = files
        .into_iter()
        .enumerate()
        .map(|(index, (source, key, ext))| {
            Media::new(source, &gallery::numbered(&title, index, count), &key, &ext)
        })
        .collect();
    Some(Listing::post(title, media))
}

/// yt-dlp's reading of the post, or `None` when it failed (the job then
/// goes to yt-dlp, which reports why).
fn read_post(
    yt_dlp: &Path,
    url: &str,
    cookie_file: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<Option<Value>, Stop> {
    let mut command = Command::new(yt_dlp);
    command.args([
        "-J",
        "--no-warnings",
        "--ignore-no-formats-error",
        "--yes-playlist",
        "--socket-timeout",
        "25",
    ]);
    if let Some(cookies) = cookie_file {
        command.arg("--cookies").arg(cookies);
    }
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(mut child) = command.spawn() else {
        return Ok(None);
    };
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(stdout) = stdout {
            let _ = stdout.take(gallery::MAX_TEXT).read_to_string(&mut text);
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
            Ok(None) => std::thread::sleep(Duration::from_millis(150)),
            Err(_) => return Ok(None),
        }
    };
    let text = reader.join().unwrap_or_default();
    if !status.success() {
        return Ok(None);
    }
    Ok(serde_json::from_str(text.trim()).ok())
}

pub(crate) fn list(job: &Job<'_>) -> Result<Listed, Stop> {
    let (Some(yt_dlp), false) = (job.yt_dlp, job.media.preset == FormatPreset::Audio) else {
        return Ok(Listed::Delegate);
    };
    Ok(
        match read_post(yt_dlp, job.url, job.cookie_file, &job.cancel)?
            .as_ref()
            .and_then(listing_of)
        {
            Some(listing) => Listed::Files(listing),
            None => Listed::Delegate,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn posts_are_told_from_reels_and_profiles() {
        for (url, code) in [
            ("https://www.instagram.com/p/DeAYvsnn4la/", "DeAYvsnn4la"),
            (
                "https://instagram.com/p/DeAYvsnn4la?img_index=2",
                "DeAYvsnn4la",
            ),
            (
                "https://www.instagram.com/nasa/p/DeAYvsnn4la/",
                "DeAYvsnn4la",
            ),
            ("https://www.instagram.com/tv/CdmYaq3LAYo/", "CdmYaq3LAYo"),
        ] {
            assert_eq!(shortcode(url).as_deref(), Some(code), "{url}");
        }
        for url in [
            "https://www.instagram.com/reel/Chunk8-jurw/",
            "https://www.instagram.com/nasa/",
            "https://www.instagram.com/stories/nasa/123/",
            "https://notinstagram.com/p/abc/",
        ] {
            assert!(!handles(url), "{url}");
        }
    }

    #[test]
    fn carousels_give_photos_and_whole_videos() {
        let post = json!({"_type": "playlist", "id": "DeAYvsnn4la", "channel": "nasa",
        "description": "Swipe for countless stars\n\nSagittarius B2…",
        "entries": [
            {"id": "DeAYnz5PGmu", "formats": [], "thumbnails": [
                {"url": "https://scontent.cdninstagram.com/v/831222439_n.jpg?stp=p640x640"},
                {"url": "https://scontent.cdninstagram.com/v/831222439_n.jpg?stp=e35"}]},
            {"id": "DeAYn0Nv_AI", "formats": [
                {"format_id": "dash-1v", "url": "https://scontent.cdninstagram.com/d.mp4", "height": 1920},
                {"format_id": "101", "url": "https://scontent.cdninstagram.com/low.mp4", "height": 640},
                {"format_id": "102", "url": "https://scontent.cdninstagram.com/high.mp4", "height": 1280}]}
        ]});
        let listing = listing_of(&post).unwrap();
        assert_eq!(listing.title, "@nasa - Swipe for countless stars");
        assert_eq!(listing.media.len(), 2);
        assert_eq!(
            listing.media[0].source.url(),
            "https://scontent.cdninstagram.com/v/831222439_n.jpg?stp=e35"
        );
        assert_eq!(
            listing.media[0].stem,
            "@nasa - Swipe for countless stars 1 [DeAYnz5PGmu]"
        );
        assert_eq!(
            listing.media[1].source.url(),
            "https://scontent.cdninstagram.com/high.mp4"
        );

        let single = json!({"id": "C1", "channel": "a", "formats": [],
            "thumbnails": [{"url": "https://scontent.cdninstagram.com/p.webp"}]});
        let listing = listing_of(&single).unwrap();
        assert!(listing.folder.is_none());
        assert_eq!(listing.media[0].ext, "webp");
        assert_eq!(listing.media[0].stem, "@a [C1]");

        let videos = json!({"_type": "playlist", "entries": [{"id": "V", "formats": [
            {"format_id": "101", "url": "https://scontent.cdninstagram.com/v.mp4"}]}]});
        assert!(listing_of(&videos).is_none());
    }
}

/// Against Instagram itself: `cargo test -- --ignored instagram_for_real --nocapture`.
#[cfg(test)]
mod live {
    use crate::core::gallery::{self, Outcome};

    #[test]
    #[ignore]
    fn instagram_for_real() {
        let root = std::env::temp_dir().join(format!("edl-instagram-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let (events, path, outcome) =
            gallery::fetch_for_real("https://www.instagram.com/p/DeAYvsnn4la/", &root);
        println!("{events:?} -> {path:?}");
        assert_eq!(outcome, Outcome::Finished);
        assert!(events.last().unwrap().starts_with("done"), "{events:?}");
        let files = std::fs::read_dir(path.unwrap()).unwrap().count();
        assert_eq!(files, 3);
        // Three videos: yt-dlp's to download.
        let (_, _, outcome) =
            gallery::fetch_for_real("https://www.instagram.com/p/BQ0eAlwhDrw/", &root);
        assert_eq!(outcome, Outcome::Delegate);
    }
}
