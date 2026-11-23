//! X (Twitter): the photos of a post.
//!
//! yt-dlp saves the videos of a post but stops at its photos ("No video
//! could be found in this tweet"). A post is read here through the
//! syndication API, the one X's embedded posts use, which answers without
//! an account: photos come at their original size, and the videos and GIFs
//! of a post that also has photos as their best MP4. A post with videos only
//! goes to yt-dlp, whose quality settings then apply.

use serde_json::Value;

use crate::core::engine::FormatPreset;
use crate::core::gallery::{
    self, cdn_key, extension, str_at, url_path, Job, Listed, Listing, Media, Site, Source, Stop,
};
use crate::core::urlx;

const HOSTS: &[&str] = &["x.com", "twitter.com", "mobile.twitter.com", "mobile.x.com"];

/// The post id of `…/status/<id>` links, from x.com or twitter.com.
pub fn tweet_id(url: &str) -> Option<String> {
    if !HOSTS.contains(&urlx::host_of(url).as_str()) {
        return None;
    }
    let segments = gallery::path_segments(url);
    let at = segments
        .iter()
        .position(|s| s == "status" || s == "statuses")?;
    let id = segments.get(at + 1)?;
    (!id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.clone())
}

/// True when a job for `url` belongs to this runner: a post. Profiles,
/// Spaces and broadcasts stay with yt-dlp.
pub fn handles(url: &str) -> bool {
    tweet_id(url).is_some()
}

/// The token X's embed script sends along: the id over 10¹⁵, times π, in
/// base 36, without its zeros and point. X does not check it today; sending
/// what its own script sends keeps the request ordinary.
fn token(id: &str) -> String {
    let value = id.parse::<f64>().unwrap_or(0.0) / 1e15 * std::f64::consts::PI;
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut whole = value.trunc() as u64;
    let mut text = Vec::new();
    loop {
        text.push(DIGITS[(whole % 36) as usize]);
        whole /= 36;
        if whole == 0 {
            break;
        }
    }
    text.reverse();
    let mut fraction = value.fract();
    for _ in 0..11 {
        if fraction <= 0.0 {
            break;
        }
        fraction *= 36.0;
        let digit = fraction.trunc() as usize;
        text.push(DIGITS[digit.min(35)]);
        fraction -= digit as f64;
    }
    text.into_iter()
        .filter(|b| *b != b'0')
        .map(char::from)
        .collect()
}

/// A photo at its original size: `…/media/<name>.jpg` →
/// `…/media/<name>?format=jpg&name=orig`.
fn original_photo(url: &str) -> Option<(String, String, String)> {
    let path = url_path(url);
    let name = path.rsplit('/').next()?;
    let (base, _) = name.rsplit_once('.').unwrap_or((name, ""));
    let ext = extension(url)
        .or_else(|| gallery::query_param(url, "format"))
        .unwrap_or_else(|| "jpg".into());
    let prefix = &path[..path.len() - name.len()];
    if base.is_empty() || !prefix.starts_with("https://") {
        return None;
    }
    Some((
        format!("{prefix}{base}?format={ext}&name=orig"),
        base.to_string(),
        ext,
    ))
}

/// The MP4 of the highest bitrate.
fn best_mp4(media: &Value) -> Option<String> {
    media
        .pointer("/video_info/variants")
        .and_then(Value::as_array)?
        .iter()
        .filter(|v| v.get("content_type").and_then(Value::as_str) == Some("video/mp4"))
        .max_by_key(|v| v.get("bitrate").and_then(Value::as_u64).unwrap_or(0))
        .and_then(|v| v.get("url").and_then(Value::as_str))
        .filter(|u| u.starts_with("https://"))
        .map(str::to_string)
}

/// The post's text without the `t.co` links X appends.
fn text_of(tweet: &Value) -> String {
    let text = str_at(tweet, "/text").unwrap_or_default();
    text.split_whitespace()
        .filter(|word| !word.starts_with("https://t.co/") && !word.starts_with("http://t.co/"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The files of a syndication answer; `None` when they are all videos.
fn listing_of(tweet: &Value, id: &str) -> Option<Listing> {
    let user = str_at(tweet, "/user/screen_name").unwrap_or("x");
    let text = text_of(tweet);
    let title = if text.is_empty() {
        format!("@{user} {id}")
    } else {
        format!("@{user} - {text}")
    };
    let details = tweet
        .get("mediaDetails")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut sources: Vec<(Source, String, String)> = Vec::new();
    let mut photos = 0;
    for media in &details {
        match media.get("type").and_then(Value::as_str) {
            Some("photo") => {
                if let Some((url, key, ext)) =
                    str_at(media, "/media_url_https").and_then(original_photo)
                {
                    photos += 1;
                    sources.push((Source::File(url), key, ext));
                }
            }
            Some("video" | "animated_gif") => {
                if let Some(url) = best_mp4(media) {
                    let key = str_at(media, "/id_str")
                        .map(str::to_string)
                        .unwrap_or_else(|| cdn_key(&url));
                    sources.push((Source::File(url), key, "mp4".into()));
                }
            }
            _ => {}
        }
    }
    if photos == 0 {
        return None;
    }
    let count = sources.len();
    let media = sources
        .into_iter()
        .enumerate()
        .map(|(index, (source, key, ext))| {
            Media::new(source, &gallery::numbered(&title, index, count), &key, &ext)
        })
        .collect();
    Some(Listing::post(title, media))
}

pub(crate) fn list(job: &Job<'_>, agent: &ureq::Agent) -> Result<Listed, Stop> {
    // Sound alone is yt-dlp's to extract.
    if job.media.preset == FormatPreset::Audio {
        return Ok(Listed::Delegate);
    }
    let Some(id) = tweet_id(job.url) else {
        return Ok(Listed::Delegate);
    };
    let url = format!(
        "https://cdn.syndication.twimg.com/tweet-result?id={id}&token={}&lang=en",
        token(&id)
    );
    let page = gallery::get(agent, &url, &[("accept", "application/json")], &job.cancel)?;
    match page.status {
        200 => {}
        404 => return Err(gallery::not_found(Site::X)),
        other => return Err(Stop::Failed(format!("X answered HTTP {other}"))),
    }
    let tweet: Value = serde_json::from_str(&page.body).map_err(|_| gallery::changed(Site::X))?;
    if let Some("TweetTombstone" | "TweetUnavailable") =
        tweet.get("__typename").and_then(Value::as_str)
    {
        // Protected or age-restricted: with the user's X cookies, yt-dlp
        // can still reach the videos.
        return if gallery::cookie_header(job.cookie_file, "x.com")
            .is_some_and(|c| gallery::cookie_value(&c, "auth_token").is_some())
        {
            Ok(Listed::Delegate)
        } else {
            Err(gallery::private(Site::X))
        };
    }
    if tweet.get("mediaDetails").is_none() && tweet.get("text").is_none() {
        return Err(gallery::changed(Site::X));
    }
    Ok(match listing_of(&tweet, &id) {
        Some(listing) => Listed::Files(listing),
        None if tweet
            .get("mediaDetails")
            .and_then(Value::as_array)
            .is_some_and(|m| !m.is_empty()) =>
        {
            Listed::Delegate
        }
        None => return Err(gallery::no_media()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn posts_are_told_from_other_pages() {
        for (url, id) in [
            (
                "https://x.com/BarackObama/status/266031293945503744",
                "266031293945503744",
            ),
            (
                "https://twitter.com/a/status/266031293945503744?s=20",
                "266031293945503744",
            ),
            ("https://mobile.twitter.com/a/status/1/photo/1", "1"),
            ("https://x.com/i/web/status/42", "42"),
        ] {
            assert_eq!(tweet_id(url).as_deref(), Some(id), "{url}");
        }
        for url in [
            "https://x.com/BarackObama",
            "https://x.com/i/spaces/1eaKbrPAqbwKX",
            "https://x.com/a/status/abc",
            "https://notx.com/a/status/1",
            "https://x.com.evil.example/a/status/1",
        ] {
            assert!(!handles(url), "{url}");
        }
    }

    #[test]
    fn the_token_matches_the_embed_script() {
        // What X's own script computes for these ids, to the precision that
        // survives the floating point.
        assert!(token("1863637593464512935").starts_with("4imsg2ai"));
        assert!(token("643211948184596480").starts_with("1k4pk2jn"));
        assert!(!token("266031293945503744").contains(['0', '.']));
    }

    #[test]
    fn photos_come_at_their_original_size() {
        let (url, key, ext) =
            original_photo("https://pbs.twimg.com/media/A7EiDWcCYAAZT1D.jpg").unwrap();
        assert_eq!(
            url,
            "https://pbs.twimg.com/media/A7EiDWcCYAAZT1D?format=jpg&name=orig"
        );
        assert_eq!((key.as_str(), ext.as_str()), ("A7EiDWcCYAAZT1D", "jpg"));
        let (_, _, png) = original_photo("https://pbs.twimg.com/media/Abc.png").unwrap();
        assert_eq!(png, "png");
    }

    #[test]
    fn photo_posts_list_every_file_and_video_posts_go_to_yt_dlp() {
        let mixed = json!({"__typename": "Tweet", "text": "Look https://t.co/abc",
        "user": {"screen_name": "nasa"},
        "mediaDetails": [
            {"type": "photo", "media_url_https": "https://pbs.twimg.com/media/AAA.jpg"},
            {"type": "video", "id_str": "158", "video_info": {"variants": [
                {"content_type": "application/x-mpegURL", "url": "https://video.twimg.com/a.m3u8"},
                {"content_type": "video/mp4", "bitrate": 832000, "url": "https://video.twimg.com/low.mp4"},
                {"content_type": "video/mp4", "bitrate": 2176000, "url": "https://video.twimg.com/high.mp4"}]}}
        ]});
        let listing = listing_of(&mixed, "1").unwrap();
        assert_eq!(listing.title, "@nasa - Look");
        assert_eq!(listing.folder.as_deref(), Some("@nasa - Look"));
        assert_eq!(listing.media.len(), 2);
        assert_eq!(listing.media[0].stem, "@nasa - Look 1 [AAA]");
        assert_eq!(
            listing.media[1].source.url(),
            "https://video.twimg.com/high.mp4"
        );
        assert_eq!(listing.media[1].stem, "@nasa - Look 2 [158]");

        let single = json!({"text": "", "user": {"screen_name": "a"},
            "mediaDetails": [{"type": "photo", "media_url_https": "https://pbs.twimg.com/media/B.png"}]});
        let listing = listing_of(&single, "9").unwrap();
        assert!(listing.folder.is_none());
        assert_eq!(listing.media[0].stem, "@a 9 [B]");
        assert_eq!(listing.media[0].ext, "png");

        let video = json!({"text": "x", "mediaDetails": [{"type": "video", "video_info": {"variants": [
            {"content_type": "video/mp4", "bitrate": 1, "url": "https://video.twimg.com/v.mp4"}]}}]});
        assert!(listing_of(&video, "1").is_none());
    }
}

/// Against X itself: `cargo test -- --ignored x_for_real --nocapture`.
#[cfg(test)]
mod live {
    use crate::core::gallery::{self, Outcome};

    #[test]
    #[ignore]
    fn x_for_real() {
        let root = std::env::temp_dir().join(format!("edl-x-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // A photo post: listed and fetched here.
        let (events, path, outcome) =
            gallery::fetch_for_real("https://x.com/BarackObama/status/266031293945503744", &root);
        println!("{events:?} -> {path:?}");
        assert_eq!(outcome, Outcome::Finished);
        assert!(events.last().unwrap().starts_with("done"), "{events:?}");
        assert!(path
            .unwrap()
            .to_string_lossy()
            .ends_with("[A7EiDWcCYAAZT1D].jpg"));
        // A video post: yt-dlp's.
        let (events, _, outcome) =
            gallery::fetch_for_real("https://x.com/elonmusk/status/1585341984679469056", &root);
        println!("{events:?}");
        assert_eq!(outcome, Outcome::Delegate);
        // Nothing there.
        let (events, _, _) =
            gallery::fetch_for_real("https://x.com/a/status/1863637593464512935", &root);
        println!("{events:?}");
        assert!(events.last().unwrap().starts_with("failed"), "{events:?}");
    }
}
