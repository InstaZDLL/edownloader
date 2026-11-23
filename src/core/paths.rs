//! Where the app keeps its files.
//!
//! Everything lives under one per-user data directory so the app stays
//! portable: settings, the cookie store, and the materialised cookie jars
//! handed to yt-dlp.

use std::path::{Path, PathBuf};

const APP_DIR: &str = "edownloader";

pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// `%APPDATA%\edownloader` on Windows, XDG data dir elsewhere.
pub fn app_data_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        Some(PathBuf::from(xdg))
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library").join("Application Support"))
    } else {
        home_dir().map(|h| h.join(".local").join("share"))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join(APP_DIR)
}

/// `%LOCALAPPDATA%\edownloader`: machine-local and not roamed, which is where
/// hundreds of megabytes of executables and downloaded installers belong.
/// The cache directory on other systems.
pub fn local_data_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        Some(PathBuf::from(xdg))
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library").join("Caches"))
    } else {
        home_dir().map(|h| h.join(".cache"))
    };
    base.map(|b| b.join(APP_DIR)).unwrap_or_else(app_data_dir)
}

/// Where the tool installer puts yt-dlp, aria2c, ffmpeg and Deno. Searched
/// before anything else (src/core/tools.rs), so an update made from the app
/// always wins over an older copy on `PATH`.
pub fn tools_dir() -> PathBuf {
    local_data_dir().join("tools")
}

/// Verified installers of newer versions, waiting to be run.
pub fn updates_dir() -> PathBuf {
    local_data_dir().join("updates")
}

pub fn settings_file() -> PathBuf {
    app_data_dir().join("settings.json")
}

pub fn cookies_file() -> PathBuf {
    app_data_dir().join("cookies.json")
}

/// The jar yt-dlp is pointed at with `--cookies`. Rewritten whenever the
/// active jar changes so no external file management is needed.
pub fn active_cookie_file() -> PathBuf {
    app_data_dir().join("active-cookies.txt")
}

pub fn default_download_dir() -> PathBuf {
    home_dir()
        .map(|h| h.join("Downloads"))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(path)
}

/// Open a directory in the platform file manager.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Native "open file" dialog.
///
/// Runs the platform's own picker in a short-lived helper process rather than
/// linking a dialog toolkit: on Windows that is the WinForms dialog through
/// PowerShell, which every supported machine already has. Returns `None` when
/// the user cancels or no dialog is available. Other systems use GPUI's
/// picker (the XDG portal on Linux), from app.rs.
#[cfg(windows)]
pub fn pick_open_file(title: &str, filter: &str) -> Option<PathBuf> {
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        // Both are translated text inside single-quoted PowerShell strings,
        // where a `'` would end the string: doubled, it stays a character.
        let title = title.replace('\'', "''");
        let filter = filter.replace('\'', "''");
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; \
             $d = New-Object System.Windows.Forms.OpenFileDialog; \
             $d.Title = '{title}'; \
             $d.Filter = '{filter}'; \
             $d.CheckFileExists = $true; \
             if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ Write-Output $d.FileName }}"
        );
        let mut command = std::process::Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-STA",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &script,
            ])
            .creation_flags(CREATE_NO_WINDOW);
        let output = command.output().ok()?;
        let chosen = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if chosen.is_empty() {
            None
        } else {
            Some(PathBuf::from(chosen))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_is_absolute_or_relative_but_stable() {
        let a = app_data_dir();
        let b = app_data_dir();
        assert_eq!(a, b);
        assert!(a.to_string_lossy().contains("edownloader"));
    }

    #[test]
    fn cookie_file_sits_in_the_data_dir() {
        assert_eq!(
            active_cookie_file().parent(),
            Some(app_data_dir().as_path())
        );
    }
}
