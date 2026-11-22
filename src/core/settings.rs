//! User preferences, persisted next to the cookie store.

use serde::{Deserialize, Serialize};

use crate::core::engine::{Container, FormatPreset};
use crate::core::paths;
use crate::theme::ThemeName;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub download_dir: String,
    /// Catppuccin flavour; `system` follows the desktop on next start.
    #[serde(default)]
    pub theme: ThemeName,
    /// Jobs running at once.
    pub max_concurrent: usize,
    /// aria2 connections per file.
    pub connections: usize,
    pub preset: FormatPreset,
    /// MP4 by default: it plays everywhere, and YouTube offers AV1-in-MP4
    /// even for 4K. The "native" mode keeps VP9/Opus, which merges to WebM.
    #[serde(default)]
    pub container: Container,
    pub playlist: bool,
    pub embed_metadata: bool,
    /// Send the active jar to yt-dlp.
    pub use_cookies: bool,
    /// Browser used by "snapshot from browser".
    pub browser: String,
    /// Look for a new version of the app at launch (src/core/update.rs).
    #[serde(default = "yes")]
    pub check_updates: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            download_dir: paths::default_download_dir().display().to_string(),
            theme: ThemeName::Mocha,
            max_concurrent: 3,
            connections: 8,
            preset: FormatPreset::Best,
            container: Container::Mp4,
            playlist: false,
            embed_metadata: false,
            use_cookies: true,
            browser: "chrome".to_string(),
            check_updates: true,
        }
    }
}

impl Settings {
    pub fn load() -> Settings {
        let path = paths::settings_file();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Settings::default();
        };
        let mut settings: Settings = serde_json::from_str(&text).unwrap_or_default();
        settings.sanitize();
        settings
    }

    pub fn save(&self) -> Result<(), String> {
        paths::ensure_dir(&paths::app_data_dir())
            .map_err(|e| format!("cannot create the data directory: {e}"))?;
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize settings: {e}"))?;
        std::fs::write(paths::settings_file(), text)
            .map_err(|e| format!("cannot write settings.json: {e}"))
    }

    /// Keep stored values inside the ranges the UI and the tools accept.
    pub fn sanitize(&mut self) {
        self.max_concurrent = self.max_concurrent.clamp(1, 8);
        self.connections = self.connections.clamp(1, 16);
        if self.download_dir.trim().is_empty() {
            self.download_dir = paths::default_download_dir().display().to_string();
        }
        if self.browser.trim().is_empty() {
            self.browser = "chrome".to_string();
        }
    }

    pub fn dir(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(self.download_dir.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_usable() {
        let settings = Settings::default();
        assert!(!settings.download_dir.is_empty());
        assert!(settings.max_concurrent >= 1);
        assert!(settings.connections >= 1);
        assert!(settings.use_cookies);
    }

    #[test]
    fn sanitize_clamps_out_of_range_values() {
        let mut settings = Settings {
            max_concurrent: 99,
            connections: 0,
            download_dir: "   ".into(),
            browser: "".into(),
            ..Settings::default()
        };
        settings.sanitize();
        assert_eq!(settings.max_concurrent, 8);
        assert_eq!(settings.connections, 1);
        assert!(!settings.download_dir.trim().is_empty());
        assert_eq!(settings.browser, "chrome");
    }

    #[test]
    fn settings_roundtrip_through_json() {
        let settings = Settings {
            max_concurrent: 2,
            preset: FormatPreset::P720,
            ..Settings::default()
        };
        let text = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.max_concurrent, 2);
        assert_eq!(back.preset, FormatPreset::P720);
    }
}
