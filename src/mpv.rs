//! libmpv wrapper: the handle, cached property reads, and the event + render threads.
//! mpv decodes and renders BGRA frames on a worker thread; the UI shows the latest one.

use std::ffi::{CString, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use libmpv2_sys::*;

use crate::{config_dir, log, settings, sources, visualizer};

/// Set by the event thread when a new file starts: the UI drops the previous file's frame so
/// "Loading…" shows instead of a stale picture while the new one opens.
pub(crate) static NEW_FILE: AtomicBool = AtomicBool::new(false);
/// Between a file's start and its video setup the render thread keeps frames to itself: mpv keeps the
/// old VO open while the next file opens (seconds, for remote files) and redraws the old picture on
/// any update (pause, OSD), which would bring the stale frame back.
/// Released on VIDEO_RECONFIG (held frames are old, dropped) or, as a fallback, on PLAYBACK_RESTART
/// (new video is playing: the last held frame is its first one, shown).
static HOLD_FRAMES: AtomicU8 = AtomicU8::new(HOLD_OFF);
const HOLD_OFF: u8 = 0;
const HOLD_ON: u8 = 1;
const HOLD_SHOW_LAST: u8 = 2;
/// Why the last file failed to play (cleared when the next one starts or a file ends normally).
pub(crate) static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Properties the UI reads every frame. mpv_get_property waits for mpv's core, which is often busy
/// (opening a stream, switching YouTube formats): 100-300 ms UI freezes, Explorer included. The event
/// thread observes these instead and keeps the latest value here; reads never block.
const OBSERVED: [&str; 27] = [
    "time-pos", "duration", "pause", "idle-active", "eof-reached", "media-title", "mute", "volume", "playlist-count", "playlist-pos", "playlist",
    // ⓘ card file facts
    "filename", "file-size", "width", "height", "video-codec", "audio-codec-name", "file-format",
    // Quick Settings › Video
    "hwdec-current",
    visualizer::PROPERTY,
    // Quick Settings › Picture (applied as a filter, see update_picture)
    "brightness", "contrast", "saturation", "gamma", "hue",
    // network stall overlay
    "paused-for-cache", "cache-buffering-state",
];
static CACHE: Mutex<std::collections::BTreeMap<&'static str, String>> = Mutex::new(std::collections::BTreeMap::new());

/// lavfi filter for a clockwise rotation in degrees ("" = none). The sw renderer can't rotate.
fn rotation_filter(degrees: i64) -> &'static str {
    match degrees.rem_euclid(360) {
        90 => "transpose=clock",
        180 => "hflip,vflip",
        270 => "transpose=cclock",
        _ => "",
    }
}

fn parse_playlist(json: &str) -> Vec<(String, String)> {
    let v: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    v.iter()
        .map(|e| {
            let s = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
            (s("filename"), s("title"))
        })
        .collect()
}

/// Some(value) for an observed property (None inside = currently unavailable); None = not observed.
fn cached(name: &str) -> Option<Option<String>> {
    let key = OBSERVED.iter().find(|k| **k == name)?;
    Some(CACHE.lock().unwrap_or_else(PoisonError::into_inner).get(key).cloned())
}
/// Rotation picked in Quick Settings (degrees); kept across files like mpv's own option was.
pub(crate) static USER_ROTATE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
pub(crate) static IN_FLIGHT: Mutex<String> = Mutex::new(String::new());

struct InFlight;
impl InFlight {
    fn new(what: &str) -> Self {
        *IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner) = what.to_string();
        InFlight
    }
}
impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }
}

/// Hide passwords in `scheme://user:pass@host` before logging (ftp/sftp URLs carry them for mpv).
fn redact(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        out.push_str(&rest[..i + 3]);
        rest = &rest[i + 3..];
        let auth_end = rest.find(['/', '"', ' ', ',', ']']).unwrap_or(rest.len());
        if let Some(at) = rest[..auth_end].rfind('@') {
            let user = rest[..at].split(':').next().unwrap_or("");
            out.push_str(&format!("{user}:***@"));
            rest = &rest[at + 1..];
        }
    }
    out + rest
}

/// Raw mpv handle (client API is thread-safe) + work to do once the next file has loaded.
pub struct Mpv(*mut mpv_handle, Mutex<AfterLoad>);

/// Applied by the event thread on MPV_EVENT_FILE_LOADED (sub-add / seek need a loaded file).
#[derive(Default)]
pub struct AfterLoad {
    subs: Vec<String>,
    start: Option<f64>,
    /// Jump to this playlist index once the (expanded) playlist starts.
    jump: Option<i64>,
}
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

impl Mpv {
    pub fn new() -> Arc<Self> {
        unsafe {
            let h = mpv_create();
            assert!(!h.is_null(), "mpv_create failed");
            let mpv = Arc::new(Mpv(h, Mutex::default()));
            let prefs = settings::player_prefs();
            mpv.opt("vo", "libmpv");
            // Decode on GPU, copy back for the sw renderer (Settings › Player can turn it off).
            mpv.opt("hwdec", settings::hwdec_value(prefs.hwdec));
            if prefs.mpv_conf {
                // Settings › mpv: read the user's mpv.conf (+ input.conf, scripts/) at initialize.
                mpv.opt("config-dir", &settings::mpv_config_dir().to_string_lossy());
                mpv.opt("config", "yes");
            }
            mpv.opt("keep-open", "yes"); // advance through playlist, hold last frame at the end
            mpv.opt("idle", "yes"); // stay alive with an empty playlist (drop files later)
            mpv.opt("screenshot-dir", "~/Pictures"); // mpv expands ~ on every OS
            // The sw renderer can't rotate: mpv still hands it rotated crop rects and aborts
            // (mp_image_crop assertion). Rotation is done by a video filter instead, see set_rotation.
            mpv.opt("video-rotate", "no");
            // "Resume where you left off" (Settings › mpv): positions saved next to the player's settings.
            mpv.opt("watch-later-dir", &config_dir().join("watch_later").to_string_lossy());
            assert!(mpv_initialize(h) >= 0, "mpv_initialize failed");
            // After mpv.conf (if any): what Settings shows is what's in effect.
            settings::apply_prefs(&mpv, &prefs);
            if prefs.mpv_conf {
                // mpv.conf loaded during initialize and may have overridden these; the player needs them.
                mpv.set_str("vo", "libmpv");
                mpv.set_str("video-rotate", "no");
            }
            mpv
        }
    }

    /// Plugins = mpv Lua/JS scripts in the plugins dir, minus those disabled in Settings.
    pub fn load_plugins(&self) {
        let off = settings::disabled("scripts");
        for p in settings::scripts() {
            if p.file_name().is_some_and(|n| off.iter().any(|o| *o == *n.to_string_lossy())) {
                continue;
            }
            log::info("scripts", &format!("loading {}", p.display()));
            self.cmd(&["load-script", &p.to_string_lossy()]);
        }
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        if let Some(v) = cached(name) {
            return v.and_then(|s| s.parse().ok());
        }
        let _g = InFlight::new(name);
        let n = CString::new(name).unwrap();
        let mut out = 0i64;
        let r = unsafe {
            mpv_get_property(self.0, n.as_ptr(), mpv_format_MPV_FORMAT_INT64, &mut out as *mut _ as *mut c_void)
        };
        (r >= 0).then_some(out)
    }

    pub fn get_string(&self, name: &str) -> Option<String> {
        if let Some(v) = cached(name) {
            return v;
        }
        let _g = InFlight::new(name);
        let n = CString::new(name).unwrap();
        unsafe {
            let p = mpv_get_property_string(self.0, n.as_ptr());
            if p.is_null() {
                return None;
            }
            let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
            mpv_free(p as *mut c_void);
            Some(s)
        }
    }

    pub fn opt(&self, k: &str, v: &str) {
        let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
        unsafe { mpv_set_option_string(self.0, k.as_ptr(), v.as_ptr()) };
    }

    /// Rotate by a `transpose` filter: user rotation plus the file's rotation metadata (phone videos).
    /// The filter changes dwidth/dheight, so the renderer gets the rotated size.
    pub fn set_rotation(&self, user: i64) {
        USER_ROTATE.store(user, Ordering::Relaxed);
        self.apply_rotation();
    }

    pub fn apply_rotation(&self) {
        let meta = self.get_i64("current-tracks/video/demux-rotation").unwrap_or(0);
        let f = rotation_filter(meta + USER_ROTATE.load(Ordering::Relaxed));
        if self.get_string("vf").is_some_and(|v| v.contains("zymrot")) {
            self.cmd(&["vf", "remove", "@zymrot"]);
        }
        if !f.is_empty() {
            self.cmd(&["vf", "add", &format!("@zymrot:lavfi=[{f}]")]);
        }
    }

    /// (filename, title) per entry, from the observed `playlist` (mpv sends it as JSON). Parsed
    /// once per change: several widgets ask every frame.
    pub fn playlist_entries(&self) -> Vec<(String, String)> {
        static PARSED: Mutex<(String, Vec<(String, String)>)> = Mutex::new((String::new(), Vec::new()));
        let json = self.get_string("playlist").unwrap_or_default();
        let mut cache = PARSED.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.0 != json {
            *cache = (json.clone(), parse_playlist(&json));
        }
        cache.1.clone()
    }

    /// mpv messages sent to the app log: warnings and up, or everything verbose (Settings › Logs).
    pub fn set_log_level(&self, verbose: bool) {
        let level = if verbose { c"v" } else { c"warn" };
        unsafe { mpv_request_log_messages(self.0, level.as_ptr()) };
    }

    pub fn cmd(&self, args: &[&str]) {
        if std::env::var_os("ZYM_DEBUG").is_some() {
            log::debug("cmd", &redact(&format!("{args:?}")));
        }
        // A NUL would panic CString::new; args can carry plugin-supplied URLs.
        let owned: Vec<CString> = args.iter().map(|a| CString::new(a.replace('\0', "")).unwrap()).collect();
        let mut ptrs: Vec<*const i8> = owned.iter().map(|a| a.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        unsafe { mpv_command_async(self.0, 0, ptrs.as_mut_ptr()) };
    }

    /// String property via the property API: the value is taken literally (no option-string parsing).
    pub fn set_str(&self, name: &str, value: &str) {
        let _g = InFlight::new(name);
        let (n, v) = (CString::new(name).unwrap(), CString::new(value.replace('\0', "")).unwrap());
        unsafe { mpv_set_property_string(self.0, n.as_ptr(), v.as_ptr()) };
    }

    /// String-list property as an mpv node array: each entry stays one literal item (commas included).
    pub fn set_str_list(&self, name: &str, items: &[String]) {
        let _g = InFlight::new(name);
        let owned: Vec<CString> = items.iter().map(|s| CString::new(s.replace('\0', "")).unwrap()).collect();
        let mut nodes: Vec<mpv_node> = owned
            .iter()
            .map(|c| mpv_node { u: mpv_node__bindgen_ty_1 { string: c.as_ptr() as *mut _ }, format: mpv_format_MPV_FORMAT_STRING })
            .collect();
        let mut list = mpv_node_list { num: nodes.len() as i32, values: nodes.as_mut_ptr(), keys: std::ptr::null_mut() };
        let mut node = mpv_node { u: mpv_node__bindgen_ty_1 { list: &mut list }, format: mpv_format_MPV_FORMAT_NODE_ARRAY };
        let n = CString::new(name).unwrap();
        unsafe { mpv_set_property(self.0, n.as_ptr(), mpv_format_MPV_FORMAT_NODE, &mut node as *mut _ as *mut c_void) };
    }

    /// Clear per-stream state a source plugin may have set (headers, UA, forced title).
    pub fn reset_stream_opts(&self) {
        self.set_str("user-agent", "libmpv");
        self.set_str("referrer", "");
        self.set_str_list("http-header-fields", &[]);
        self.set_str("force-media-title", "");
        *self.1.lock().unwrap_or_else(PoisonError::into_inner) = AfterLoad::default();
    }

    /// Load a user-chosen file/URL/folder/.m3u (mpv expands folders and playlists itself).
    pub fn load(&self, target: &str, mode: &str) {
        self.reset_stream_opts();
        self.cmd(&["loadfile", target, mode]);
    }

    /// Replace the queue with a playlist file and start at entry `index`. mpv expands the .m3u on
    /// load, so the jump happens when its first entry is loaded (it may flash for a moment).
    pub fn load_from(&self, playlist: &str, index: i64) {
        self.reset_stream_opts();
        self.1.lock().unwrap_or_else(PoisonError::into_inner).jump = Some(index);
        self.cmd(&["loadfile", playlist, "replace"]);
    }

    /// Play a stream resolved by a source plugin. Only validated fields reach mpv, all via properties.
    pub fn play_stream(&self, pb: &sources::Playback) {
        self.reset_stream_opts();
        let mut extra = Vec::new();
        for (k, v) in &pb.headers {
            match k.to_ascii_lowercase().as_str() {
                "user-agent" => self.set_str("user-agent", v),
                "referer" | "referrer" => self.set_str("referrer", v),
                _ => extra.push(format!("{k}: {v}")),
            }
        }
        self.set_str_list("http-header-fields", &extra);
        if let Some(t) = &pb.title {
            self.set_str("force-media-title", t);
        }
        *self.1.lock().unwrap_or_else(PoisonError::into_inner) = AfterLoad { subs: pb.subtitles.iter().map(|s| s.url.clone()).collect(), start: pb.start, jump: None };
        self.cmd(&["loadfile", &pb.url, "replace"]);
        self.cmd(&["set", "pause", "no"]);
    }

    /// Append a resolved stream to the playlist with its headers/title as *per-file* options
    /// (`loadfile url append-play -1 {opts}` via the node API: each option is its own map key).
    pub fn append_stream(&self, pb: &sources::Playback) {
        let mut opts: Vec<(&str, String)> = Vec::new();
        let mut extra = Vec::new();
        for (k, v) in &pb.headers {
            match k.to_ascii_lowercase().as_str() {
                "user-agent" => opts.push(("user-agent", v.clone())),
                "referer" | "referrer" => opts.push(("referrer", v.clone())),
                // ponytail: string-list option value splits on ','; such headers are skipped here (play_stream sets them exactly).
                _ if v.contains(',') => log::warn("append", &format!("skipped header {k} (contains ',')")),
                _ => extra.push(format!("{k}: {v}")),
            }
        }
        if !extra.is_empty() {
            opts.push(("http-header-fields", extra.join(",")));
        }
        if let Some(t) = &pb.title {
            opts.push(("force-media-title", t.clone()));
        }
        let c = |s: &str| CString::new(s.replace('\0', "")).unwrap();
        let str_node = |s: &CString| mpv_node { u: mpv_node__bindgen_ty_1 { string: s.as_ptr() as *mut _ }, format: mpv_format_MPV_FORMAT_STRING };
        let keys: Vec<CString> = opts.iter().map(|(k, _)| c(k)).collect();
        let vals: Vec<CString> = opts.iter().map(|(_, v)| c(v)).collect();
        let mut key_ptrs: Vec<*mut std::os::raw::c_char> = keys.iter().map(|k| k.as_ptr() as *mut _).collect();
        let mut val_nodes: Vec<mpv_node> = vals.iter().map(str_node).collect();
        let mut map = mpv_node_list { num: val_nodes.len() as i32, values: val_nodes.as_mut_ptr(), keys: key_ptrs.as_mut_ptr() };
        let words = [c("loadfile"), c(&pb.url), c("append-play")];
        let mut args: Vec<mpv_node> = words.iter().map(str_node).collect();
        args.push(mpv_node { u: mpv_node__bindgen_ty_1 { int64: -1 }, format: mpv_format_MPV_FORMAT_INT64 });
        args.push(mpv_node { u: mpv_node__bindgen_ty_1 { list: &mut map }, format: mpv_format_MPV_FORMAT_NODE_MAP });
        let mut list = mpv_node_list { num: args.len() as i32, values: args.as_mut_ptr(), keys: std::ptr::null_mut() };
        let mut root = mpv_node { u: mpv_node__bindgen_ty_1 { list: &mut list }, format: mpv_format_MPV_FORMAT_NODE_ARRAY };
        if std::env::var_os("ZYM_DEBUG").is_some() {
            log::debug("cmd", &format!("[\"loadfile\", {}, \"append-play\", -1, {opts:?}]", redact(&pb.url)));
        }
        unsafe { mpv_command_node_async(self.0, 0, &mut root) }; // mpv copies args before returning
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        if let Some(v) = cached(name) {
            return v.and_then(|s| s.parse().ok());
        }
        let _g = InFlight::new(name);
        let n = CString::new(name).unwrap();
        let mut out = 0f64;
        let r = unsafe {
            mpv_get_property(self.0, n.as_ptr(), mpv_format_MPV_FORMAT_DOUBLE, &mut out as *mut _ as *mut c_void)
        };
        (r >= 0).then_some(out)
    }

    pub fn get_flag(&self, name: &str) -> bool {
        if let Some(v) = cached(name) {
            return v.as_deref() == Some("yes");
        }
        let _g = InFlight::new(name);
        let n = CString::new(name).unwrap();
        let mut out: i32 = 0;
        unsafe { mpv_get_property(self.0, n.as_ptr(), mpv_format_MPV_FORMAT_FLAG, &mut out as *mut _ as *mut c_void) };
        out != 0
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        unsafe { mpv_terminate_destroy(self.0) };
    }
}

/// Window size in physical pixels (w << 32 | h), written by the UI tick. The render thread fits the
/// video into it: a 4K frame shown in an 1100 px window is ~33 MB of wasted work per frame otherwise.
pub(crate) static VIEW_PX: AtomicU64 = AtomicU64::new(0);

/// Aspect-fit (w, h) into the view box, never upscaling. Unknown view (0) -> native size.
fn fit_to_view(w: u32, h: u32, view: u64) -> (u32, u32) {
    let (vw, vh) = ((view >> 32) as f64, (view & 0xffff_ffff) as f64);
    if vw < 1.0 || vh < 1.0 {
        return (w, h);
    }
    let scale = (vw / w as f64).min(vh / h as f64).min(1.0);
    (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1))
}

/// Signalled by mpv's update callback and the event thread; waited on by the render thread.
pub(crate) type Wake = (Mutex<bool>, Condvar);

fn wake(w: &Wake) {
    *w.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
    w.1.notify_one();
}

unsafe extern "C" fn on_update(ctx: *mut c_void) {
    wake(unsafe { &*(ctx as *const Wake) });
}

/// Latest rendered frame: (width, height, BGRA bytes).
pub(crate) type Latest = Arc<Mutex<Option<(u32, u32, Vec<u8>)>>>;

/// Display size packed as (w << 32 | h); 0 = unknown. Written by the event thread so the
/// render thread never calls into the mpv core (render.h warns that can deadlock).
pub(crate) type VideoSize = Arc<AtomicU64>;

/// Drains mpv's event queue (a full queue makes async commands fail silently), tracks video size,
/// and passes mpv's log messages to the app log (log.rs).
#[allow(non_upper_case_globals)] // bindgen constants used as match patterns
pub(crate) fn spawn_event_thread(mpv: Arc<Mpv>, size: VideoSize, latest: Latest, wake_render: &'static Wake) -> std::thread::JoinHandle<()> {
    unsafe {
        let level = if std::env::var_os("ZYM_DEBUG").is_some() { c"v" } else { c"warn" };
        mpv_request_log_messages(mpv.0, level.as_ptr());
        mpv_observe_property(mpv.0, 0, c"dwidth".as_ptr(), mpv_format_MPV_FORMAT_INT64);
        mpv_observe_property(mpv.0, 0, c"dheight".as_ptr(), mpv_format_MPV_FORMAT_INT64);
        for name in OBSERVED {
            let n = CString::new(name).unwrap();
            mpv_observe_property(mpv.0, 0, n.as_ptr(), mpv_format_MPV_FORMAT_STRING);
        }
    }
    std::thread::spawn(move || unsafe {
        let (mut w, mut h) = (0u64, 0u64);
        loop {
            let ev = &*mpv_wait_event(mpv.0, -1.0);
            match ev.event_id {
                mpv_event_id_MPV_EVENT_SHUTDOWN => break,
                mpv_event_id_MPV_EVENT_START_FILE => {
                    latest.lock().unwrap_or_else(PoisonError::into_inner).take(); // old file's last frame
                    HOLD_FRAMES.store(HOLD_ON, Ordering::Relaxed);
                    NEW_FILE.store(true, Ordering::Relaxed);
                    *LAST_ERROR.lock().unwrap_or_else(PoisonError::into_inner) = None;
                }
                // Sent after the VO is reconfigured for the new file's video: frames from here on are new.
                mpv_event_id_MPV_EVENT_VIDEO_RECONFIG => HOLD_FRAMES.store(HOLD_OFF, Ordering::Relaxed),
                mpv_event_id_MPV_EVENT_PLAYBACK_RESTART => {
                    if HOLD_FRAMES.compare_exchange(HOLD_ON, HOLD_SHOW_LAST, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                        wake(wake_render); // publish the held frame even if mpv sends no new one (paused)
                    }
                }
                mpv_event_id_MPV_EVENT_LOG_MESSAGE => {
                    let m = &*(ev.data as *const mpv_event_log_message);
                    let s = |p: *const i8| std::ffi::CStr::from_ptr(p).to_string_lossy();
                    log::add(log::Level::from_mpv(&s(m.level)), &format!("mpv/{}", s(m.prefix)), &s(m.text));
                }
                mpv_event_id_MPV_EVENT_FILE_LOADED => {
                    let after = std::mem::take(&mut *mpv.1.lock().unwrap_or_else(PoisonError::into_inner));
                    for url in after.subs {
                        mpv.cmd(&["sub-add", &url, "auto"]);
                    }
                    if let Some(t) = after.start.filter(|t| *t > 0.0) {
                        mpv.cmd(&["seek", &t.to_string(), "absolute"]);
                    }
                    if let Some(i) = after.jump.filter(|i| *i > 0) {
                        mpv.cmd(&["playlist-play-index", &i.to_string()]);
                    }
                    mpv.apply_rotation(); // this file's rotation metadata may differ
                }
                mpv_event_id_MPV_EVENT_END_FILE => {
                    let e = &*(ev.data as *const mpv_event_end_file);
                    let failed = e.reason == mpv_end_file_reason_MPV_END_FILE_REASON_ERROR;
                    if failed {
                        log::error("mpv", &format!("file failed: {}", mpv_error_str(e.error)));
                    }
                    *LAST_ERROR.lock().unwrap_or_else(PoisonError::into_inner) = failed.then(|| mpv_error_str(e.error).to_string());
                }
                mpv_event_id_MPV_EVENT_PROPERTY_CHANGE => {
                    let p = &*(ev.data as *const mpv_event_property);
                    let name = std::ffi::CStr::from_ptr(p.name).to_string_lossy();
                    if let Some(key) = OBSERVED.iter().find(|k| **k == name) {
                        let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
                        if p.format == mpv_format_MPV_FORMAT_STRING {
                            let s = std::ffi::CStr::from_ptr(*(p.data as *const *const std::ffi::c_char));
                            cache.insert(key, s.to_string_lossy().into_owned());
                        } else {
                            cache.remove(key); // unavailable (e.g. no file loaded)
                        }
                        continue;
                    }
                    let v = if p.format == mpv_format_MPV_FORMAT_INT64 { *(p.data as *const i64) as u64 } else { 0 };
                    match std::ffi::CStr::from_ptr(p.name).to_bytes() {
                        b"dwidth" => w = v,
                        b"dheight" => h = v,
                        _ => {}
                    }
                    size.store(if w > 0 && h > 0 { w << 32 | h } else { 0 }, Ordering::Release);
                    wake(wake_render); // a frame may be waiting on the size
                }
                _ => {}
            }
        }
    })
}

pub(crate) fn spawn_render_thread(mpv: Arc<Mpv>, latest: Latest, size: VideoSize, wake_me: &'static Wake) {
    // Create the context before returning: a file whose VO initialises before it exists fails with
    // "No render context set" and plays without video. Callers should spawn this before any loadfile.
    let rctx = unsafe {
        let mut api = *MPV_RENDER_API_TYPE_SW;
        let mut params = [
            mpv_render_param { type_: mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE, data: api.as_mut_ptr() as *mut c_void },
            mpv_render_param { type_: 0, data: std::ptr::null_mut() },
        ];
        let mut rctx: *mut mpv_render_context = std::ptr::null_mut();
        assert!(mpv_render_context_create(&mut rctx, mpv.0, params.as_mut_ptr()) >= 0, "sw render ctx failed");
        mpv_render_context_set_update_callback(rctx, Some(on_update), wake_me as *const Wake as *mut c_void);
        rctx as usize // raw pointers aren't Send; only this thread uses it from here on
    };
    std::thread::spawn(move || unsafe {
        let rctx = rctx as *mut mpv_render_context;

        let fmt = c"bgr0"; // GPUI's atlas wants BGRA
        let mut pending = false; // mpv has a frame for us that we haven't rendered yet
        let mut spare: Option<Vec<u8>> = None; // buffer of a frame the UI never showed
        let mut held: Option<(u32, u32, Vec<u8>)> = None; // last frame kept back (HOLD_FRAMES)
        loop {
            {
                let mut flag = wake_me.0.lock().unwrap_or_else(PoisonError::into_inner);
                // Timeout = safety net: never sit forever on an unrendered frame (mpv's VO waits on us).
                while !*flag {
                    let (f, t) = wake_me.1.wait_timeout(flag, Duration::from_millis(50)).unwrap();
                    flag = f;
                    if t.timed_out() && pending {
                        break;
                    }
                }
                *flag = false;
            }
            match HOLD_FRAMES.load(Ordering::Relaxed) {
                HOLD_SHOW_LAST => {
                    HOLD_FRAMES.store(HOLD_OFF, Ordering::Relaxed);
                    if let Some(f) = held.take() {
                        *latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(f);
                    }
                }
                HOLD_OFF => spare = held.take().map(|(_, _, b)| b).or(spare.take()),
                _ => {}
            }
            if mpv_render_context_update(rctx) & (mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64) != 0 {
                pending = true;
            }
            let packed = size.load(Ordering::Acquire);
            if !pending || packed == 0 {
                continue;
            }
            let (w, h) = fit_to_view((packed >> 32) as u32, packed as u32, VIEW_PX.load(Ordering::Relaxed));
            let mut dims = [w as i32, h as i32];
            let mut stride = (w * 4) as usize;
            let n = stride * h as usize;
            // Reuse the buffer of a frame the UI skipped (swapped out below), and skip zero-filling:
            // mpv overwrites every byte. Never take a frame out of `latest` before the new one is
            // ready: slow renders would leave it empty most of the time and the picture freezes.
            let mut buf = match spare.take() {
                Some(mut b) if b.capacity() >= n => {
                    b.clear();
                    b
                }
                _ => Vec::with_capacity(n),
            };
            let mut params = [
                mpv_render_param { type_: mpv_render_param_type_MPV_RENDER_PARAM_SW_SIZE, data: dims.as_mut_ptr() as *mut c_void },
                mpv_render_param { type_: mpv_render_param_type_MPV_RENDER_PARAM_SW_FORMAT, data: fmt.as_ptr() as *mut c_void },
                mpv_render_param { type_: mpv_render_param_type_MPV_RENDER_PARAM_SW_STRIDE, data: &mut stride as *mut _ as *mut c_void },
                mpv_render_param { type_: mpv_render_param_type_MPV_RENDER_PARAM_SW_POINTER, data: buf.as_mut_ptr() as *mut c_void },
                mpv_render_param { type_: 0, data: std::ptr::null_mut() },
            ];
            let r = mpv_render_context_render(rctx, params.as_mut_ptr());
            pending = false; // mpv consumed (or rejected) the frame either way
            if r < 0 {
                log::error("render", &format!("render failed: {}", mpv_error_str(r)));
                continue;
            }
            // SAFETY: capacity >= n, and render.h guarantees every byte in [pointer, pointer + stride*h)
            // is overwritten; stride == w*4, so that's the whole buffer.
            buf.set_len(n);
            // "0" byte is garbage; force opaque alpha.
            for px in buf.as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
            if HOLD_FRAMES.load(Ordering::Relaxed) == HOLD_ON {
                // Maybe the old file's picture, redrawn while the next one opens; maybe the new file's first.
                if let Some((_, _, b)) = held.replace((w, h, buf)) {
                    spare = Some(b);
                }
                continue;
            }
            let skipped = latest.lock().unwrap_or_else(PoisonError::into_inner).replace((w, h, buf));
            spare = skipped.map(|(_, _, b)| b);
        }
    });
}


#[cfg(test)]
mod cache_tests {
    #[test]
    fn observed_props_come_from_cache() {
        assert_eq!(super::cached("sub-delay"), None, "not observed: read from mpv");
        assert_eq!(super::cached("volume"), Some(None), "observed, no value yet");
        super::CACHE.lock().unwrap().insert("volume", "80.000000".into());
        assert_eq!(super::cached("volume"), Some(Some("80.000000".into())));
        let pl = super::parse_playlist(r#"[{"filename":"/a.mkv","current":true,"id":1},{"filename":"https://x","title":"X","id":2}]"#);
        assert_eq!(pl, [("/a.mkv".to_string(), String::new()), ("https://x".to_string(), "X".to_string())]);
        assert!(super::parse_playlist("").is_empty());
        assert_eq!(super::rotation_filter(0), "");
        assert_eq!(super::rotation_filter(90 + 270), "", "metadata + user wrap around");
        assert_eq!(super::rotation_filter(-90), "transpose=cclock");
        assert_eq!(super::rotation_filter(450), "transpose=clock");
    }
}

#[cfg(test)]
mod fit_tests {
    use super::fit_to_view;

    #[test]
    fn fits_without_upscaling() {
        let view = |w: u64, h: u64| w << 32 | h;
        assert_eq!(fit_to_view(3840, 2160, view(1100, 650)), (1100, 619)); // 4K into small window
        assert_eq!(fit_to_view(1440, 2560, view(1100, 650)), (366, 650)); // portrait: height-bound
        assert_eq!(fit_to_view(640, 480, view(2560, 1440)), (640, 480)); // never upscale
        assert_eq!(fit_to_view(1920, 1080, 0), (1920, 1080)); // unknown view -> native
    }
}

#[cfg(test)]
mod redact_tests {
    #[test]
    fn hides_passwords_only() {
        assert_eq!(super::redact(r#"["loadfile", "sftp://me:p@ss@nas:22/a b.mkv", "replace"]"#), r#"["loadfile", "sftp://me:***@nas:22/a b.mkv", "replace"]"#);
        assert_eq!(super::redact("http://host/x?a=b@c"), "http://host/x?a=b@c"); // '@' after the path isn't userinfo
        assert_eq!(super::redact("no url here"), "no url here");
    }
}
