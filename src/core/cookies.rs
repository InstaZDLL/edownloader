//! Cookie import, storage and analysis.
//!
//! YouTube (and increasingly every site) answers a logged-out client with a
//! bot check, so a downloader needs a cookie jar. Three formats are accepted,
//! because that is what the tools people actually use produce:
//!
//! * **Netscape** — `cookies.txt`, what yt-dlp consumes directly and what the
//!   "Get cookies.txt LOCALLY" style extensions export.
//! * **JSON** — EditThisCookie / cookie-editor arrays.
//! * **`Cookie:` header** — the single line copied out of devtools, which
//!   needs a target domain to become a real jar.
//!
//! Jars are stored in one JSON file and materialised to
//! `active-cookies.txt` for yt-dlp on demand.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::human::now_epoch;
use crate::core::paths;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cookie {
    pub domain: String,
    #[serde(default)]
    pub include_subdomains: bool,
    #[serde(default = "root_path")]
    pub path: String,
    #[serde(default)]
    pub secure: bool,
    /// Unix seconds; `0` means a session cookie.
    #[serde(default)]
    pub expires: i64,
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub http_only: bool,
}

fn root_path() -> String {
    "/".to_string()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CookieFormat {
    Netscape,
    Json,
    Header,
    Unknown,
}

pub fn detect(content: &str) -> CookieFormat {
    let trimmed = strip_bom(content).trim_start();
    if trimmed.is_empty() {
        return CookieFormat::Unknown;
    }
    let first = trimmed.chars().next().unwrap_or(' ');
    if first == '[' || first == '{' {
        return CookieFormat::Json;
    }
    if trimmed.starts_with("# Netscape")
        || trimmed.starts_with("#HttpOnly_")
        || trimmed.contains('\t')
    {
        return CookieFormat::Netscape;
    }
    if looks_like_header(trimmed) {
        return CookieFormat::Header;
    }
    CookieFormat::Unknown
}

fn is_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'%'))
}

fn looks_like_header(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() || trimmed.contains('\t') || trimmed.starts_with('#') {
        return false;
    }
    let body = trimmed.strip_prefix("Cookie:").unwrap_or(trimmed).trim();
    let mut found = 0usize;
    for part in body.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((name, value)) = part.split_once('=') else {
            return false;
        };
        if !is_cookie_name(name.trim()) || value.trim().is_empty() {
            return false;
        }
        found += 1;
    }
    found > 0
}

/// Parse whatever was pasted, using `domain_hint` only for header-style input.
pub fn parse_auto(content: &str, domain_hint: Option<&str>) -> Result<Vec<Cookie>, String> {
    match detect(content) {
        CookieFormat::Netscape => parse_netscape(content),
        CookieFormat::Json => parse_json(content),
        CookieFormat::Header => {
            let domain = domain_hint
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .ok_or_else(|| {
                    "This looks like a `Cookie:` header — enter its domain first (for example youtube.com)."
                        .to_string()
                })?;
            parse_header(content, domain)
        }
        CookieFormat::Unknown => Err(
            "Unrecognized cookie format. Accepted: Netscape cookies.txt, JSON (cookie-editor), or a Cookie: header plus a domain."
                .into(),
        ),
    }
}

pub fn parse_netscape(content: &str) -> Result<Vec<Cookie>, String> {
    let mut cookies = Vec::new();
    for raw in strip_bom(content).lines() {
        let line = raw.trim_end_matches(['\r', '\n']).trim_end();
        if line.is_empty() {
            continue;
        }
        let (effective, http_only) = if let Some(rest) = line.strip_prefix("#HttpOnly_") {
            (rest, true)
        } else if line.starts_with('#') {
            continue;
        } else {
            (line, false)
        };
        let fields: Vec<&str> = effective.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        let name = fields[5].trim();
        if name.is_empty() {
            continue;
        }
        cookies.push(Cookie {
            domain: fields[0].trim().to_string(),
            include_subdomains: fields[1].eq_ignore_ascii_case("TRUE"),
            path: if fields[2].trim().is_empty() {
                root_path()
            } else {
                fields[2].trim().to_string()
            },
            secure: fields[3].eq_ignore_ascii_case("TRUE"),
            expires: fields[4].trim().parse::<i64>().unwrap_or(0),
            name: name.to_string(),
            // A value may itself contain tabs; keep the remainder intact.
            value: fields[6..].join("\t").trim().to_string(),
            http_only,
        });
    }
    if cookies.is_empty() {
        return Err("No cookies found in that Netscape file.".into());
    }
    Ok(cookies)
}

/// A JSON export may spell the expiry as a number or as a numeric string.
#[derive(Deserialize)]
#[serde(untagged)]
enum Numberish {
    Number(f64),
    Text(String),
}

impl Numberish {
    fn as_i64(&self) -> Option<i64> {
        match self {
            Numberish::Number(value) if value.is_finite() => Some(*value as i64),
            Numberish::Number(_) => None,
            Numberish::Text(value) => value.trim().parse::<f64>().ok().map(|number| number as i64),
        }
    }
}

#[derive(Deserialize)]
struct JsonCookie {
    #[serde(default)]
    domain: Option<String>,
    /// Some exporters carry the page URL instead of a domain.
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default, alias = "httpOnly")]
    http_only: Option<bool>,
    #[serde(default, alias = "hostOnly")]
    host_only: Option<bool>,
    #[serde(default)]
    secure: Option<bool>,
    #[serde(
        default,
        alias = "expirationDate",
        alias = "expires",
        alias = "expiration"
    )]
    expiration: Option<Numberish>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    session: Option<bool>,
}

/// Files written on Windows often start with a UTF-8 BOM, which is not part
/// of the JSON document but breaks a strict parser.
fn strip_bom(text: &str) -> &str {
    text.trim_start_matches('\u{feff}')
}

/// The outermost array or object, for snippets copied with a `name = …;`
/// wrapper or a trailing comment.
fn json_slice(text: &str) -> Option<&str> {
    let start = text.find(['[', '{'])?;
    let end = text.rfind([']', '}'])?;
    (end > start).then(|| &text[start..=end])
}

fn json_items(value: serde_json::Value) -> Option<Vec<serde_json::Value>> {
    match value {
        serde_json::Value::Array(items) => Some(items),
        serde_json::Value::Object(map) => {
            // Some tools wrap the export: {"cookies":[...]} or {"data":[...]}.
            for key in ["cookies", "data"] {
                if let Some(serde_json::Value::Array(items)) = map.get(key) {
                    return Some(items.clone());
                }
            }
            Some(vec![serde_json::Value::Object(map)])
        }
        _ => None,
    }
}

fn host_from_url(url: &str) -> Option<String> {
    let after = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    (!host.is_empty() && host.contains('.')).then_some(host)
}

pub fn parse_json(content: &str) -> Result<Vec<Cookie>, String> {
    let text = strip_bom(content).trim();
    if text.is_empty() {
        return Err("That JSON is empty.".into());
    }
    let value = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => value,
        Err(first_error) => {
            let Some(slice) = json_slice(text) else {
                return Err(format!("That JSON could not be parsed: {first_error}"));
            };
            serde_json::from_str::<serde_json::Value>(slice).map_err(|_| {
                format!(
                    "That JSON could not be parsed: {first_error}. Expected an array of cookie objects."
                )
            })?
        }
    };

    let Some(items) = json_items(value) else {
        return Err("Expected a JSON array of cookie objects.".into());
    };
    let found = items.len();
    let mut cookies = Vec::with_capacity(found);
    for item in items {
        let Ok(parsed) = serde_json::from_value::<JsonCookie>(item) else {
            continue;
        };
        let Some(name) = parsed.name.filter(|name| !name.trim().is_empty()) else {
            continue;
        };
        let domain = parsed
            .domain
            .filter(|domain| !domain.trim().is_empty())
            .or_else(|| parsed.url.as_deref().and_then(host_from_url))
            .unwrap_or_default();
        if domain.is_empty() {
            continue;
        }
        let expires = if parsed.session.unwrap_or(false) {
            0
        } else {
            parsed
                .expiration
                .as_ref()
                .and_then(Numberish::as_i64)
                .unwrap_or(0)
        };
        cookies.push(Cookie {
            domain,
            include_subdomains: !parsed.host_only.unwrap_or(false),
            path: parsed
                .path
                .filter(|path| !path.is_empty())
                .unwrap_or_else(root_path),
            secure: parsed.secure.unwrap_or(false),
            expires,
            name,
            value: parsed.value.unwrap_or_default(),
            http_only: parsed.http_only.unwrap_or(false),
        });
    }
    if cookies.is_empty() {
        return Err(if found == 0 {
            "That JSON held no cookie objects.".to_string()
        } else {
            format!("{found} JSON object(s) found, but none carried both a domain and a name.")
        });
    }
    Ok(cookies)
}

/// Turn `a=b; c=d` into host cookies, defaulting to a three-day lifetime
/// because a header carries no expiry.
pub fn parse_header(content: &str, domain: &str) -> Result<Vec<Cookie>, String> {
    let root = domain.trim().trim_start_matches('.').to_ascii_lowercase();
    if root.is_empty() {
        return Err("A domain is required for Cookie header import.".into());
    }
    let cookie_domain = if root.contains('.') {
        format!(".{root}")
    } else {
        root.clone()
    };
    let expires = now_epoch() + 3 * 86_400;
    let body = content
        .trim()
        .strip_prefix("Cookie:")
        .unwrap_or(content.trim());
    let mut cookies = Vec::new();
    for part in body.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if !is_cookie_name(name) || value.is_empty() {
            continue;
        }
        cookies.push(Cookie {
            domain: cookie_domain.clone(),
            include_subdomains: true,
            path: root_path(),
            secure: true,
            expires,
            name: name.to_string(),
            value: value.to_string(),
            http_only: true,
        });
    }
    if cookies.is_empty() {
        return Err("No `name=value` pairs found in that header.".into());
    }
    Ok(cookies)
}

/// Netscape serialization, the format yt-dlp and curl read.
pub fn to_netscape(cookies: &[Cookie]) -> String {
    let mut out = String::from("# Netscape HTTP Cookie File\n");
    out.push_str("# Written by edownloader. https://curl.se/docs/http-cookies.html\n\n");
    let mut sorted: Vec<&Cookie> = cookies.iter().collect();
    sorted.sort_by(|a, b| {
        a.domain
            .to_ascii_lowercase()
            .cmp(&b.domain.to_ascii_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    for cookie in sorted {
        let prefix = if cookie.http_only { "#HttpOnly_" } else { "" };
        out.push_str(&format!(
            "{prefix}{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            cookie.domain,
            if cookie.include_subdomains {
                "TRUE"
            } else {
                "FALSE"
            },
            if cookie.path.is_empty() {
                "/"
            } else {
                &cookie.path
            },
            if cookie.secure { "TRUE" } else { "FALSE" },
            cookie.expires,
            cookie.name,
            cookie.value,
        ));
    }
    out
}

/// True when a jar entry for `cookie_domain` applies to `host`.
pub fn domain_matches(cookie_domain: &str, host: &str) -> bool {
    let cookie_domain = cookie_domain
        .trim()
        .trim_start_matches("#HttpOnly_")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let host = host
        .trim()
        .trim_start_matches("www.")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if cookie_domain.is_empty() || host.is_empty() {
        return false;
    }
    cookie_domain == host
        || cookie_domain.ends_with(&format!(".{host}"))
        || host.ends_with(&format!(".{cookie_domain}"))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CookieJar {
    pub id: String,
    pub name: String,
    /// `netscape`, `json`, `header`, `browser` or `manual`.
    pub source: String,
    /// Primary domain, derived from the most frequent cookie domain.
    pub domain: String,
    pub cookies: Vec<Cookie>,
    pub updated: i64,
}

impl CookieJar {
    pub fn build(
        name: impl Into<String>,
        source: impl Into<String>,
        cookies: Vec<Cookie>,
    ) -> CookieJar {
        let domain = dominant_domain(&cookies).unwrap_or_default();
        let mut name = name.into();
        if name.trim().is_empty() {
            name = if domain.is_empty() {
                "cookies".to_string()
            } else {
                domain.clone()
            };
        }
        CookieJar {
            id: new_id(),
            name,
            source: source.into(),
            domain,
            cookies,
            updated: now_epoch(),
        }
    }

    pub fn netscape(&self) -> String {
        to_netscape(&self.cookies)
    }

    /// Distinct domains sorted by cookie count, for the detail pane.
    pub fn domains(&self) -> Vec<(String, usize)> {
        let mut tally: Vec<(String, usize)> = Vec::new();
        for cookie in &self.cookies {
            let key = cookie.domain.trim_start_matches('.').to_ascii_lowercase();
            match tally.iter_mut().find(|(d, _)| *d == key) {
                Some((_, count)) => *count += 1,
                None => tally.push((key, 1)),
            }
        }
        tally.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        tally
    }

    pub fn expired(&self, now: i64) -> usize {
        self.cookies
            .iter()
            .filter(|c| c.expires > 0 && c.expires < now)
            .count()
    }

    pub fn youtube(&self) -> YoutubeAuth {
        analyse_youtube(&self.cookies, now_epoch())
    }
}

fn dominant_domain(cookies: &[Cookie]) -> Option<String> {
    let mut tally: Vec<(String, usize)> = Vec::new();
    for cookie in cookies {
        let key = cookie.domain.trim_start_matches('.').to_ascii_lowercase();
        match tally.iter_mut().find(|(d, _)| *d == key) {
            Some((_, count)) => *count += 1,
            None => tally.push((key, 1)),
        }
    }
    // Ascending, so the most frequent domain is the one popped.
    tally.sort_by_key(|entry| entry.1);
    tally.pop().map(|(d, _)| d)
}

fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthState {
    /// The jar has no youtube.com cookie at all.
    NotYouTube,
    /// The cookies that only a signed-in session carries are both present.
    SignedIn,
    /// Some authentication cookies exist but the set is incomplete.
    Partial,
    /// YouTube cookies, none of them authentication.
    NoAuth,
}

impl AuthState {
    pub fn label(self) -> &'static str {
        match self {
            AuthState::NotYouTube => "no YouTube cookies",
            AuthState::SignedIn => "signed in",
            AuthState::Partial => "incomplete sign-in",
            AuthState::NoAuth => "not signed in",
        }
    }
}

#[derive(Clone, Debug)]
pub struct YoutubeAuth {
    pub state: AuthState,
    pub present: Vec<String>,
    pub missing: Vec<&'static str>,
    pub expired: usize,
    pub total: usize,
}

/// Cookies a signed-in YouTube session always carries.
const YT_PRIMARY: &[&str] = &["SID", "__Secure-1PSID", "__Secure-3PSID"];
const YT_REQUIRED: &[&str] = &["SID", "HSID", "SSID", "APISID", "SAPISID", "LOGIN_INFO"];

pub fn analyse_youtube(cookies: &[Cookie], now: i64) -> YoutubeAuth {
    const YT_HOST: &str = "youtube.com";
    let youtube: Vec<&Cookie> = cookies
        .iter()
        .filter(|c| domain_matches(&c.domain, YT_HOST))
        .collect();
    if youtube.is_empty() {
        return YoutubeAuth {
            state: AuthState::NotYouTube,
            present: Vec::new(),
            missing: YT_REQUIRED.to_vec(),
            expired: 0,
            total: 0,
        };
    }
    let names: Vec<String> = youtube.iter().map(|c| c.name.clone()).collect();
    let has = |needle: &str| names.iter().any(|n| n == needle);
    let primary = YT_PRIMARY.iter().any(|n| has(n));
    let login_info = has("LOGIN_INFO");
    let state = match (primary, login_info) {
        (true, true) => AuthState::SignedIn,
        (true, false) | (false, true) => AuthState::Partial,
        (false, false) => AuthState::NoAuth,
    };
    YoutubeAuth {
        state,
        present: YT_REQUIRED
            .iter()
            .filter(|n| has(n))
            .map(|n| (*n).to_string())
            .collect(),
        missing: YT_REQUIRED.iter().filter(|n| !has(n)).copied().collect(),
        expired: youtube
            .iter()
            .filter(|c| c.expires > 0 && c.expires < now)
            .count(),
        total: youtube.len(),
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub jars: Vec<CookieJar>,
    #[serde(default)]
    pub active: Option<String>,
}

impl Store {
    pub fn load() -> Store {
        let path = paths::cookies_file();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Store::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        paths::ensure_dir(&paths::app_data_dir())
            .map_err(|e| format!("cannot create the data directory: {e}"))?;
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize cookies: {e}"))?;
        std::fs::write(paths::cookies_file(), text)
            .map_err(|e| format!("cannot write cookies.json: {e}"))
    }

    pub fn active_jar(&self) -> Option<&CookieJar> {
        let id = self.active.as_deref()?;
        self.jars.iter().find(|jar| jar.id == id)
    }

    pub fn set_active(&mut self, id: Option<&str>) {
        self.active = id.map(|s| s.to_string());
    }

    pub fn upsert(&mut self, jar: CookieJar) {
        match self.jars.iter_mut().find(|existing| existing.id == jar.id) {
            Some(slot) => *slot = jar,
            None => self.jars.push(jar),
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.jars.retain(|jar| jar.id != id);
        if self.active.as_deref() == Some(id) {
            self.active = None;
        }
    }

    /// Write (or delete) the cookie file yt-dlp is pointed at.
    pub fn sync_active_file(&self) -> Option<PathBuf> {
        let target = paths::active_cookie_file();
        match self.active_jar() {
            Some(jar) => {
                if paths::ensure_dir(&paths::app_data_dir()).is_err() {
                    return None;
                }
                std::fs::write(&target, jar.netscape()).ok()?;
                Some(target)
            }
            None => {
                let _ = std::fs::remove_file(&target);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_netscape() -> &'static str {
        "# Netscape HTTP Cookie File\n\
         .youtube.com\tTRUE\t/\tTRUE\t1830000000\tSID\tabc123\n\
         #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1830000000\tLOGIN_INFO\txyz\n\
         .example.com\tTRUE\t/\tFALSE\t0\tsess\tv\n"
    }

    #[test]
    fn detects_the_three_accepted_formats() {
        assert_eq!(detect(sample_netscape()), CookieFormat::Netscape);
        assert_eq!(detect("[{\"domain\":\"a.test\"}]"), CookieFormat::Json);
        assert_eq!(detect("SID=abc; HSID=def"), CookieFormat::Header);
        assert_eq!(detect("just words"), CookieFormat::Unknown);
    }

    #[test]
    fn netscape_keeps_httponly_prefix_and_values() {
        let cookies = parse_netscape(sample_netscape()).unwrap();
        assert_eq!(cookies.len(), 3);
        assert!(cookies[1].http_only);
        assert_eq!(cookies[1].name, "LOGIN_INFO");
        assert_eq!(cookies[2].expires, 0);
    }

    #[test]
    fn netscape_roundtrip_preserves_meaning() {
        let cookies = parse_netscape(sample_netscape()).unwrap();
        let text = to_netscape(&cookies);
        let again = parse_netscape(&text).unwrap();
        assert_eq!(cookies.len(), again.len());
        let sid = again.iter().find(|c| c.name == "SID").unwrap();
        assert_eq!(sid.value, "abc123");
        assert!(sid.secure);
    }

    /// Files written on Windows often begin with a UTF-8 BOM; it is not JSON
    /// and used to make the whole import fail as "unrecognized format".
    #[test]
    fn json_survives_a_byte_order_mark() {
        let raw = "\u{feff}[{\"domain\":\"a.test\",\"name\":\"k\",\"value\":\"v\"}]";
        assert_eq!(detect(raw), CookieFormat::Json);
        assert_eq!(parse_auto(raw, None).unwrap().len(), 1);
    }

    #[test]
    fn json_accepts_a_wrapper_object() {
        let raw = r#"{"url":"https://a.test","cookies":[{"domain":"a.test","name":"k","value":"v","expirationDate":"1830000000"}]}"#;
        let cookies = parse_json(raw).unwrap();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].expires, 1830000000);
    }

    #[test]
    fn json_derives_the_domain_from_a_url() {
        let raw = r#"[{"url":"https://sub.a.test/path","name":"k","value":"v"}]"#;
        assert_eq!(parse_json(raw).unwrap()[0].domain, "sub.a.test");
    }

    #[test]
    fn json_tolerates_null_values_and_session_cookies() {
        let raw = r#"[{"domain":"a.test","name":"k","value":null,"session":true}]"#;
        let cookies = parse_json(raw).unwrap();
        assert_eq!(cookies[0].value, "");
        assert_eq!(cookies[0].expires, 0);
    }

    #[test]
    fn json_wrapped_in_surrounding_text_still_parses() {
        let raw = "cookies = [{\"domain\":\"a.test\",\"name\":\"k\",\"value\":\"v\"}];";
        assert_eq!(parse_json(raw).unwrap().len(), 1);
    }

    #[test]
    fn json_explains_entries_without_a_domain() {
        let error = parse_json(r#"[{"name":"k","value":"v"}]"#).unwrap_err();
        assert!(error.contains("domain"), "{error}");
    }

    #[test]
    fn json_reports_unparsable_input() {
        let error = parse_json("[{oops").unwrap_err();
        assert!(error.contains("could not be parsed"), "{error}");
    }

    #[test]
    fn json_accepts_cookie_editor_shape() {
        let raw = r#"[{"domain":".youtube.com","expirationDate":1830000000.9,"hostOnly":false,
            "httpOnly":true,"name":"SID","path":"/","secure":true,"session":false,"value":"v"}]"#;
        let cookies = parse_json(raw).unwrap();
        assert_eq!(cookies[0].expires, 1830000000);
        assert!(cookies[0].include_subdomains);
    }

    /// The shape a real extension exported: every extra field present, a
    /// fractional expiry, `hostOnly` set and a non-netscape domain.
    #[test]
    fn json_accepts_extension_export_with_extra_fields() {
        let raw = r#"[
  {
    "domain": "waveflow.app",
    "expirationDate": 1826803965.243466,
    "hostOnly": true,
    "httpOnly": false,
    "name": "waveflow-locale",
    "path": "/",
    "sameSite": "lax",
    "secure": false,
    "session": false,
    "storeId": "0",
    "value": "en"
  }
]"#;
        let cookies = parse_json(raw).unwrap();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name, "waveflow-locale");
        assert_eq!(cookies[0].value, "en");
        assert_eq!(cookies[0].domain, "waveflow.app");
        assert_eq!(cookies[0].expires, 1826803965);
        assert!(!cookies[0].include_subdomains);
        assert!(!cookies[0].secure);
        // The active-file round trip must keep the entry readable by yt-dlp.
        let text = to_netscape(&cookies);
        assert!(text.contains("waveflow-locale\ten"));
    }

    #[test]
    fn header_needs_a_domain() {
        assert!(parse_auto("SID=abc; HSID=def", None).is_err());
        let cookies = parse_auto("SID=abc; HSID=def", Some("youtube.com")).unwrap();
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].domain, ".youtube.com");
    }

    #[test]
    fn malformed_input_is_rejected_not_panicked() {
        assert!(parse_netscape("garbage").is_err());
        assert!(parse_json("{not json").is_err());
        assert!(parse_auto("", None).is_err());
        assert!(parse_header("no pairs here", "x.com").is_err());
    }

    #[test]
    fn domain_matching_is_suffix_aware() {
        assert!(domain_matches(".youtube.com", "www.youtube.com"));
        assert!(domain_matches("youtube.com", "youtube.com"));
        assert!(!domain_matches(".youtube.com", "notyoutube.com"));
        assert!(!domain_matches("", "x.com"));
    }

    #[test]
    fn signed_in_jar_is_detected() {
        let mut cookies = parse_netscape(sample_netscape()).unwrap();
        for name in ["HSID", "SSID", "APISID", "SAPISID"] {
            cookies.push(Cookie {
                domain: ".youtube.com".into(),
                include_subdomains: true,
                path: "/".into(),
                secure: true,
                expires: 0,
                name: name.into(),
                value: "v".into(),
                http_only: true,
            });
        }
        assert_eq!(analyse_youtube(&cookies, 0).state, AuthState::SignedIn);
    }

    #[test]
    fn primary_without_login_info_is_partial() {
        let cookies = vec![Cookie {
            domain: ".youtube.com".into(),
            include_subdomains: true,
            path: "/".into(),
            secure: true,
            expires: 0,
            name: "SID".into(),
            value: "v".into(),
            http_only: true,
        }];
        assert_eq!(analyse_youtube(&cookies, 0).state, AuthState::Partial);
    }

    #[test]
    fn unrelated_jar_is_not_youtube() {
        let cookies = vec![Cookie {
            domain: ".example.com".into(),
            include_subdomains: true,
            path: "/".into(),
            secure: false,
            expires: 0,
            name: "SID".into(),
            value: "v".into(),
            http_only: false,
        }];
        assert_eq!(analyse_youtube(&cookies, 0).state, AuthState::NotYouTube);
    }

    #[test]
    fn jar_metadata_is_derived() {
        let jar = CookieJar::build("", "netscape", parse_netscape(sample_netscape()).unwrap());
        assert_eq!(jar.domain, "youtube.com");
        assert_eq!(jar.name, "youtube.com");
        assert_eq!(jar.domains().first().unwrap().0, "youtube.com");
    }

    #[test]
    fn store_activation_and_removal_stay_consistent() {
        let jar = CookieJar::build("j", "netscape", parse_netscape(sample_netscape()).unwrap());
        let id = jar.id.clone();
        let mut store = Store::default();
        store.upsert(jar);
        store.set_active(Some(&id));
        assert!(store.active_jar().is_some());
        store.remove(&id);
        assert!(store.active_jar().is_none());
        assert!(store.jars.is_empty());
    }
}
