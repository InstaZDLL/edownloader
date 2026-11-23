//! TikTok: photo posts (slideshows).
//!
//! yt-dlp saves TikTok's videos, but a photo post is several pictures and a
//! sound, which it does not save: its extractor refuses `/photo/` links. A
//! post is read here from the data TikTok's own page carries
//! (`__UNIVERSAL_DATA_FOR_REHYDRATION__`): a photo post gives its pictures
//! and its sound, a video post goes to yt-dlp.

use serde_json::Value;

use crate::core::engine::FormatPreset;
use crate::core::gallery::{
    self, cdn_key, extension, str_at, Job, Listed, Listing, Media, Site, Source, Stop,
};
use crate::core::urlx;

pub(crate) const REFERER: &str = "https://www.tiktok.com/";
const DATA_START: &str =
    "<script id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\" type=\"application/json\">";

fn is_host(host: &str) -> bool {
    host == "tiktok.com" || host.ends_with(".tiktok.com")
}

/// `vm.tiktok.com/…`, `vt.tiktok.com/…` and `tiktok.com/t/…`: share links
/// that say nothing of the post until followed.
fn is_short(url: &str) -> bool {
    let host = urlx::host_of(url);
    host == "vm.tiktok.com"
        || host == "vt.tiktok.com"
        || (is_host(&host)
            && gallery::path_segments(url)
                .first()
                .is_some_and(|first| first == "t"))
}

/// The post id of `/@user/video/<id>`, `/@user/photo/<id>` and
/// `m.tiktok.com/v/<id>.html` links.
pub fn post_id(url: &str) -> Option<String> {
    if !is_host(&urlx::host_of(url)) {
        return None;
    }
    let segments = gallery::path_segments(url);
    let raw = segments
        .iter()
        .position(|s| s == "video" || s == "photo" || s == "v")
        .and_then(|at| segments.get(at + 1))?;
    let id = raw.strip_suffix(".html").unwrap_or(raw);
    (!id.is_empty() && id.len() <= 25 && id.bytes().all(|b| b.is_ascii_digit()))
        .then(|| id.to_string())
}

/// True when a job for `url` belongs to this runner: a post, or a share
/// link to one. Profiles, sounds and tags stay with yt-dlp.
pub fn handles(url: &str) -> bool {
    post_id(url).is_some() || is_short(url)
}

/// The JSON a TikTok page carries for its scripts.
fn page_data(html: &str) -> Option<Value> {
    let start = html.find(DATA_START)? + DATA_START.len();
    let end = html[start..].find("</script>")? + start;
    serde_json::from_str(&html[start..end]).ok()
}

/// The files of a photo post; `None` for a video.
fn listing_of(item: &Value, id: &str, audio_only: bool) -> Option<Listing> {
    let images = item
        .pointer("/imagePost/images")
        .and_then(Value::as_array)
        .filter(|images| !images.is_empty())?;
    let author = str_at(item, "/author/uniqueId").unwrap_or("tiktok");
    let title = match str_at(item, "/desc") {
        Some(desc) => gallery::clean_title(desc, 80),
        None => format!("@{author} {id}"),
    };
    let mut media = Vec::new();
    if !audio_only {
        let urls: Vec<String> = images
            .iter()
            .filter_map(|image| {
                image
                    .pointer("/imageURL/urlList")
                    .and_then(Value::as_array)?
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|u| u.starts_with("https://"))
                    .map(str::to_string)
            })
            .collect();
        let count = urls.len();
        for (index, url) in urls.into_iter().enumerate() {
            let ext = match extension(&url).as_deref() {
                Some("jpeg") | None => "jpg".to_string(),
                Some(other) => other.to_string(),
            };
            let key = cdn_key(&url);
            media.push(Media::new(
                Source::File(url),
                &gallery::numbered(&title, index, count),
                &key,
                &ext,
            ));
        }
    }
    // The sound the slideshow plays.
    if let Some(sound) = str_at(item, "/music/playUrl").filter(|u| u.starts_with("https://")) {
        let name = str_at(item, "/music/title").unwrap_or("sound");
        let key = str_at(item, "/music/id")
            .map(str::to_string)
            .unwrap_or_else(|| cdn_key(sound));
        media.push(Media::new(
            Source::File(sound.to_string()),
            &format!("{} - {name}", gallery::clean_title(&title, 60)),
            &key,
            "mp3",
        ));
    }
    Some(Listing::post(title, media))
}

pub(crate) fn list(job: &Job<'_>, agent: &ureq::Agent) -> Result<Listed, Stop> {
    let mut url = job.url.to_string();
    if is_short(&url) {
        url = gallery::follow(agent, &url, &job.cancel)?;
    }
    let Some(id) = post_id(&url) else {
        return Ok(Listed::Delegate);
    };
    let cookie = gallery::cookie_header(job.cookie_file, "www.tiktok.com");
    let mut headers = vec![
        (
            "accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        ),
        ("accept-language", "en-US,en;q=0.9"),
    ];
    if let Some(cookie) = &cookie {
        headers.push(("cookie", cookie));
    }
    // The `/video/` address serves the post's data whatever the post is;
    // the `/photo/` one leaves it to the page's scripts.
    let page = gallery::get(
        agent,
        &format!("https://www.tiktok.com/@i/video/{id}"),
        &headers,
        &job.cancel,
    )?;
    match page.status {
        200 => {}
        404 => return Err(gallery::not_found(Site::TikTok)),
        _ => return Ok(Listed::Delegate),
    }
    let Some(data) = page_data(&page.body) else {
        return Ok(Listed::Delegate);
    };
    let Some(detail) = data.pointer("/__DEFAULT_SCOPE__/webapp.video-detail") else {
        return Ok(Listed::Delegate);
    };
    match detail
        .get("statusCode")
        .and_then(Value::as_i64)
        .unwrap_or(0)
    {
        0 => {}
        // Private ("self-see"), or for friends only.
        10204 | 10222 => return Err(gallery::private(Site::TikTok)),
        10216 | 10217 => return Err(gallery::not_found(Site::TikTok)),
        _ => return Ok(Listed::Delegate),
    }
    let Some(item) = detail.pointer("/itemInfo/itemStruct") else {
        return Ok(Listed::Delegate);
    };
    let audio_only = job.media.preset == FormatPreset::Audio;
    match listing_of(item, &id, audio_only) {
        Some(listing) if listing.media.is_empty() => Err(gallery::no_media()),
        Some(listing) => Ok(Listed::Files(listing)),
        None => Ok(Listed::Delegate),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn posts_and_share_links_are_recognised() {
        for (url, id) in [
            (
                "https://www.tiktok.com/@adventurers00/photo/7684712891978665249",
                "7684712891978665249",
            ),
            (
                "https://www.tiktok.com/@scout2015/video/6718335390845095173?lang=en",
                "6718335390845095173",
            ),
            (
                "https://m.tiktok.com/v/6718335390845095173.html",
                "6718335390845095173",
            ),
        ] {
            assert_eq!(post_id(url).as_deref(), Some(id), "{url}");
        }
        for url in [
            "https://vm.tiktok.com/ZMabcdefg/",
            "https://vt.tiktok.com/ZSabcdefg/",
            "https://www.tiktok.com/t/ZTabcdefg/",
        ] {
            assert!(handles(url) && post_id(url).is_none(), "{url}");
        }
        for url in [
            "https://www.tiktok.com/@scout2015",
            "https://www.tiktok.com/music/original-sound-123",
            "https://nottiktok.com/@a/video/1",
            "https://www.tiktok.com/@a/video/abc",
        ] {
            assert!(!handles(url), "{url}");
        }
    }

    #[test]
    fn the_page_data_is_found() {
        let html = format!(
            "<html>{DATA_START}{{\"__DEFAULT_SCOPE__\":{{\"webapp.video-detail\":{{\"statusCode\":0}}}}}}</script></html>"
        );
        let data = page_data(&html).unwrap();
        assert_eq!(
            data.pointer("/__DEFAULT_SCOPE__/webapp.video-detail/statusCode"),
            Some(&json!(0))
        );
        assert!(page_data("<html></html>").is_none());
    }

    #[test]
    fn photo_posts_give_their_pictures_then_their_sound() {
        let item = json!({"desc": "#italy #dolomites", "author": {"uniqueId": "graci3jane"},
            "imagePost": {"images": [
                {"imageURL": {"urlList": ["https://p16-common-sign.tiktokcdn-eu.com/tos/3176ed3c4d164607821cd2f7cff9eed4~tplv-photomode-image.jpeg?x=1"]}},
                {"imageURL": {"urlList": ["https://p16-common-sign.tiktokcdn-eu.com/tos/aaaabbbbccccdddd~tplv-photomode-image.jpeg?x=1"]}}
            ]},
            "music": {"id": "7684712", "title": "original sound", "playUrl": "https://v45.tiktokcdn-eu.com/a/?mime_type=audio_mpeg"}});
        let listing = listing_of(&item, "1", false).unwrap();
        assert_eq!(listing.folder.as_deref(), Some("#italy #dolomites"));
        assert_eq!(listing.media.len(), 3);
        assert_eq!(listing.media[0].stem, "#italy #dolomites 1 [3176ed3c4d16]");
        assert_eq!(listing.media[0].ext, "jpg");
        assert_eq!(
            listing.media[2].stem,
            "#italy #dolomites - original sound [7684712]"
        );
        assert_eq!(listing.media[2].ext, "mp3");

        let sound = listing_of(&item, "1", true).unwrap();
        assert_eq!(sound.media.len(), 1);
        assert_eq!(sound.media[0].ext, "mp3");

        let video = json!({"desc": "x", "video": {"playAddr": "https://v.tiktok.com/x"}});
        assert!(listing_of(&video, "1", false).is_none());
    }
}

/// Against TikTok itself: `cargo test -- --ignored tiktok_for_real --nocapture`.
#[cfg(test)]
mod live {
    use crate::core::gallery::{self, Outcome};

    #[test]
    #[ignore]
    fn tiktok_for_real() {
        let root = std::env::temp_dir().join(format!("edl-tiktok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let (events, path, outcome) = gallery::fetch_for_real(
            "https://www.tiktok.com/@adventurers00/photo/7684712891978665249",
            &root,
        );
        println!("{events:?} -> {path:?}");
        assert_eq!(outcome, Outcome::Finished);
        assert!(events.last().unwrap().starts_with("done"), "{events:?}");
        let dir = path.unwrap();
        let files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        println!("{files:#?}");
        assert_eq!(files.len(), 8, "7 pictures and the sound");

        let (events, _, outcome) = gallery::fetch_for_real(
            "https://www.tiktok.com/@scout2015/video/6718335390845095173",
            &root,
        );
        println!("{events:?}");
        assert_eq!(outcome, Outcome::Delegate);
    }
}
