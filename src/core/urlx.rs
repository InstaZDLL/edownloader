//! Link normalization and routing.
//!
//! The app only needs to answer one question up front: should this link go to
//! yt-dlp (a site with its own extractor) or to aria2 (a plain file)? Getting
//! that wrong is not fatal — yt-dlp has a generic extractor — but routing
//! media links to yt-dlp is what makes titles, formats and merges work.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkKind {
    /// A site yt-dlp knows how to extract (YouTube, Vimeo, …).
    Media,
    /// A plain file over http(s).
    Direct,
    /// A `magnet:` URI, handed to aria2's BitTorrent engine.
    Torrent,
}

impl LinkKind {
    pub fn badge(self) -> &'static str {
        match self {
            LinkKind::Media => "MEDIA",
            LinkKind::Direct => "HTTP",
            LinkKind::Torrent => "TORRENT",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Link {
    pub url: String,
    pub host: String,
    pub kind: LinkKind,
    /// Human site label when the host is a known media site.
    pub site: Option<&'static str>,
}

/// Hosts with a dedicated yt-dlp extractor. Suffix-matched, so `m.youtube.com`
/// and `www.youtube.com` both land on YouTube.
const MEDIA_SITES: &[(&str, &str)] = &[
    ("youtube.com", "YouTube"),
    ("youtu.be", "YouTube"),
    ("youtube-nocookie.com", "YouTube"),
    ("vimeo.com", "Vimeo"),
    ("soundcloud.com", "SoundCloud"),
    ("twitter.com", "X"),
    ("x.com", "X"),
    ("instagram.com", "Instagram"),
    ("tiktok.com", "TikTok"),
    ("twitch.tv", "Twitch"),
    ("dailymotion.com", "Dailymotion"),
    ("facebook.com", "Facebook"),
    ("fb.watch", "Facebook"),
    ("reddit.com", "Reddit"),
    ("redd.it", "Reddit"),
    ("bilibili.com", "Bilibili"),
    ("b23.tv", "Bilibili"),
    ("rumble.com", "Rumble"),
    ("odysee.com", "Odysee"),
    ("streamable.com", "Streamable"),
    ("vk.com", "VK"),
    ("ok.ru", "Odnoklassniki"),
    ("linkedin.com", "LinkedIn"),
    ("threads.net", "Threads"),
    ("snapchat.com", "Snapchat"),
    ("kick.com", "Kick"),
    ("bandcamp.com", "Bandcamp"),
    ("archive.org", "Archive.org"),
    ("imgur.com", "Imgur"),
    ("giphy.com", "Giphy"),
    ("nicovideo.jp", "niconico"),
    ("ted.com", "TED"),
    ("coursera.org", "Coursera"),
];

fn looks_like_host(text: &str) -> bool {
    let head = text.split(['/', '?', '#']).next().unwrap_or("");
    !head.is_empty()
        && head.contains('.')
        && !head.contains(char::is_whitespace)
        && head
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
}

/// Trim user noise (quotes, angle brackets, stray whitespace) and add the
/// scheme when a bare host was pasted.
pub fn normalize(raw: &str) -> Result<String, String> {
    let trimmed = raw
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '<' | '>' | '`'))
        .trim();
    if trimmed.is_empty() {
        return Err(t!("link.empty").into());
    }
    if trimmed.to_ascii_lowercase().starts_with("magnet:") {
        return Ok(trimmed.to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(trimmed.to_string());
    }
    // A `blob:` address names memory inside one browser tab: nothing outside
    // that tab can read it, but the page it plays on usually can be.
    if lower.starts_with("blob:") {
        return Err(t!("link.blob").into());
    }
    if lower.contains("://") {
        return Err(t!("link.scheme").into());
    }
    if looks_like_host(trimmed) {
        return Ok(format!("https://{trimmed}"));
    }
    Err(t!("link.not_a_link").into())
}

/// Host without userinfo, port or `www.` noise.
pub fn host_of(url: &str) -> String {
    let after = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after.split(['/', '?', '#']).next().unwrap_or("");
    let without_userinfo = authority.rsplit('@').next().unwrap_or(authority);
    let host = without_userinfo
        .split(':')
        .next()
        .unwrap_or(without_userinfo)
        .to_ascii_lowercase();
    host.trim_start_matches("www.").to_string()
}

pub fn site_label(host: &str) -> Option<&'static str> {
    // Every country site and pin.it: pinterest.rs handles them all.
    if crate::core::pinterest::is_host(host) {
        return Some("Pinterest");
    }
    MEDIA_SITES
        .iter()
        .find(|(domain, _)| host == *domain || host.ends_with(&format!(".{domain}")))
        .map(|(_, label)| *label)
}

/// Last path segment when it looks like a filename.
pub fn file_hint(url: &str) -> Option<String> {
    // Only the path counts: the authority can contain dots and slashes.
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path_start = after_scheme.find('/')?;
    let path = after_scheme[path_start..]
        .split(['?', '#'])
        .next()
        .unwrap_or_default();
    let last = path.rsplit('/').find(|s| !s.is_empty())?;
    let decoded = percent_decode(last);
    let stem = decoded.split('.').next().unwrap_or("");
    if stem.is_empty() || !decoded.contains('.') {
        return None;
    }
    Some(sanitize_filename(&decoded))
}

pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &text[i + 1..i + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Keep a server-provided name usable on Windows.
pub fn sanitize_filename(name: &str) -> String {
    let mut cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    cleaned = cleaned.trim().trim_end_matches('.').to_string();
    if cleaned.is_empty() {
        cleaned = "download".into();
    }
    if cleaned.chars().count() > 150 {
        cleaned = cleaned.chars().take(150).collect();
    }
    cleaned
}

/// Extensions that name a file to fetch as it is. Pages (`.html`, `.php`…)
/// and stream playlists (`.m3u8`, `.mpd`) are not here: yt-dlp handles them.
const FILE_EXTENSIONS: &[&str] = &[
    // archives and disk images
    "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "cab", "iso", "img", "dmg",
    // programs and packages
    "exe", "msi", "msix", "appx", "apk", "deb", "rpm", "appimage", "bin", "jar", "torrent",
    // documents and data
    "pdf", "epub", "mobi", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf",
    "txt", "csv", "json", "xml", "sql", // images and fonts
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "svg", "ico", "psd", "heic", "ttf",
    "otf", "woff", "woff2", // audio and video files served as they are
    "mp3", "flac", "wav", "m4a", "aac", "ogg", "opus", "mp4", "mkv", "webm", "avi", "mov", "m4v",
    "wmv", "flv",
];

/// Whether the URL's last path segment is a file name with a download
/// extension.
pub fn looks_like_file(url: &str) -> bool {
    extension(url).is_some_and(|ext| FILE_EXTENSIONS.contains(&ext.as_str()))
}

/// Whether the URL is a stream's playlist itself (HLS `.m3u8`, DASH `.mpd`)
/// rather than the page that plays it. Such addresses usually carry a token
/// or expect the page's `Referer`, which a copied link no longer has.
pub fn is_stream_manifest(url: &str) -> bool {
    extension(url).is_some_and(|ext| ext == "m3u8" || ext == "mpd")
}

fn extension(url: &str) -> Option<String> {
    file_hint(url).and_then(|name| {
        name.rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
    })
}

pub fn classify(raw: &str) -> Result<Link, String> {
    let url = normalize(raw)?;
    if url.to_ascii_lowercase().starts_with("magnet:") {
        return Ok(Link {
            url,
            host: "magnet".into(),
            kind: LinkKind::Torrent,
            site: None,
        });
    }
    let host = host_of(&url);
    if host.is_empty() {
        return Err(t!("link.no_host").into());
    }
    let site = site_label(&host);
    // A known site is media. Elsewhere the path decides: a file name with a
    // download extension is a plain file for aria2; anything else (a page,
    // no extension at all) goes to yt-dlp, whose 1,800-odd extractors know
    // far more sites than this list, and which hands an unsupported link
    // back to aria2 (engine.rs).
    let kind = if site.is_some() || !looks_like_file(&url) {
        LinkKind::Media
    } else {
        LinkKind::Direct
    };
    Ok(Link {
        url,
        host,
        kind,
        site,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_links_are_refused_with_their_own_reason() {
        let blob = normalize("blob:https://www.example.com/0b6c1f2e-6a3b").unwrap_err();
        let ftp = normalize("ftp://example.com/file").unwrap_err();
        assert!(blob.contains("blob:"), "{blob}");
        assert_ne!(blob, ftp);
    }

    #[test]
    fn stream_manifests_are_told_from_pages() {
        assert!(is_stream_manifest(
            "https://cdn.example.com/a/master.m3u8?token=x"
        ));
        assert!(is_stream_manifest("https://cdn.example.com/a/manifest.MPD"));
        assert!(!is_stream_manifest("https://www.example.com/watch/123"));
        assert!(!is_stream_manifest("https://example.com/clip.mp4"));
    }

    #[test]
    fn youtube_variants_are_media() {
        for url in [
            "https://www.youtube.com/watch?v=jNQXAC9IVRw",
            "http://youtube.com/watch?v=abc",
            "https://m.youtube.com/watch?v=abc",
            "https://music.youtube.com/watch?v=abc",
            "https://youtu.be/jNQXAC9IVRw",
        ] {
            let link = classify(url).unwrap();
            assert_eq!(link.kind, LinkKind::Media, "{url}");
            assert_eq!(link.site, Some("YouTube"), "{url}");
        }
    }

    #[test]
    fn media_site_matching_ignores_www_and_case() {
        let link = classify("https://WWW.Vimeo.com/12345").unwrap();
        assert_eq!(link.host, "vimeo.com");
        assert_eq!(link.site, Some("Vimeo"));
    }

    #[test]
    fn lookalike_hosts_are_not_labelled() {
        let link = classify("https://notyoutube.com/watch?v=1").unwrap();
        assert_eq!(link.site, None);
    }

    /// yt-dlp knows far more sites than the label list: a page on any host
    /// goes to it, a file name with a download extension goes to aria2.
    #[test]
    fn unknown_hosts_are_routed_by_their_path() {
        for url in [
            "https://www.arte.tv/fr/videos/123456-000-A/some-show/",
            "https://example.com/watch?v=1",
            "https://example.com/",
            "https://example.com/video.html",
            "https://cdn.example.com/live/stream.m3u8",
        ] {
            assert_eq!(classify(url).unwrap().kind, LinkKind::Media, "{url}");
        }
        for url in [
            "https://example.com/files/report.pdf",
            "https://github.com/a/b/releases/download/v1/tool-setup.EXE",
            "https://mirror.example.org/linux.iso?mirror=eu",
            "https://example.com/clip.mp4",
        ] {
            assert_eq!(classify(url).unwrap().kind, LinkKind::Direct, "{url}");
        }
    }

    #[test]
    fn bare_hosts_get_a_scheme() {
        assert_eq!(
            normalize("youtube.com/watch?v=1").unwrap(),
            "https://youtube.com/watch?v=1"
        );
    }

    #[test]
    fn quotes_and_space_are_trimmed() {
        assert_eq!(
            normalize("  \"https://a.test/x.zip\"  ").unwrap(),
            "https://a.test/x.zip"
        );
    }

    #[test]
    fn magnet_links_are_torrents() {
        let link = classify("magnet:?xt=urn:btih:abc123&dn=file").unwrap();
        assert_eq!(link.kind, LinkKind::Torrent);
        assert_eq!(link.host, "magnet");
    }

    #[test]
    fn plain_files_are_direct() {
        let link = classify("https://example.com/files/report.pdf").unwrap();
        assert_eq!(link.kind, LinkKind::Direct);
        assert_eq!(file_hint(&link.url).as_deref(), Some("report.pdf"));
    }

    #[test]
    fn non_links_are_rejected() {
        assert!(classify("hello world").is_err());
        assert!(classify("   ").is_err());
        assert!(classify("ftp://a.test/x").is_err());
    }

    #[test]
    fn port_and_userinfo_do_not_leak_into_host() {
        assert_eq!(host_of("https://user:pw@example.com:8443/a"), "example.com");
    }

    #[test]
    fn filenames_are_sanitized_for_windows() {
        assert_eq!(sanitize_filename("a:b*c?.mp4"), "a_b_c_.mp4");
        assert_eq!(sanitize_filename("..."), "download");
    }

    #[test]
    fn percent_encoded_names_are_decoded() {
        assert_eq!(
            file_hint("https://a.test/My%20File.mp4").as_deref(),
            Some("My File.mp4")
        );
    }
}
