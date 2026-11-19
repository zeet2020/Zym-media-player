//! Zym Player source plugin: IPTV channels from an M3U/M3U8 playlist.
//! Spec: docs/plugins.md (api 1). Groups (`group-title`) become folders; channels are `live` items.

use serde_json::{Value, json};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Channel {
    pub name: String,
    pub url: String,
    pub logo: Option<String>,
    pub group: Option<String>,
    pub user_agent: Option<String>,
    pub referrer: Option<String>,
    /// Extra headers from Kodi-style `url|Header=Value&...` suffixes.
    pub headers: Vec<(String, String)>,
}

/// `key="value"` attributes of an `#EXTINF` line, plus the display name after the first unquoted comma.
fn parse_extinf(line: &str) -> (Vec<(String, String)>, String) {
    let body = line.trim_start_matches("#EXTINF:");
    let (mut attrs, mut in_q, mut name_at) = (Vec::new(), false, None);
    for (i, c) in body.char_indices() {
        match c {
            '"' => in_q = !in_q,
            ',' if !in_q && name_at.is_none() => name_at = Some(i), // name = after FIRST unquoted comma
            _ => {}
        }
    }
    let (head, name) = match name_at {
        Some(i) => (&body[..i], body[i + 1..].trim().to_string()),
        None => (body, String::new()),
    };
    let mut rest = head;
    while let Some(eq) = rest.find("=\"") {
        let key = rest[..eq].rsplit(|c: char| c.is_whitespace()).next().unwrap_or("").to_string();
        let after = &rest[eq + 2..];
        let Some(end) = after.find('"') else { break };
        attrs.push((key.to_ascii_lowercase(), after[..end].to_string()));
        rest = &after[end + 1..];
    }
    (attrs, name)
}

pub fn parse(text: &str) -> Vec<Channel> {
    let mut out = Vec::new();
    let mut cur: Option<Channel> = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("#EXTINF:") {
            let (attrs, name) = parse_extinf(line);
            let get = |k: &str| attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
            cur = Some(Channel {
                name: if name.is_empty() { get("tvg-name").unwrap_or_default() } else { name },
                logo: get("tvg-logo"),
                group: get("group-title"),
                user_agent: get("user-agent").or_else(|| get("http-user-agent")),
                referrer: get("referrer").or_else(|| get("http-referrer")),
                ..Default::default()
            });
        } else if let Some(g) = line.strip_prefix("#EXTGRP:") {
            if let Some(c) = cur.as_mut().filter(|c| c.group.is_none()) {
                c.group = Some(g.trim().to_string());
            }
        } else if let Some(opt) = line.strip_prefix("#EXTVLCOPT:") {
            if let (Some(c), Some((k, v))) = (cur.as_mut(), opt.split_once('=')) {
                match k.trim() {
                    "http-user-agent" => c.user_agent = Some(v.trim().to_string()),
                    "http-referrer" => c.referrer = Some(v.trim().to_string()),
                    _ => {}
                }
            }
        } else if !line.starts_with('#') {
            let mut c = cur.take().unwrap_or_default();
            // Kodi style: url|User-Agent=foo&Referer=bar
            let (url, extra) = line.split_once('|').unwrap_or((line, ""));
            c.url = url.trim().to_string();
            for (k, v) in extra.split('&').filter_map(|kv| kv.split_once('=')) {
                match k.to_ascii_lowercase().as_str() {
                    "user-agent" => c.user_agent = Some(v.to_string()),
                    "referer" | "referrer" => c.referrer = Some(v.to_string()),
                    _ => c.headers.push((k.to_string(), v.to_string())),
                }
            }
            if c.name.is_empty() {
                c.name = c.url.rsplit('/').next().unwrap_or(&c.url).to_string();
            }
            if is_stream_url(&c.url) {
                out.push(c);
            }
        }
    }
    out
}

/// Only network streams: the player rejects anything else anyway (spec §4 Playback).
fn is_stream_url(u: &str) -> bool {
    ["http://", "https://", "rtmp://", "rtmps://", "rtsp://", "rtp://", "udp://", "srt://", "mms://"]
        .iter()
        .any(|s| u.to_ascii_lowercase().starts_with(s))
}

const PAGE: usize = 300;

fn channel_item(i: usize, c: &Channel) -> Value {
    let mut item = json!({
        "id": format!("ch:{i}"),
        "kind": "live",
        "title": c.name,
        "playable": true,
    });
    if let Some(g) = &c.group {
        item["subtitle"] = json!(g);
    }
    if let Some(logo) = c.logo.as_ref().filter(|l| l.starts_with("http://") || l.starts_with("https://")) {
        item["art"] = json!({ "logo": logo, "thumb": logo });
    }
    item
}

/// One page of `channels` (indices into the full list), `PAGE` at a time.
pub fn channel_page(title: &str, all: &[Channel], picks: Vec<usize>, cursor: Option<&str>) -> Value {
    let start: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
    let items: Vec<Value> = picks.iter().skip(start).take(PAGE).map(|&i| channel_item(i, &all[i])).collect();
    let mut page = json!({ "title": title, "layout": "channels", "sections": [{ "items": items }] });
    if start + PAGE < picks.len() {
        page["next"] = json!((start + PAGE).to_string());
    }
    page
}

/// Home: one folder per group, or the channel list directly if there's only one group.
pub fn home_page(all: &[Channel]) -> Value {
    let mut groups: Vec<(String, usize)> = Vec::new();
    for c in all {
        let g = c.group.clone().unwrap_or_else(|| "Ungrouped".into());
        match groups.iter_mut().find(|(n, _)| *n == g) {
            Some(e) => e.1 += 1,
            None => groups.push((g, 1)),
        }
    }
    if groups.len() <= 1 {
        return channel_page("Channels", all, (0..all.len()).collect(), None);
    }
    let mut items = vec![json!({
        "id": "all", "kind": "folder", "title": "All channels",
        "subtitle": format!("{} channels", all.len()), "route": "all"
    })];
    items.extend(groups.iter().map(|(g, n)| {
        json!({ "id": format!("group:{g}"), "kind": "folder", "title": g,
                "subtitle": format!("{n} channels"), "route": format!("group:{g}") })
    }));
    json!({ "title": "IPTV", "layout": "list", "sections": [{ "items": items }] })
}

pub fn browse_page(all: &[Channel], route: &str, cursor: Option<&str>) -> Value {
    match route {
        "" => match cursor {
            None => home_page(all),
            Some(_) => channel_page("Channels", all, (0..all.len()).collect(), cursor),
        },
        "all" => channel_page("All channels", all, (0..all.len()).collect(), cursor),
        r => {
            let g = r.strip_prefix("group:").unwrap_or(r);
            let picks = (0..all.len()).filter(|&i| all[i].group.as_deref().unwrap_or("Ungrouped") == g).collect();
            channel_page(g, all, picks, cursor)
        }
    }
}

pub fn search_page(all: &[Channel], query: &str, cursor: Option<&str>) -> Value {
    let q = query.to_lowercase();
    let picks = (0..all.len()).filter(|&i| all[i].name.to_lowercase().contains(&q)).collect();
    channel_page(&format!("Search: {query}"), all, picks, cursor)
}

pub fn playback(c: &Channel) -> Value {
    let mut headers = serde_json::Map::new();
    if let Some(ua) = &c.user_agent {
        headers.insert("User-Agent".into(), json!(ua));
    }
    if let Some(r) = &c.referrer {
        headers.insert("Referer".into(), json!(r));
    }
    for (k, v) in &c.headers {
        headers.insert(k.clone(), json!(v));
    }
    let mut pb = json!({ "url": c.url, "title": c.name });
    if !headers.is_empty() {
        pb["headers"] = Value::Object(headers);
    }
    pb
}

// ---------------------------------------------------------------- wasm exports
#[cfg(target_arch = "wasm32")]
mod exports {
    use super::*;
    use extism_pdk::*;
    use std::cell::RefCell;

    thread_local! {
        /// (playlist url, parsed channels). Plugin instance lives across calls, so this caches the fetch.
        static CACHE: RefCell<Option<(String, Vec<Channel>)>> = const { RefCell::new(None) };
    }

    fn with_channels<T>(f: impl FnOnce(&[Channel]) -> T) -> FnResult<T> {
        let url = config::get("playlist_url")?
            .filter(|u| !u.trim().is_empty())
            .ok_or_else(|| Error::msg("Set the playlist URL in this source's settings."))?;
        let cached = CACHE.with(|c| c.borrow().as_ref().is_some_and(|(u, _)| *u == url));
        if !cached {
            let res = http::request::<()>(&HttpRequest::new(&url), None)?;
            if res.status_code() >= 400 {
                return Err(Error::msg(format!("Playlist download failed: HTTP {}", res.status_code())).into());
            }
            let channels = parse(&String::from_utf8_lossy(&res.body()));
            if channels.is_empty() {
                return Err(Error::msg("No playable channels found in the playlist.").into());
            }
            CACHE.with(|c| *c.borrow_mut() = Some((url, channels)));
        }
        Ok(CACHE.with(|c| f(&c.borrow().as_ref().unwrap().1)))
    }

    fn arg<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
        v.get(k).and_then(Value::as_str)
    }

    #[plugin_fn]
    pub fn home(_: Json<Value>) -> FnResult<Json<Value>> {
        Ok(Json(with_channels(home_page)?))
    }

    #[plugin_fn]
    pub fn browse(Json(a): Json<Value>) -> FnResult<Json<Value>> {
        Ok(Json(with_channels(|all| browse_page(all, arg(&a, "route").unwrap_or(""), arg(&a, "cursor")))?))
    }

    #[plugin_fn]
    pub fn search(Json(a): Json<Value>) -> FnResult<Json<Value>> {
        Ok(Json(with_channels(|all| search_page(all, arg(&a, "query").unwrap_or(""), arg(&a, "cursor")))?))
    }

    #[plugin_fn]
    pub fn resolve(Json(a): Json<Value>) -> FnResult<Json<Value>> {
        let idx: usize = arg(&a, "id")
            .and_then(|id| id.strip_prefix("ch:"))
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| Error::msg("Unknown channel id"))?;
        with_channels(|all| all.get(idx).map(playback))?
            .map(Json)
            .ok_or_else(|| Error::msg("Channel no longer in playlist").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"#EXTM3U x-tvg-url="http://epg"
#EXTINF:-1 tvg-id="news.1" tvg-logo="http://logo/n.png" group-title="News",World News, HD
#EXTVLCOPT:http-user-agent=VLC/3.0
http://cdn.example/news.m3u8
#EXTINF:-1 tvg-name="Sports One" group-title="Sports",
https://cdn.example/sports.m3u8|User-Agent=Kodi&Referer=https://site/&X-Token=abc
#EXTINF:0,Radio
#EXTGRP:Music
rtmp://live.example/radio
#EXTINF:-1,Local file
file:///etc/passwd
#EXTINF:-1 group-title="News",Business
https://cdn.example/biz.m3u8
"#;

    #[test]
    fn parses_attributes_names_groups_and_headers() {
        let ch = parse(SAMPLE);
        assert_eq!(ch.len(), 4, "file:// entry must be dropped: {ch:#?}");
        assert_eq!(ch[0].name, "World News, HD"); // comma inside the name survives
        assert_eq!(ch[0].logo.as_deref(), Some("http://logo/n.png"));
        assert_eq!(ch[0].group.as_deref(), Some("News"));
        assert_eq!(ch[0].user_agent.as_deref(), Some("VLC/3.0"));
        assert_eq!(ch[1].name, "Sports One"); // falls back to tvg-name
        assert_eq!(ch[1].url, "https://cdn.example/sports.m3u8");
        assert_eq!(ch[1].user_agent.as_deref(), Some("Kodi"));
        assert_eq!(ch[1].referrer.as_deref(), Some("https://site/"));
        assert_eq!(ch[1].headers, vec![("X-Token".into(), "abc".into())]);
        assert_eq!(ch[2].group.as_deref(), Some("Music")); // #EXTGRP
    }

    #[test]
    fn home_groups_and_browse_pages() {
        let ch = parse(SAMPLE);
        let home = home_page(&ch);
        let titles: Vec<&str> = home["sections"][0]["items"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["All channels", "News", "Sports", "Music"]);
        let news = browse_page(&ch, "group:News", None);
        assert_eq!(news["layout"], "channels");
        assert_eq!(news["sections"][0]["items"].as_array().unwrap().len(), 2);
        assert_eq!(news["sections"][0]["items"][1]["id"], "ch:3");
        assert_eq!(search_page(&ch, "sport", None)["sections"][0]["items"][0]["title"], "Sports One");
    }

    #[test]
    fn playback_carries_headers() {
        let ch = parse(SAMPLE);
        let pb = playback(&ch[1]);
        assert_eq!(pb["headers"]["User-Agent"], "Kodi");
        assert_eq!(pb["headers"]["Referer"], "https://site/");
        assert_eq!(pb["headers"]["X-Token"], "abc");
        assert!(playback(&ch[3]).get("headers").is_none());
    }

    #[test]
    fn paging() {
        let many: String = (0..650).map(|i| format!("#EXTINF:-1,Ch {i}\nhttp://x/{i}\n")).collect();
        let ch = parse(&many);
        let p1 = browse_page(&ch, "all", None);
        assert_eq!(p1["sections"][0]["items"].as_array().unwrap().len(), PAGE);
        assert_eq!(p1["next"], "300");
        let p3 = browse_page(&ch, "all", Some("600"));
        assert_eq!(p3["sections"][0]["items"].as_array().unwrap().len(), 50);
        assert!(p3.get("next").is_none());
    }
}
