//! The little HTTPS the app does on its own: the update manifest, installers
//! and the tools. Downloads themselves never come through here — they are
//! aria2's and yt-dlp's job.
//!
//! Everything is blocking and runs on threads of its own; errors are
//! sentences, as everywhere else in the app.

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// One client for a whole job: connections are reused between the checksum
/// file and the archive it describes.
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(concat!("edownloader/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(15)))
        // A stalled transfer must end; a slow one must not. This bounds the
        // wait for the next bytes, not the whole download.
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    agent.get(url).call().map_err(|error| match error {
        ureq::Error::StatusCode(code) => format!("{} answered HTTP {code}", host_of(url)),
        other => format!("cannot reach {}: {other}", host_of(url)),
    })
}

/// A small text resource (a manifest, a checksum list), refused past `max`.
pub fn get_text(agent: &ureq::Agent, url: &str, max: u64) -> Result<String, String> {
    let response = get(agent, url)?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_with_config()
        .limit(max)
        .reader()
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", host_of(url)))?;
    String::from_utf8(bytes)
        .map_err(|_| format!("{} sent something that is not text", host_of(url)))
}

/// Streams `url` into `dest`, at most `max` bytes, and returns the SHA-256 of
/// what was written. `progress(done, total)` is called a few times a second.
/// A failed download leaves no file behind.
pub fn download_to(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    max: u64,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String, String> {
    let response = get(agent, url)?;
    let total = response.body().content_length();
    if total.is_some_and(|len| len > max) {
        return Err(format!("{} is larger than expected", file_of(url)));
    }
    let result = (|| -> Result<String, String> {
        let mut reader = response.into_body().into_with_config().limit(max).reader();
        let mut file = std::fs::File::create(dest)
            .map_err(|error| format!("cannot write {}: {error}", dest.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 256 * 1024];
        let mut done = 0u64;
        let mut last = Instant::now();
        progress(0, total);
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| format!("download of {} interrupted: {error}", file_of(url)))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| format!("cannot write {}: {error}", dest.display()))?;
            done += read as u64;
            if last.elapsed() >= Duration::from_millis(150) {
                progress(done, total);
                last = Instant::now();
            }
        }
        file.sync_all()
            .map_err(|error| format!("cannot write {}: {error}", dest.display()))?;
        if total.is_some_and(|len| len != done) {
            return Err(format!("download of {} ended early", file_of(url)));
        }
        progress(done, total);
        Ok(hex(&hasher.finalize()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 a checksum list gives for `file_name`, lower case.
///
/// Projects publish these in several shapes: `sha256sum` lines
/// (`<hash>  <name>` or `<hash> *<name>`), BSD tags, or PowerShell's
/// `Get-FileHash | Format-List` (Deno), where the hash and the file sit on
/// separate `Hash :` and `Path :` lines. A line holding both a hash and the
/// name wins; otherwise a list holding exactly one hash, and naming the file
/// somewhere, is taken as that file's.
pub fn find_sha256(list: &str, file_name: &str) -> Option<String> {
    let hash_in = |line: &str| -> Option<String> {
        line.split(|c: char| c.is_whitespace() || c == '=' || c == '*' || c == ':')
            .find(|token| token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase)
    };
    let names = |line: &str| {
        line.split(|c: char| {
            c.is_whitespace() || c == '*' || c == '/' || c == '\\' || c == '(' || c == ')'
        })
        .any(|token| token == file_name)
    };
    let hashed: Vec<(&str, String)> = list
        .lines()
        .filter_map(|line| hash_in(line).map(|hash| (line, hash)))
        .collect();
    if let Some((_, hash)) = hashed.iter().find(|(line, _)| names(line)) {
        return Some(hash.clone());
    }
    match hashed.as_slice() {
        [(_, only)] if list.lines().any(names) => Some(only.clone()),
        _ => None,
    }
}

/// The host of a URL, for messages.
pub fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// The last path segment of a URL, if it is a plain file name: what a
/// download is saved as, so nothing in it may climb out of the folder.
pub fn file_of(url: &str) -> &str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('/').next().unwrap_or(path)
}

pub fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-+".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0d7ec4303a02bcbe66154d478852cdf2e07688d39dc4597316e4424ac4e74356";
    const B: &str = "f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d";

    #[test]
    fn sha256sum_lines_are_matched_by_name() {
        let list = format!("{A}  yt-dlp\n{B}  yt-dlp.exe\n");
        assert_eq!(find_sha256(&list, "yt-dlp.exe").as_deref(), Some(B));
        assert_eq!(find_sha256(&list, "yt-dlp").as_deref(), Some(A));
        assert_eq!(find_sha256(&list, "yt-dlp_x86.exe"), None);
    }

    #[test]
    fn binary_marker_and_bsd_tags_are_understood() {
        assert_eq!(
            find_sha256(&format!("{B} *ffmpeg.zip\n"), "ffmpeg.zip").as_deref(),
            Some(B)
        );
        assert_eq!(
            find_sha256(&format!("SHA256 (ffmpeg.zip) = {B}\n"), "ffmpeg.zip").as_deref(),
            Some(B)
        );
    }

    /// Deno's `.sha256sum`, as published: `Get-FileHash | Format-List`, CRLF,
    /// the hash and the file on separate lines.
    #[test]
    fn a_powershell_list_gives_its_one_hash_to_the_file_it_names() {
        let list = format!(
            "\r\nAlgorithm : SHA256\r\nHash      : {}\r\nPath      : C:\\a\\deno\\deno\\target\\release\\deno-x86_64-pc-windows-msvc.zip\r\n\r\n",
            B.to_ascii_uppercase()
        );
        assert_eq!(
            find_sha256(&list, "deno-x86_64-pc-windows-msvc.zip").as_deref(),
            Some(B)
        );
        // The one hash of a list naming another file is not this file's.
        assert_eq!(find_sha256(&list, "deno.zip"), None);
    }

    #[test]
    fn hosts_and_file_names_are_read_from_urls() {
        let url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe?x=1";
        assert_eq!(host_of(url), "github.com");
        assert_eq!(file_of(url), "yt-dlp.exe");
        assert!(is_plain_file_name("edownloader_0.1.0_x64-setup.exe"));
        assert!(!is_plain_file_name(".."));
        assert!(!is_plain_file_name("a\\b.exe"));
    }
}
