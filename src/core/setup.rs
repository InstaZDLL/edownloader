//! The tool installer: fetches yt-dlp, aria2c, ffmpeg and Deno from their
//! projects' own releases into the app's tools folder
//! (`%LOCALAPPDATA%\edownloader\tools`, `~/.cache/edownloader/tools`).
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
//!
//! Linux (x86_64 and arm64) gets the same projects' Linux builds, except
//! aria2, which publishes none: it comes from the distribution's package
//! (a dependency of the .deb and .rpm), and the app says so when it is
//! missing.

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
    packing: Packing,
    /// No genuine release comes near this; a larger answer is not one.
    max: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Packing {
    /// The download is the executable.
    Plain,
    Zip,
    /// `.tar.xz`, unpacked by the system's `tar` (Linux only).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    TarXz,
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
            Component::YtDlp => t!("setup.purpose.ytdlp"),
            Component::Aria2 => t!("setup.purpose.aria2"),
            Component::Ffmpeg => t!("setup.purpose.ffmpeg"),
            Component::Deno => t!("setup.purpose.deno"),
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

    /// Where this build gets the component, if it can install it at all.
    fn source(self) -> Option<Source> {
        sources::get(self)
    }

    /// Whether only the system's package manager provides it here.
    pub fn system_provided(self) -> bool {
        supported() && self.source().is_none()
    }
}

/// Each project's build for this platform. yt-dlp's own FFmpeg builds are
/// static, carry the patches yt-dlp relies on, and are what its
/// documentation recommends.
#[cfg(all(windows, target_arch = "x86_64"))]
mod sources {
    use super::{Component, Packing, Source};

    pub fn get(component: Component) -> Option<Source> {
        Some(match component {
            Component::YtDlp => Source {
                url: "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
                sums: Some("https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS"),
                files: &["yt-dlp.exe"],
                packing: Packing::Plain,
                max: 128 << 20,
            },
            Component::Aria2 => Source {
                url: "https://github.com/aria2/aria2/releases/download/release-1.37.0/aria2-1.37.0-win-64bit-build1.zip",
                sums: None,
                files: &["aria2c.exe"],
                packing: Packing::Zip,
                max: 32 << 20,
            },
            Component::Ffmpeg => Source {
                url: "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip",
                sums: Some("https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/checksums.sha256"),
                files: &["ffmpeg.exe", "ffprobe.exe"],
                packing: Packing::Zip,
                max: 512 << 20,
            },
            Component::Deno => Source {
                url: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip",
                sums: Some("https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip.sha256sum"),
                files: &["deno.exe"],
                packing: Packing::Zip,
                max: 256 << 20,
            },
        })
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod sources {
    use super::{Component, Packing, Source};

    #[cfg(target_arch = "x86_64")]
    const YT_DLP: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux";
    #[cfg(target_arch = "aarch64")]
    const YT_DLP: &str =
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux_aarch64";
    #[cfg(target_arch = "x86_64")]
    const FFMPEG: &str = "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz";
    #[cfg(target_arch = "aarch64")]
    const FFMPEG: &str = "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linuxarm64-gpl.tar.xz";
    #[cfg(target_arch = "x86_64")]
    const DENO: (&str, &str) = (
        "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip",
        "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip.sha256sum",
    );
    #[cfg(target_arch = "aarch64")]
    const DENO: (&str, &str) = (
        "https://github.com/denoland/deno/releases/latest/download/deno-aarch64-unknown-linux-gnu.zip",
        "https://github.com/denoland/deno/releases/latest/download/deno-aarch64-unknown-linux-gnu.zip.sha256sum",
    );

    pub fn get(component: Component) -> Option<Source> {
        Some(match component {
            Component::YtDlp => Source {
                url: YT_DLP,
                sums: Some("https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS"),
                files: &["yt-dlp"],
                packing: Packing::Plain,
                max: 128 << 20,
            },
            // No Linux build is published: the distribution's package.
            Component::Aria2 => return None,
            Component::Ffmpeg => Source {
                url: FFMPEG,
                sums: Some("https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/checksums.sha256"),
                files: &["ffmpeg", "ffprobe"],
                packing: Packing::TarXz,
                max: 512 << 20,
            },
            Component::Deno => Source {
                url: DENO.0,
                sums: Some(DENO.1),
                files: &["deno"],
                packing: Packing::Zip,
                max: 256 << 20,
            },
        })
    }
}

#[cfg(not(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
mod sources {
    use super::{Component, Source};

    pub fn get(_: Component) -> Option<Source> {
        None
    }
}

/// Whether this build can install tools at all. Elsewhere the system's
/// package manager is the way.
pub fn supported() -> bool {
    cfg!(any(
        all(windows, target_arch = "x86_64"),
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ))
}

/// What the app cannot work fully without and can install itself, in
/// install order. Any JavaScript runtime already present (Node, Bun…)
/// stands in for Deno.
pub fn missing(tools: &Tools) -> Vec<Component> {
    absent(tools)
        .into_iter()
        .filter(|component| !component.system_provided())
        .collect()
}

/// What is missing and only the system's package manager provides here.
pub fn missing_from_system(tools: &Tools) -> Vec<Component> {
    absent(tools)
        .into_iter()
        .filter(|component| component.system_provided())
        .collect()
}

fn absent(tools: &Tools) -> Vec<Component> {
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
    let Some(source) = component.source() else {
        return Err(t!("setup.unsupported").into());
    };
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
                    t!("setup.no_checksum", tool = component.label(), file = name)
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
            return Err(t!(
                "setup.checksum_mismatch",
                file = name,
                tool = component.label()
            ));
        }
    }

    if source.packing != Packing::Plain {
        emit(SetupEvent::Progress {
            component,
            stage: "extracting",
            done: 0,
            total: None,
        });
    }
    let result = match source.packing {
        Packing::Plain => place(&download, &dir.join(source.files[0])),
        Packing::Zip => extract(&download, dir, source.files),
        Packing::TarXz => extract_tar_xz(&download, dir, source.files),
    };
    let _ = std::fs::remove_file(&download);
    result
}

/// Puts `from` at `to`, replacing what is there. Windows refuses while the
/// old one runs, which is said plainly.
fn place(from: &Path, to: &Path) -> Result<(), String> {
    // Unix: what is placed is a program. Archives (zip especially) do not
    // always carry the executable bit, and a plain download never does.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(from, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("cannot make {} executable: {error}", from.display()))?;
    }
    std::fs::rename(from, to).map_err(|error| {
        let name = to
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            t!("setup.in_use", file = name)
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

/// `.tar.xz` (Linux): the system's `tar` unpacks only the entries named
/// `wanted`, wherever they sit, into a scratch folder; they are then placed
/// in `dir` like a zip's, and the scratch folder removed. GNU tar refuses
/// absolute paths and `..` in member names.
fn extract_tar_xz(archive: &Path, dir: &Path, wanted: &[&str]) -> Result<(), String> {
    let scratch = dir.join(".unpack");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .map_err(|error| format!("cannot create {}: {error}", scratch.display()))?;
    let result = (|| {
        let mut command = std::process::Command::new("tar");
        command
            .arg("-xJf")
            .arg(archive)
            .arg("-C")
            .arg(&scratch)
            .arg("--wildcards")
            .arg("--no-same-owner");
        for name in wanted {
            command.arg(format!("*/{name}"));
        }
        let output = command
            .output()
            .map_err(|error| format!("cannot run tar: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "tar could not unpack the archive: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        for target in wanted {
            let found = find_file(&scratch, target)
                .ok_or_else(|| format!("{target} is not in the archive"))?;
            place(&found, &dir.join(target))?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// The first regular file named `name` under `root`, depth first.
fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = entry.file_type().ok()?;
        if kind.is_file() && entry.file_name() == name {
            return Some(path);
        }
        if kind.is_dir() {
            dirs.push(path);
        }
    }
    dirs.into_iter().find_map(|dir| find_file(&dir, name))
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
        // Installed by the app or by the system's package manager: together,
        // every component.
        let mut absent = missing(&none);
        absent.extend(missing_from_system(&none));
        for component in Component::ALL {
            assert!(absent.contains(&component), "{component:?}");
        }
        assert_eq!(absent.len(), Component::ALL.len());
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
            let Some(source) = component.source() else {
                assert!(component.system_provided() || !supported(), "{component:?}");
                continue;
            };
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

    /// Downloads, checks and places every component this platform installs,
    /// into a scratch folder, then runs each one. Network-bound and about
    /// 300 MB: `cargo test -- --ignored installs_every_component`.
    #[test]
    #[ignore]
    fn installs_every_component_for_real() {
        let dir = std::env::temp_dir().join(format!("edl-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let agent = net::agent();
        for component in Component::ALL {
            let Some(source) = component.source() else {
                continue;
            };
            install(&agent, &dir, component, &|_| {})
                .unwrap_or_else(|error| panic!("{component:?}: {error}"));
            for file in source.files {
                let path = dir.join(file);
                let flag = if *file == "ffmpeg"
                    || *file == "ffmpeg.exe"
                    || *file == "ffprobe"
                    || *file == "ffprobe.exe"
                {
                    "-version"
                } else {
                    "--version"
                };
                let output = std::process::Command::new(&path).arg(flag).output();
                assert!(
                    output.as_ref().is_ok_and(|out| out.status.success()),
                    "{} does not run: {output:?}",
                    path.display()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shape of yt-dlp's FFmpeg builds: `<name>/bin/ffmpeg`, among
    /// files that must not be written.
    #[cfg(target_os = "linux")]
    #[test]
    fn tar_xz_entries_are_taken_by_name_and_made_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("edl-tar-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let tree = dir.join("src/ffmpeg-master-latest-linux64-gpl/bin");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("ffmpeg"), b"ffmpeg").unwrap();
        std::fs::write(tree.join("ffprobe"), b"ffprobe").unwrap();
        std::fs::write(tree.join("ffplay"), b"no").unwrap();
        let archive = dir.join("a.tar.xz");
        let status = std::process::Command::new("tar")
            .arg("-cJf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.join("src"))
            .arg("ffmpeg-master-latest-linux64-gpl")
            .status()
            .unwrap();
        assert!(status.success());
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        extract_tar_xz(&archive, &out, &["ffmpeg", "ffprobe"]).unwrap();
        assert_eq!(std::fs::read(out.join("ffmpeg")).unwrap(), b"ffmpeg");
        let mode = std::fs::metadata(out.join("ffprobe"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
        assert!(!out.join("ffplay").exists());
        assert!(!out.join(".unpack").exists());
        assert!(extract_tar_xz(&archive, &out, &["deno"]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
