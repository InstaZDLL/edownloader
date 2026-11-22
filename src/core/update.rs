//! Updates of the app itself.
//!
//! At launch, unless switched off in Settings, the app reads the manifest of
//! the latest release (`latest.json`) from two hosts: the GitHub release, and
//! its copy on the R2 bucket (scripts/release/publish-r2.sh), which keeps
//! working if the GitHub account does not. When either offers a newer
//! version, its installer is downloaded in the background and checked
//! against the release key (minisign). The user then restarts into it from
//! the banner, or it installs silently when the app closes.
//!
//! The manifest is not trusted for the version: each installer's signature
//! names the version and the file it was signed for, so neither host, nor
//! anyone answering in its place, can pass an older installer off as a newer
//! version. A version no installer backs is passed over for the next newest.
//!
//! Only the installer's copy updates itself. A portable copy (an executable
//! run from anywhere else) only hears that a version is out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::core::net;
use crate::core::paths;

/// Asked in this order. A host added here reaches only the versions built
/// after it, so both are there from the first release.
const MANIFEST_URLS: [&str; 2] = [
    "https://github.com/InstaZDLL/edownloader/releases/latest/download/latest.json",
    "https://download.waveflow.app/edownloader/latest.json",
];
/// The release key: its secret half signs the installers in the release
/// workflow (`EDOWNLOADER_UPDATE_KEY`, docs/RELEASING.md).
const PUBLIC_KEY: &str = "RWTg+b5e/g96GQAO1MoTKyOMe/Q9TpVbY5KKotdzU8Om9LZoiqP6Esh3";
/// A release's page, before its version.
pub const RELEASE_PAGE: &str = "https://github.com/InstaZDLL/edownloader/releases/tag/v";
/// The manifest key of the package this build installs.
const PLATFORM: &str = "windows-x86_64";
/// No installer comes near this, nor a manifest near the other.
const MAX_PACKAGE: u64 = 64 << 20;
const MAX_MANIFEST: u64 = 1 << 20;
/// Replaces both hosts with one manifest, in any build: how an update is
/// tried before it is published.
const TEST_URL_VAR: &str = "EDOWNLOADER_UPDATE_URL";

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    platforms: HashMap<String, Package>,
}

#[derive(Debug, Deserialize)]
struct Package {
    url: String,
    /// The minisign signature, whose trusted comment is
    /// `edownloader <version> <file name>`.
    signature: String,
}

/// What a check found.
#[derive(Debug)]
pub enum Found {
    /// Downloaded and verified, ready to install.
    Ready(Update),
    /// A newer version this copy cannot install by itself.
    Available(String),
}

/// A verified installer.
#[derive(Debug, Clone)]
pub struct Update {
    pub version: String,
    file: PathBuf,
}

/// Whether this copy was put there by the installer, which leaves its
/// uninstaller beside the executable. Only that copy can be replaced.
pub fn installed() -> bool {
    std::env::current_exe()
        .map(|exe| exe.with_file_name("uninstall.exe").is_file())
        .unwrap_or(false)
}

/// Whether to look at all: release builds do; a debug build only when it is
/// pointed at a test manifest.
pub fn wanted() -> bool {
    !cfg!(debug_assertions) || std::env::var_os(TEST_URL_VAR).is_some()
}

/// Looks for a newer version and, when this copy can install it, downloads
/// and verifies the installer. Blocking: run it on a thread of its own.
pub fn check() -> Result<Option<Found>, String> {
    let urls: Vec<String> = match std::env::var(TEST_URL_VAR) {
        Ok(url) => vec![url],
        Err(_) => MANIFEST_URLS.iter().map(|u| u.to_string()).collect(),
    };
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| format!("this build has no valid version: {error}"))?;
    check_at(
        &urls,
        PUBLIC_KEY,
        &current,
        installed(),
        &paths::updates_dir(),
    )
}

fn check_at(
    urls: &[String],
    key: &str,
    current: &semver::Version,
    can_install: bool,
    dir: &Path,
) -> Result<Option<Found>, String> {
    let agent = net::agent();
    // Each host on its own: one that fails, whatever the way, hides nothing
    // the other offers. Only all of them failing is an error.
    let mut manifests = Vec::new();
    let mut last_error = None;
    for url in urls {
        match read_manifest(&agent, url) {
            Ok(manifest) => manifests.push(manifest),
            Err(error) => last_error = Some(error),
        }
    }
    if manifests.is_empty() {
        return Err(last_error.unwrap_or_else(|| "no update host".into()));
    }
    let offers = newer(manifests, current);
    let Some((newest, _)) = offers.first() else {
        // The installer of the version now running goes.
        clear(dir, &[]);
        return Ok(None);
    };
    let candidates: Vec<(&str, &Package)> = if can_install {
        offers
            .iter()
            .flat_map(|(v, sources)| {
                sources
                    .iter()
                    .filter_map(move |m| Some((v.as_str(), m.platforms.get(PLATFORM)?)))
            })
            .collect()
    } else {
        Vec::new()
    };
    if candidates.is_empty() {
        return Ok(Some(Found::Available(newest.clone())));
    }
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let keep: Vec<PathBuf> = candidates
        .iter()
        .map(|(_, p)| net::file_of(&p.url))
        .filter(|name| net::is_plain_file_name(name))
        .map(|name| dir.join(name))
        .collect();
    clear(dir, &keep);
    // The first installer that downloads and verifies wins; a version that
    // no installer backs is passed over for the next one.
    let mut last_error = None;
    for (version, package) in candidates {
        match fetch(&agent, key, dir, package, version) {
            Ok(file) => {
                return Ok(Some(Found::Ready(Update {
                    version: version.to_string(),
                    file,
                })))
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| "no installer".into()))
}

fn read_manifest(agent: &ureq::Agent, url: &str) -> Result<Manifest, String> {
    let text = net::get_text(agent, url, MAX_MANIFEST)?;
    let manifest: Manifest = serde_json::from_str(&text)
        .map_err(|error| format!("{} sent an invalid manifest: {error}", net::host_of(url)))?;
    semver::Version::parse(&manifest.version)
        .map_err(|error| format!("{} sent an invalid version: {error}", net::host_of(url)))?;
    Ok(manifest)
}

/// The versions newer than `current`, newest first, each with every manifest
/// offering it in the order the hosts were asked.
fn newer(manifests: Vec<Manifest>, current: &semver::Version) -> Vec<(String, Vec<Manifest>)> {
    let mut offers: Vec<(semver::Version, Vec<Manifest>)> = Vec::new();
    for manifest in manifests {
        let Ok(version) = semver::Version::parse(&manifest.version) else {
            continue;
        };
        if version <= *current {
            continue;
        }
        match offers.iter_mut().find(|(v, _)| *v == version) {
            Some((_, sources)) => sources.push(manifest),
            None => offers.push((version, vec![manifest])),
        }
    }
    offers.sort_by(|(a, _), (b, _)| b.cmp(a));
    offers
        .into_iter()
        .map(|(v, sources)| (v.to_string(), sources))
        .collect()
}

/// The installer in the cache, verified: left by an earlier launch, or
/// downloaded now.
fn fetch(
    agent: &ureq::Agent,
    key: &str,
    dir: &Path,
    package: &Package,
    version: &str,
) -> Result<PathBuf, String> {
    let name = net::file_of(&package.url);
    if !net::is_plain_file_name(name) || !name.ends_with(".exe") {
        return Err(format!("{} is not an installer name", package.url));
    }
    let file = dir.join(name);
    let cached = std::fs::read(&file)
        .ok()
        .filter(|bytes| verify(key, bytes, &package.signature, version, name).is_ok());
    if cached.is_none() {
        let partial = dir.join(format!("{name}.part"));
        net::download_to(agent, &package.url, &partial, MAX_PACKAGE, &mut |_, _| {})?;
        let bytes = std::fs::read(&partial)
            .map_err(|error| format!("cannot read the download: {error}"))?;
        if let Err(error) = verify(key, &bytes, &package.signature, version, name) {
            let _ = std::fs::remove_file(&partial);
            return Err(error);
        }
        std::fs::rename(&partial, &file)
            .map_err(|error| format!("cannot keep the installer: {error}"))?;
    }
    Ok(file)
}

/// `bytes` are what the release key signed for `version`, under `name`.
fn verify(
    key: &str,
    bytes: &[u8],
    signature: &str,
    version: &str,
    name: &str,
) -> Result<(), String> {
    let key = minisign_verify::PublicKey::from_base64(key)
        .map_err(|error| format!("bad release key: {error}"))?;
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|error| format!("bad signature: {error}"))?;
    key.verify(bytes, &signature, false)
        .map_err(|_| format!("{name} is not signed by the release key"))?;
    let expected = format!("edownloader {version} {name}");
    if signature.trusted_comment() != expected {
        return Err(format!(
            "{name} was signed as \"{}\", not \"{expected}\"",
            signature.trusted_comment()
        ));
    }
    Ok(())
}

/// Installers of versions passed over or installed, and partial downloads,
/// go; `keep` stays.
fn clear(dir: &Path, keep: &[PathBuf]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !keep.contains(&path) && path.is_file() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Runs the installer. `restart`: a passive install with a progress window
/// that starts the new version when it is done, for "Restart to update";
/// otherwise silent, as the app closes. The installer itself ends a copy
/// still running before it replaces the files; `/NS` keeps it from bringing
/// back shortcuts the user removed.
pub fn install(update: &Update, restart: bool) -> std::io::Result<()> {
    let args: &[&str] = if restart {
        &["/P", "/NS", "/R"]
    } else {
        &["/S", "/NS"]
    };
    std::process::Command::new(&update.file)
        .args(args)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key made for these tests only, and a package it signed.
    const TEST_KEY: &str = "RWTpeB+/amfO4nhCDUFXPj39xUgkcWs4jm/RCCNxzOUNaH8O489q6Qgn";
    const TEST_PACKAGE: &[u8] = b"edownloader update test package\n";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from rsign secret key
RUTpeB+/amfO4s+DJwcA9d2DQlIx7uC9N1ww2veVXJ+KJmphDLHwUPeUr+Ouu7Dag76BzIht7G1hfPzbb8N9p9+yUW6lxEGPmQg=
trusted comment: edownloader 1.2.3 test.bin
T0NTt9JKdRSUtfv0iXmHTMq+tf3vdl2Y9WA9cOosNF/MwWS6BpzLpUlOrWxrSF5uN3TE+qnGbzMZDLZ442TMBA==
";

    #[test]
    fn a_signed_package_passes() {
        verify(TEST_KEY, TEST_PACKAGE, TEST_SIGNATURE, "1.2.3", "test.bin").unwrap();
    }

    #[test]
    fn a_changed_package_fails() {
        let mut changed = TEST_PACKAGE.to_vec();
        changed[0] ^= 1;
        assert!(verify(TEST_KEY, &changed, TEST_SIGNATURE, "1.2.3", "test.bin").is_err());
    }

    #[test]
    fn the_release_key_does_not_accept_the_test_key() {
        assert!(verify(
            PUBLIC_KEY,
            TEST_PACKAGE,
            TEST_SIGNATURE,
            "1.2.3",
            "test.bin"
        )
        .is_err());
    }

    /// The signature names the version and the file: a genuine package
    /// cannot be offered as another version or under another name.
    #[test]
    fn version_and_name_are_bound_to_the_signature() {
        assert!(verify(TEST_KEY, TEST_PACKAGE, TEST_SIGNATURE, "9.9.9", "test.bin").is_err());
        assert!(verify(TEST_KEY, TEST_PACKAGE, TEST_SIGNATURE, "1.2.3", "other.exe").is_err());
    }

    #[test]
    fn the_release_key_parses() {
        minisign_verify::PublicKey::from_base64(PUBLIC_KEY).unwrap();
    }

    fn manifest(version: &str) -> Manifest {
        Manifest {
            version: version.into(),
            platforms: HashMap::new(),
        }
    }

    #[test]
    fn only_newer_versions_are_offered_newest_first() {
        let current = semver::Version::parse("1.2.0").unwrap();
        let offers = newer(
            vec![
                manifest("1.1.0"),
                manifest("1.3.0"),
                manifest("1.2.0"),
                manifest("1.10.0"),
                manifest("1.3.0"),
            ],
            &current,
        );
        let versions: Vec<&str> = offers.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(versions, ["1.10.0", "1.3.0"]);
        assert_eq!(offers[1].1.len(), 2);
    }

    #[test]
    fn this_version_is_valid_semver() {
        semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    }
}
