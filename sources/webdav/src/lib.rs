//! Zym Player source plugin: browse a WebDAV server, play media files. Spec: docs/plugins.md (api 1).
//! Folders come from `PROPFIND Depth: 1`; playback passes the same Basic auth header to the player.

use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;
use serde_json::{Value, json};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Entry {
    /// Path as the server sent it (percent-encoded), absolute URLs reduced to their path.
    pub href: String,
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

/// Parse a `207 Multi-Status` body. Prefix-agnostic (`D:`, `d:`, `lp1:` … all seen in the wild).
pub fn parse_multistatus(xml: &str) -> Result<Vec<Entry>, String> {
    let mut r = Reader::from_str(xml);
    let (mut out, mut cur, mut field, mut text) = (Vec::new(), None::<Entry>, String::new(), String::new());
    loop {
        match r.read_event().map_err(|e| format!("bad WebDAV XML: {e}"))? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_ascii_lowercase();
                match name.as_str() {
                    "response" => cur = Some(Entry::default()),
                    "collection" => cur.iter_mut().for_each(|c| c.is_dir = true),
                    _ => {}
                }
                field = name;
                text.clear();
            }
            Event::Empty(e) => {
                if e.local_name().as_ref().eq_ignore_ascii_case(b"collection") {
                    cur.iter_mut().for_each(|c| c.is_dir = true);
                }
            }
            Event::Text(t) => text.push_str(&t.decode().map_err(|e| e.to_string())?),
            Event::CData(t) => text.push_str(&String::from_utf8_lossy(&t)),
            Event::GeneralRef(g) => {
                if let Ok(Some(c)) = g.resolve_char_ref() {
                    text.push(c);
                } else if let Some(s) = resolve_predefined_entity(&g.decode().unwrap_or_default()) {
                    text.push_str(s);
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_ascii_lowercase();
                if let Some(c) = cur.as_mut() {
                    let t = text.trim();
                    match name.as_str() {
                        "href" if c.href.is_empty() => c.href = path_only(t).to_string(),
                        "displayname" if !t.is_empty() => c.name = t.to_string(),
                        "getcontentlength" => c.size = t.parse().ok(),
                        "response" => {
                            let mut e = cur.take().unwrap();
                            if e.name.is_empty() {
                                e.name = percent_decode(e.href.trim_end_matches('/').rsplit('/').next().unwrap_or(""));
                            }
                            out.push(e);
                        }
                        _ => {}
                    }
                }
                if name == field {
                    text.clear();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// `https://host:port/a/b` -> `/a/b`; a bare path is returned as is.
pub fn path_only(href: &str) -> &str {
    match href.find("://") {
        Some(i) => href[i + 3..].find('/').map_or("/", |j| &href[i + 3 + j..]),
        None => href,
    }
}

/// `https://host:port/dav/` -> `https://host:port`
pub fn origin(base: &str) -> &str {
    let start = base.find("://").map_or(0, |i| i + 3);
    base[start..].find('/').map_or(base, |j| &base[..start + j])
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c))) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn same_dir(a: &str, b: &str) -> bool {
    percent_decode(a.trim_end_matches('/')) == percent_decode(b.trim_end_matches('/'))
}

pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, &b)| acc | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

pub fn basic_auth(user: &str, pass: &str) -> Option<String> {
    (!user.is_empty()).then(|| format!("Basic {}", base64(format!("{user}:{pass}").as_bytes())))
}

const MEDIA: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "wmv", "flv", "mpg", "mpeg", "ts", "m2ts", "3gp", "ogv", "m3u8",
    "mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "wma", "alac",
];

fn ext(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

pub fn is_media(name: &str) -> bool {
    MEDIA.contains(&ext(name).as_str())
}

fn human(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut u) = (n as f64, 0);
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", units[u]) }
}

/// Folder listing -> Page. `dir` is the listed path; its own entry is skipped. Non-media files hidden.
// ponytail: whole folder in one page; add `next` paging if huge folders show up.
pub fn listing_page(title: &str, dir: &str, entries: &[Entry]) -> Value {
    let mut rows: Vec<&Entry> = entries.iter().filter(|e| !same_dir(&e.href, dir)).filter(|e| e.is_dir || is_media(&e.name)).collect();
    rows.sort_by_key(|e| (!e.is_dir, e.name.to_lowercase()));
    let items: Vec<Value> = rows
        .iter()
        .map(|e| {
            if e.is_dir {
                json!({ "id": e.href, "kind": "folder", "title": e.name, "route": e.href })
            } else {
                let mut badges = vec![ext(&e.name).to_uppercase()];
                badges.extend(e.size.map(human));
                json!({ "id": e.href, "kind": "stream", "title": e.name, "badges": badges, "playable": true })
            }
        })
        .collect();
    let hidden = entries.len().saturating_sub(rows.len() + 1);
    let mut page = json!({ "title": title, "layout": "list", "sections": [{ "items": items }] });
    if hidden > 0 && rows.is_empty() {
        page["sections"][0]["title"] = json!(format!("No media files ({hidden} other files hidden)"));
    }
    page
}

pub fn dir_title(dir: &str) -> String {
    let last = percent_decode(dir.trim_end_matches('/').rsplit('/').next().unwrap_or(""));
    if last.is_empty() { "WebDAV".into() } else { last }
}

pub fn playback(base: &str, href: &str, auth: Option<String>) -> Value {
    let url = format!("{}{}", origin(base), path_only(href));
    let name = percent_decode(href.rsplit('/').next().unwrap_or(href));
    let mut pb = json!({ "url": url, "title": name });
    if let Some(a) = auth {
        pb["headers"] = json!({ "Authorization": a });
    }
    pb
}

pub const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:displayname/><d:resourcetype/><d:getcontentlength/></d:prop></d:propfind>"#;

// ---------------------------------------------------------------- wasm exports
#[cfg(target_arch = "wasm32")]
mod exports {
    use super::*;
    use extism_pdk::*;

    struct Conf {
        base: String,
        auth: Option<String>,
    }

    fn conf() -> FnResult<Conf> {
        let base = config::get("url")?
            .map(|u| u.trim().to_string())
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
            .ok_or_else(|| Error::msg("Set the server URL (http:// or https://) in this source's settings."))?;
        let user = config::get("username")?.unwrap_or_default();
        let pass = config::get("password")?.unwrap_or_default();
        Ok(Conf { base, auth: basic_auth(user.trim(), &pass) })
    }

    fn list(c: &Conf, dir: &str) -> FnResult<Vec<Entry>> {
        let mut req = HttpRequest::new(format!("{}{}", origin(&c.base), dir))
            .with_method("PROPFIND")
            .with_header("Depth", "1")
            .with_header("Content-Type", "application/xml; charset=utf-8");
        if let Some(a) = &c.auth {
            req = req.with_header("Authorization", a);
        }
        let res = http::request::<&str>(&req, Some(PROPFIND_BODY))?;
        match res.status_code() {
            207 | 200 => Ok(parse_multistatus(&String::from_utf8_lossy(&res.body())).map_err(Error::msg)?),
            401 | 403 => Err(Error::msg("Login failed — check username and password in settings.").into()),
            404 => Err(Error::msg(format!("Folder not found: {}", percent_decode(dir))).into()),
            405 | 501 => Err(Error::msg("This server doesn't speak WebDAV at that URL.").into()),
            s => Err(Error::msg(format!("WebDAV request failed: HTTP {s}")).into()),
        }
    }

    fn page(dir: &str) -> FnResult<Value> {
        let c = conf()?;
        let dir = if dir.is_empty() { path_only(&c.base).to_string() } else { dir.to_string() };
        let dir = if dir.ends_with('/') { dir } else { format!("{dir}/") };
        Ok(listing_page(&dir_title(&dir), &dir, &list(&c, &dir)?))
    }

    #[plugin_fn]
    pub fn home(_: Json<Value>) -> FnResult<Json<Value>> {
        Ok(Json(page("")?))
    }

    #[plugin_fn]
    pub fn browse(Json(a): Json<Value>) -> FnResult<Json<Value>> {
        Ok(Json(page(a.get("route").and_then(Value::as_str).unwrap_or(""))?))
    }

    #[plugin_fn]
    pub fn resolve(Json(a): Json<Value>) -> FnResult<Json<Value>> {
        let c = conf()?;
        let href = a.get("id").and_then(Value::as_str).ok_or_else(|| Error::msg("missing id"))?;
        Ok(Json(playback(&c.base, href, c.auth)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Apache mod_dav style (D: prefix, displayname absent, encoded names, entity in name).
    const APACHE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
<D:response><D:href>/media/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:propstat></D:response>
<D:response><D:href>/media/Kids%20Shows/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:propstat></D:response>
<D:response><D:href>/media/Tom%20%26%20Jerry.mkv</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>1572864000</D:getcontentlength></D:prop></D:propstat></D:response>
<D:response><D:href>/media/notes.txt</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>12</D:getcontentlength></D:prop></D:propstat></D:response>
<D:response><D:href>/media/a-song.mp3</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>2048</D:getcontentlength></D:prop></D:propstat></D:response>
</D:multistatus>"#;

    // Nextcloud style (d: prefix, absolute href, displayname with entity, empty displayname on self).
    const NEXTCLOUD: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
 <d:response><d:href>https://cloud.example.com/remote.php/dav/files/me/Movies/</d:href>
  <d:propstat><d:prop><d:displayname></d:displayname><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/files/me/Movies/Up.mp4</d:href>
  <d:propstat><d:prop><d:displayname>Up &amp; Away.mp4</d:displayname><d:resourcetype/><d:getcontentlength>99</d:getcontentlength></d:prop></d:propstat></d:response>
</d:multistatus>"#;

    #[test]
    fn parses_apache_listing() {
        let e = parse_multistatus(APACHE).unwrap();
        assert_eq!(e.len(), 5);
        assert!(e[0].is_dir && e[1].is_dir && !e[2].is_dir);
        assert_eq!(e[1].name, "Kids Shows");
        assert_eq!(e[2].name, "Tom & Jerry.mkv"); // %26 decoded
        assert_eq!(e[2].size, Some(1572864000));
    }

    #[test]
    fn parses_nextcloud_listing() {
        let e = parse_multistatus(NEXTCLOUD).unwrap();
        assert_eq!(e[0].href, "/remote.php/dav/files/me/Movies/"); // absolute href reduced to path
        assert_eq!(e[0].name, "Movies"); // empty displayname -> from href
        assert_eq!(e[1].name, "Up & Away.mp4"); // &amp; resolved
    }

    #[test]
    fn page_skips_self_hides_non_media_sorts_folders_first() {
        let p = listing_page("media", "/media/", &parse_multistatus(APACHE).unwrap());
        let items = p["sections"][0]["items"].as_array().unwrap();
        let titles: Vec<&str> = items.iter().map(|i| i["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Kids Shows", "a-song.mp3", "Tom & Jerry.mkv"]);
        assert_eq!(items[0]["route"], "/media/Kids%20Shows/");
        assert_eq!(items[2]["badges"], json!(["MKV", "1.5 GB"]));
        assert_eq!(items[2]["playable"], true);
    }

    #[test]
    fn urls_and_auth() {
        assert_eq!(origin("https://h:8443/dav/x/"), "https://h:8443");
        assert_eq!(origin("http://h"), "http://h");
        assert_eq!(path_only("https://h:8443/dav/x/"), "/dav/x/");
        assert_eq!(path_only("https://h"), "/");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"Aladdin:open sesame"), "QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        assert_eq!(basic_auth("", "x"), None);
        let pb = playback("https://h/dav/", "/dav/Tom%20%26%20Jerry.mkv", basic_auth("a", "b"));
        assert_eq!(pb["url"], "https://h/dav/Tom%20%26%20Jerry.mkv");
        assert_eq!(pb["title"], "Tom & Jerry.mkv");
        assert_eq!(pb["headers"]["Authorization"], "Basic YTpi");
        assert_eq!(dir_title("/dav/Kids%20Shows/"), "Kids Shows");
        assert_eq!(dir_title("/"), "WebDAV");
    }
}
