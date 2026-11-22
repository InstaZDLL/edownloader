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
    ("rumble.com", "Rumble"),
    ("odysee.com", "Odysee"),
    ("streamable.com", "Streamable"),
    ("vk.com", "VK"),
    ("ok.ru", "Odnoklassniki"),
    ("pinterest.com", "Pinterest"),
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
        return Err("Paste a link first.".into());
    }
    if trimmed.to_ascii_lowercase().starts_with("magnet:") {
        return Ok(trimmed.to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(trimmed.to_string());
    }
    if lower.contains("://") {
        return Err("Only http(s) and magnet links are supported.".into());
    }
    if looks_like_host(trimmed) {
        return Ok(format!("https://{trimmed}"));
    }
    Err("That does not look like a link.".into())
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

fn percent_decode(text: &str) -> String {
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
        return Err("That link has no host.".into());
    }
    let site = site_label(&host);
    let kind = if site.is_some() {
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
    fn lookalike_hosts_are_not_media() {
        let link = classify("https://notyoutube.com/watch?v=1").unwrap();
        assert_eq!(link.kind, LinkKind::Direct);
        assert_eq!(link.site, None);
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
