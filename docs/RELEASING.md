# Releasing

Version in `Cargo.toml`, notes in `docs/releases/v<version>.md`, then a
`v<version>` tag. [`release.yml`](../.github/workflows/release.yml) does the
rest: it tests, builds and Authenticode-signs the installer, publishes the
GitHub release with its checksum and its update manifest, and copies the
whole release to R2.

```powershell
cargo set-version 0.2.0          # or edit Cargo.toml, then: cargo check
# write docs/releases/v0.2.0.md
git commit -am "release: v0.2.0"
git tag -s v0.2.0 -m "edownloader v0.2.0"
git push origin main v0.2.0
```

## What a release contains

| File | What it is |
| --- | --- |
| `edownloader_<version>_x64-setup.exe` | per-user NSIS installer (cargo-packager), signed |
| `edownloader_<version>_amd64.deb`, `_arm64.deb` | Debian/Ubuntu packages (cargo-packager), depend on aria2 |
| `edownloader-<version>-1.x86_64.rpm`, `.aarch64.rpm` | Fedora/openSUSE packages (cargo-generate-rpm), require aria2 |
| `edownloader_<version>_x86_64.AppImage`, `_aarch64.AppImage` | portable Linux builds (cargo-packager + linuxdeploy), self-updating |
| `SHA256SUMS.txt` | the SHA-256 of every package |
| `latest.json` | the update manifest: the Windows installer's and the AppImages' addresses and minisign signatures |

Linux packages are built on Ubuntu 22.04, natively for each architecture
(`ubuntu-22.04-arm` for arm64), so they run on any distribution with glibc
2.35 or newer. Building locally on a newer distribution: linuxdeploy's own
`strip` cannot read recent libraries (`.relr.dyn`), so run cargo-packager
with `NO_STRIP=1`.

The packages do **not** carry yt-dlp, aria2, ffmpeg or Deno: the app
offers to install them at first launch (`src/core/setup.rs`), from each
project's own releases, checksums verified.

## Repository secrets

| Secret | What it is |
| --- | --- |
| `EDOWNLOADER_UPDATE_KEY` | the release key: the rsign secret key file, without a password. Its public half is `PUBLIC_KEY` in `src/core/update.rs` |
| `SIGNTOOL_PFX_BASE64`, `SIGNTOOL_PFX_PASSWORD` | the Authenticode certificate, a PFX in base64 and its password |
| `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` | an R2 API token with Object Read & Write on the bucket |
| `R2_ACCOUNT_ID`, `R2_BUCKET` | the Cloudflare account id (or `https://<id>.r2.cloudflarestorage.com`) and the bucket's name |
| `GITLAB_DEPLOY_KEY` | private half of a GitLab deploy key with write access to the backup project (mirror-gitlab.yml) |

The `R2_*` secrets are optional: with none of them the `Copy to R2` job ends
green after saying so; with only some of them it fails, alone, after the
GitHub release is out. `GITLAB_DEPLOY_KEY` is optional the same way. A
repository **variable** `R2_PUBLIC_URL` is needed only if the bucket's domain
is not `https://download.waveflow.app`, and then the address in
`src/core/update.rs` changes with it, in a release that ships first.

### The release key (updates)

Generated once with rsign and kept in `secrets/` (ignored by git):

```powershell
rsign generate -W -p secrets/edownloader-update.pub -s secrets/edownloader-update.key
gh secret set EDOWNLOADER_UPDATE_KEY < secrets/edownloader-update.key
```

Every installed copy trusts only that public key: **losing the secret key
means no copy can ever update itself again**, and they would have to be
reinstalled by hand. Keep a backup of `secrets/edownloader-update.key` off
this machine. Never regenerate it for a project that has shipped.

### The signing certificate (Authenticode)

Self-signed, as WaveFlow, QuickPlay and Resona are. Two ways:

- **Reuse the existing certificate** (recommended: one publisher identity for
  every app): set the same two secrets as the WaveFlow repository.
- **Make one for edownloader**:

  ```powershell
  pwsh scripts/release/new-signing-cert.ps1 -Subject "CN=InstaZDLL"
  ```

  It writes the PFX and its base64 form to `secrets/` and prints the
  password and the `gh secret set` commands.

A self-signed certificate makes tampering visible but does not make Windows
trust the publisher: SmartScreen warns until the file has built a
reputation, or until a certificate from a CA in Microsoft's Trusted Root
Program (or Azure Trusted Signing, or SignPath for open source) takes the
same two secrets. Nothing else changes.

## The R2 copy

Every installed copy finds its updates at the addresses compiled into it
(`MANIFEST_URLS` in `src/core/update.rs`): the GitHub release, then a copy on
the R2 bucket under the project's own domain. The copy is there from the
first release so that losing the GitHub account never strands an installed
copy:

```text
https://download.waveflow.app/edownloader/
├── releases/<tag>/…    every file of the release, same names, same bytes
└── latest.json         the manifest of the latest release, pointing there
```

The signatures cover the installer's bytes, its version and its file name,
not the address it came from, so the app verifies an update from either host
under its one key.

The `Copy to R2` job runs [`publish-r2.sh`](../scripts/release/publish-r2.sh):
it downloads the release back from GitHub, uploads every file under
`edownloader/releases/<tag>/`, checks each one through the public domain,
refuses to move `latest.json` backwards (`R2_FORCE=1` overrides, for a
deliberate rollback), and uploads the manifest last, `no-cache`. A
pre-release tag has its files copied, never its manifest.

Without credentials or network:

```bash
R2_DRY_RUN=1 TAG_NAME=v0.1.0 bash scripts/release/publish-r2.sh <files dir> <files dir>/latest.json
```

## The GitLab backup

[`mirror-gitlab.yml`](../.github/workflows/mirror-gitlab.yml) pushes `main`
and every `v*` tag to `gitlab.com/waveflow-groupe/edownloader` as they land
(override with the repository variable `GITLAB_REPOSITORY`), with `ci.skip`.
Setup, once:

1. Create the private project on GitLab and disable its CI/CD until the
   switch (Settings › General › Visibility).
2. Make a deploy key: `ssh-keygen -t ed25519 -f secrets/gitlab-deploy -N ""`,
   add `secrets/gitlab-deploy.pub` to the project with **write access**
   (Settings › Repository › Deploy keys), and
   `gh secret set GITLAB_DEPLOY_KEY < secrets/gitlab-deploy`.

## Switching to GitLab

[`.gitlab-ci.yml`](../.gitlab-ci.yml) is a standby copy of `ci.yml` and
`release.yml`, off until the project variable `EDOWNLOADER_GITLAB_CI` is
`on`. On GitLab, R2 is the only host: the manifest points there directly,
and the GitLab release holds links to the files.

Variables (Settings › CI/CD › Variables), **protected**, with the `v*` tags
protected too, and each secret scoped to the one environment that needs it,
so no job that compiles ever sees one:

| Variable | Environment scope |
| --- | --- |
| `EDOWNLOADER_UPDATE_KEY` | `release-manifest` |
| `SIGNTOOL_PFX_BASE64`, `SIGNTOOL_PFX_PASSWORD` | `release-signing` |
| `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, `R2_ACCOUNT_ID`, `R2_BUCKET` | `release-publish` (**required** there) |
| `R2_PUBLIC_URL` | `*`, only if the domain differs |
| `EDOWNLOADER_GITLAB_CI` | `*`, **not protected**; `on` turns the pipelines on |

Then, in order: set `EDOWNLOADER_GITLAB_CI` to `on` last; release as usual
with the tag pushed to GitLab; point the README's download links at
`https://download.waveflow.app/edownloader/releases/<tag>/`, and
`RELEASE_PAGE` in `src/core/update.rs` at the GitLab releases page, in the
next release.
