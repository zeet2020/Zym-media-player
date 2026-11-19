//! Source plugins (docs/plugins.md, api 1): load, run sandboxed via Extism, validate what they return.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

pub const API: u32 = 1;
const STREAM_SCHEMES: &[&str] =
    &["http://", "https://", "rtmp://", "rtmps://", "rtsp://", "rtp://", "udp://", "srt://", "mms://", "ftp://", "sftp://"];

// ------------------------------------------------------------------ manifest

#[derive(Deserialize, Clone, Debug)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub settings: Vec<Setting>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Setting {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String, // string | secret | bool | select
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub required: bool,
    /// Group heading in the settings form.
    #[serde(default)]
    pub section: Option<String>,
    /// Help text under the field.
    #[serde(default)]
    pub description: Option<String>,
}

// ------------------------------------------------------------------ content

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub title: String,
    pub layout: String,
    pub sections: Vec<Section>,
    pub next: Option<String>,
    /// Items dropped because they failed validation (shown as a hint, logged).
    pub dropped: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Section {
    pub title: Option<String>,
    pub items: Vec<Item>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub art: Art,
    #[serde(default)]
    pub badges: Vec<String>,
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub playable: bool,
    #[serde(default)]
    pub live: Option<Live>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Art {
    pub poster: Option<String>,
    pub thumb: Option<String>,
    pub backdrop: Option<String>,
    pub logo: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Live {
    pub now: Option<Programme>,
    pub next: Option<Programme>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Programme {
    pub title: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Playback {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub subtitles: Vec<SubtitleRef>,
    #[serde(default)]
    pub start: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct SubtitleRef {
    pub url: String,
}

fn ok_image(u: &Option<String>) -> bool {
    u.as_deref().is_none_or(|u| u.starts_with("http://") || u.starts_with("https://") || u.starts_with("data:image/"))
}

/// Lenient page parse (spec §6): unknown fields ignored, bad items dropped, only a broken shape fails.
pub fn parse_page(v: Value) -> Result<Page, String> {
    let sections = v.get("sections").and_then(Value::as_array).ok_or("plugin returned a page without `sections`")?;
    let mut page = Page {
        title: v.get("title").and_then(Value::as_str).unwrap_or_default().to_string(),
        layout: v.get("layout").and_then(Value::as_str).unwrap_or("list").to_string(),
        next: v.get("next").and_then(Value::as_str).map(str::to_string),
        ..Default::default()
    };
    for s in sections {
        let mut section = Section { title: s.get("title").and_then(Value::as_str).map(str::to_string), items: vec![] };
        for raw in s.get("items").and_then(Value::as_array).into_iter().flatten() {
            match serde_json::from_value::<Item>(raw.clone()) {
                Ok(mut it) if !it.id.is_empty() && (it.route.is_some() || it.playable) => {
                    // Bad art URLs: drop the field, keep the item.
                    for f in [&mut it.art.poster, &mut it.art.thumb, &mut it.art.backdrop, &mut it.art.logo] {
                        if !ok_image(f) {
                            *f = None;
                        }
                    }
                    section.items.push(it);
                }
                Ok(_) | Err(_) => {
                    page.dropped += 1;
                    eprintln!("source: dropped invalid item: {raw}");
                }
            }
        }
        page.sections.push(section);
    }
    Ok(page)
}

fn header_name_ok(n: &str) -> bool {
    !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+.^_`|~-".contains(&b))
}

/// Strict playback check (spec §4): network schemes only, safe headers, http(s) subtitles.
pub fn parse_playback(v: Value) -> Result<Playback, String> {
    let pb: Playback = serde_json::from_value(v).map_err(|e| format!("invalid playback: {e}"))?;
    if !STREAM_SCHEMES.iter().any(|s| pb.url.to_ascii_lowercase().starts_with(s)) {
        return Err(format!("refused stream URL scheme: {}", pb.url.split(':').next().unwrap_or("")));
    }
    for (k, v) in &pb.headers {
        if !header_name_ok(k) || v.contains(['\r', '\n']) {
            return Err(format!("refused invalid header {k:?}"));
        }
    }
    if let Some(s) = pb.subtitles.iter().find(|s| !(s.url.starts_with("http://") || s.url.starts_with("https://"))) {
        return Err(format!("refused subtitle URL: {}", s.url));
    }
    Ok(pb)
}

// ------------------------------------------------------------------ runtime

/// An installed plugin (code + manifest). Each configured server/playlist of it is a `Source`.
pub struct Installed {
    pub manifest: Manifest,
    /// Where manifest.json + plugin.wasm live (may be a read-only install dir).
    pub dir: PathBuf,
    /// Per-user, writable: <config>/zym-player/sources/<id>/ (holds connections/).
    user_dir: PathBuf,
}

/// One connection of a plugin ("Home NAS", "Seedbox", …): its own name, settings and sandboxed instance.
pub struct Source {
    pub installed: Arc<Installed>,
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub conn_id: String,
    name: Mutex<String>,
    /// <user_dir>/connections/<conn_id>.json = {"name", "settings"}
    file: PathBuf,
    plugin: Mutex<Option<extism::Plugin>>,
    settings: Mutex<BTreeMap<String, String>>,
}

#[derive(serde::Serialize, Deserialize, Default)]
struct ConnFile {
    name: String,
    #[serde(default)]
    settings: BTreeMap<String, String>,
}

impl Installed {
    pub fn load(dir: &Path, user_dir: PathBuf) -> Result<Self, String> {
        let text = std::fs::read_to_string(dir.join("manifest.json")).map_err(|e| format!("manifest.json: {e}"))?;
        let manifest: Manifest = serde_json::from_str(&text).map_err(|e| format!("manifest.json: {e}"))?;
        if manifest.api != API {
            return Err(format!("{}: needs api {}, this player speaks api {API}", manifest.id, manifest.api));
        }
        if !dir.join("plugin.wasm").is_file() {
            return Err(format!("{}: plugin.wasm missing", manifest.id));
        }
        Ok(Self { manifest, dir: dir.to_path_buf(), user_dir })
    }

    fn conn_dir(&self) -> PathBuf {
        self.user_dir.join("connections")
    }

    /// Saved connections, oldest first. A pre-connections `settings.json` becomes connection "default".
    pub fn connections(self: &Arc<Self>) -> Vec<Arc<Source>> {
        let legacy = self.user_dir.join("settings.json");
        if legacy.is_file() && !self.conn_dir().exists() {
            let settings = std::fs::read_to_string(&legacy).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            let migrated = ConnFile { name: self.manifest.name.clone(), settings };
            if std::fs::create_dir_all(self.conn_dir()).is_ok()
                && std::fs::write(self.conn_dir().join("default.json"), serde_json::to_string_pretty(&migrated).unwrap()).is_ok()
            {
                std::fs::remove_file(&legacy).ok();
            }
        }
        let Ok(entries) = std::fs::read_dir(self.conn_dir()) else { return vec![] };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_string))
            .collect();
        ids.sort(); // "default" < "c<millis>"… order is stable; good enough for a sidebar
        ids.into_iter().map(|id| Arc::new(Source::open(self, &id))).collect()
    }

    /// A new, unsaved connection (persisted on first save).
    pub fn new_connection(self: &Arc<Self>, existing: usize) -> Arc<Source> {
        let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let src = Source::open(self, &format!("c{millis}"));
        *src.name.lock().unwrap() =
            if existing == 0 { self.manifest.name.clone() } else { format!("{} {}", self.manifest.name, existing + 1) };
        Arc::new(src)
    }
}

impl Source {
    fn open(installed: &Arc<Installed>, conn_id: &str) -> Self {
        let file = installed.conn_dir().join(format!("{conn_id}.json"));
        // ponytail: settings (incl. secrets) are plain JSON per connection; move `secret` fields to the OS keyring.
        let saved: ConnFile = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Self {
            installed: installed.clone(),
            manifest: installed.manifest.clone(),
            dir: installed.dir.clone(),
            conn_id: conn_id.to_string(),
            name: Mutex::new(if saved.name.is_empty() { installed.manifest.name.clone() } else { saved.name }),
            file,
            plugin: Mutex::new(None),
            settings: Mutex::new(saved.settings),
        }
    }

    /// Test helper: plugin at `dir`, one connection stored under `user_dir`.
    #[cfg(test)]
    pub fn load(dir: &Path, user_dir: PathBuf) -> Result<Self, String> {
        Ok(Source::open(&Arc::new(Installed::load(dir, user_dir)?), "test"))
    }

    pub fn name(&self) -> String {
        self.name.lock().unwrap().clone()
    }

    pub fn is_saved(&self) -> bool {
        self.file.is_file()
    }

    /// Persist name + settings for this connection.
    pub fn save(&self, name: String, values: BTreeMap<String, String>) -> Result<(), String> {
        let name = if name.trim().is_empty() { self.manifest.name.clone() } else { name.trim().to_string() };
        let data = ConnFile { name: name.clone(), settings: values.clone() };
        std::fs::create_dir_all(self.file.parent().unwrap()).map_err(|e| format!("saving settings: {e}"))?;
        std::fs::write(&self.file, serde_json::to_string_pretty(&data).unwrap()).map_err(|e| format!("saving settings: {e}"))?;
        *self.name.lock().unwrap() = name;
        *self.settings.lock().unwrap() = values;
        *self.plugin.lock().unwrap() = None; // config is baked into the instance; rebuild on next call
        Ok(())
    }

    pub fn remove(&self) -> Result<(), String> {
        match std::fs::remove_file(&self.file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("removing connection: {e}")),
        }
    }

    pub fn settings(&self) -> BTreeMap<String, String> {
        self.settings.lock().unwrap().clone()
    }

    pub fn has(&self, cap: &str) -> bool {
        self.manifest.capabilities.iter().any(|c| c == cap)
    }

    pub fn needs_setup(&self) -> bool {
        let s = self.settings.lock().unwrap();
        self.manifest.settings.iter().any(|f| f.required && s.get(&f.key).is_none_or(|v| v.trim().is_empty()))
    }

    pub fn save_settings(&self, values: BTreeMap<String, String>) -> Result<(), String> {
        self.save(self.name(), values)
    }

    /// Run one export. Blocking — call from a background thread.
    pub fn call(&self, func: &str, input: &Value) -> Result<Value, String> {
        let mut slot = self.plugin.lock().unwrap();
        if slot.is_none() {
            let mut config: BTreeMap<String, String> = self
                .manifest
                .settings
                .iter()
                .filter_map(|s| s.default.as_ref().map(|d| (s.key.clone(), d.as_str().map(str::to_string).unwrap_or_else(|| d.to_string()))))
                .collect();
            config.extend(self.settings());
            let m = extism::Manifest::new([extism::Wasm::file(self.dir.join("plugin.wasm"))])
                .with_allowed_hosts(self.manifest.hosts.clone().into_iter())
                .with_config(config.into_iter())
                .with_timeout(Duration::from_secs(if func == "resolve" { 30 } else { 15 }));
            // Host functions available to every plugin; each enforces this plugin's manifest `hosts`.
            let host_fns = [crate::remote::host_function(self.manifest.hosts.clone())];
            *slot = Some(extism::Plugin::new(&m, host_fns, true).map_err(|e| format!("loading plugin: {e}"))?);
        }
        let plugin = slot.as_mut().unwrap();
        let out = plugin.call::<&str, &str>(func, &input.to_string()).map_err(|e| {
            let msg = format!("{e:#}"); // full cause chain; bare to_string() is just "error while executing…"
            if msg.contains("timeout") { format!("{} timed out", self.manifest.name) } else { msg }
        });
        let out = match out {
            Ok(s) => s.to_string(),
            Err(e) => {
                *slot = None; // a trapped/timed-out instance may be poisoned; start fresh next time
                return Err(e);
            }
        };
        serde_json::from_str(&out).map_err(|e| format!("plugin returned invalid JSON: {e}"))
    }

    pub fn home(&self) -> Result<Page, String> {
        parse_page(self.call("home", &json!({}))?)
    }
    pub fn browse(&self, route: &str, cursor: Option<&str>) -> Result<Page, String> {
        parse_page(self.call("browse", &json!({ "route": route, "cursor": cursor }))?)
    }
    pub fn search(&self, query: &str, cursor: Option<&str>) -> Result<Page, String> {
        parse_page(self.call("search", &json!({ "query": query, "cursor": cursor }))?)
    }
    pub fn resolve(&self, id: &str) -> Result<Playback, String> {
        parse_playback(self.call("resolve", &json!({ "id": id }))?)
    }
}

/// <config>/zym-player/sources/<id>/{manifest.json, plugin.wasm}
pub fn sources_dir() -> PathBuf {
    crate::plugins_dir().parent().unwrap().join("sources")
}

/// Plugins shipped with the app: `<exe dir>/sources` (Windows, tarball) and
/// `<exe dir>/../share/zym-player/sources` (deb/rpm: /usr/bin -> /usr/share).
fn bundled_dirs() -> Vec<PathBuf> {
    let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) else { return vec![] };
    vec![exe_dir.join("sources"), exe_dir.join("../share/zym-player/sources")]
}

/// User-installed plugins first, then bundled ones; the first plugin with a given id wins.
/// Settings always live in the user dir (bundled install dirs are read-only).
pub fn load_all() -> Vec<Arc<Installed>> {
    load_from(&sources_dir(), &bundled_dirs())
}

fn load_from(user: &Path, bundled: &[PathBuf]) -> Vec<Arc<Installed>> {
    let mut out: Vec<Arc<Installed>> = Vec::new();
    for root in std::iter::once(user.to_path_buf()).chain(bundled.iter().cloned()) {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for e in entries.flatten() {
            let dir = e.path();
            if !dir.join("manifest.json").is_file() {
                continue; // e.g. a settings-only folder for a bundled plugin
            }
            let id = e.file_name().to_string_lossy().into_owned();
            match Installed::load(&dir, user.join(&id)) {
                Ok(s) if out.iter().any(|o| o.manifest.id == s.manifest.id) => {}
                Ok(s) => out.push(Arc::new(s)),
                Err(err) => eprintln!("source: skipped {}: {err}", dir.display()),
            }
        }
    }
    out.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    out
}

// ------------------------------------------------------------------ images

/// GPUI's `img(url)` fetches through the app's HttpClient; the default one refuses everything.
pub struct ImageFetcher;

impl gpui::http_client::HttpClient for ImageFetcher {
    fn type_name(&self) -> &'static str {
        "zym-image-fetcher"
    }

    fn user_agent(&self) -> Option<&gpui::http_client::http::HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&gpui::http_client::Url> {
        None
    }

    fn send(
        &self,
        req: gpui::http_client::Request<gpui::http_client::AsyncBody>,
    ) -> futures::future::BoxFuture<'static, anyhow::Result<gpui::http_client::Response<gpui::http_client::AsyncBody>>> {
        let uri = req.uri().to_string();
        let (tx, rx) = futures::channel::oneshot::channel();
        // ponytail: thread per image; a small pool if big poster grids ever spawn too many.
        std::thread::spawn(move || {
            let res = (|| -> anyhow::Result<_> {
                let mut resp = ureq::get(&uri).call()?;
                let status = resp.status().as_u16();
                let bytes = resp.body_mut().with_config().limit(8 * 1024 * 1024).read_to_vec()?;
                Ok(gpui::http_client::Response::builder().status(status).body(gpui::http_client::AsyncBody::from(bytes))?)
            })();
            tx.send(res).ok();
        });
        Box::pin(async move { rx.await? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_drops_bad_items_and_bad_art_only() {
        let p = parse_page(json!({
            "title": "T", "layout": "carousel", "extra": 1,
            "sections": [{ "items": [
                { "id": "a", "kind": "live", "title": "ok", "playable": true, "rating": 9,
                  "art": { "logo": "http://logo/a.png", "thumb": "file:///etc/passwd" } },
                { "id": "b", "kind": "movie", "title": "no route nor playable" },
                { "kind": "movie", "title": "no id", "route": "x" },
                { "id": "c", "kind": "podcast", "title": "unknown kind ok", "route": "r" }
            ]}]
        }))
        .unwrap();
        let items = &p.sections[0].items;
        assert_eq!(items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(p.dropped, 2);
        assert_eq!(items[0].art.logo.as_deref(), Some("http://logo/a.png"));
        assert_eq!(items[0].art.thumb, None); // file:// art stripped
        assert_eq!(p.layout, "carousel"); // renderer falls back to list
        assert!(parse_page(json!({ "title": "no sections" })).is_err());
    }

    #[test]
    fn playback_rules() {
        assert!(parse_playback(json!({ "url": "https://a/b.m3u8", "headers": { "User-Agent": "x, y" } })).is_ok());
        assert!(parse_playback(json!({ "url": "file:///etc/passwd" })).is_err());
        assert!(parse_playback(json!({ "url": "edl://x" })).is_err());
        assert!(parse_playback(json!({ "url": "https://a", "headers": { "X": "a\r\nB: c" } })).is_err());
        assert!(parse_playback(json!({ "url": "https://a", "headers": { "Bad Name": "v" } })).is_err());
        assert!(parse_playback(json!({ "url": "https://a", "subtitles": [{ "url": "file:///x.srt" }] })).is_err());
        // unknown fields (e.g. smuggled mpv options) are simply not forwarded
        let pb = parse_playback(json!({ "url": "https://a", "mpv_options": { "script": "x.lua" } })).unwrap();
        assert_eq!(pb.url, "https://a");
    }

    /// Tiny HTTP server: `respond(request_head)` -> (status line, body). Returns (port, request log).
    fn serve(respond: fn(&str) -> (&'static str, String)) -> (u16, Arc<Mutex<Vec<String>>>) {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for mut s in server.incoming().flatten() {
                // Read head + Content-Length body; answering early makes clients that are still sending fail.
                let (mut data, mut buf) = (Vec::new(), [0u8; 8192]);
                loop {
                    let n = s.read(&mut buf).unwrap_or(0);
                    data.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&data).to_string();
                    let Some(end) = text.find("\r\n\r\n") else {
                        if n == 0 { break } else { continue }
                    };
                    let len: usize = text[..end]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                        .unwrap_or(0);
                    if n == 0 || data.len() >= end + 4 + len {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&data).to_string();
                let (status, body) = respond(&req);
                log2.lock().unwrap().push(req);
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        (port, log)
    }

    /// Install a built plugin from sources/<name> into a temp dir; None if not built yet.
    fn load_built(name: &str, crate_name: &str) -> Option<Source> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sources").join(name);
        let wasm = root.join(format!("target/wasm32-unknown-unknown/release/{crate_name}.wasm"));
        if !wasm.exists() {
            eprintln!("skip: build it first: sources/install.sh sources/{name}");
            return None;
        }
        let dir = std::env::temp_dir().join(format!("zym-src-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(&wasm, dir.join("plugin.wasm")).unwrap();
        std::fs::copy(root.join("manifest.json"), dir.join("manifest.json")).unwrap();
        std::fs::remove_file(dir.join("settings.json")).ok();
        Some(Source::load(&dir, dir.clone()).unwrap())
    }

    /// End-to-end: the real M3U plugin running in Extism, fetching from a local HTTP server.
    #[test]
    fn m3u_plugin_end_to_end() {
        let (port, _) = serve(|_| {
            ("200 OK", "#EXTM3U\n#EXTINF:-1 tvg-logo=\"http://l/n.png\" group-title=\"News\",World News, HD\n#EXTVLCOPT:http-user-agent=VLC/3\nhttp://cdn/news.m3u8\n#EXTINF:-1 group-title=\"Sports\",Sports One\nhttps://cdn/s.m3u8|Referer=https://site/\n".into())
        });
        let Some(src) = load_built("m3u", "zym_source_m3u") else { return };
        let dir = src.dir.clone();
        assert!(src.needs_setup());

        // No URL configured -> readable error, not a crash.
        let err = src.home().unwrap_err();
        assert!(err.contains("playlist URL"), "{err}");

        src.save_settings(BTreeMap::from([("playlist_url".into(), format!("http://127.0.0.1:{port}/list.m3u"))])).unwrap();
        assert!(!src.needs_setup());
        let home = src.home().unwrap();
        let titles: Vec<_> = home.sections[0].items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["All channels", "News", "Sports"]);

        let news = src.browse("group:News", None).unwrap();
        assert_eq!(news.layout, "channels");
        let ch = &news.sections[0].items[0];
        assert_eq!((ch.title.as_str(), ch.kind.as_str(), ch.playable), ("World News, HD", "live", true));
        assert_eq!(ch.art.logo.as_deref(), Some("http://l/n.png"));

        assert_eq!(src.search("sport", None).unwrap().sections[0].items[0].title, "Sports One");

        let pb = src.resolve(&ch.id).unwrap();
        assert_eq!(pb.url, "http://cdn/news.m3u8");
        assert_eq!(pb.headers.get("User-Agent").map(String::as_str), Some("VLC/3"));
        let pb2 = src.resolve("ch:1").unwrap();
        assert_eq!(pb2.headers.get("Referer").map(String::as_str), Some("https://site/"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bundled_plugins_load_and_user_overrides() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sources/m3u");
        let wasm = root.join("target/wasm32-unknown-unknown/release/zym_source_m3u.wasm");
        if !wasm.exists() {
            return;
        }
        let tmp = std::env::temp_dir().join(format!("zym-bundle-test-{}", std::process::id()));
        let (user, bundle) = (tmp.join("user"), tmp.join("bundle"));
        let put = |dir: &Path, name: &str| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::copy(&wasm, dir.join("plugin.wasm")).unwrap();
            let m = std::fs::read_to_string(root.join("manifest.json")).unwrap().replace("\"IPTV (M3U)\"", &format!("\"{name}\""));
            std::fs::write(dir.join("manifest.json"), m).unwrap();
        };
        put(&bundle.join("org.zym.m3u"), "Bundled");
        // Pre-connections settings.json in the user dir (folder without a manifest: not a plugin).
        std::fs::create_dir_all(user.join("org.zym.m3u")).unwrap();
        std::fs::write(user.join("org.zym.m3u/settings.json"), r#"{"playlist_url":"http://old/"}"#).unwrap();

        let p = load_from(&user, &[bundle.clone()]);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].manifest.name, "Bundled");

        // Migration: old settings become connection "default"; legacy file removed.
        let conns = p[0].connections();
        assert_eq!(conns.len(), 1);
        assert_eq!((conns[0].conn_id.as_str(), conns[0].name().as_str()), ("default", "Bundled"));
        assert_eq!(conns[0].settings().get("playlist_url").map(String::as_str), Some("http://old/"));
        assert!(!user.join("org.zym.m3u/settings.json").exists());

        // A second connection, saved with its own name/settings, never touching the install dir.
        let c2 = p[0].new_connection(1);
        assert!(!c2.is_saved());
        assert_eq!(c2.name(), "Bundled 2");
        c2.save("Provider B".into(), BTreeMap::from([("playlist_url".into(), "http://b/".into())])).unwrap();
        assert!(!bundle.join("org.zym.m3u/connections").exists(), "never write into the install dir");
        let conns = load_from(&user, &[bundle.clone()])[0].connections();
        assert_eq!(conns.len(), 2);
        assert!(conns.iter().any(|c| c.name() == "Provider B" && c.settings()["playlist_url"] == "http://b/"));
        assert!(conns.iter().any(|c| c.conn_id == "default" && c.settings()["playlist_url"] == "http://old/"));

        // Remove one; the other stays.
        conns.iter().find(|c| c.name() == "Provider B").unwrap().remove().unwrap();
        assert_eq!(load_from(&user, &[bundle.clone()])[0].connections().len(), 1);

        put(&user.join("org.zym.m3u"), "User copy"); // same id installed by the user wins
        let p = load_from(&user, &[bundle.clone()]);
        assert_eq!((p.len(), p[0].manifest.name.as_str()), (1, "User copy"));
        assert_eq!(p[0].connections().len(), 1, "connections are per user id, independent of which copy wins");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// End-to-end: the real Stremio plugin against a fake catalog+meta addon and a debrid-style stream addon.
    #[test]
    fn stremio_plugin_end_to_end() {
        let (port, log) = serve(|req| {
            let path = req.split_whitespace().nth(1).unwrap_or("");
            let body = match path {
                "/cine/manifest.json" => r#"{"name":"Cinemeta","types":["movie","series"],"idPrefixes":["tt"],"resources":["catalog","meta"],
                    "catalogs":[{"type":"movie","id":"top","name":"Popular","extra":[{"name":"search"}]}]}"#.to_string(),
                "/cine/catalog/movie/top.json" => r#"{"metas":[{"id":"tt1","type":"movie","name":"Film One","releaseInfo":"2001"},
                    {"id":"tt2","type":"series","name":"Show Two"}]}"#.to_string(),
                "/cine/catalog/movie/top/search=show%20two.json" => r#"{"metas":[{"id":"tt2","type":"series","name":"Show Two"}]}"#.to_string(),
                "/cine/meta/series/tt2.json" => r#"{"meta":{"type":"series","name":"Show Two","videos":[{"id":"tt2:1:1","season":1,"episode":1,"name":"Pilot"}]}}"#.to_string(),
                "/rd/manifest.json" => r#"{"name":"DebridStreams","types":["movie","series"],"resources":[{"name":"stream","idPrefixes":["tt"]}],"catalogs":[]}"#.to_string(),
                "/rd/stream/series/tt2:1:1.json" => r#"{"streams":[{"name":"RD 1080p","title":"Pilot.1080p.mkv","url":"https://cdn.example/pilot.mkv",
                    "behaviorHints":{"proxyHeaders":{"request":{"User-Agent":"Stremio"}}}},{"name":"Torrent","infoHash":"abc"}]}"#.to_string(),
                _ => return ("404 Not Found", String::new()),
            };
            ("200 OK", body)
        });
        let Some(src) = load_built("stremio", "zym_source_stremio") else { return };
        let base = format!("http://127.0.0.1:{port}");
        src.save_settings(BTreeMap::from([("addons".into(), format!("{base}/cine/manifest.json, {base}/rd/manifest.json"))])).unwrap();

        let home = src.home().unwrap();
        assert_eq!(home.layout, "rows");
        let row = &home.sections[0].items;
        let titles: Vec<_> = row.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["All Popular ›", "Film One", "Show Two"]);

        let found = src.search("show two", None).unwrap();
        assert_eq!(found.sections[0].items[0].title, "Show Two");

        let eps = src.browse(row[2].route.as_deref().unwrap(), None).unwrap(); // series -> episodes
        assert_eq!(eps.sections[0].title.as_deref(), Some("Season 1"));
        let ep = &eps.sections[0].items[0];
        assert_eq!(ep.title, "1. Pilot");

        let streams = src.browse(ep.route.as_deref().unwrap(), None).unwrap();
        assert_eq!(streams.sections[0].title.as_deref(), Some("DebridStreams"));
        assert_eq!(streams.sections[0].items.len(), 1, "torrent-only stream not offered");
        assert!(streams.sections.last().unwrap().title.as_deref().unwrap().contains("1 torrent stream"));

        let pb = src.resolve(&streams.sections[0].items[0].id).unwrap();
        assert_eq!(pb.url, "https://cdn.example/pilot.mkv");
        assert_eq!(pb.headers.get("User-Agent").map(String::as_str), Some("Stremio"));

        // Manifests are fetched once per instance, not per page.
        let manifests = log.lock().unwrap().iter().filter(|r| r.contains("manifest.json")).count();
        assert_eq!(manifests, 2, "manifest requests: {manifests}");
        std::fs::remove_dir_all(&src.dir).ok();
    }

    /// Manual: real Cinemeta over the internet (`cargo test --release stremio_live -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn stremio_live_cinemeta() {
        let Some(src) = load_built("stremio", "zym_source_stremio") else { return };
        src.save_settings(BTreeMap::from([("addons".into(), "https://v3-cinemeta.strem.io/manifest.json".into())])).unwrap();
        let home = src.home().unwrap();
        for s in &home.sections {
            eprintln!("row {:?}: {} items, first={:?}", s.title, s.items.len(), s.items.get(1).map(|i| &i.title));
        }
        let series = home.sections.iter().flat_map(|s| &s.items).find(|i| i.kind == "series").expect("a series in the rows");
        let eps = src.browse(series.route.as_deref().unwrap(), None).unwrap();
        eprintln!("{}: {} seasons, first ep {:?}", series.title, eps.sections.len(), eps.sections.first().and_then(|s| s.items.first()).map(|i| &i.title));
        assert!(!eps.sections.is_empty());
        let found = src.search("matrix", None).unwrap();
        eprintln!("search 'matrix': {:?}", found.sections.iter().map(|s| (s.title.clone(), s.items.len())).collect::<Vec<_>>());
        std::fs::remove_dir_all(&src.dir).ok();
    }

    /// End-to-end: the real FTP/SFTP plugin listing via the `zym_list_dir` host function.
    #[test]
    fn ftp_plugin_end_to_end() {
        let port = crate::remote::tests::spawn_ftp_server();
        let Some(src) = load_built("ftp", "zym_source_ftp") else { return };
        assert!(src.needs_setup());
        src.save_settings(BTreeMap::from([
            ("url".into(), format!("ftp://127.0.0.1:{port}/media")),
            ("username".into(), "alice".into()),
            ("password".into(), "secret".into()),
        ]))
        .unwrap();
        let home = src.home().unwrap();
        let items = &home.sections[0].items;
        let titles: Vec<_> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["Shows", "Ep 1.mkv"]); // folders first; "." dropped
        assert_eq!(items[0].route.as_deref(), Some("/media/Shows"));
        assert_eq!(items[1].badges, ["MKV", "1.0 MB"]);
        let pb = src.resolve(&items[1].id).unwrap(); // passes the ftp:// scheme check
        assert_eq!(pb.url, format!("ftp://alice:secret@127.0.0.1:{port}/media/Ep%201.mkv"));
        std::fs::remove_dir_all(&src.dir).ok();
    }

    /// End-to-end: the real WebDAV plugin sending PROPFIND + Basic auth through Extism's HTTP.
    #[test]
    fn webdav_plugin_end_to_end() {
        let (port, log) = serve(|req| {
            let authed = req.contains("Authorization: Basic YWxpY2U6czNjcmV0") || req.contains("authorization: Basic YWxpY2U6czNjcmV0");
            if !authed {
                return ("401 Unauthorized", String::new());
            }
            let body = if req.starts_with("PROPFIND /dav/Shows/ ") {
                r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/Shows/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response><d:response><d:href>/dav/Shows/Ep%201.mkv</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>1048576</d:getcontentlength></d:prop></d:propstat></d:response></d:multistatus>"#
            } else {
                r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response><d:response><d:href>/dav/Shows/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response><d:response><d:href>/dav/movie.mp4</d:href><d:propstat><d:prop><d:resourcetype/></d:prop></d:propstat></d:response><d:response><d:href>/dav/readme.txt</d:href><d:propstat><d:prop><d:resourcetype/></d:prop></d:propstat></d:response></d:multistatus>"#
            };
            ("207 Multi-Status", body.into())
        });
        let Some(src) = load_built("webdav", "zym_source_webdav") else { return };
        let base = format!("http://127.0.0.1:{port}/dav/");
        assert!(src.needs_setup());

        // Wrong password -> readable login error.
        src.save_settings(BTreeMap::from([("url".into(), base.clone()), ("username".into(), "alice".into()), ("password".into(), "nope".into())])).unwrap();
        let err = src.home().unwrap_err();
        assert!(err.contains("Login failed"), "{err}");

        src.save_settings(BTreeMap::from([("url".into(), base.clone()), ("username".into(), "alice".into()), ("password".into(), "s3cret".into())])).unwrap();
        let home = src.home().unwrap();
        let titles: Vec<_> = home.sections[0].items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["Shows", "movie.mp4"]); // self skipped, readme hidden, folders first

        let shows = src.browse(home.sections[0].items[0].route.as_deref().unwrap(), None).unwrap();
        let ep = &shows.sections[0].items[0];
        assert_eq!((ep.title.as_str(), ep.playable), ("Ep 1.mkv", true));
        assert_eq!(ep.badges, ["MKV", "1.0 MB"]);

        let pb = src.resolve(&ep.id).unwrap();
        assert_eq!(pb.url, format!("http://127.0.0.1:{port}/dav/Shows/Ep%201.mkv"));
        assert_eq!(pb.headers.get("Authorization").map(String::as_str), Some("Basic YWxpY2U6czNjcmV0"));

        let reqs = log.lock().unwrap();
        assert!(reqs.iter().all(|r| r.starts_with("PROPFIND ")), "{reqs:?}");
        assert!(reqs.iter().all(|r| r.to_ascii_lowercase().contains("depth: 1")));
        std::fs::remove_dir_all(&src.dir).ok();
    }
}
