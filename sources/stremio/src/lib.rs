//! Zym Media Player content plugin: Stremio addons. Spec: docs/plugins.md (api 1).
//! Speaks the Stremio addon protocol over HTTP: manifest.json, /catalog, /meta, /stream.
//! Catalog addons feed the home rows and search; stream addons feed the stream list of a title.
//! Only streams with a direct `url` are playable (no torrent engine): debrid-enabled addons work.

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub search: bool,
    /// Needs an extra (e.g. search/genre) to be listed at all.
    pub needs_extra: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Addon {
    pub base: String,
    pub name: String,
    pub catalogs: Vec<Catalog>,
    pub meta: bool,
    pub stream: bool,
    pub types: Vec<String>,
    /// From the stream/meta resource or the manifest; empty = any id.
    pub id_prefixes: Vec<String>,
}

/// `https://host/cfg/manifest.json` -> `https://host/cfg`
pub fn base_of(manifest_url: &str) -> String {
    let u = manifest_url.trim();
    u.strip_suffix("/manifest.json").unwrap_or(u).trim_end_matches('/').to_string()
}

fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

pub fn parse_manifest(base: &str, m: &Value) -> Addon {
    let mut a = Addon {
        base: base.to_string(),
        name: m.get("name").and_then(Value::as_str).unwrap_or("Addon").to_string(),
        types: strs(m.get("types")),
        id_prefixes: strs(m.get("idPrefixes")),
        ..Default::default()
    };
    // resources: ["catalog","stream"] or [{ "name": "stream", "types": [...], "idPrefixes": [...] }]
    for r in m.get("resources").and_then(Value::as_array).into_iter().flatten() {
        let (name, prefixes) = match r {
            Value::String(s) => (s.as_str(), vec![]),
            Value::Object(o) => (o.get("name").and_then(Value::as_str).unwrap_or(""), strs(o.get("idPrefixes"))),
            _ => continue,
        };
        match name {
            "stream" => a.stream = true,
            "meta" => a.meta = true,
            _ => {}
        }
        if !prefixes.is_empty() && (name == "stream" || name == "meta") {
            a.id_prefixes = prefixes;
        }
    }
    for c in m.get("catalogs").and_then(Value::as_array).into_iter().flatten() {
        let extras = c.get("extra").and_then(Value::as_array).cloned().unwrap_or_default();
        let has = |n: &str| extras.iter().any(|e| e.get("name").and_then(Value::as_str) == Some(n));
        let required = extras.iter().any(|e| e.get("isRequired").and_then(Value::as_bool) == Some(true))
            || strs(c.get("extraRequired")).len() > 0;
        a.catalogs.push(Catalog {
            kind: c.get("type").and_then(Value::as_str).unwrap_or("movie").to_string(),
            id: c.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            name: c.get("name").and_then(Value::as_str).unwrap_or("Catalog").to_string(),
            search: has("search") || strs(c.get("extraSupported")).iter().any(|e| e == "search"),
            needs_extra: required,
        });
    }
    a
}

impl Addon {
    pub fn serves(&self, kind: &str, id: &str) -> bool {
        (self.types.is_empty() || self.types.iter().any(|t| t == kind))
            && (self.id_prefixes.is_empty() || self.id_prefixes.iter().any(|p| id.starts_with(p)))
    }
}

/// encodeURIComponent, but keep ':' (Stremio ids like `tt123:1:2` go raw in paths).
pub fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b':' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn catalog_url(a: &Addon, c: &Catalog, extra: Option<(&str, &str)>) -> String {
    match extra {
        Some((k, v)) => format!("{}/catalog/{}/{}/{}={}.json", a.base, enc(&c.kind), enc(&c.id), k, enc(v)),
        None => format!("{}/catalog/{}/{}.json", a.base, enc(&c.kind), enc(&c.id)),
    }
}

fn https_art(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).filter(|u| u.starts_with("https://") || u.starts_with("http://")).map(str::to_string)
}

/// A catalog `meta` preview -> Item. Movies go straight to their streams; series to their episodes.
pub fn meta_item(m: &Value) -> Option<Value> {
    let id = m.get("id")?.as_str()?;
    let kind = m.get("type").and_then(Value::as_str).unwrap_or("movie");
    let name = m.get("name").and_then(Value::as_str).unwrap_or(id);
    let route = if kind == "series" || kind == "channel" { format!("meta:{kind}:{id}") } else { format!("streams:{kind}:{id}") };
    let mut it = json!({ "id": format!("{kind}:{id}"), "kind": if kind == "series" { "series" } else { "movie" }, "title": name, "route": route });
    if let Some(r) = m.get("releaseInfo").and_then(Value::as_str) {
        it["subtitle"] = json!(r);
    }
    if let Some(p) = https_art(m.get("poster")) {
        it["art"] = json!({ "poster": p, "backdrop": https_art(m.get("background")) });
    }
    // Shown on the player's ⓘ card for whatever is played from this title.
    if let Some(d) = m.get("description").and_then(Value::as_str).filter(|d| !d.is_empty()) {
        it["description"] = json!(d);
    }
    let rating = m.get("imdbRating").and_then(Value::as_str).filter(|r| !r.is_empty()).map(|r| format!("★ {r}"));
    let genres = m.get("genres").or_else(|| m.get("genre")).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).take(3).map(str::to_string);
    let badges: Vec<String> = rating.into_iter().chain(genres).collect();
    if !badges.is_empty() {
        it["badges"] = json!(badges);
    }
    Some(it)
}

pub fn metas_items(body: &Value) -> Vec<Value> {
    body.get("metas").and_then(Value::as_array).into_iter().flatten().filter_map(meta_item).collect()
}

/// Series meta -> one section per season of episode items (each routes to its streams).
pub fn episodes_page(meta: &Value) -> Value {
    let m = meta.get("meta").unwrap_or(meta);
    let kind = m.get("type").and_then(Value::as_str).unwrap_or("series");
    let mut videos: Vec<&Value> = m.get("videos").and_then(Value::as_array).into_iter().flatten().collect();
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    videos.sort_by_key(|v| (num(v, "season") == 0, num(v, "season"), num(v, "episode")));
    let mut sections: Vec<(u64, Vec<Value>)> = Vec::new();
    for v in videos {
        let Some(vid) = v.get("id").and_then(Value::as_str) else { continue };
        let (s, e) = (num(v, "season"), num(v, "episode"));
        let name = v.get("name").or(v.get("title")).and_then(Value::as_str).unwrap_or("");
        let mut it = json!({
            "id": format!("{kind}:{vid}"), "kind": "episode",
            "title": if name.is_empty() { format!("Episode {e}") } else { format!("{e}. {name}") },
            "route": format!("streams:{kind}:{vid}")
        });
        if let Some(t) = https_art(v.get("thumbnail")) {
            it["art"] = json!({ "thumb": t });
        }
        match sections.last_mut() {
            Some((ss, items)) if *ss == s => items.push(it),
            _ => sections.push((s, vec![it])),
        }
    }
    json!({
        "title": m.get("name").and_then(Value::as_str).unwrap_or("Episodes"),
        "layout": "list",
        "sections": sections.into_iter().map(|(s, items)| json!({ "title": if s == 0 { "Specials".to_string() } else { format!("Season {s}") }, "items": items })).collect::<Vec<_>>()
    })
}

/// Stream list -> playable items (direct urls only) + count of torrent-only streams skipped.
/// The item id carries what `resolve` needs: {"u": url, "h": headers, "t": title}.
pub fn stream_items(addon_name: &str, body: &Value) -> (Vec<Value>, usize) {
    let (mut items, mut skipped) = (Vec::new(), 0);
    for s in body.get("streams").and_then(Value::as_array).into_iter().flatten() {
        let Some(url) = s.get("url").and_then(Value::as_str).filter(|u| u.contains("://")) else {
            skipped += 1; // infoHash / ytId / externalUrl: not directly playable here
            continue;
        };
        let line = |k: &str| s.get(k).and_then(Value::as_str).map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "));
        let name = line("name").unwrap_or_else(|| addon_name.to_string());
        let detail = line("title").or_else(|| line("description")).unwrap_or_default();
        let headers = s.pointer("/behaviorHints/proxyHeaders/request").cloned().unwrap_or(json!({}));
        let payload = json!({ "u": url, "h": headers, "t": detail.clone() }).to_string();
        let mut it = json!({ "id": format!("s:{payload}"), "kind": "stream", "title": name, "playable": true });
        if !detail.is_empty() {
            it["subtitle"] = json!(detail);
        }
        items.push(it);
    }
    (items, skipped)
}

/// `resolve` id -> Playback.
pub fn playback_from_id(id: &str) -> Result<Value, String> {
    let p: Value = serde_json::from_str(id.strip_prefix("s:").ok_or("not a stream id")?).map_err(|e| e.to_string())?;
    let url = p.get("u").and_then(Value::as_str).ok_or("stream id has no url")?;
    let mut pb = json!({ "url": url });
    if let Some(h) = p.get("h").filter(|h| h.as_object().is_some_and(|o| !o.is_empty())) {
        pb["headers"] = h.clone();
    }
    if let Some(t) = p.get("t").and_then(Value::as_str).filter(|t| !t.is_empty()) {
        pb["title"] = json!(t.lines().next().unwrap_or(t));
    }
    Ok(pb)
}

#[cfg(target_arch = "wasm32")]
mod exports {
    use super::*;
    use extism_pdk::*;
    use std::cell::RefCell;

    thread_local! {
        /// (addons setting, parsed addons): manifests fetched once per plugin instance.
        static ADDONS: RefCell<Option<(String, Vec<Addon>)>> = const { RefCell::new(None) };
    }

    fn get(url: &str) -> FnResult<Value> {
        let res = http::request::<()>(&HttpRequest::new(url).with_header("Accept", "application/json"), None)?;
        if res.status_code() >= 400 {
            return Err(Error::msg(format!("HTTP {} from {}", res.status_code(), url.split('/').take(3).collect::<Vec<_>>().join("/"))).into());
        }
        Ok(serde_json::from_slice(&res.body()).map_err(|e| Error::msg(format!("bad JSON from addon: {e}")))?)
    }

    fn addons() -> FnResult<Vec<Addon>> {
        let setting = config::get("addons")?.unwrap_or_default();
        if let Some(a) = ADDONS.with(|c| c.borrow().as_ref().filter(|(s, _)| *s == setting).map(|(_, a)| a.clone())) {
            return Ok(a);
        }
        let mut out = Vec::new();
        for url in setting.split([',', '\n', ' ']).map(str::trim).filter(|u| !u.is_empty()) {
            let url = url.replacen("stremio://", "https://", 1); // "Install" links use stremio://
            let base = base_of(&url);
            match get(&format!("{base}/manifest.json")) {
                Ok(m) => out.push(parse_manifest(&base, &m)),
                Err(e) => return Err(Error::msg(format!("Couldn't load addon {url}: {e:?}")).into()),
            }
        }
        if out.is_empty() {
            return Err(Error::msg("Add at least one addon manifest URL in this source's settings.").into());
        }
        ADDONS.with(|c| *c.borrow_mut() = Some((setting, out.clone())));
        Ok(out)
    }

    // ponytail: catalogs/streams fetched sequentially; parallel requests if home pages feel slow.
    fn home_page() -> FnResult<Value> {
        let addons = addons()?;
        let mut sections = Vec::new();
        for (ai, a) in addons.iter().enumerate() {
            for (ci, c) in a.catalogs.iter().enumerate().filter(|(_, c)| !c.needs_extra).take(6) {
                let Ok(body) = get(&catalog_url(a, c, None)) else { continue };
                let mut items = metas_items(&body);
                items.truncate(20);
                if !items.is_empty() {
                    // Section title doubles as "see all": first item is a folder into the full catalog.
                    items.insert(0, json!({ "id": format!("all:{ai}:{ci}"), "kind": "folder", "title": format!("All {} ›", c.name), "route": format!("cat:{ai}:{ci}") }));
                    sections.push(json!({ "title": format!("{} · {}", c.name, cap(&c.kind)), "items": items }));
                }
            }
        }
        if sections.is_empty() {
            return Err(Error::msg("None of your addons has a browsable catalog. Add a catalog addon such as Cinemeta.").into());
        }
        Ok(json!({ "title": "Stremio", "layout": "rows", "sections": sections }))
    }

    fn cap(s: &str) -> String {
        let mut c = s.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    }

    fn catalog_page(ai: usize, ci: usize, cursor: Option<&str>) -> FnResult<Value> {
        let addons = addons()?;
        let a = addons.get(ai).ok_or_else(|| Error::msg("addon list changed; go back"))?;
        let c = a.catalogs.get(ci).ok_or_else(|| Error::msg("catalog list changed; go back"))?;
        let skip: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
        let skip_s = skip.to_string();
        let url = catalog_url(a, c, (skip > 0).then_some(("skip", skip_s.as_str())));
        let items = metas_items(&get(&url)?);
        let mut page = json!({ "title": c.name, "layout": "grid", "sections": [{ "items": items }] });
        if !items.is_empty() {
            page["next"] = json!((skip + items.len()).to_string());
        }
        Ok(page)
    }

    fn meta_page(kind: &str, id: &str) -> FnResult<Value> {
        let addons = addons()?;
        let a = addons.iter().find(|a| a.meta && a.serves(kind, id)).ok_or_else(|| Error::msg("No addon provides episode lists for this title (add Cinemeta)."))?;
        Ok(episodes_page(&get(&format!("{}/meta/{}/{}.json", a.base, enc(kind), enc(id)))?))
    }

    fn streams_page(kind: &str, id: &str) -> FnResult<Value> {
        let addons = addons()?;
        let (mut sections, mut skipped, mut asked) = (Vec::new(), 0, 0);
        for a in addons.iter().filter(|a| a.stream && a.serves(kind, id)) {
            asked += 1;
            let Ok(body) = get(&format!("{}/stream/{}/{}.json", a.base, enc(kind), enc(id))) else { continue };
            let (items, s) = stream_items(&a.name, &body);
            skipped += s;
            if !items.is_empty() {
                sections.push(json!({ "title": a.name, "items": items }));
            }
        }
        if asked == 0 {
            return Err(Error::msg("No stream addon installed. Add a stream addon manifest URL in settings.").into());
        }
        if skipped > 0 {
            sections.push(json!({ "title": format!("{skipped} torrent stream(s) hidden — use a debrid-enabled addon to play them"), "items": [] }));
        }
        Ok(json!({ "title": "Streams", "layout": "list", "sections": sections }))
    }

    fn browse_route(route: &str, cursor: Option<&str>) -> FnResult<Value> {
        let parts: Vec<&str> = route.splitn(3, ':').collect();
        match parts.as_slice() {
            [""] => home_page(),
            ["cat", ai, ci] => catalog_page(ai.parse().unwrap_or(usize::MAX), ci.parse().unwrap_or(usize::MAX), cursor),
            ["meta", kind, id] => meta_page(kind, id),
            ["streams", kind, id] => streams_page(kind, id),
            _ => Err(Error::msg(format!("unknown route {route}")).into()),
        }
    }

    fn arg<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
        v.get(k).and_then(Value::as_str)
    }

    #[plugin_fn]
    pub fn home(_: Json<Value>) -> FnResult<Json<Value>> {
        Ok(Json(home_page()?))
    }

    #[plugin_fn]
    pub fn browse(Json(a): Json<Value>) -> FnResult<Json<Value>> {
        Ok(Json(browse_route(arg(&a, "route").unwrap_or(""), arg(&a, "cursor"))?))
    }

    #[plugin_fn]
    pub fn search(Json(a): Json<Value>) -> FnResult<Json<Value>> {
        let q = arg(&a, "query").unwrap_or("").trim().to_string();
        let addons = addons()?;
        let mut sections = Vec::new();
        for a in &addons {
            for c in a.catalogs.iter().filter(|c| c.search) {
                let Ok(body) = get(&catalog_url(a, c, Some(("search", &q)))) else { continue };
                let items = metas_items(&body);
                if !items.is_empty() {
                    sections.push(json!({ "title": format!("{} · {}", c.name, c.kind), "items": items }));
                }
            }
        }
        Ok(Json(json!({ "title": format!("“{q}”"), "layout": "rows", "sections": sections })))
    }

    #[plugin_fn]
    pub fn resolve(Json(a): Json<Value>) -> FnResult<Json<Value>> {
        let id = arg(&a, "id").ok_or_else(|| Error::msg("missing id"))?;
        Ok(Json(playback_from_id(id).map_err(Error::msg)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cinemeta() -> Value {
        json!({
            "id": "com.linvo.cinemeta", "name": "Cinemeta", "types": ["movie", "series"], "idPrefixes": ["tt"],
            "resources": ["catalog", "meta"],
            "catalogs": [
                { "type": "movie", "id": "top", "name": "Popular", "extra": [{ "name": "search" }, { "name": "skip" }] },
                { "type": "series", "id": "top", "name": "Popular", "extraSupported": ["search"] },
                { "type": "movie", "id": "by-genre", "name": "Genre", "extra": [{ "name": "genre", "isRequired": true }] }
            ]
        })
    }

    #[test]
    fn manifests() {
        assert_eq!(base_of("https://h/cfg=1/manifest.json"), "https://h/cfg=1");
        let c = parse_manifest("https://c", &cinemeta());
        assert!(c.meta && !c.stream);
        assert_eq!(c.catalogs.len(), 3);
        assert!(c.catalogs[0].search && c.catalogs[1].search && !c.catalogs[2].search);
        assert!(c.catalogs[2].needs_extra && !c.catalogs[0].needs_extra);
        let t = parse_manifest("https://t", &json!({ "name": "Torrentio", "types": ["movie", "series"],
            "resources": [{ "name": "stream", "types": ["movie", "series"], "idPrefixes": ["tt", "kitsu"] }], "catalogs": [] }));
        assert!(t.stream && t.serves("series", "tt0944947:1:1") && !t.serves("series", "yt:abc") && !t.serves("tv", "tt1"));
    }

    #[test]
    fn urls() {
        let a = parse_manifest("https://c", &cinemeta());
        assert_eq!(catalog_url(&a, &a.catalogs[0], None), "https://c/catalog/movie/top.json");
        assert_eq!(catalog_url(&a, &a.catalogs[0], Some(("search", "lord of the rings"))), "https://c/catalog/movie/top/search=lord%20of%20the%20rings.json");
        assert_eq!(catalog_url(&a, &a.catalogs[0], Some(("skip", "100"))), "https://c/catalog/movie/top/skip=100.json");
        assert_eq!(enc("tt0944947:1:1"), "tt0944947:1:1");
    }

    #[test]
    fn catalog_items() {
        let body = json!({ "metas": [
            { "id": "tt0111161", "type": "movie", "name": "The Shawshank Redemption", "poster": "https://img/p.jpg", "releaseInfo": "1994",
              "description": "Two imprisoned men bond.", "imdbRating": "9.3", "genres": ["Drama", "Crime"], "background": "file:///x" },
            { "id": "tt0944947", "type": "series", "name": "Game of Thrones", "poster": "file:///x" },
            { "type": "movie", "name": "no id" }
        ]});
        let items = metas_items(&body);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["route"], "streams:movie:tt0111161");
        assert_eq!(items[0]["art"]["poster"], "https://img/p.jpg");
        assert_eq!(items[0]["subtitle"], "1994");
        assert_eq!(items[0]["description"], "Two imprisoned men bond.");
        assert_eq!(items[0]["badges"], json!(["★ 9.3", "Drama", "Crime"]));
        assert_eq!(items[0]["art"]["backdrop"], Value::Null, "non-http background dropped");
        assert_eq!(items[1]["route"], "meta:series:tt0944947");
        assert!(items[1].get("art").is_none(), "non-http poster dropped");
    }

    #[test]
    fn episodes_grouped_by_season() {
        let meta = json!({ "meta": { "type": "series", "name": "GoT", "videos": [
            { "id": "tt0944947:2:1", "season": 2, "episode": 1, "name": "The North Remembers" },
            { "id": "tt0944947:1:2", "season": 1, "episode": 2, "title": "The Kingsroad" },
            { "id": "tt0944947:1:1", "season": 1, "episode": 1, "name": "Winter Is Coming", "thumbnail": "https://t/1.jpg" },
            { "id": "tt0944947:0:1", "season": 0, "episode": 1, "name": "Making of" }
        ]}});
        let p = episodes_page(&meta);
        let titles: Vec<&str> = p["sections"].as_array().unwrap().iter().map(|s| s["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Season 1", "Season 2", "Specials"]);
        assert_eq!(p["sections"][0]["items"][0]["title"], "1. Winter Is Coming");
        assert_eq!(p["sections"][0]["items"][0]["route"], "streams:series:tt0944947:1:1");
        assert_eq!(p["sections"][0]["items"][1]["title"], "2. The Kingsroad");
    }

    #[test]
    fn streams_and_resolve() {
        let body = json!({ "streams": [
            { "name": "RD+\n2160p", "title": "Movie.2160p.mkv\n💾 12 GB", "url": "https://rd.example/dl/abc.mkv",
              "behaviorHints": { "proxyHeaders": { "request": { "User-Agent": "Stremio", "Referer": "https://x/" } } } },
            { "name": "Torrentio\n1080p", "title": "Movie.1080p", "infoHash": "abcd", "fileIdx": 0 },
            { "name": "YT", "ytId": "dQw4w9WgXcQ" }
        ]});
        let (items, skipped) = stream_items("Torrentio", &body);
        assert_eq!((items.len(), skipped), (1, 2));
        assert_eq!(items[0]["title"], "RD+ 2160p");
        assert_eq!(items[0]["subtitle"], "Movie.2160p.mkv 💾 12 GB");
        let pb = playback_from_id(items[0]["id"].as_str().unwrap()).unwrap();
        assert_eq!(pb["url"], "https://rd.example/dl/abc.mkv");
        assert_eq!(pb["headers"]["User-Agent"], "Stremio");
        assert_eq!(pb["title"], "Movie.2160p.mkv 💾 12 GB");
        assert!(playback_from_id("tt123").is_err());
    }
}
