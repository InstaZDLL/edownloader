//! Interface languages.
//!
//! Every string the app shows lives in `locales/<code>.json`, a flat map of
//! dotted keys, compiled into the binary. English is the reference: a key a
//! translation lacks falls back to it, and the tests below make sure no
//! translation lacks one, adds one, or drops a `{placeholder}`.
//!
//! Plurals are two keys, `<key>.one` and `<key>.other`, picked by the
//! language's own rule (French and Portuguese put 0 with 1; Japanese and
//! Chinese have no plural form).
//!
//! The language is a process-wide setting, so worker threads (the engine,
//! the installer) word their messages in it too.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Lang {
    En,
    Fr,
    De,
    Es,
    It,
    Pt,
    Ja,
    ZhCn,
}

impl Lang {
    pub(crate) const ALL: [Lang; 8] = [
        Lang::En,
        Lang::Fr,
        Lang::De,
        Lang::Es,
        Lang::It,
        Lang::Pt,
        Lang::Ja,
        Lang::ZhCn,
    ];

    /// The code stored in settings.json.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Fr => "fr",
            Lang::De => "de",
            Lang::Es => "es",
            Lang::It => "it",
            Lang::Pt => "pt-BR",
            Lang::Ja => "ja",
            Lang::ZhCn => "zh-CN",
        }
    }

    /// The language's name in itself, as the picker shows it.
    pub(crate) fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Fr => "Français",
            Lang::De => "Deutsch",
            Lang::Es => "Español",
            Lang::It => "Italiano",
            Lang::Pt => "Português",
            Lang::Ja => "日本語",
            Lang::ZhCn => "简体中文",
        }
    }

    pub(crate) fn from_code(code: &str) -> Option<Lang> {
        Lang::ALL
            .into_iter()
            .find(|lang| lang.code().eq_ignore_ascii_case(code))
    }

    /// A BCP 47 tag such as `fr-CA` or `zh-Hans-CN`, matched by its language.
    /// Any Chinese gets Simplified Chinese, any Portuguese the Brazilian
    /// translation: closer than English for either.
    pub(crate) fn from_tag(tag: &str) -> Option<Lang> {
        let primary = tag.split(['-', '_']).next()?.to_ascii_lowercase();
        Some(match primary.as_str() {
            "en" => Lang::En,
            "fr" => Lang::Fr,
            "de" => Lang::De,
            "es" => Lang::Es,
            "it" => Lang::It,
            "pt" => Lang::Pt,
            "ja" => Lang::Ja,
            "zh" => Lang::ZhCn,
            _ => return None,
        })
    }

    fn index(self) -> usize {
        self as usize
    }

    fn source(self) -> &'static str {
        match self {
            Lang::En => include_str!("../locales/en.json"),
            Lang::Fr => include_str!("../locales/fr.json"),
            Lang::De => include_str!("../locales/de.json"),
            Lang::Es => include_str!("../locales/es.json"),
            Lang::It => include_str!("../locales/it.json"),
            Lang::Pt => include_str!("../locales/pt-BR.json"),
            Lang::Ja => include_str!("../locales/ja.json"),
            Lang::ZhCn => include_str!("../locales/zh-CN.json"),
        }
    }

    /// Whether `n` takes the singular form.
    fn is_one(self, n: u64) -> bool {
        match self {
            Lang::En | Lang::De | Lang::Es | Lang::It => n == 1,
            Lang::Fr | Lang::Pt => n <= 1,
            Lang::Ja | Lang::ZhCn => false,
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);
static CATALOGS: [OnceLock<HashMap<String, String>>; 8] = [const { OnceLock::new() }; 8];

pub(crate) fn current() -> Lang {
    Lang::ALL[CURRENT.load(Ordering::Relaxed) as usize % Lang::ALL.len()]
}

pub(crate) fn set(lang: Lang) {
    CURRENT.store(lang.index() as u8, Ordering::Relaxed);
}

/// The stored choice (`"system"` or a code) as a language.
pub(crate) fn resolve(choice: &str) -> Lang {
    if choice == "system" {
        system()
    } else {
        Lang::from_code(choice).unwrap_or_else(system)
    }
}

/// The Windows display language, English when it is not one of ours.
///
/// Windows keeps the user's display languages in preference order (Settings
/// › Time & language › Language); the first one we translate wins. This is
/// the interface language, not the regional format: a French Windows set to
/// US date formats is still French.
pub(crate) fn system() -> Lang {
    first_supported(&system_tags())
}

fn first_supported(tags: &[String]) -> Lang {
    tags.iter()
        .find_map(|tag| Lang::from_tag(tag))
        .unwrap_or(Lang::En)
}

#[cfg(windows)]
fn system_tags() -> Vec<String> {
    use windows::core::PWSTR;
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

    let mut count = 0u32;
    let mut len = 0u32;
    // SAFETY: the first call only asks for the buffer size; the second gets
    // a buffer of exactly that many UTF-16 units.
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut len).is_err()
            || len == 0
        {
            return Vec::new();
        }
        let mut buffer = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut len,
        )
        .is_err()
        {
            return Vec::new();
        }
        split_multi_sz(&buffer)
    }
}

#[cfg(not(windows))]
fn system_tags() -> Vec<String> {
    std::env::var("LANG").ok().into_iter().collect()
}

/// `fr-FR\0en-US\0\0` → `["fr-FR", "en-US"]`.
#[cfg_attr(not(windows), allow(dead_code))]
fn split_multi_sz(buffer: &[u16]) -> Vec<String> {
    buffer
        .split(|&unit| unit == 0)
        .filter(|name| !name.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

fn catalog(lang: Lang) -> &'static HashMap<String, String> {
    CATALOGS[lang.index()].get_or_init(|| {
        serde_json::from_str(lang.source())
            .unwrap_or_else(|error| panic!("locales/{}.json: {error}", lang.code()))
    })
}

/// The text for `key` in the current language, English if it has none, the
/// key itself if English has none either.
pub(crate) fn text(key: &'static str) -> &'static str {
    let lang = current();
    catalog(lang)
        .get(key)
        .or_else(|| catalog(Lang::En).get(key))
        .map(String::as_str)
        .unwrap_or(key)
}

/// `one` or `other`, by the current language's plural rule.
pub(crate) fn plural(one: &'static str, other: &'static str, n: u64) -> &'static str {
    text(if current().is_one(n) { one } else { other })
}

/// `{name}` in `template` replaced by each argument's value.
pub(crate) fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(arg, _)| *arg == name) {
                    Some((_, value)) => out.push_str(&value.to_string()),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// `t!("key")` is the text for a key; `t!("key", name = value, …)` fills its
/// `{name}` placeholders.
macro_rules! t {
    ($key:literal) => {
        $crate::i18n::text($key)
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::fill(
            $crate::i18n::text($key),
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}

/// `tn!("key", n, …)` picks `key.one` or `key.other` for `n` and fills `{n}`
/// along with any other placeholder.
macro_rules! tn {
    ($key:literal, $n:expr $(, $name:ident = $value:expr)* $(,)?) => {{
        let n = $n;
        $crate::i18n::fill(
            $crate::i18n::plural(concat!($key, ".one"), concat!($key, ".other"), n as u64),
            &[("n", &n as &dyn ::std::fmt::Display) $(, (stringify!($name), &$value as &dyn ::std::fmt::Display))*],
        )
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(text: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else { break };
            found.insert(after[..close].to_string());
            rest = &after[close + 1..];
        }
        found
    }

    #[test]
    fn every_translation_has_the_english_keys_and_placeholders() {
        let english = catalog(Lang::En);
        for lang in Lang::ALL {
            let other = catalog(lang);
            let missing: Vec<&String> = english
                .keys()
                .filter(|key| !other.contains_key(*key))
                .collect();
            let extra: Vec<&String> = other
                .keys()
                .filter(|key| !english.contains_key(*key))
                .collect();
            assert!(missing.is_empty(), "{}: missing {missing:?}", lang.code());
            assert!(extra.is_empty(), "{}: unknown {extra:?}", lang.code());
            for (key, text) in english {
                assert_eq!(
                    placeholders(text),
                    placeholders(&other[key]),
                    "{}: {key}",
                    lang.code()
                );
                assert!(
                    !other[key].trim().is_empty(),
                    "{}: {key} is empty",
                    lang.code()
                );
            }
        }
    }

    #[test]
    fn plurals_come_in_pairs() {
        for key in catalog(Lang::En).keys() {
            if let Some(stem) = key.strip_suffix(".one") {
                assert!(
                    catalog(Lang::En).contains_key(&format!("{stem}.other")),
                    "{key}"
                );
            }
            if let Some(stem) = key.strip_suffix(".other") {
                assert!(
                    catalog(Lang::En).contains_key(&format!("{stem}.one")),
                    "{key}"
                );
            }
        }
    }

    #[test]
    fn every_key_the_code_uses_exists() {
        let english = catalog(Lang::En);
        let sources = [
            include_str!("app.rs"),
            include_str!("views.rs"),
            include_str!("theme.rs"),
            include_str!("core/cookies.rs"),
            include_str!("core/engine.rs"),
            include_str!("core/human.rs"),
            include_str!("core/setup.rs"),
            include_str!("core/tools.rs"),
            include_str!("core/urlx.rs"),
        ];
        for source in sources {
            for (marker, plural) in [("t!(\"", false), ("tn!(\"", true)] {
                let mut rest = source;
                while let Some(at) = rest.find(marker) {
                    let after = &rest[at + marker.len()..];
                    let end = after.find('"').unwrap();
                    let key = &after[..end];
                    // `format!("` ends in `t!("` too: only a macro name
                    // that starts there counts.
                    let named = rest[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_');
                    rest = &after[end..];
                    if named {
                        continue;
                    }
                    if plural {
                        for form in ["one", "other"] {
                            let full = format!("{key}.{form}");
                            assert!(english.contains_key(&full), "en.json lacks {full}");
                        }
                    } else {
                        assert!(english.contains_key(key), "en.json lacks {key}");
                    }
                }
            }
        }
    }

    #[test]
    fn fill_replaces_known_names_and_keeps_the_rest() {
        assert_eq!(fill("{a} of {b}", &[("a", &1), ("b", &"two")]), "1 of two");
        assert_eq!(fill("{a} {z}", &[("a", &1)]), "1 {z}");
        assert_eq!(fill("open {", &[]), "open {");
    }

    #[test]
    fn tags_map_to_languages() {
        assert_eq!(Lang::from_tag("fr-CA"), Some(Lang::Fr));
        assert_eq!(Lang::from_tag("zh-Hant-TW"), Some(Lang::ZhCn));
        assert_eq!(Lang::from_tag("pt-PT"), Some(Lang::Pt));
        assert_eq!(Lang::from_tag("ko-KR"), None);
        assert_eq!(Lang::from_code("PT-br"), Some(Lang::Pt));
    }

    #[test]
    fn the_first_translated_display_language_wins() {
        let wide: Vec<u16> = "ko-KR\0fr-FR\0en-US\0\0".encode_utf16().collect();
        let tags = split_multi_sz(&wide);
        assert_eq!(tags, ["ko-KR", "fr-FR", "en-US"]);
        assert_eq!(first_supported(&tags), Lang::Fr);
        assert_eq!(first_supported(&["ko-KR".to_string()]), Lang::En);
        assert_eq!(first_supported(&[]), Lang::En);
    }

    #[test]
    fn plural_rules() {
        assert!(Lang::Fr.is_one(0));
        assert!(!Lang::En.is_one(0));
        assert!(Lang::En.is_one(1));
        assert!(!Lang::Ja.is_one(1));
    }
}
