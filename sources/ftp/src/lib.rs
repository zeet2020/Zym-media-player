//! Zym Player source plugin: FTP / SFTP. Spec: docs/plugins.md (api 1).
//! Sandboxed plugins have no sockets, so folder listing goes through the player's `zym_list_dir`
//! host function (which enforces this manifest's `hosts`). mpv plays ftp:// and sftp:// itself.

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    pub scheme: String,
    /// `host` or `host:port` (as written by the user).
    pub authority: String,
    /// Start folder, decoded, always starting with '/'.
    pub root: String,
}

pub fn parse_server(url: &str) -> Result<Server, String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://").ok_or("Server URL must start with ftp:// or sftp://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "ftp" && scheme != "sftp" {
        return Err("Server URL must start with ftp:// or sftp://".into());
    }
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    if authority.is_empty() || authority.contains('@') {
        return Err("Put the host in the URL and the login in the Username/Password fields".into());
    }
    let root = if path.is_empty() { "/".into() } else { path.to_string() };
    Ok(Server { scheme, authority: authority.to_string(), root })
}

/// RFC 3986 percent-encoding; `keep` lists extra characters left as-is (e.g. '/').
pub fn encode(s: &str, keep: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ if keep.as_bytes().contains(&b) => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn join(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), name)
}

/// Listing URL for the host function (no credentials; they travel separately).
pub fn list_url(s: &Server, path: &str) -> String {
    format!("{}://{}{}", s.scheme, s.authority, encode(path, "/"))
}

/// Playback URL for mpv: credentials in userinfo (mpv/FFmpeg take them only there).
pub fn play_url(s: &Server, path: &str, user: &str, pass: &str) -> String {
    let auth = match (user.is_empty(), pass.is_empty()) {
        (true, _) => String::new(),
        (false, true) => format!("{}@", encode(user, "")),
        (false, false) => format!("{}:{}@", encode(user, ""), encode(pass, "")),
    };
    format!("{}://{}{}{}", s.scheme, auth, s.authority, encode(path, "/"))
}

const MEDIA: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "wmv", "flv", "mpg", "mpeg", "ts", "m2ts", "3gp", "ogv",
    "mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "wma",
];

fn ext(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
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

/// Host-function reply -> Page. Folders first, media files only, hidden dot-files skipped.
pub fn listing_page(dir: &str, reply: &Value) -> Result<Value, String> {
    if let Some(e) = reply.get("error").and_then(Value::as_str) {
        return Err(e.to_string());
    }
    let mut rows: Vec<(String, bool, Option<u64>)> = reply
        .get("entries")
        .and_then(Value::as_array)
        .ok_or("listing reply has no entries")?
        .iter()
        .filter_map(|e| {
            let name = e.get("name")?.as_str()?.to_string();
            let is_dir = e.get("is_dir").and_then(Value::as_bool).unwrap_or(false);
            (!name.starts_with('.') && (is_dir || MEDIA.contains(&ext(&name).as_str())))
                .then(|| (name, is_dir, e.get("size").and_then(Value::as_u64)))
        })
        .collect();
    rows.sort_by_key(|(n, d, _)| (!*d, n.to_lowercase()));
    let items: Vec<Value> = rows
        .iter()
        .map(|(name, is_dir, size)| {
            let path = join(dir, name);
            if *is_dir {
                json!({ "id": path, "kind": "folder", "title": name, "route": path })
            } else {
                let mut badges = vec![ext(name).to_uppercase()];
                badges.extend(size.map(human));
                json!({ "id": path, "kind": "stream", "title": name, "badges": badges, "playable": true })
            }
        })
        .collect();
    let title = dir.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("/");
    Ok(json!({ "title": title, "layout": "list", "sections": [{ "items": items }] }))
}

#[cfg(target_arch = "wasm32")]
mod exports {
    use super::*;
    use extism_pdk::*;

    #[host_fn]
    extern "ExtismHost" {
        fn zym_list_dir(req: String) -> String;
    }

    struct Conf {
        server: Server,
        user: String,
        pass: String,
    }

    fn conf() -> FnResult<Conf> {
        let url = config::get("url")?.ok_or_else(|| Error::msg("Set the server URL (sftp:// or ftp://) in this source's settings."))?;
        let server = parse_server(&url).map_err(Error::msg)?;
        Ok(Conf {
            server,
            user: config::get("username")?.unwrap_or_default().trim().to_string(),
            pass: config::get("password")?.unwrap_or_default(),
        })
    }

    fn page(route: &str) -> FnResult<Value> {
        let c = conf()?;
        let dir = if route.is_empty() { c.server.root.clone() } else { route.to_string() };
        let req = json!({ "url": list_url(&c.server, &dir), "username": c.user, "password": c.pass });
        let reply: Value = serde_json::from_str(&unsafe { zym_list_dir(req.to_string())? })?;
        Ok(listing_page(&dir, &reply).map_err(Error::msg)?)
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
        let path = a.get("id").and_then(Value::as_str).ok_or_else(|| Error::msg("missing id"))?;
        let title = path.rsplit('/').next().unwrap_or(path);
        Ok(Json(json!({ "url": play_url(&c.server, path, &c.user, &c.pass), "title": title })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls() {
        let s = parse_server("sftp://nas.local:2222/Media").unwrap();
        assert_eq!((s.scheme.as_str(), s.authority.as_str(), s.root.as_str()), ("sftp", "nas.local:2222", "/Media"));
        assert_eq!(parse_server("ftp://h").unwrap().root, "/");
        assert!(parse_server("http://h/").is_err());
        assert!(parse_server("sftp://me@h/").is_err());
    }

    #[test]
    fn urls_encode_paths_and_credentials() {
        let s = parse_server("sftp://nas:22/").unwrap();
        assert_eq!(list_url(&s, "/My Movies/Up & Away.mkv"), "sftp://nas:22/My%20Movies/Up%20%26%20Away.mkv");
        assert_eq!(play_url(&s, "/a b.mkv", "me", "p@ss:w/rd"), "sftp://me:p%40ss%3Aw%2Frd@nas:22/a%20b.mkv");
        assert_eq!(play_url(&s, "/x.mp4", "", "ignored"), "sftp://nas:22/x.mp4");
        assert_eq!(play_url(&s, "/x.mp4", "me", ""), "sftp://me@nas:22/x.mp4");
    }

    #[test]
    fn page_from_reply() {
        let reply = json!({ "entries": [
            { "name": "Shows", "is_dir": true },
            { "name": ".hidden", "is_dir": true },
            { "name": "b.mkv", "is_dir": false, "size": 1048576 },
            { "name": "notes.txt", "is_dir": false, "size": 3 },
            { "name": "A.mp4", "is_dir": false }
        ]});
        let p = listing_page("/media/", &reply).unwrap();
        let items = p["sections"][0]["items"].as_array().unwrap();
        let titles: Vec<&str> = items.iter().map(|i| i["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Shows", "A.mp4", "b.mkv"]);
        assert_eq!(items[0]["route"], "/media/Shows");
        assert_eq!(items[2]["badges"], json!(["MKV", "1.0 MB"]));
        assert_eq!(p["title"], "media");
        assert_eq!(listing_page("/", &json!({ "error": "SFTP login failed" })).unwrap_err(), "SFTP login failed");
    }
}
