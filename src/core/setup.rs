//! The tool installer: fetches yt-dlp, aria2c, ffmpeg and Deno from their
//! projects' own releases into `%LOCALAPPDATA%\edownloader\tools`.
//!
//! The installer of the app carries none of them: they are large (ffmpeg
//! alone is 200 MB), yt-dlp has to follow YouTube's changes far more often
//! than the app does, and each comes from the project that builds it. So the
//! app says what is missing at launch and installs it on a click, and
//! Settings can refresh yt-dlp at any time.
//!
//! Every file is checked against the SHA-256 its project publishes next to
//! it (yt-dlp, the yt-dlp FFmpeg builds and Deno do). aria2's last release,
//! 1.37.0 from 2023, publishes none, so its archive is pinned to that version
//! and taken over HTTPS from GitHub alone.

use std::path::{Path, PathBuf};

use crate::core::net;
use crate::core::paths;
use crate::core::tools::Tools;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Component {
    YtDlp,
    Aria2,
    Ffmpeg,
    Deno,
}

/// How a component is published.
struct Source {
    url: &'static str,
    /// The project's checksum list for `url`, if it publishes one.
    sums: Option<&'static str>,
    /// The executables to keep: the download itself when it is one, or the
    /// entries of the zip archive with these names, wherever they sit in it.
    files: &'static [&'static str],
    archive: bool,
    /// No genuine release comes near this; a larger answer is not one.
    max: u64,
}

impl Component {
    #[cfg(test)]
    pub const ALL: [Component; 4] = [
        Component::YtDlp,
        Component::Aria2,
        Component::Ffmpeg,
        Component::Deno,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Component::YtDlp => "yt-dlp",
            Component::Aria2 => "aria2",
            Component::Ffmpeg => "ffmpeg",
            Component::Deno => "Deno",
        }
    }

    /// What it is for, in the words of someone who just wants the file.
    pub fn purpose(self) -> &'static str {
        match self {
            Component::YtDlp => "YouTube and 1,800 other sites",
            Component::Aria2 => "fast, resumable direct links and magnets",
            Component::Ffmpeg => "merging video and audio, MP4 output",
            Component::Deno => "solves YouTube's player challenge",
        }
    }

    /// Download size, for the prompt.
    pub fn approx_size(self) -> &'static str {
        match self {
            Component::YtDlp => "23 MB",
            Component::Aria2 => "2.5 MB",
            Component::Ffmpeg => "200 MB",
            Component::Deno => "43 MB",
        }
    }

    fn source(self) -> Source {
        match self {
            Component::YtDlp => Source {
                url: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
                sums: Some("https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS"),
                files: &["yt-dlp.exe"],
                archive: false,
                max: 128 << 20,
            },
            Component::Aria2 => Source {
                url: "https://github.com/aria2/aria2/releases/download/release-1.37.0/aria2-1.37.0-win-64bit-build1.zip",
                sums: None,
                files: &["aria2c.exe"],
                archive: true,
                max: 32 << 20,
            },
            // yt-dlp's own FFmpeg builds: static, with the patches yt-dlp
            // relies on, and what its documentation recommends.
            Component::Ffmpeg => Source {
                url: "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip",
                sums: Some("https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/checksums.sha256"),
                files: &["ffmpeg.exe", "ffprobe.exe"],
                archive: true,
                max: 512 << 20,
            },
            Component::Deno => Source {
                url: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip",
                sums: Some("https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip.sha256sum"),
                files: &["deno.exe"],
                archive: true,
                max: 256 << 20,
            },
        }
    }
}

/// Whether this build can install tools: the sources above are Windows
/// builds. Elsewhere the system's package manager is the way.
pub fn supported() -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
}

/// What the app cannot work fully without, in install order. Any JavaScript
/// runtime already present (Node, Bun…) stands in for Deno.
pub fn missing(tools: &Tools) -> Vec<Component> {
    let mut list = Vec::new();
    if tools.yt_dlp.is_none() {
        list.push(Component::YtDlp);
    }
    if tools.aria2.is_none() {
        list.push(Component::Aria2);
    }
    if tools.ffmpeg.is_none() {
        list.push(Component::Ffmpeg);
    }
    if tools.js_runtime.is_none() {
        list.push(Component::Deno);
    }
    list
}

#[derive(Clone, Debug)]
pub enum SetupEvent {
    Progress {
        component: Component,
        stage: &'static str,
        done: u64,
        total: Option<u64>,
    },
    Installed(Component),
    Failed {
        component: Component,
        error: String,
    },
    /// Every component was attempted; `failed` of them did not install.
    Finished {
        failed: usize,
    },
}

/// Installs `components` one after the other on a thread of its own. One
/// failing does not stop the others.
pub fn spawn(components: Vec<Component>, emit: impl Fn(SetupEvent) + Send + 'static) {
    std::thread::spawn(move || {
        let agent = net::agent();
        let dir = paths::tools_dir();
        let mut failed = 0;
        for component in components {
            match install(&agent, &dir, component, &emit) {
                Ok(()) => {
                    emit(SetupEvent::Installed(component));
                }
                Err(error) => {
                    failed += 1;
                    emit(SetupEvent::Failed { component, error });
                }
            }
        }
        emit(SetupEvent::Finished { failed });
    });
}

fn install(
    agent: &ureq::Agent,
    dir: &Path,
    component: Component,
    emit: &dyn Fn(SetupEvent),
) -> Result<(), String> {
    if !supported() {
        return Err("automatic installation is only available on 64-bit Windows".into());
    }
    let source = component.source();
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let name = net::file_of(source.url);

    emit(SetupEvent::Progress {
        component,
        stage: "checking",
        done: 0,
        total: None,
    });
    let expected =
        match source.sums {
            Some(url) => {
                let list = net::get_text(agent, url, 1 << 20)?;
                Some(net::find_sha256(&list, name).ok_or_else(|| {
                    format!("{} publishes no checksum for {name}", component.label())
                })?)
            }
            None => None,
        };

    let download = dir.join(format!(".{name}.download"));
    let mut report = |done, total| {
        emit(SetupEvent::Progress {
            component,
            stage: "downloading",
            done,
            total,
        })
    };
    let actual = net::download_to(agent, source.url, &download, source.max, &mut report)?;
    if let Some(expected) = expected {
        if actual != expected {
            let _ = std::fs::remove_file(&download);
            return Err(format!(
                "{name} does not match the checksum {} publishes; nothing was installed",
                component.label()
            ));
        }
    }

    let result = if source.archive {
        emit(SetupEvent::Progress {
            component,
            stage: "extracting",
            done: 0,
            total: None,
        });
        extract(&download, dir, source.files)
    } else {
        place(&download, &dir.join(source.files[0]))
    };
    let _ = std::fs::remove_file(&download);
    result
}

/// Puts `from` at `to`, replacing what is there. Windows refuses while the
/// old one runs, which is said plainly.
fn place(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::rename(from, to).map_err(|error| {
        let name = to
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            format!("{name} is in use; let the running downloads finish, then try again")
        } else {
            format!("cannot install {name}: {error}")
        }
    })
}

/// Takes each of `wanted` out of the archive, whatever folder it is in, and
/// places it in `dir`. Nothing else in the archive is written anywhere.
fn extract(archive: &Path, dir: &Path, wanted: &[&str]) -> Result<(), String> {
    let file = std::fs::File::open(archive)
        .map_err(|error| format!("cannot open the archive: {error}"))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|error| format!("the archive is damaged: {error}"))?;
    for target in wanted {
        let index = (0..zip.len())
            .find(|&i| {
                zip.by_index(i).ok().is_some_and(|entry| {
                    entry.is_file()
                        && entry
                            .name()
                            .is_ok_and(|name| entry_file_name(&name).eq_ignore_ascii_case(target))
                })
            })
            .ok_or_else(|| format!("{target} is not in the archive"))?;
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("cannot read {target} from the archive: {error}"))?;
        let staged: PathBuf = dir.join(format!(".{target}.part"));
        let written = (|| -> std::io::Result<()> {
            let mut out = std::fs::File::create(&staged)?;
            std::io::copy(&mut entry, &mut out)?;
            out.sync_all()
        })();
        if let Err(error) = written {
            let _ = std::fs::remove_file(&staged);
            return Err(format!("cannot extract {target}: {error}"));
        }
        if let Err(error) = place(&staged, &dir.join(target)) {
            let _ = std::fs::remove_file(&staged);
            return Err(error);
        }
    }
    Ok(())
}

/// The last component of an archive path, whichever separator it uses.
fn entry_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn missing_lists_what_detection_did_not_find() {
        let none = Tools::default();
        assert_eq!(missing(&none), Component::ALL.to_vec());
        let all = Tools {
            yt_dlp: Some("y".into()),
            aria2: Some("a".into()),
            ffmpeg: Some("f".into()),
            js_runtime: Some(crate::core::tools::JsRuntime {
                name: "node".into(),
                path: "n".into(),
            }),
            bin_dir: None,
        };
        assert!(missing(&all).is_empty());
    }

    #[test]
    fn every_source_is_https_and_names_a_plain_file() {
        for component in Component::ALL {
            let source = component.source();
            assert!(source.url.starts_with("https://"), "{component:?}");
            assert!(net::is_plain_file_name(net::file_of(source.url)));
            assert!(source.sums.is_none_or(|s| s.starts_with("https://")));
            assert!(!source.files.is_empty());
        }
    }

    #[test]
    fn archive_entries_are_found_in_any_folder_and_nothing_else_is_written() {
        let dir = std::env::temp_dir().join(format!("edl-setup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("a.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("ffmpeg-master/bin/ffmpeg.exe", options)
                .unwrap();
            zip.write_all(b"ffmpeg").unwrap();
            zip.start_file("ffmpeg-master/bin/ffprobe.exe", options)
                .unwrap();
            zip.write_all(b"ffprobe").unwrap();
            zip.start_file("../../evil.exe", options).unwrap();
            zip.write_all(b"no").unwrap();
            zip.finish().unwrap();
        }
        extract(&archive, &dir, &["ffmpeg.exe", "ffprobe.exe"]).unwrap();
        assert_eq!(std::fs::read(dir.join("ffmpeg.exe")).unwrap(), b"ffmpeg");
        assert_eq!(std::fs::read(dir.join("ffprobe.exe")).unwrap(), b"ffprobe");
        assert!(!dir.join("evil.exe").exists());
        assert!(extract(&archive, &dir, &["deno.exe"]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
