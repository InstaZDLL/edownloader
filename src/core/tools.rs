//! Discovery of the three external binaries the app drives.
//!
//! The workspace ships them next to the project (`bin/`), so a portable drop
//! must work with no PATH setup at all: the executable's own directory, its
//! parents, the crate root at build time, and finally `PATH` are searched in
//! that order.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Tools {
    pub yt_dlp: Option<PathBuf>,
    pub aria2: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    /// A JavaScript engine yt-dlp can drive to solve YouTube's signature and
    /// `n` challenges. Without one YouTube degrades to storyboard-only
    /// responses, which looks exactly like a cookie problem.
    pub js_runtime: Option<JsRuntime>,
    /// Directory holding ffmpeg, handed to yt-dlp as `--ffmpeg-location`.
    pub bin_dir: Option<PathBuf>,
}

/// Everything the tool panel reports on: three bundled binaries plus a JS runtime.
pub const TOOL_COUNT: usize = 4;

/// Runtime names yt-dlp knows, in its own preference order.
pub const JS_RUNTIMES: &[&str] = &["deno", "node", "qjs", "bun"];

#[derive(Clone, Debug)]
pub struct JsRuntime {
    pub name: String,
    pub path: PathBuf,
}

impl JsRuntime {
    /// The `RUNTIME:PATH` form `--js-runtimes` accepts, which works whether or
    /// not the binary sits on the search path.
    pub fn ytdlp_arg(&self) -> String {
        format!("{}:{}", self.name, self.path.display())
    }
}

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn candidate_dirs() -> Vec<PathBuf> {
    // What the app installed itself comes first: "update yt-dlp" must take
    // effect even when an older copy sits next to the executable.
    let mut dirs: Vec<PathBuf> = vec![crate::core::paths::tools_dir()];
    let exe = crate::core::paths::exe_dir();
    // A portable drop keeps its tools in <exe>/bin.
    dirs.push(exe.join("bin"));
    // Development builds run from `target/<profile>`: the parents cover the
    // workspace's `bin/`. A release build never walks up: a portable copy
    // run from `C:\Something\edownloader.exe` would otherwise trust
    // `C:\bin`, a folder any local user may create and fill.
    if cfg!(debug_assertions) {
        let mut cursor = exe.as_path();
        for _ in 0..4 {
            if let Some(parent) = cursor.parent() {
                dirs.push(parent.join("bin"));
                cursor = parent;
            }
        }
    }
    // Development builds run from `target/<profile>`; the crate root is the
    // reliable anchor for `bin/` in that case. A release build never looks
    // there: that path is the build machine's.
    if cfg!(debug_assertions) {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        dirs.push(manifest.join("bin"));
    }
    dirs
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn locate(stem: &str) -> Option<PathBuf> {
    let name = exe_name(stem);
    for dir in candidate_dirs() {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    find_on_path(&name)
}

/// First JavaScript engine available, preferring the ones yt-dlp trusts most.
fn detect_js_runtime() -> Option<JsRuntime> {
    JS_RUNTIMES.iter().find_map(|name| {
        locate(name).map(|path| JsRuntime {
            name: (*name).to_string(),
            path,
        })
    })
}

impl Tools {
    pub fn detect() -> Tools {
        let yt_dlp = locate("yt-dlp");
        let aria2 = locate("aria2c");
        let ffmpeg = locate("ffmpeg");
        let js_runtime = detect_js_runtime();
        let bin_dir = ffmpeg
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| {
                yt_dlp
                    .as_ref()
                    .and_then(|p| p.parent().map(Path::to_path_buf))
            });
        Tools {
            yt_dlp,
            aria2,
            ffmpeg,
            js_runtime,
            bin_dir,
        }
    }

    pub fn yt_dlp(&self) -> Result<&Path, String> {
        self.yt_dlp
            .as_deref()
            .ok_or_else(|| "yt-dlp is not installed: install it from Settings › Tools".to_string())
    }

    pub fn aria2(&self) -> Result<&Path, String> {
        self.aria2
            .as_deref()
            .ok_or_else(|| "aria2 is not installed: install it from Settings › Tools".to_string())
    }

    /// Compact status for the toolbar: which tools are usable.
    pub fn ready_count(&self) -> usize {
        let binaries = [&self.yt_dlp, &self.aria2, &self.ffmpeg]
            .iter()
            .filter(|tool| tool.is_some())
            .count();
        binaries + usize::from(self.js_runtime.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_binaries_are_discovered_in_this_workspace() {
        let tools = Tools::detect();
        // The repository ships `bin/`; a checkout without it still passes by
        // simply reporting nothing.
        if Path::new(env!("CARGO_MANIFEST_DIR")).join("bin").is_dir() {
            assert!(tools.yt_dlp.is_some(), "yt-dlp should be found in bin/");
            assert!(tools.aria2.is_some(), "aria2c should be found in bin/");
        }
    }

    #[test]
    fn ffmpeg_directory_is_derived() {
        let tools = Tools::detect();
        if tools.ffmpeg.is_some() {
            assert!(tools.bin_dir.is_some());
        }
    }

    #[test]
    fn js_runtime_argument_uses_the_name_path_form() {
        let runtime = JsRuntime {
            name: "node".into(),
            path: PathBuf::from("/usr/bin/node"),
        };
        assert_eq!(runtime.ytdlp_arg(), "node:/usr/bin/node");
    }

    /// A machine without any of these is still valid; detection must not panic.
    #[test]
    fn js_runtime_detection_is_optional() {
        let tools = Tools::detect();
        if let Some(runtime) = tools.js_runtime.as_ref() {
            assert!(JS_RUNTIMES.contains(&runtime.name.as_str()));
            assert!(runtime.path.is_file());
        }
    }
}
