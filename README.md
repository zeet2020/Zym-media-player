# Zym Media Player

A lightweight media player for Linux and Windows, written in Rust. The UI is built with
GPUI; playback is handled by libmpv, so it plays anything mpv plays.

## Features

- Playlist: add files, folders, `.m3u` playlists and URLs (playlist "+" popup or `Ctrl+L`), or drag and drop
- Right-click menu for playback, loop, chapters, tracks and more
- Quick-settings popover: speed, subtitles, audio, video, picture, sync
- Explorer for sandboxed content plugins, bundled: IPTV (M3U), WebDAV, FTP / SFTP, Stremio addons
  (only direct/debrid stream URLs play; torrents are not supported)
- Several connections per plugin (e.g. two WebDAV servers), switchable from a dropdown
- Appearance: dark / light / system theme with accent colours
- Music visualizer for audio files (right-click › Visualizer): bars, LED, wave, circle, dots, horizon, stars — reacting to kicks, snares, tempo and drops
- Scripts: any mpv Lua/JS script

## Build (Ubuntu)

Needs Rust 1.93+, `libmpv-dev`, and GPUI's usual X11/Wayland/Vulkan development packages.

```sh
sudo apt install libmpv-dev
cargo build --release
./target/release/zym-media-player [files...]
```

`zym-media-player --help` lists the options.

## Content plugins

Content plugins are sandboxed WebAssembly modules that feed content (catalogs, channels, search)
into the Explorer. Build and install one with:

```sh
rustup target add wasm32-unknown-unknown
sources/install.sh sources/m3u
```

They are installed to `~/.config/zym-player/sources/`. The API is described in [docs/plugins.md](docs/plugins.md).

## Scripts

Drop mpv `.lua` or `.js` scripts into `~/.config/zym-player/plugins/`
(Windows: `%APPDATA%\zym-player\plugins\`). See `plugins/now-playing.lua` for an example.

## Keyboard shortcuts

| Key | Action |
|---|---|
| Space / K | Play / pause |
| Left / Right | Seek 5 s |
| J / L | Seek 10 s |
| Up / Down | Volume +/- 5 |
| M | Mute |
| F | Fullscreen |
| . / , | Frame step forward / back |
| N / B | Next / previous in playlist |
| P | Playlist panel |
| E | Explorer |
| S | Quick settings |
| Ctrl+O | Open file |
| Ctrl+L | Open URL |
| Esc | Close menu / popover / Explorer, leave fullscreen |
| Q | Quit |

Other keys are passed to mpv, so script key bindings work.

## Debugging

Run with `ZYM_DEBUG=1` for verbose mpv logging and a UI-stall watchdog.

## License

GPL-3.0-or-later (see `LICENSE`). Third-party crate licenses are listed in `THIRD-PARTY-LICENSES.md`
(regenerate with `packaging/gen-licenses.sh`). Windows builds bundle libmpv (GPL); see
`packaging/windows/LIBMPV-NOTICE.txt`.
