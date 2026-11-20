# Zym Media Player — Content Plugin API (v1)

Status: **draft**, `api: 1`. Machine-readable schema: [`schema/source-v1.schema.json`](schema/source-v1.schema.json).

Zym has two kinds of plugins:

| Kind | What it is | Runs as | Purpose |
|---|---|---|---|
| **Script** | mpv Lua/JS script | inside mpv | Playback hooks: OSD, key bindings, auto-skip, etc. (already supported) |
| **Content** | WebAssembly module (this document) | sandboxed, [Extism](https://extism.org) | Feeds content to the **Explorer**: catalogs, channels, search, playable links |

The player never talks to Stremio, IPTV providers, debrid services, etc. itself. A content plugin does
that and hands the player content in the format below. The player only browses, displays and plays.

---

## 1. Package

A plugin is a `.zym` file — a zip archive containing:

```
manifest.json   # required, see §2
plugin.wasm     # required, Extism module exporting the functions in §3
icon.png        # optional, square, ≥ 128 px
```

Installed packages are unpacked to `<config>/zym-player/sources/<id>/`
(`~/.config/zym-player/sources/` on Linux, `%APPDATA%\zym-player\sources\` on Windows).

## 2. Manifest

```jsonc
{
  "id": "org.example.iptv",              // reverse-DNS, unique, never changes
  "name": "Example IPTV",
  "version": "1.2.0",                    // semver
  "api": 1,                              // this spec's version
  "description": "Live TV from an M3U playlist",
  "author": "Jane Doe",
  "icon": "icon.png",
  "capabilities": ["browse", "search"],  // "browse" required; "search" optional
  "hosts": ["*.example-iptv.com"],       // the ONLY hosts the plugin may contact (enforced)
  "settings": [
    { "key": "playlist_url", "label": "Playlist URL", "type": "string", "required": true,
      "section": "Account", "description": "M3U/M3U8 link from your provider" },
    { "key": "api_key",      "label": "API key",      "type": "secret", "section": "Account" },
    { "key": "quality",      "label": "Quality",      "type": "select",
      "options": ["auto", "1080p", "720p"], "default": "auto", "section": "Playback" },
    { "key": "adult",        "label": "Show adult",   "type": "bool", "default": false, "section": "Content" }
  ]
}
```

- `hosts` is shown to the user before install and enforced by the runtime (Extism `allowed_hosts`).
  It limits **only the plugin's own HTTP requests**. Artwork and streams are fetched by the player from
  wherever the plugin points, so the gallery describes `hosts` as "this plugin talks to …", not as a
  limit on what can be played. Use `"*"` only if unavoidable (e.g. user-supplied URLs); the gallery flags it.
- The player draws the settings form from `settings`, grouped under `section` headings (in order of
  first use; settings without a section come first, untitled). `description` is shown as help text
  under the field. The form opens automatically while a `required` setting is empty.
- `secret` values are meant for the OS keyring, never plain config files.
  *Current build:* stored in `settings.json` next to the plugin — keyring support is pending.

## 3. Functions the plugin exports

Every function takes a JSON string and returns a JSON string (Extism `input`/`output`).

| Export | Input | Output | When |
|---|---|---|---|
| `home` | `{}` | `Page` | User opens the plugin. If it returns `next`, more items load via `browse({"route": "", "cursor": next})` — the empty route means "home". |
| `browse` | `{ "route": string, "cursor"?: string }` | `Page` | User opens a folder/show/season, or scrolls for more |
| `search` | `{ "query": string, "cursor"?: string }` | `Page` | User searches (only if `"search"` capability) |
| `resolve` | `{ "id": string }` | `Playback` | User plays an item — resolve links **late** (tokens expire) |

**Errors:** return an Extism error (`Err(...)` in the PDK) with a short, user-readable message.
The player shows it on the page; it never blocks playback of other content.

**Settings** are passed as Extism config: `config::get("playlist_url")` in the PDK.

**HTTP**: use the PDK's `http::request`. Requests to hosts not in the manifest fail.

**Timeouts:** `home`/`browse`/`search` 15 s, `resolve` 30 s (debrid may need to cache a file).

## 4. Content model

### Page

```jsonc
{
  "title": "Movies",
  "layout": "grid",            // grid | list | rows | channels   (a hint; player may adapt)
  "sections": [
    { "title": "Popular", "items": [ /* Item */ ] }
  ],
  "next": "cursor-abc"         // optional: more items exist → player calls browse/search with this cursor
}
```

Layouts:
- **grid** — poster wall (movies, series)
- **list** — compact rows (files, episodes, stream choices)
- **rows** — multiple horizontal shelves, one per section (home pages)
- **channels** — live TV list with "now / next" (uses `Item.live`)

### Item

```jsonc
{
  "id": "tt0111161",            // unique within the plugin; passed back to resolve()
  "kind": "movie",              // folder | movie | series | season | episode | live | stream
  "title": "The Example",
  "subtitle": "1994 · Drama",   // optional second line
  "description": "…",           // optional, shown on hover/details
  "art": { "poster": "https://…", "thumb": "https://…", "backdrop": "https://…", "logo": "https://…" },
  "badges": ["1080p", "4.2 GB"],
  "route": "movie/tt0111161",   // EITHER: opening this item calls browse(route)
  "playable": false,            // OR: true → playing calls resolve(id)
  "live": {                     // only for kind=live
    "now":  { "title": "News", "start": "2026-11-20T18:00:00Z", "end": "2026-11-20T19:00:00Z" },
    "next": { "title": "Weather", "start": "2026-11-20T19:00:00Z" }
  }
}
```

Rules:
- An item has a `route`, or `playable: true`, or both (e.g. an episode that plays but also has a detail page).
- **Stream choices are just a Page.** A movie's `route` can return a `list` page of `kind: "stream"` items
  (`"1080p · 4.2 GB · Cached"`), each `playable`. No separate stream type exists.
- Art URLs may be `http(s):` or `data:` images (≤ 64 KB). The player fetches and caches them; `http`
  is allowed because IPTV logos (`tvg-logo`) very often use it.

### Playback

```jsonc
{
  "url": "https://cdn.example.com/stream.m3u8",   // http(s), rtmp(s), rtsp, rtp, udp, srt, mms only
  "title": "The Example (1994)",                  // optional override for the title bar
  "headers": { "User-Agent": "…", "Referer": "…" },
  "subtitles": [ { "url": "https://…/en.srt", "lang": "en", "label": "English" } ],
  "start": 0,                                     // seconds; resume position
  "info": {                                       // optional: the ⓘ card on the video
    "title": "The Example", "subtitle": "1994 · Drama",
    "description": "…",                           // ≤ 4000 chars shown
    "poster": "https://…", "backdrop": "https://…", // same rules as Item art
    "badges": ["★ 8.1", "1080p"],                 // ≤ 8
    "details": [ { "label": "Director", "value": "…" } ]  // ≤ 20
  }
}
```

Only these fields reach mpv, and the player copies them field by field (unknown fields are ignored, never
forwarded). Plugins cannot pass arbitrary mpv options — some can load scripts or write files.

- `url` is limited to network streaming schemes (http(s), rtmp(s), rtsp, rtp, udp, srt, mms, ftp, sftp): no `file://`, `edl://`, `lavfi://`, `memory://` etc.,
  which would let a plugin read local files or drive mpv internals. Subtitle URLs must be `http(s)`.
- `headers`: names are RFC 7230 tokens, values contain no CR/LF. **Player requirement:** headers are passed
  to mpv as a string-list *property* (`http-header-fields`) via the property API, never by joining text into
  an option string — a value like `x,script=/tmp/a.lua` must stay one literal header value.

**`info`** fills the ⓘ card the player shows at the top right of the video while this stream plays.
It's display-only text and images; nothing in it reaches mpv. When a plugin omits it, the player builds
the card from the item that was played: its `title`, `subtitle`, `description`, art and `badges`. For a
`stream` item (one of several choices on a title's page) the card describes the item whose `route` opened
that page, with the stream listed under "Source". So filling `description` and `art.poster` on catalog
items is usually enough.

## 5. Gallery index

A gallery is a JSON file at any URL. Users can add several (like Kodi repositories).

```jsonc
{
  "name": "Zym Community",
  "plugins": [
    {
      "id": "org.example.iptv", "name": "Example IPTV", "version": "1.2.0", "api": 1,
      "description": "Live TV from an M3U playlist", "author": "Jane Doe",
      "icon": "https://…/icon.png",
      "hosts": ["*.example-iptv.com"],
      "download": "https://…/example-iptv-1.2.0.zym",
      "sha256": "e3b0c442…"          // verified before install
    }
  ]
}
```

Install = download → verify `sha256` → check `api` → unzip → read `manifest.json` (must match the index entry).

## 6. Versioning

- `api` bumps only on breaking changes. The player refuses plugins with an `api` it doesn't know.
- Additive changes (new optional fields, new `kind`/`layout` values) keep `api: 1`.
  Plugins must ignore unknown input fields; the player renders unknown kinds as `folder`/`stream`.
- Every response is validated against the schema, which allows unknown fields and unknown `kind`/`layout`
  strings. Validation is **per item**: an invalid item is dropped (and logged), an invalid field is ignored;
  only a structurally broken page (e.g. not JSON, no `sections`) shows an error card.
- Unknown `layout` falls back to `list`.

## 7. Minimal plugin (Rust)

> Not yet compiled — no wasm target on the dev machine. Treat as illustrative until the reference plugin lands.

`Cargo.toml`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
extism-pdk = "1.4"
serde_json = "1"
```

`src/lib.rs`:

```rust
use extism_pdk::*;
use serde_json::{json, Value};

fn page() -> Value {
    json!({
        "title": "Demo",
        "layout": "list",
        "sections": [{ "items": [{
            "id": "bbb", "kind": "movie", "title": "Big Buck Bunny", "playable": true
        }]}]
    })
}

#[plugin_fn]
pub fn home(_: Json<Value>) -> FnResult<Json<Value>> {
    Ok(Json(page()))
}

#[plugin_fn]
pub fn browse(_: Json<Value>) -> FnResult<Json<Value>> {
    Ok(Json(page()))
}

#[plugin_fn]
pub fn resolve(_: Json<Value>) -> FnResult<Json<Value>> {
    Ok(Json(json!({ "url": "https://download.blender.org/peach/bigbuckbunny_movies/BigBuckBunny_320x180.mp4" })))
}
```

Build: `cargo build --release --target wasm32-unknown-unknown`, then zip `manifest.json` + the built
`.wasm` (renamed `plugin.wasm`) into `demo.zym`.
Plugins can also be written in Go, JavaScript, Python, C#, Zig, … via the Extism PDKs.
