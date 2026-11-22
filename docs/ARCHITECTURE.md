# Architecture

edownloader is one Rust binary drawn with [GPUI](https://crates.io/crates/gpui),
the renderer behind Zed. It does not download anything itself: it drives
**aria2** (direct links, magnets) and **yt-dlp** (media sites, with **ffmpeg**
for merging and **Deno** or another JavaScript runtime for YouTube's player
challenge), and makes their state readable.

Everything blocking runs on OS threads owned by the engine; the UI thread only
reads an event channel every 120 ms, so rendering never waits on a subprocess
or a socket.

## Layout

```text
src/
  main.rs            window, key bindings, entry point
  app.rs             state: tasks, cookie store, settings, tools, updates, event pump
  views.rs           rendering: title bar, banners, tabs, task list, cookie manager, settings
  theme.rs           Catppuccin palettes, type scale, runtime theme selection
  ui/
    text_input.rs    single-line field with IME via EntityInputHandler
    widgets.rs       buttons, chips, segments, progress track, rows
  core/
    aria2.rs         daemon + JSON-RPC over a loopback socket (no HTTP crate)
    engine.rs        job orchestration, yt-dlp supervision, progress parsing
    cookies.rs       formats, store, domain matching, sign-in analysis
    settings.rs      persisted preferences
    tools.rs         binary discovery
    setup.rs         tool installer: official releases, checksums, extraction
    update.rs        app updates: manifest, minisign verification, installer
    net.rs           the app's own HTTPS (ureq + rustls), checksum lists
    urlx.rs          link normalization and routing
    human.rs         size/speed/ETA/expiry formatting
    paths.rs         data directories, file picker, reveal
```

## Routing

The link is classified up front (`urlx.rs`): a magnet goes to aria2's
BitTorrent engine; a URL whose file name has a download extension (`.zip`,
`.exe`, `.pdf`, `.mp4`…) goes to aria2; anything else, a page on any host,
goes to yt-dlp. When yt-dlp answers "Unsupported URL" for a host the app has
no label for, the job is handed to aria2 as a plain file. Known sites
(YouTube, Vimeo, SoundCloud…) get a label and always go to yt-dlp;
`youtube.com` and `notyoutube.com` are told apart by host, not substring.

- **aria2** runs as a daemon driven over JSON-RPC (`aria2.tellStatus`), not by
  scraping a log, so byte counts, speed and connections are real. Jobs pause
  and resume in place; cancel removes the download.
- **yt-dlp** runs once per job with a machine-readable `--progress-template`.
  Its failures are translated (`engine.rs`): "Sign in to confirm you're not a
  bot" becomes "this needs cookies", storyboard-only answers become "no
  JavaScript runtime".
- **Containers** are decided by codecs: `mp4` asks for H.264/AV1 + AAC (AV1 is
  the only MP4-compatible codec YouTube offers in 4K), `native` keeps VP9/Opus
  in WebM. Every media preset merges `bv*` with `ba`, so resolutions above
  1080p, served as video-only DASH, still come out with sound.

## Cookies

Jars are stored in `cookies.json` and the active one is materialised as a
Netscape file handed to yt-dlp with `--cookies`. Import accepts Netscape
`cookies.txt`, cookie-editor JSON (array, single object or `{"cookies":[…]}`,
BOM, string expiries and `null` tolerated) or a raw `Cookie:` header, and
yt-dlp's `--cookies-from-browser`. Each jar gets a YouTube verdict from the
cookies it holds (`SID`/`__Secure-*PSID` with `LOGIN_INFO`). Values are never
rendered.

## Tools

`tools.rs` looks for each binary, in order: the app's own tools folder
(`%LOCALAPPDATA%\edownloader\tools`), `bin/` next to the executable and its
parents, the crate's `bin/` (debug builds only), then `PATH`. The first
matching JavaScript runtime among Deno, Node, QuickJS and Bun is passed to
yt-dlp as `--js-runtimes`.

`setup.rs` installs what is missing into the tools folder:

| Tool | Source | Verified with |
| --- | --- | --- |
| yt-dlp | `yt-dlp/yt-dlp` latest release, `yt-dlp.exe` | `SHA2-256SUMS` |
| aria2 | `aria2/aria2` release 1.37.0, win-64bit zip | HTTPS only (no checksum published) |
| ffmpeg + ffprobe | `yt-dlp/FFmpeg-Builds` latest, win64 GPL zip | `checksums.sha256` |
| Deno | `denoland/deno` latest, x86_64 Windows zip | `.zip.sha256sum` |

Only the named executables are taken out of an archive, by file name, so an
entry path such as `../../x` is never written. A file replaced while it runs
is reported as "in use" rather than half-written.

## Updates

`update.rs` reads `latest.json` from the GitHub release and from its copy on
R2 (docs/RELEASING.md). For the newest version above the running one, it
downloads the installer into `%LOCALAPPDATA%\edownloader\updates` and checks
its minisign signature against the release key compiled into the app. The
signature's trusted comment is `edownloader <version> <file>`, so the
unsigned manifest cannot pass an older installer off as a newer version.

The verified installer then runs either from the banner (`/P /NS /R`: a
progress window, then the new version starts) or silently when the app
closes (`/S /NS`). The NSIS installer ends a running copy before it replaces
the files. A copy that was not installed by the installer (no
`uninstall.exe` beside it) only says that a version is out.

## Data

| What | Where |
| --- | --- |
| settings, `cookies.json`, `active-cookies.txt` | `%APPDATA%\edownloader` |
| installed tools, pending updates | `%LOCALAPPDATA%\edownloader` |
| downloads | `~/Downloads` by default |

## Tests

```powershell
cargo test
```

The unit tests cover the parts that are easy to get subtly wrong: URL
classification, cookie parsing and round-tripping, domain matching, the
YouTube verdict, yt-dlp progress lines, error translation, formatting,
settings clamping, checksum lists in every shape the tools publish, archive
extraction, and update signatures (a changed byte, another key, another
version or another file name must all fail).

The aria2 daemon test starts a real process and is opt-in:

```powershell
cargo test -- --ignored
```
