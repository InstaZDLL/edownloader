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
  main.rs            window, key bindings, fonts, entry point
  i18n.rs            interface languages: t!/tn!, plural rules, system language
  app.rs             state: tasks, cookie store, settings, tools, updates, event pump
  views.rs           rendering: title bar, banners, tabs, task list, cookie manager, settings
  theme.rs           Catppuccin palettes, type scale, runtime theme selection
  ui/
    text_input.rs    single-line field with IME via EntityInputHandler
    widgets.rs       buttons, chips, segments, progress track, rows
    frame.rs         resizable window edges where the app draws its own frame (Linux)
  core/
    aria2.rs         daemon + JSON-RPC over a loopback socket (no HTTP crate)
    engine.rs        job orchestration, yt-dlp supervision, progress parsing
    gallery.rs       the sites read natively: listing → files fetched 3 at a time, or back to yt-dlp
    pinterest.rs     Pinterest pins, boards and sections through the website's API
    twitter.rs       X posts through the syndication API (photos; videos alongside)
    instagram.rs     Instagram posts read by yt-dlp -J, photos fetched here
    tiktok.rs        TikTok photo posts from the page's rehydration data
    bilibili.rs      Bilibili videos and parts: view + playurl API, DASH muxed by ffmpeg
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

## Linux

The same code, with what differs gathered in a few places:

- **Window**: the app asks for client-side decorations (`main.rs`) and
  draws its own title bar, as on Windows; `ui/frame.rs` gives back what a
  system frame does, a band along each edge that resizes the window, with
  the matching cursor. A double click on the title bar maximizes and
  restores; a right click opens the window menu.
- **Fonts**: Inter and JetBrains Mono are compiled in (`assets/fonts`, SIL
  OFL) and registered at start (`theme::load_fonts`); scripts they lack
  (CJK) fall back to the system's fonts. Windows keeps Segoe UI and Consolas.
- **File picker**: GPUI's, which is the XDG desktop portal: the desktop's
  own chooser, sandboxed or not.
- **Language**: `LANGUAGE`, then `LC_ALL`, `LC_MESSAGES`, `LANG`.
- **Build**: GPUI's tar dependency pins `xattr` 0.2.3, which reads
  `libc::ENOATTR`, gone from libc 0.2.190 on Linux; `patches/xattr` is that
  crate with the one line changed (`[patch.crates-io]` in Cargo.toml).

## Languages

Every string the interface shows is a key in `locales/<code>.json`, compiled
into the binary: English (`en`, the reference), French, German, Spanish,
Italian, Brazilian Portuguese, Japanese and Simplified Chinese. Code asks for
`t!("key")`, or `t!("key", name = value)` to fill `{name}`; counts go through
`tn!("key", n)`, which picks `key.one` or `key.other` by the language's rule
(French and Portuguese put 0 with 1, Japanese and Chinese have one form).

The language is Settings › appearance › Language: `system` follows the
Windows display languages (`GetUserPreferredUILanguages`, in the user's
order, not the regional format), the first one translated, else English. It is a process-wide
setting, so the engine's threads word their errors in it too. Technical
diagnostics (network, RPC, update internals) stay in English.

To add or change a string, edit `en.json` and every other file with it: the
tests fail when a translation lacks a key, has one English does not, or
drops a `{placeholder}`, and when the code uses a key `en.json` lacks.

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
  HLS and DASH streams (`.m3u8`, `.mpd`, and most sites) are fetched with
  `--concurrent-fragments` set from the connections setting, capped at 8.
  A `blob:` link is refused up front (it only exists in a browser tab), a
  403 on a copied playlist points at the page's address, and DRM is named.
  Its failures are translated (`engine.rs`): "Sign in to confirm you're not a
  bot" becomes "this needs cookies", storyboard-only answers become "no
  JavaScript runtime".
- **Containers** are decided by codecs: `mp4` asks for H.264/AV1 + AAC (AV1 is
  the only MP4-compatible codec YouTube offers in 4K), `native` keeps VP9/Opus
  in WebM. Every media preset merges `bv*` with `ba`, so resolutions above
  1080p, served as video-only DASH, still come out with sound.
- **Sites read natively** (`gallery.rs`): Pinterest, X, Instagram, TikTok
  and Bilibili links of the shapes their module understands
  (`gallery::Site::of`) go to a runner of their own; every other link on
  these sites still goes to yt-dlp. A site module turns the link into a
  `Listing` (a title, an optional folder, the files), and `gallery::run`
  fetches the files three at a time: plain files streamed through `.part`,
  HLS through ffmpeg, separate video and audio (DASH) fetched then muxed by
  ffmpeg with `-c copy`. Each name ends with `[key]`, an identifier the
  site gives the file; a file already on disk with the same key and
  extension is skipped, so a second run or a retry only fetches what is
  missing. A module can answer `Listed::Delegate` instead: the link holds
  nothing yt-dlp would miss, and `Engine::run_site` hands the job to
  yt-dlp, presets and all. The row counts files when there are several
  (`Event::Items`), and shows the post's or board's name (`Event::Title`).
  - **X** (`twitter.rs`): yt-dlp stops at "No video could be found" on a
    photo post. A post (`/status/<id>`) is read from the syndication API
    X's embeds use (`cdn.syndication.twimg.com/tweet-result`), which needs
    no account: photos as `?name=orig`, videos and GIFs of a mixed post as
    their highest-bitrate MP4. Video-only posts, the Audio preset, and
    protected posts when the jar holds X cookies go to yt-dlp.
  - **Instagram** (`instagram.rs`): Instagram answers anonymous visitors
    only over a browser's TLS handshake, which yt-dlp imitates (curl_cffi)
    and ureq cannot. So `yt-dlp -J --ignore-no-formats-error` reads the
    post (`/p/`, `/tv/`), with the jar's cookies; the photos it lists as
    thumbnails (largest last) and the whole (non-`dash-`) MP4 of the videos
    of a mixed carousel are fetched here. Reels and video-only posts go to
    yt-dlp.
  - **TikTok** (`tiktok.rs`): yt-dlp refuses `/photo/` links. The post's
    data comes from `__UNIVERSAL_DATA_FOR_REHYDRATION__` in the page at
    `/@i/video/<id>` (the `/photo/` page leaves it to its scripts): a photo
    post gives its pictures, numbered in order, and its sound as MP3 (the
    sound alone with the Audio preset). Videos go to yt-dlp. Share links
    (`vm.`, `vt.`, `/t/`) are followed first.
  - **Bilibili** (`bilibili.rs`): its pages answer yt-dlp with HTTP 412,
    its API does not, given a `buvid3` cookie (from the jar, else from
    `x/frontend/finger/spi`). `x/web-interface/view` gives the parts,
    `x/player/playurl` (`fnval=4048`) each part's DASH streams: the tallest
    video within the preset's cap, H.264 first in MP4 mode and AV1/HEVC
    first in native mode, with the best audio; the audio stream alone for
    the Audio preset. `?p=N` picks a part; otherwise the playlist toggle
    decides between the first part and all of them, in a folder. Without
    an account Bilibili serves up to 480p.
- **Pinterest** (`pinterest.rs`): yt-dlp only extracts Pinterest videos,
  and fails on image pins and on any board holding one.
  Every Pinterest host (`pinterest.fr`, `fr.pinterest.com`…) and `pin.it`
  are recognised. The runner calls the JSON API the website uses
  (`/resource/<Name>Resource/get/`, with the `x-pinterest-pws-handler`
  header it requires), anonymously, or with the pinterest.com cookies of the
  active jar for secret boards. A board is listed page by page through
  bookmarks, 25 pins a page, a short pause between pages; rate limits wait
  for `Retry-After`. Each pin gives its original picture, its MP4 (`V_720P`
  is the original upload) or its HLS stream through ffmpeg, every page of a
  story pin and every slot of a carousel. A board lands in a folder named
  after it; the key is the start of the CDN file name, which the same
  picture keeps on every board.

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

On Linux (x86_64 and arm64) the same projects' Linux builds are used:
`yt-dlp_linux` / `yt-dlp_linux_aarch64`, the `linux64` / `linuxarm64` GPL
`.tar.xz` of FFmpeg-Builds (unpacked by the system's `tar`, only `ffmpeg` and
`ffprobe`), and Deno's `unknown-linux-gnu` zip, with the same checksums.
aria2 publishes no Linux build: it comes from the distribution (a dependency
of the .deb and .rpm), and the banner says so when it is missing. Everything
placed is made executable.

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

On Linux the manifest's `linux-x86_64` / `linux-aarch64` entries are the
AppImages. A copy run from an AppImage (`$APPIMAGE`) in a folder it may write
to replaces that file: the verified AppImage is copied beside it, then
renamed over it, so it is never seen half-written and the running copy keeps
its open file. A copy from a .deb or .rpm only says that a version is out:
the package manager installs it.

## Data

| What | Where |
| --- | --- |
| settings, `cookies.json`, `active-cookies.txt` | `%APPDATA%\edownloader`, `~/.local/share/edownloader` |
| installed tools, pending updates | `%LOCALAPPDATA%\edownloader`, `~/.cache/edownloader` |
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
