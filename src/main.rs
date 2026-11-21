// No console window next to the player on Windows (logs still go to stderr when run from a terminal).
#![cfg_attr(windows, windows_subsystem = "windows")]

// Zym Media Player: GPUI window + libmpv software renderer.
// mpv decodes and renders BGRA frames on a worker thread; GPUI shows the latest one as an image.

mod explorer;
mod infocard;
mod install;
mod log;
mod menu;
mod mpvopts;
mod panel;
mod playlists;
mod remote;
mod settings;
mod textbox;
mod update;
mod visualizer;
mod sources;
mod theme;

use theme::t;

pub const APP_NAME: &str = "Zym Media Player";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

use std::borrow::Cow;
use std::ffi::{CString, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    App, Application, AssetSource, Bounds, ClickEvent, ExternalPaths, Context, Div, FocusHandle, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, RenderImage, ScrollWheelEvent, SharedString, Stateful, Timer,
    Window, WindowBounds, WindowOptions, div, img, linear_color_stop, linear_gradient, prelude::*,
    px, relative, rgb, rgba, size, svg,
};
use image::{Frame, ImageBuffer};
use explorer::Explorer;
use textbox::Field;
use menu::{Item, MenuState, mpv as mcmd, sub, ui};
use libmpv2_sys::*;

// ---- debug watchdog (ZYM_DEBUG): which sync mpv call, if any, the UI thread is stuck in.
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);

/// Properties the UI reads every frame. mpv_get_property waits for mpv's core, which is often busy
/// (opening a stream, switching YouTube formats): 100-300 ms UI freezes, Explorer included. The event
/// thread observes these instead and keeps the latest value here; reads never block.
const OBSERVED: [&str; 25] = [
    "time-pos", "duration", "pause", "idle-active", "eof-reached", "media-title", "mute", "volume", "playlist-count", "playlist-pos", "playlist",
    // ⓘ card file facts
    "filename", "file-size", "width", "height", "video-codec", "audio-codec-name", "file-format",
    // Quick Settings › Video
    "hwdec-current",
    visualizer::PROPERTY,
    // Quick Settings › Picture (applied as a filter, see update_picture)
    "brightness", "contrast", "saturation", "gamma", "hue",
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

const PIC_PROPS: [&str; 5] = ["brightness", "contrast", "saturation", "gamma", "hue"];

/// mpv's -100..100 picture values -> (ffmpeg option, value, filter) for `eq` and `hue`.
fn picture_args(v: [i64; 5]) -> [(&'static str, String, &'static str); 5] {
    let f = |x: i64| x as f64 / 100.;
    [
        ("brightness", format!("{:.3}", f(v[0]) * 0.5), "eq"),
        ("contrast", format!("{:.3}", 1. + f(v[1])), "eq"),
        ("saturation", format!("{:.3}", 1. + f(v[2])), "eq"),
        ("gamma", format!("{:.3}", 2f64.powf(f(v[3]) * 2.)), "eq"),
        ("h", format!("{:.1}", f(v[4]) * 180.), "hue"),
    ]
}

/// Audio EQ presets: (name, [(Hz, gain dB)]), one-octave peaking bands. The same presets as the
/// GoCast Android TV receiver.
const AUDIO_EQ: [(&str, &[(u32, i32)]); 12] = [
    ("Flat", &[]),
    ("Rock", &[(70, 4), (180, 3), (320, -1), (600, -1), (1000, 0), (3000, 1), (6000, 3), (12000, 4), (14000, 4), (16000, 4)]),
    ("Pop", &[(70, -1), (180, 2), (320, 4), (600, 4), (1000, 2), (3000, 0), (6000, -1), (12000, -2), (14000, -2), (16000, -2)]),
    ("Jazz", &[(70, 3), (180, 2), (320, 0), (600, 2), (1000, -2), (3000, -2), (6000, 0), (12000, 2), (14000, 3), (16000, 3)]),
    ("Classical", &[(6000, -4), (12000, -4), (14000, -4), (16000, -6)]),
    ("Electronic", &[(70, 5), (180, 4), (320, 1), (600, 0), (1000, -2), (3000, 2), (6000, 2), (12000, 3), (14000, 4), (16000, 5)]),
    ("Hip-hop", &[(70, 5), (180, 4), (320, 2), (600, 3), (1000, -1), (3000, -1), (6000, 2), (12000, 3), (14000, 3), (16000, 2)]),
    ("R&B", &[(70, 5), (180, 4), (320, 0), (600, -2), (1000, -2), (3000, 2), (6000, 2), (12000, 3), (14000, 3), (16000, 4)]),
    ("Acoustic", &[(70, 4), (180, 3), (320, 2), (600, 0), (1000, 0), (3000, 2), (6000, 4), (12000, 4), (14000, 4), (16000, 4)]),
    ("Bass boost", &[(70, 8), (180, 6), (320, 4), (600, 2)]),
    ("Treble", &[(3000, 2), (6000, 4), (12000, 6), (14000, 7), (16000, 8)]),
    ("Dance", &[(70, 6), (180, 5), (320, 2), (600, 0), (1000, -2), (3000, 2), (6000, 3), (12000, 4), (14000, 5), (16000, 5)]),
];

/// `af add` argument for EQ preset `name`; None for Flat (or an unknown name). Boosts of up to
/// +8 dB clip loud tracks, so a limiter ends the chain: peaks stay just under full scale, and
/// `level=disabled` keeps it from raising quiet parts.
fn eq_filter(name: &str) -> Option<String> {
    let (_, bands) = AUDIO_EQ.iter().find(|(n, _)| *n == name)?;
    let chain: Vec<String> = bands.iter().map(|(f, g)| format!("equalizer=f={f}:width_type=o:width=1:g={g}")).collect();
    (!chain.is_empty()).then(|| format!("@zymeq:lavfi=[{},alimiter=limit=0.97:level=disabled]", chain.join(",")))
}

/// Aspect as a number: "16:9", "2.35:1" or what mpv reports back ("1.777778"); None = the
/// video's own (mpv says "no", "-1" or "-2").
fn aspect_ratio(s: &str) -> Option<f64> {
    let r = match s.split_once(':') {
        Some((a, b)) => a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?,
        None => s.trim().parse().ok()?,
    };
    (r.is_finite() && r > 0.).then_some(r)
}

/// What's decoding the video right now, worded so "hardware decoding on" + CPU decoding makes sense:
/// the GPU can't decode every codec (e.g. AV1 or VP9 on older cards) and mpv then falls back to the CPU.
fn decoder_note(hw_on: bool, idle: bool, current: Option<&str>, codec: Option<&str>) -> Option<String> {
    if idle {
        return None;
    }
    let codec = codec.map(|c| c.split_whitespace().next().unwrap_or(c).to_uppercase());
    Some(match (hw_on, current.filter(|c| !c.is_empty() && *c != "no")) {
        (_, Some(api)) => format!("Decoding on the GPU ({})", api.trim_end_matches("-copy")),
        (false, None) => "Decoding on the CPU".into(),
        (true, None) => match codec {
            Some(c) => format!("Decoding on the CPU: your GPU can't decode {c} video"),
            None => "Decoding on the CPU: your GPU can't decode this video".into(),
        },
    })
}

/// Some(value) for an observed property (None inside = currently unavailable); None = not observed.
fn cached(name: &str) -> Option<Option<String>> {
    let key = OBSERVED.iter().find(|k| **k == name)?;
    Some(CACHE.lock().unwrap().get(key).cloned())
}
/// Rotation picked in Quick Settings (degrees); kept across files like mpv's own option was.
static USER_ROTATE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static IN_FLIGHT: Mutex<String> = Mutex::new(String::new());

struct InFlight;
impl InFlight {
    fn new(what: &str) -> Self {
        *IN_FLIGHT.lock().unwrap() = what.to_string();
        InFlight
    }
}
impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.lock().unwrap().clear();
    }
}

fn spawn_watchdog() {
    std::thread::spawn(|| {
        let pid = std::process::id();
        let mut last = 0;
        loop {
            std::thread::sleep(Duration::from_millis(1000));
            let hb = HEARTBEAT.load(Ordering::Relaxed);
            if hb == last && hb > 0 {
                let wchan = std::fs::read_to_string(format!("/proc/self/task/{pid}/wchan")).unwrap_or_default();
                log::error("watchdog", &format!("UI tick stalled; in-flight mpv call: {:?}; main wchan: {wchan}", IN_FLIGHT.lock().unwrap()));
            }
            last = hb;
        }
    });
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
    fn new() -> Arc<Self> {
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
    fn load_plugins(&self) {
        let off = settings::disabled("scripts");
        for p in settings::scripts() {
            if p.file_name().is_some_and(|n| off.iter().any(|o| *o == *n.to_string_lossy())) {
                continue;
            }
            log::info("scripts", &format!("loading {}", p.display()));
            self.cmd(&["load-script", &p.to_string_lossy()]);
        }
    }

    fn get_i64(&self, name: &str) -> Option<i64> {
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

    fn get_string(&self, name: &str) -> Option<String> {
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

    fn opt(&self, k: &str, v: &str) {
        let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
        unsafe { mpv_set_option_string(self.0, k.as_ptr(), v.as_ptr()) };
    }

    /// Rotate by a `transpose` filter: user rotation plus the file's rotation metadata (phone videos).
    /// The filter changes dwidth/dheight, so the renderer gets the rotated size.
    pub fn set_rotation(&self, user: i64) {
        USER_ROTATE.store(user, Ordering::Relaxed);
        self.apply_rotation();
    }

    fn apply_rotation(&self) {
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
        let mut cache = PARSED.lock().unwrap();
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
        let owned: Vec<CString> = args.iter().map(|a| CString::new(*a).unwrap()).collect();
        let mut ptrs: Vec<*const i8> = owned.iter().map(|a| a.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        unsafe { mpv_command_async(self.0, 0, ptrs.as_mut_ptr()) };
    }

    /// String property via the property API: the value is taken literally (no option-string parsing).
    fn set_str(&self, name: &str, value: &str) {
        let _g = InFlight::new(name);
        let (n, v) = (CString::new(name).unwrap(), CString::new(value.replace('\0', "")).unwrap());
        unsafe { mpv_set_property_string(self.0, n.as_ptr(), v.as_ptr()) };
    }

    /// String-list property as an mpv node array: each entry stays one literal item (commas included).
    fn set_str_list(&self, name: &str, items: &[String]) {
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
    fn reset_stream_opts(&self) {
        self.set_str("user-agent", "libmpv");
        self.set_str("referrer", "");
        self.set_str_list("http-header-fields", &[]);
        self.set_str("force-media-title", "");
        *self.1.lock().unwrap() = AfterLoad::default();
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
        self.1.lock().unwrap().jump = Some(index);
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
        *self.1.lock().unwrap() = AfterLoad { subs: pb.subtitles.iter().map(|s| s.url.clone()).collect(), start: pb.start, jump: None };
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

    fn get_f64(&self, name: &str) -> Option<f64> {
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

    fn get_flag(&self, name: &str) -> bool {
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

/// <config>/zym-player — %APPDATA% on Windows, $XDG_CONFIG_HOME or ~/.config elsewhere. Settings,
/// playlists, scripts and plugins live here. Deliberately still "zym-player" after the binary
/// became zym-media-player, so existing users keep everything.
pub fn config_dir() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(Into::into)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("zym-player")
}

/// <config>/zym-player/plugins: mpv scripts.
pub fn plugins_dir() -> std::path::PathBuf {
    config_dir().join("plugins")
}

/// Window size in physical pixels (w << 32 | h), written by the UI tick. The render thread fits the
/// video into it: a 4K frame shown in an 1100 px window is ~33 MB of wasted work per frame otherwise.
static VIEW_PX: AtomicU64 = AtomicU64::new(0);

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
type Wake = (Mutex<bool>, Condvar);

fn wake(w: &Wake) {
    *w.0.lock().unwrap() = true;
    w.1.notify_one();
}

unsafe extern "C" fn on_update(ctx: *mut c_void) {
    wake(unsafe { &*(ctx as *const Wake) });
}

/// Latest rendered frame: (width, height, BGRA bytes).
type Latest = Arc<Mutex<Option<(u32, u32, Vec<u8>)>>>;

/// Display size packed as (w << 32 | h); 0 = unknown. Written by the event thread so the
/// render thread never calls into the mpv core (render.h warns that can deadlock).
type VideoSize = Arc<AtomicU64>;

/// Drains mpv's event queue (a full queue makes async commands fail silently), tracks video size,
/// and passes mpv's log messages to the app log (log.rs).
#[allow(non_upper_case_globals)] // bindgen constants used as match patterns
fn spawn_event_thread(mpv: Arc<Mpv>, size: VideoSize, wake_render: &'static Wake) {
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
                mpv_event_id_MPV_EVENT_LOG_MESSAGE => {
                    let m = &*(ev.data as *const mpv_event_log_message);
                    let s = |p: *const i8| std::ffi::CStr::from_ptr(p).to_string_lossy();
                    log::add(log::Level::from_mpv(&s(m.level)), &format!("mpv/{}", s(m.prefix)), &s(m.text));
                }
                mpv_event_id_MPV_EVENT_FILE_LOADED => {
                    let after = std::mem::take(&mut *mpv.1.lock().unwrap());
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
                    if e.reason == mpv_end_file_reason_MPV_END_FILE_REASON_ERROR {
                        log::error("mpv", &format!("file failed: {}", mpv_error_str(e.error)));
                    }
                }
                mpv_event_id_MPV_EVENT_PROPERTY_CHANGE => {
                    let p = &*(ev.data as *const mpv_event_property);
                    let name = std::ffi::CStr::from_ptr(p.name).to_string_lossy();
                    if let Some(key) = OBSERVED.iter().find(|k| **k == name) {
                        let mut cache = CACHE.lock().unwrap();
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
    });
}

fn spawn_render_thread(mpv: Arc<Mpv>, latest: Latest, size: VideoSize, wake_me: &'static Wake) {
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
        loop {
            {
                let mut flag = wake_me.0.lock().unwrap();
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
            let mut buf = vec![0u8; stride * h as usize];
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
            // "0" byte is garbage; force opaque alpha.
            for px in buf.chunks_exact_mut(4) {
                px[3] = 255;
            }
            *latest.lock().unwrap() = Some((w, h, buf));
        }
    });
}

const PAD: f32 = 16.0; // overlay side padding; seek-bar math depends on it
const HIDE_AFTER: Duration = Duration::from_millis(2500);

pub struct Player {
    pub mpv: Arc<Mpv>,
    latest: Latest,
    frame: Option<Arc<RenderImage>>,
    title: SharedString,
    focus: FocusHandle,
    last_move: Instant,
    seek_hover: Option<f32>, // window x of mouse over seek bar
    dragging: bool,
    panel: Panel,
    menu: Option<MenuState>,
    about: bool,
    /// Plugin install dialog (install.rs).
    install: Option<install::Dialog>,
    pop: Option<Pop>,
    slider_drag: Option<Slider>,
    /// Controls opacity 0..1, eased toward shown/hidden each frame.
    ui_alpha: f32,
    last_frame: Instant,
    pub explorer: Explorer,
    pub settings: Option<settings::SettingsPanel>,
    pub pl: playlists::PlaylistUi,
    /// Caret + selection of the focused text box (see textbox.rs).
    pub caret: Option<textbox::Cursor>,
    /// A text box is being drag-selected.
    pub text_drag: bool,
    /// Big panel width set by dragging its edge (None = default), and a drag in progress.
    /// Dragged width of each big panel, by its id ("explorer", "playlists", "settings").
    pub panel_w: std::collections::HashMap<String, f32>,
    /// The big panel whose edge is being dragged.
    pub panel_drag: Option<&'static str>,
    /// Details cards from content plugins, by stream URL (= mpv playlist filename).
    pub infos: std::collections::HashMap<String, sources::Info>,
    /// Music visualizer (audio-only files).
    pub viz: visualizer::Viz,
    /// Audio EQ preset in Quick Settings › Audio (a name from AUDIO_EQ).
    eq: &'static str,
    /// Picture values in the `@zympic` filter (None = no filter).
    pic: Option<[i64; 5]>,
    pub info_open: bool,
    /// Settings › mpv: couldn't create/open mpv.conf.
    pub mpv_conf_error: Option<String>,
    /// Settings › Logs: mpv sends everything (this session only).
    pub mpv_verbose: bool,
    /// Result of the update check About runs (once per session unless it failed).
    pub update: update::Update,
    /// Per text box (by field): text start x, font and size from the last paint, for click-to-caret.
    pub text_geo: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, (gpui::Pixels, gpui::Font, gpui::Pixels)>>>,
    /// Text box with keyboard focus (search, add-URL, settings form).
    pub editing: Option<Field>,
    pub add_url: String,
    /// Playlist "+" popup (paste URL / choose files) is open.
    pub add_popup: bool,
    /// Playlist index that was playing when Stop was pressed (▶ resumes there).
    stopped_at: Option<i64>,
}

#[derive(Clone, Copy, PartialEq)]
enum OpenKind {
    Media,
    Folder,
    Subtitle,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Panel {
    None,
    Playlist,
}

/// Pages of the quick-settings popover (gear button), YouTube/Clapper style.
#[derive(Clone, Copy, PartialEq)]
enum Pop {
    Main,
    Speed,
    Subtitles,
    Audio,
    Video,
    Picture,
    Sync,
}

/// A draggable slider bound to a numeric mpv property.
#[derive(Clone, Copy)]
struct Slider {
    label: &'static str,
    prop: &'static str,
    min: f64,
    max: f64,
    default: f64,
    step: f64,
    fmt: fn(f64) -> String,
}

const fn slider(label: &'static str, prop: &'static str, min: f64, max: f64, default: f64, step: f64, fmt: fn(f64) -> String) -> Slider {
    Slider { label, prop, min, max, default, step, fmt }
}

fn f_int(v: f64) -> String { format!("{v:.0}") }
fn f_secs(v: f64) -> String { format!("{v:+.2}s") }
fn f_x(v: f64) -> String { format!("{v:.2}×") }
fn f_zoom(v: f64) -> String { format!("{v:+.2}") }

const SPEED: Slider = slider("Custom speed", "speed", 0.25, 4.0, 1.0, 0.05, f_x);
const SUB_SIZE: Slider = slider("Size", "sub-scale", 0.5, 3.0, 1.0, 0.05, f_x);
const SUB_POS: Slider = slider("Position", "sub-pos", 0.0, 150.0, 100.0, 1.0, f_int);
const PICTURE: [Slider; 6] = [
    slider("Brightness", "brightness", -100.0, 100.0, 0.0, 1.0, f_int),
    slider("Contrast", "contrast", -100.0, 100.0, 0.0, 1.0, f_int),
    slider("Saturation", "saturation", -100.0, 100.0, 0.0, 1.0, f_int),
    slider("Gamma", "gamma", -100.0, 100.0, 0.0, 1.0, f_int),
    slider("Hue", "hue", -100.0, 100.0, 0.0, 1.0, f_int),
    slider("Zoom", "video-zoom", -1.0, 2.0, 0.0, 0.05, f_zoom),
];
/// (name, [brightness, contrast, saturation, gamma]); hue and zoom are left alone.
/// The same presets as the GoCast Android TV receiver.
const PICTURE_PRESETS: [(&str, [i64; 4]); 10] = [
    ("Auto", [0, 0, 0, 0]),
    ("Vivid", [5, 10, 25, 0]),
    ("Movie", [-3, 8, 10, 3]),
    ("Cinematic", [-5, 5, -10, 5]),
    ("Sports", [3, 8, 20, -3]),
    ("Animation", [5, 5, 30, 0]),
    ("Documentary", [-2, 5, -5, 5]),
    ("Classic B&W", [0, 5, -100, 0]),
    ("Warm", [2, 3, 10, 0]),
    ("Night", [-15, -5, -10, 10]),
];
const AUDIO_DELAY: Slider = slider("Audio delay", "audio-delay", -5.0, 5.0, 0.0, 0.05, f_secs);
const SUB_DELAY: Slider = slider("Subtitle delay", "sub-delay", -10.0, 10.0, 0.0, 0.05, f_secs);

const POP_W: f32 = 300.;
const POP_INSET: f32 = 14.; // popover inner padding; slider math depends on it

impl Player {
    fn new(files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mpv = Mpv::new();
        mpv.load_plugins();
        let latest: Latest = Arc::default();
        let size: VideoSize = Arc::default();
        let wake_render: &'static Wake = Box::leak(Box::new((Mutex::new(false), Condvar::new())));
        spawn_event_thread(mpv.clone(), size.clone(), wake_render);
        spawn_render_thread(mpv.clone(), latest.clone(), size, wake_render);
        // Only after the render context exists: loading earlier races vo/libmpv ("No render context set").
        for f in &files {
            mpv.load(f, "append-play"); // dirs and .m3u expand natively in mpv
        }

        // ponytail: 60Hz poll; swap for a channel + cx.notify from the render thread if it matters.
        cx.on_app_quit(|this, _| {
            this.save_position();
            async {}
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| loop {
            Timer::after(Duration::from_millis(16)).await;
            if let Err(e) = this.update_in(cx, |p, window, cx| p.tick(window, cx)) {
                // Only a dropped player ends the loop; anything else is logged and retried.
                if this.upgrade().is_none() {
                    break;
                }
                log::error("tick", &format!("{e:#}"));
            }
        })
        .detach();

        let focus = cx.focus_handle();
        window.focus(&focus);
        let player = Self {
            mpv,
            latest,
            frame: None,
            title: SharedString::default(),
            focus,
            last_move: Instant::now(),
            seek_hover: None,
            dragging: false,
            panel: if files.len() > 1 { Panel::Playlist } else { Panel::None }, // empty start: nothing open
            menu: None,
            about: false,
            install: None,
            pop: None,
            slider_drag: None,
            ui_alpha: 1.0,
            last_frame: Instant::now(),
            explorer: Explorer::new(),
            settings: None,
            pl: Default::default(),
            caret: None,
            text_drag: false,
            panel_w: settings::get("panel_widths").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
            panel_drag: None,
            infos: Default::default(),
            viz: visualizer::Viz::new(),
            pic: None,
            eq: "Flat",
            info_open: false,
            mpv_conf_error: None,
            mpv_verbose: std::env::var_os("ZYM_DEBUG").is_some(),
            update: Default::default(),
            text_geo: Default::default(),
            editing: None,
            add_url: String::new(),
            add_popup: false,
            stopped_at: None,
        };
        if std::env::var_os("ZYM_DEBUG").is_some() {
            spawn_watchdog();
        }
        // Debug: play a source item through the same path as a click, no input needed. Value = item id.
        if let Ok(id) = std::env::var("ZYM_DEBUG_AUTOPLAY") {
            cx.spawn(async move |this, cx| {
                Timer::after(Duration::from_secs(2)).await;
                this.update(cx, |p, cx| p.debug_play(id, cx)).ok();
            })
            .detach();
        }
        player
    }

    /// Playing with no video track (an mp3 without cover art): mpv never sends a frame.
    fn audio_only(&self) -> bool {
        self.mpv.get_f64("time-pos").is_some() && self.mpv.get_string("video-codec").is_none()
    }

    /// Keep mpv's `@viz` analysis filter in the audio chain while music plays with a visualizer
    /// chosen; it stays across tracks and while stopped (adding it restarts the audio), and goes
    /// when a file with video plays or the visualizer is off.
    pub fn set_visualizer(&mut self, style: visualizer::Style) {
        self.viz.style = style;
        style.save();
    }

    /// Quick Settings › Picture. The software renderer ignores mpv's brightness / contrast /
    /// saturation / gamma / hue, so the sliders' values drive an ffmpeg `eq` + `hue` filter instead:
    /// added on the first change, updated in place while dragging (vf-command, no re-init),
    /// removed when everything is back to 0.
    fn update_picture(&mut self) {
        let vals = PIC_PROPS.map(|p| self.mpv.get_f64(p).unwrap_or(0.).round() as i64);
        if self.pic == Some(vals) || (self.pic.is_none() && vals == [0; 5]) {
            return;
        }
        if vals == [0; 5] {
            self.mpv.cmd(&["vf", "remove", "@zympic"]);
            self.pic = None;
            return;
        }
        let args = picture_args(vals);
        match self.pic {
            None => {
                let eq: Vec<String> = args[..4].iter().map(|(k, v, _)| format!("{k}={v}")).collect();
                self.mpv.cmd(&["vf", "add", &format!("@zympic:lavfi=[eq={},hue=h={}]", eq.join(":"), args[4].1)]);
            }
            Some(old) => {
                for (_, (k, v, target)) in args.iter().enumerate().filter(|(i, _)| old[*i] != vals[*i]) {
                    self.mpv.cmd(&["vf-command", "zympic", k, v, target]);
                }
            }
        }
        self.pic = Some(vals);
    }

    /// Swap the `@zymeq` audio filter for preset `name` (Flat = none). Only this filter changes;
    /// loudness normalisation and the visualizer stay in the chain.
    fn set_eq(&mut self, name: &'static str) {
        if self.eq != "Flat" {
            self.mpv.cmd(&["af", "remove", "@zymeq"]);
        }
        if let Some(f) = eq_filter(name) {
            self.mpv.cmd(&["af", "add", &f]);
        }
        self.eq = name;
    }

    fn update_visualizer(&mut self) {
        let want = if self.viz.style == visualizer::Style::Off || self.mpv.get_string("video-codec").is_some() {
            false
        } else {
            self.viz.active || self.audio_only()
        };
        if want != self.viz.active {
            self.viz.active = want;
            if want {
                self.mpv.cmd(&["af", "add", &visualizer::filter()]);
            } else {
                self.mpv.cmd(&["af", "remove", &format!("@{}", visualizer::LABEL)]);
            }
        }
        if self.viz.active {
            let playing = !self.mpv.get_flag("pause") && !self.mpv.get_flag("idle-active");
            self.viz.update(self.mpv.get_string(visualizer::PROPERTY).as_deref(), playing);
        }
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        HEARTBEAT.fetch_add(1, Ordering::Relaxed);
        theme::resolve(window.appearance());
        let (vs, scale) = (window.viewport_size(), window.scale_factor());
        let (vw, vh) = ((f32::from(vs.width) * scale) as u64, (f32::from(vs.height) * scale) as u64);
        VIEW_PX.store(vw << 32 | vh, Ordering::Relaxed);
        if let Some((w, h, bytes)) = self.latest.lock().unwrap().take() {
            let buf = ImageBuffer::from_raw(w, h, bytes).expect("frame size");
            let new = Arc::new(RenderImage::new([Frame::new(buf)]));
            if let Some(old) = self.frame.replace(new) {
                window.drop_image(old).ok(); // free atlas slot, or GPU memory grows forever
            }
        }
        self.update_visualizer();
        self.update_picture();
        // Stopped, or now playing audio only: don't leave the last video's frame on screen.
        if self.mpv.get_flag("idle-active") || self.audio_only() {
            if let Some(old) = self.frame.take() {
                window.drop_image(old).ok();
            }
        }
        // Stopped/empty, or the last item finished (keep-open holds its final frame): no stale title.
        let title: SharedString = if self.mpv.get_flag("idle-active") || self.mpv.get_flag("eof-reached") {
            APP_NAME.into()
        } else {
            self.mpv.get_string("media-title").unwrap_or_else(|| APP_NAME.into()).into()
        };
        if title != self.title {
            window.set_window_title(&if title.as_ref() == APP_NAME { title.to_string() } else { format!("{title} — {APP_NAME}") });
            self.title = title;
        }
        cx.notify(); // also refreshes time readout + auto-hide
    }

    /// (display name, is_current) for each playlist entry.
    fn playlist(&self) -> Vec<(String, bool)> {
        let cur = self.mpv.get_i64("playlist-pos").unwrap_or(-1);
        self.mpv
            .playlist_entries()
            .into_iter()
            .enumerate()
            .map(|(i, (file, title))| (playlists::display_name(&(title, file)), i as i64 == cur))
            .collect()
    }

    /// Window x -> 0..1 along the seek bar (bar spans window width minus PAD each side).
    fn seek_frac(x: f32, window: &Window) -> f32 {
        let w = f32::from(window.viewport_size().width) - 2.0 * PAD;
        ((x - PAD) / w).clamp(0.0, 1.0)
    }

    fn seek_to(&self, frac: f32) {
        self.mpv.cmd(&["seek", &(frac * 100.0).to_string(), "absolute-percent"]);
    }

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &e.keystroke;
        if let Some(f) = self.editing.clone() {
            let plain = !ks.modifiers.control && !ks.modifiers.alt && !ks.modifiers.platform;
            match ks.key.as_str() {
                "escape" => {
                    self.editing = None;
                    self.add_popup = false;
                    self.menu = None;
                }
                "enter" => self.submit_field(f, cx),
                "v" if !plain => self.paste_into(&f, cx),
                "c" if !plain => self.copy_selection(&f, false, cx),
                "x" if !plain => self.copy_selection(&f, true, cx),
                "a" if !plain => {
                    self.field_edit(&f, "all", false, None);
                }
                k @ ("left" | "right" | "home" | "end" | "backspace" | "delete") => {
                    self.field_edit(&f, k, ks.modifiers.shift, None);
                }
                _ => {
                    if let (true, Some(c)) = (plain, ks.key_char.clone()) {
                        self.field_edit(&f, "", false, Some(&c));
                    }
                }
            }
            cx.notify();
            return;
        }
        if ks.key == "escape" && (self.menu.is_some() || self.about || self.install.is_some()) {
            self.menu = None;
            self.about = false;
            self.install = None;
            return;
        }
        if ks.key == "escape" && self.any_overlay() {
            return self.close_overlays();
        }
        let m = &self.mpv;
        if ks.modifiers.control && ks.key == "o" {
            return self.open_dialog(OpenKind::Media, cx);
        }
        if ks.modifiers.control && ks.key == "l" {
            return self.open_url_box();
        }
        if ks.modifiers.control && ks.key == "," {
            return self.toggle_settings(cx);
        }
        match ks.key.as_str() {
            "space" | "k" => return self.play_pause(),
            "left" => m.cmd(&["seek", "-5"]),
            "right" => m.cmd(&["seek", "5"]),
            "j" => m.cmd(&["seek", "-10"]),
            "l" => m.cmd(&["seek", "10"]),
            "up" => m.cmd(&["add", "volume", "5"]),
            "down" => m.cmd(&["add", "volume", "-5"]),
            "m" => m.cmd(&["cycle", "mute"]),
            "f" => window.toggle_fullscreen(),
            "escape" if window.is_fullscreen() => window.toggle_fullscreen(),
            "." => m.cmd(&["frame-step"]),
            "," => m.cmd(&["frame-back-step"]),
            "n" => m.cmd(&["playlist-next"]),
            "b" => m.cmd(&["playlist-prev"]),
            "p" => self.toggle_panel(Panel::Playlist),
            "e" => self.toggle_explorer(cx),
            "s" => self.toggle_pop(),
            "q" => cx.quit(),
            // Anything else goes to mpv so plugin key bindings fire.
            _ => m.cmd(&["keypress", &mpv_key_name(&e.keystroke)]),
        }
        self.last_move = Instant::now(); // flash controls on any shortcut
    }
}

impl Player {
    /// One overlay at a time: every panel/popover opener calls this first, so a new overlay type
    /// only has to be added here.
    /// Any panel/popover/card open (what Esc and a click on the video close first).
    pub fn any_overlay(&self) -> bool {
        self.panel != Panel::None || self.explorer.open || self.settings.is_some() || self.pl.open || self.pop.is_some() || self.add_popup || self.info_open
    }

    pub fn close_overlays(&mut self) {
        self.panel = Panel::None;
        self.explorer.open = false;
        self.settings = None;
        self.pl.open = false;
        self.pop = None;
        self.add_popup = false;
        self.info_open = false;
        self.editing = None;
    }

    fn toggle_panel(&mut self, p: Panel) {
        let open = self.panel != p;
        self.close_overlays();
        if open {
            self.panel = p;
        }
    }

    /// Show the playlist with the "add URL" box focused.
    /// "Resume where you left off": mpv only saves on its own quit command, which the player never
    /// sends, so save explicitly on Stop and when the app quits.
    pub fn save_position(&self) {
        if self.mpv.get_string("save-position-on-quit").as_deref() == Some("yes") && !self.mpv.get_flag("idle-active") {
            self.mpv.cmd(&["write-watch-later-config"]);
        }
    }

    fn stop(&mut self) {
        self.save_position();
        self.stopped_at = self.mpv.get_i64("playlist-pos").filter(|p| *p >= 0);
        self.mpv.reset_stream_opts(); // drop plugin-forced title/headers too
        self.mpv.cmd(&["stop"]); // full stop: ends playback and clears the playlist
    }

    /// Play/pause; after Stop, restart the item that was playing (or the first).
    fn play_pause(&mut self) {
        if self.mpv.get_flag("idle-active") && self.mpv.get_i64("playlist-count").unwrap_or(0) > 0 {
            let i = self.stopped_at.take().unwrap_or(0).to_string();
            self.mpv.cmd(&["playlist-play-index", &i]);
            self.mpv.cmd(&["set", "pause", "no"]);
        } else {
            self.mpv.cmd(&["cycle", "pause"]);
        }
    }

    /// Playlist + its "+" popup with the URL box focused.
    fn open_url_box(&mut self) {
        self.close_overlays();
        self.panel = Panel::Playlist;
        self.add_popup = true;
        self.editing = Some(Field::AddUrl);
    }

    /// Audio/subtitle tracks of the current file: (id, label, selected).
    fn tracks(&self, kind: &str) -> Vec<(i64, String, bool)> {
        let n = self.mpv.get_i64("track-list/count").unwrap_or(0);
        (0..n)
            .filter(|i| self.mpv.get_string(&format!("track-list/{i}/type")).as_deref() == Some(kind))
            .map(|i| {
                let g = |k: &str| self.mpv.get_string(&format!("track-list/{i}/{k}"));
                let id = self.mpv.get_i64(&format!("track-list/{i}/id")).unwrap_or(0);
                let label = match (g("title"), g("lang")) {
                    (Some(t), Some(l)) => format!("{t} ({l})"),
                    (Some(t), None) => t,
                    (None, Some(l)) => l,
                    (None, None) => format!("Track {id}"),
                };
                (id, label, self.mpv.get_flag(&format!("track-list/{i}/selected")))
            })
            .collect()
    }

    fn toggle_pop(&mut self) {
        let open = self.pop.is_none();
        self.close_overlays();
        if open {
            self.pop = Some(Pop::Main);
        }
    }

    /// Window x -> slider value. Popover is pinned PAD from the right edge, so geometry is fixed.
    fn slider_value(sl: &Slider, x: f32, window: &Window) -> f64 {
        let left = f32::from(window.viewport_size().width) - PAD - POP_W + POP_INSET;
        let frac = ((x - left) / (POP_W - 2. * POP_INSET)).clamp(0., 1.) as f64;
        let v = sl.min + frac * (sl.max - sl.min);
        (v / sl.step).round() * sl.step
    }

    fn set_slider(&self, sl: &Slider, x: f32, window: &Window) {
        self.mpv.cmd(&["set", sl.prop, &format!("{:.4}", Self::slider_value(sl, x, window))]);
    }

    fn slider_row(&self, sl: Slider, cx: &mut Context<Self>) -> Div {
        let v = self.mpv.get_f64(sl.prop).unwrap_or(sl.default);
        let frac = (((v - sl.min) / (sl.max - sl.min)).clamp(0., 1.)) as f32;
        let changed = (v - sl.default).abs() > sl.step / 2.;
        let active = self.slider_drag.is_some_and(|d| d.prop == sl.prop);
        div()
            .px(px(POP_INSET))
            .py_1()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(div().text_color(t().fg()).child(sl.label))
                    .child(
                        div()
                            .id(SharedString::from(format!("val-{}", sl.prop)))
                            .text_xs()
                            .text_color(if changed { t().accent() } else { t().muted() })
                            .when(changed, |d| d.cursor_pointer().hover(|s| s.underline()))
                            .child((sl.fmt)(v))
                            // click the value to reset
                            .on_click(cx.listener(move |this, _, _, _| this.mpv.cmd(&["set", sl.prop, &sl.default.to_string()]))),
                    ),
            )
            .child(
                div()
                    .id(SharedString::from(format!("slider-{}", sl.prop)))
                    .relative()
                    .h(px(18.))
                    .w_full()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, window, _| {
                            this.slider_drag = Some(sl);
                            this.set_slider(&sl, e.position.x.into(), window);
                        }),
                    )
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(4.))
                            .rounded_full()
                            .bg(t().pressed())
                            .child(div().absolute().h_full().rounded_full().bg(t().accent()).w(relative(frac))),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(relative(frac))
                            .ml(px(-7.))
                            .size(px(14.))
                            .rounded_full()
                            .bg(rgb(0xffffff))
                            .when(active, |d| d.size(px(16.)).ml(px(-8.))),
                    ),
            )
    }

    fn pop_header(&self, title: &str, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("pop-back")
            .flex()
            .items_center()
            .gap_2()
            .h(px(40.))
            .px(px(POP_INSET))
            .mb_1()
            .border_b_1()
            .border_color(t().border())
            .cursor_pointer()
            .hover(|s| s.bg(t().hover()))
            .child(div().text_color(t().muted()).child("‹"))
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title.to_string()))
            .on_click(cx.listener(|this, _, _, _| this.pop = Some(Pop::Main)))
    }

    fn pop_row(id: impl Into<SharedString>) -> Stateful<Div> {
        div()
            .id(id.into())
            .flex()
            .items_center()
            .gap_2()
            .h(px(36.))
            .px(px(POP_INSET))
            .cursor_pointer()
            .hover(|s| s.bg(t().hover()))
    }

    /// "Label ........ value ›" on the main page.
    fn nav_row(&self, label: &str, value: String, page: Pop, cx: &mut Context<Self>) -> Stateful<Div> {
        Self::pop_row(format!("nav-{label}"))
            .child(div().flex_1().child(label.to_string()))
            .child(div().max_w(px(140.)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(t().muted()).child(value))
            .child(div().text_color(t().muted()).child("›"))
            .on_click(cx.listener(move |this, _, _, _| this.pop = Some(page)))
    }

    /// Radio entry: ✓ when selected, runs `cmd` on click.
    fn radio_row(&self, id: String, label: impl Into<SharedString>, selected: bool, cmd: Vec<String>, cx: &mut Context<Self>) -> Stateful<Div> {
        Self::radio_pick(id, label, selected, cx, move |this| this.mpv.cmd(&cmd.iter().map(String::as_str).collect::<Vec<_>>()))
    }

    /// ✓ row that runs `pick` when clicked.
    fn radio_pick(id: String, label: impl Into<SharedString>, selected: bool, cx: &mut Context<Self>, pick: impl Fn(&mut Self) + 'static) -> Stateful<Div> {
        Self::pop_row(id)
            .child(div().w(px(16.)).text_color(t().accent()).child(if selected { "✓" } else { "" }))
            .child(div().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label.into()))
            .on_click(cx.listener(move |this, _, _, _| pick(this)))
    }

    /// Label + switch.
    fn toggle_row(&self, label: &str, on: bool, cmd: Vec<String>, cx: &mut Context<Self>) -> Stateful<Div> {
        Self::pop_row(format!("tg-{label}"))
            .child(div().flex_1().child(label.to_string()))
            .child(switch(on))
            .on_click(cx.listener(move |this, _, _, _| this.mpv.cmd(&cmd.iter().map(String::as_str).collect::<Vec<_>>())))
    }

    fn pop_note(text: &str) -> Div {
        div().px(px(POP_INSET)).py_1().text_xs().text_color(t().dim()).child(text.to_string())
    }

    fn quick_settings(&self, page: Pop, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = &self.mpv;
        let v = |cmd: &[&str]| cmd.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let selected_track = |kind: &str| self.tracks(kind).into_iter().find(|t| t.2).map(|t| t.1);
        let speed = m.get_f64("speed").unwrap_or(1.0);
        let aspect = m.get_string("video-aspect-override").unwrap_or_default();
        let aspect = if aspect.starts_with('-') || aspect.is_empty() { "no".to_string() } else { aspect };
        const ASPECTS: [(&str, &str); 5] = [("Default", "no"), ("16:9", "16:9"), ("4:3", "4:3"), ("21:9", "2.35:1"), ("1:1", "1:1")];
        let current = aspect_ratio(&aspect);
        let aspect_label = ASPECTS
            .iter()
            .find(|a| match (aspect_ratio(a.1), current) {
                (None, None) => true,
                (Some(x), Some(y)) => (x - y).abs() < 0.01,
                _ => false,
            })
            .map(|a| a.0)
            .unwrap_or("Custom");

        let body = div().flex().flex_col();
        let body = match page {
            Pop::Main => {
                let picture_changed = PICTURE.iter().any(|s| (m.get_f64(s.prop).unwrap_or(s.default) - s.default).abs() > s.step / 2.);
                let (ad, sd) = (m.get_f64("audio-delay").unwrap_or(0.0), m.get_f64("sub-delay").unwrap_or(0.0));
                body.child(self.nav_row("Speed", if (speed - 1.0).abs() < 0.001 { "Normal".into() } else { f_x(speed) }, Pop::Speed, cx))
                    .child(self.nav_row("Subtitles", selected_track("sub").unwrap_or_else(|| "Off".into()), Pop::Subtitles, cx))
                    .child(self.nav_row("Audio", selected_track("audio").unwrap_or_else(|| "None".into()), Pop::Audio, cx))
                    .child(self.nav_row("Video", aspect_label.to_string(), Pop::Video, cx))
                    .child(self.nav_row("Picture", if picture_changed { "Adjusted".into() } else { "Default".into() }, Pop::Picture, cx))
                    .child(self.nav_row(
                        "Sync",
                        if ad == 0.0 && sd == 0.0 { "In sync".into() } else { format!("A {} · S {}", f_secs(ad), f_secs(sd)) },
                        Pop::Sync,
                        cx,
                    ))
            }
            Pop::Speed => body
                .child(self.pop_header("Speed", cx))
                .children([0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0].map(|s| {
                    let label = if s == 1.0 { "Normal".to_string() } else { format!("{s}×") };
                    self.radio_row(format!("spd-{s}"), label, (speed - s).abs() < 0.001, v(&["set", "speed", &s.to_string()]), cx)
                }))
                .child(self.slider_row(SPEED, cx)),
            Pop::Subtitles => {
                let tracks = self.tracks("sub");
                let none = !tracks.iter().any(|t| t.2);
                body.child(self.pop_header("Subtitles", cx))
                    .child(self.radio_row("sid-off".into(), "Off", none, v(&["set", "sid", "no"]), cx))
                    .children(tracks.into_iter().map(|(id, l, sel)| self.radio_row(format!("sid-{id}"), l, sel, v(&["set", "sid", &id.to_string()]), cx)))
                    .child(
                        Self::pop_row("sub-add")
                            .child(div().w(px(16.)).text_color(t().muted()).child("+"))
                            .child("Add subtitle file…")
                            .on_click(cx.listener(|this, _, _, cx| this.open_dialog(OpenKind::Subtitle, cx))),
                    )
                    .child(Self::pop_note("Or drop a .srt / .ass / .vtt onto the video"))
                    .child(self.slider_row(SUB_SIZE, cx))
                    .child(self.slider_row(SUB_POS, cx))
            }
            Pop::Audio => {
                let tracks = self.tracks("audio");
                let empty = tracks.is_empty();
                let norm = m.get_string("af").unwrap_or_default().contains("loudnorm");
                body.child(self.pop_header("Audio", cx))
                    .children(tracks.into_iter().map(|(id, l, sel)| self.radio_row(format!("aid-{id}"), l, sel, v(&["set", "aid", &id.to_string()]), cx)))
                    .when(empty, |d| d.child(Self::pop_note("No audio tracks")))
                    .child(div().h(px(1.)).my_1().bg(t().border()))
                    .child(self.toggle_row("Normalize loudness", norm, v(&["af", "toggle", "@norm:lavfi=[loudnorm]"]), cx))
                    .child(self.toggle_row("Mute", m.get_flag("mute"), v(&["cycle", "mute"]), cx))
                    .child(div().h(px(1.)).my_1().bg(t().border()))
                    .child(Self::pop_note("EQUALIZER"))
                    .children(AUDIO_EQ.map(|(label, _)| Self::radio_pick(format!("eq-{label}"), label, self.eq == label, cx, move |this| this.set_eq(label))))
            }
            Pop::Video => {
                let rotate = USER_ROTATE.load(Ordering::Relaxed);
                let hw_on = m.get_string("hwdec").unwrap_or_default() != "no";
                let decoder = decoder_note(hw_on, m.get_flag("idle-active"), m.get_string("hwdec-current").as_deref(), m.get_string("video-codec").as_deref());
                body.child(self.pop_header("Video", cx))
                    .child(Self::pop_note("ASPECT RATIO"))
                    .children(ASPECTS.map(|(l, val)| self.radio_row(format!("asp-{l}"), l, aspect_label == l, v(&["set", "video-aspect-override", val]), cx)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px(px(POP_INSET))
                            .py_1()
                            .child(div().flex_1().child("Rotate"))
                            .children([0, 90, 180, 270].map(|r| {
                                chip(SharedString::from(format!("rot-{r}")), format!("{r}°"), rotate == r)
                                    .on_click(cx.listener(move |this, _, _, _| this.mpv.set_rotation(r)))
                            })),
                    )
                    .child(self.toggle_row("Deinterlace", m.get_flag("deinterlace"), v(&["cycle", "deinterlace"]), cx))
                    .child(
                        Self::pop_row("tg-hwdec")
                            .child(div().flex_1().child("Hardware decoding"))
                            .child(switch(hw_on))
                            .on_click(cx.listener(move |this, _, _, _| this.set_hwdec(!hw_on))),
                    )
                    .children(decoder.map(|d| Self::pop_note(&d)))
            }
            Pop::Picture => {
                let now = ["brightness", "contrast", "saturation", "gamma"].map(|p| m.get_f64(p).unwrap_or(0.).round() as i64);
                body.child(self.pop_header("Picture", cx))
                .child(Self::pop_note("PRESETS"))
                .children(PICTURE_PRESETS.map(|(label, vals)| {
                    Self::radio_pick(format!("pp-{label}"), label, now == vals, cx, move |this| {
                        for (prop, v) in ["brightness", "contrast", "saturation", "gamma"].iter().zip(vals) {
                            this.mpv.cmd(&["set", prop, &v.to_string()]);
                        }
                    })
                }))
                .child(div().h(px(1.)).my_1().bg(t().border()))
                .children(PICTURE.map(|sl| self.slider_row(sl, cx)))
                .child(
                    Self::pop_row("pic-reset")
                        .justify_center()
                        .text_color(t().accent())
                        .child("Reset picture")
                        .on_click(cx.listener(|this, _, _, _| {
                            for sl in PICTURE {
                                this.mpv.cmd(&["set", sl.prop, &sl.default.to_string()]);
                            }
                        })),
                )
            }
            Pop::Sync => body
                .child(self.pop_header("Sync", cx))
                .child(self.slider_row(AUDIO_DELAY, cx))
                .child(self.slider_row(SUB_DELAY, cx))
                .child(Self::pop_note("Positive = later. Click a value to reset.")),
        };

        div()
            .id("quick-settings")
            .absolute()
            .right(px(PAD))
            .bottom(px(84.)) // just above the control row
            .w(px(POP_W))
            .max_h(window.viewport_size().height - px(120.))
            .overflow_y_scroll()
            .py_1()
            .rounded_xl()
            .border_1()
            .border_color(t().border())
            .bg(t().surface())
            .shadow_lg()
            .text_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()) // don't reach the video
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(body)
    }

    fn open_dialog(&mut self, kind: OpenKind, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: kind != OpenKind::Folder,
            directories: kind == OpenKind::Folder,
            multiple: kind != OpenKind::Subtitle,
            prompt: None,
        });
        let mpv = self.mpv.clone();
        cx.spawn(async move |_, _| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            for p in paths {
                let p = p.to_string_lossy();
                match kind {
                    OpenKind::Subtitle => mpv.cmd(&["sub-add", &p, "select"]),
                    _ => mpv.load(&p, "append-play"),
                }
            }
        })
        .detach();
    }

    fn about_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mpv_v = self.mpv.get_string("mpv-version").unwrap_or_default();
        let ff_v = self.mpv.get_string("ffmpeg-version").unwrap_or_default();
        let line = |k: &str, v: String| {
            div().flex().justify_between().gap_4().child(div().text_color(t().muted()).child(k.to_string())).child(v)
        };
        div()
            .id("about-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .bg(rgba(0x00000099))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, _, _| this.about = false))
            .child(
                div()
                    .id("about")
                    .w(px(360.))
                    .p_6()
                    .rounded_xl()
                    .bg(t().surface_solid())
                    .border_1()
                    .border_color(t().border())
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .on_click(|_, _, cx| cx.stop_propagation()) // clicks inside don't close
                    .child(div().text_2xl().font_weight(gpui::FontWeight::BOLD).child(APP_NAME))
                    .child(div().text_color(t().accent()).child(format!("Version {VERSION}")))
                    .child(div().text_color(t().muted()).pb_2().child("A lightweight media player built on mpv and GPUI."))
                    .child(line("Playback engine", mpv_v))
                    .child(line("FFmpeg", ff_v))
                    .child(self.update_row(cx))
                    .child(
                        div().pt_3().flex().justify_end().child(
                            chip("about-close", "Close", true).on_click(cx.listener(|this, _, _, _| this.about = false)),
                        ),
                    ),
            )
    }

    /// Main menu, rebuilt from live mpv state each time it's drawn.
    fn build_menu(&self, window: &Window) -> Vec<Item> {
        let m = &self.mpv;
        let s = |p: &str| m.get_string(p).unwrap_or_default();
        let loop_file = s("loop-file") != "no";
        let loop_pl = s("loop-playlist") != "no";
        let ab_label = match (s("ab-loop-a") != "no", s("ab-loop-b") != "no") {
            (false, _) => "Set Loop Start (A)",
            (true, false) => "Set Loop End (B)",
            (true, true) => "Clear A-B Loop",
        };

        let chapters = m.get_i64("chapters").unwrap_or(0);
        let cur_ch = m.get_i64("chapter").unwrap_or(-1);
        let chapter_items: Vec<Item> = (0..chapters)
            .map(|i| {
                let t = m.get_string(&format!("chapter-list/{i}/title")).unwrap_or_else(|| format!("Chapter {}", i + 1));
                mcmd(t, &["set", "chapter", &i.to_string()]).checked(i == cur_ch)
            })
            .collect();

        let mut playback = vec![
            ui(if m.get_flag("pause") || m.get_flag("idle-active") { "Play" } else { "Pause" }, |p, _, _| p.play_pause()).hint("Space"),
            ui("Stop", |p, _, _| p.stop()),
            mcmd("Previous", &["playlist-prev"]).hint("B"),
            mcmd("Next", &["playlist-next"]).hint("N"),
            Item::Sep,
            sub("Loop", vec![
                ui("Off", |p, _, _| { p.mpv.cmd(&["set", "loop-file", "no"]); p.mpv.cmd(&["set", "loop-playlist", "no"]); }).checked(!loop_file && !loop_pl),
                ui("Current File", |p, _, _| p.mpv.cmd(&["set", "loop-file", "inf"])).checked(loop_file),
                ui("Playlist", |p, _, _| { p.mpv.cmd(&["set", "loop-file", "no"]); p.mpv.cmd(&["set", "loop-playlist", "inf"]); }).checked(loop_pl && !loop_file),
            ]),
            mcmd(ab_label, &["ab-loop"]),
            Item::Sep,
            mcmd("Frame Step", &["frame-step"]).hint("."),
            mcmd("Frame Back", &["frame-back-step"]).hint(","),
            mcmd("Jump Forward 10s", &["seek", "10"]).hint("L"),
            mcmd("Jump Back 10s", &["seek", "-10"]).hint("J"),
        ];
        if chapters > 0 {
            playback.insert(4, sub("Chapters", chapter_items));
        }

        vec![
            ui("Open File…", |p, _, cx| p.open_dialog(OpenKind::Media, cx)).hint("Ctrl+O"),
            ui("Open Folder…", |p, _, cx| p.open_dialog(OpenKind::Folder, cx)),
            ui("Open URL…", |p, _, _| p.open_url_box()).hint("Ctrl+L"),
            ui("Explorer", |p, _, cx| p.toggle_explorer(cx)).hint("E").checked(self.explorer.open),
            ui("Playlist", |p, _, _| p.toggle_panel(Panel::Playlist)).hint("P").checked(self.panel == Panel::Playlist),
            ui("Manage Playlists…", |p, _, _| p.open_playlists()).checked(self.pl.open),
            Item::Sep,
            sub("Playback", playback),
            // Per-video adjustments (speed, tracks, delays, picture, aspect, rotate, deinterlace,
            // loudness) live in Quick Settings; saved preferences in Settings.
            sub("Tools", vec![
                mcmd("Screenshot", &["screenshot"]),
                mcmd("Screenshot (no subtitles)", &["screenshot", "video"]),
                mcmd("Statistics Overlay", &["script-binding", "stats/display-stats-toggle"]),
                mcmd("Shuffle Playlist", &["playlist-shuffle"]),
                ui("Fullscreen", |_, w, _| w.toggle_fullscreen()).hint("F").checked(window.is_fullscreen()),
                ui("Add Subtitle File…", |p, _, cx| p.open_dialog(OpenKind::Subtitle, cx)),
            ]),
        ]
        .into_iter()
        // Visualizer style, shown while no video plays (music); saved as ui.visualizer.
        .chain(self.mpv.get_string("video-codec").is_none().then(|| {
            sub("Visualizer", visualizer::Style::ALL.iter().map(|&(label, style)| ui(label, move |p, _, _| p.set_visualizer(style)).checked(self.viz.style == style)).collect())
        }))
        .chain([
            Item::Sep,
            ui("Quick Settings", |p, _, _| p.toggle_pop()).hint("S").checked(self.pop.is_some()),
            ui("Settings", |p, _, cx| p.open_settings(settings::Tab::Appearance, cx)).hint("Ctrl+,"),
            Item::Sep,
            ui(format!("About {APP_NAME}"), |p, _, cx| {
                p.close_overlays();
                p.about = true;
                p.check_for_update(cx);
            }),
            ui("Quit", |_, _, cx| cx.quit()).hint("Q"),
        ])
        .collect()
    }
}

/// GPUI keystroke -> mpv key name ("ctrl+s" -> "Ctrl+s", "enter" -> "ENTER").
fn mpv_key_name(k: &gpui::Keystroke) -> String {
    let mut out = String::new();
    let m = &k.modifiers;
    for (on, name) in [(m.control, "Ctrl+"), (m.alt, "Alt+"), (m.shift && k.key.chars().count() > 1, "Shift+"), (m.platform, "Meta+")] {
        if on {
            out.push_str(name);
        }
    }
    let key = match k.key.as_str() {
        "enter" => "ENTER",
        "escape" => "ESC",
        "space" => "SPACE",
        "tab" => "TAB",
        "backspace" => "BS",
        "delete" => "DEL",
        "home" => "HOME",
        "end" => "END",
        "pageup" => "PGUP",
        "pagedown" => "PGDWN",
        k if k.starts_with('f') && k.len() > 1 && k[1..].parse::<u8>().is_ok() => &k.to_uppercase(),
        k if k.len() == 1 && m.shift => &k.to_uppercase(),
        k => k,
    };
    out + key
}

pub fn fmt_time(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn icon_button(id: &'static str, label: impl Into<SharedString>) -> Stateful<Div> {
    let label: SharedString = label.into();
    // Icon-only buttons are a fixed square so every icon centres on the same line;
    // an empty text child would still add a line box and nudge the icon off-centre.
    div()
        .id(id)
        .h(px(32.))
        .when(label.is_empty(), |d| d.w(px(36.)))
        .when(!label.is_empty(), |d| d.min_w(px(36.)).px_2().child(label))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_color(rgb(0xf0f0f0))
        .hover(|s| s.bg(rgba(0xffffff22)))
        .cursor_pointer()
}

const SUB_EXTS: &[&str] = &["srt", "ass", "ssa", "vtt", "sub", "sup"];

/// Right-side overlay that hosts the playlist.
fn side_panel(window: &Window) -> Stateful<Div> {
    div()
        .id("side-panel")
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()) // don't reach the video
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        // Floating card: inset from the edges, soft corners, shadow.
        .absolute()
        .top(px(PANEL_V))
        .right(px(PANEL_GAP))
        .bottom(px(PANEL_V)) // equal top/bottom = vertically centred, clear of title + seek bar
        .w(panel::panel_width(window, 0., 320., 320.))
        .rounded_xl()
        .overflow_hidden()
        .shadow_lg()
        .bg(t().surface())
        .border_1()
        .border_color(t().border())
}

/// Inset of the floating side panels (playlist, explorer) from the right edge.
pub const PANEL_GAP: f32 = 8.;
/// Top and bottom inset: equal so panels sit vertically centred above the seek bar (H-53).
pub const PANEL_V: f32 = 72.;

pub fn chip(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .text_xs()
        .bg(if active { t().accent_a(0x40) } else { t().hover() })
        .text_color(if active { t().accent() } else { t().fg() })
        .hover(|s| s.bg(t().pressed()))
        .child(label.into())
}

/// On/off switch (display only; the row around it handles clicks).
pub fn switch(on: bool) -> Div {
    div()
        .w(px(32.))
        .h(px(18.))
        .p(px(2.))
        .rounded_full()
        .bg(if on { t().accent() } else { t().dim() })
        .flex()
        .when(on, |d| d.justify_end())
        .child(div().size(px(14.)).rounded_full().bg(rgb(0xffffff)))
}

pub fn icon(name: &str) -> gpui::Svg {
    svg().path(format!("icons/{name}.svg")).size(px(18.)).text_color(rgb(0xf0f0f0))
}

impl Render for Player {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pos = self.mpv.get_f64("time-pos").unwrap_or(0.0);
        let dur = self.mpv.get_f64("duration").unwrap_or(0.0);
        let paused = self.mpv.get_flag("pause");
        let muted = self.mpv.get_flag("mute");
        let volume = self.mpv.get_f64("volume").unwrap_or(100.0);
        let progress = if dur > 0.0 { (pos / dur) as f32 } else { 0.0 };
        // Hide after HIDE_AFTER without mouse movement unless something needs the controls.
        let show_ui = paused
            || self.pop.is_some()
            || self.menu.is_some()
            || self.dragging
            || self.slider_drag.is_some()
            || self.editing.is_some()
            || self.last_move.elapsed() < HIDE_AFTER;
        // Fade: ~150 ms in, ~400 ms out (frame-time based, so it's smooth at any refresh rate).
        let dt = self.last_frame.elapsed().as_secs_f32().min(0.1);
        self.last_frame = Instant::now();
        self.ui_alpha = if show_ui { (self.ui_alpha + dt / 0.15).min(1.0) } else { (self.ui_alpha - dt / 0.4).max(0.0) };
        let ui_alpha = self.ui_alpha;
        let show_ui = ui_alpha > 0.0;

        // --- video: click = pause, double-click = fullscreen ---
        let video = div()
            .id("video")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, e: &ClickEvent, window, _| {
                // A click outside an open popover/panel just closes it (no pause toggle), like YouTube.
                if this.any_overlay() {
                    this.close_overlays();
                    return;
                }
                this.play_pause();
                if e.click_count() == 2 {
                    window.toggle_fullscreen(); // 2nd pause toggle above undoes the 1st click's
                }
            }))
            .child(match &self.frame {
                Some(f) => img(f.clone()).size_full().into_any_element(),
                None if self.audio_only() && self.viz.active => div()
                    .size_full()
                    .relative()
                    .child(self.viz.element(t().accent().into()))
                    .child(div().absolute().top(px(64.)).left_0().right_0().px_8().text_center().text_lg().text_color(rgb(0xb0b0b0)).child(self.title.clone()))
                    .into_any_element(),
                None if self.audio_only() => div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_4()
                    .px_8()
                    .text_color(rgb(0x9a9a9a))
                    .child(icon("music").size(px(72.)).text_color(rgb(0x707070)))
                    .child(div().text_lg().text_center().child(self.title.clone()))
                    .into_any_element(),
                None => div()
                    .text_color(rgb(0x808080))
                    .child(match (self.mpv.get_i64("playlist-count").unwrap_or(0), self.mpv.get_flag("idle-active")) {
                        (0, _) => "Drop video files here",
                        (_, true) => "Stopped — press ▶ to play",
                        _ => "Loading…",
                    })
                    .into_any_element(),
            });

        // --- seek bar with hover time + drag ---
        let hover_frac = self.seek_hover.map(|x| Self::seek_frac(x, window));
        let bar_h = if self.seek_hover.is_some() || self.dragging { 6. } else { 4. };
        let seek = div()
            .id("seek")
            .relative()
            .w_full()
            .h(px(16.)) // fat hit area, thin visual bar
            .flex()
            .items_center()
            .cursor_pointer()
            .on_hover(cx.listener(|this, h: &bool, _, _| {
                if !h {
                    this.seek_hover = None;
                }
            }))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, _| this.seek_hover = Some(e.position.x.into())))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, _| {
                    this.dragging = true;
                    this.seek_to(Self::seek_frac(e.position.x.into(), window));
                }),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(bar_h))
                    .rounded_full()
                    .bg(rgba(0xffffff33))
                    // hover preview fill
                    .children(hover_frac.map(|f| div().absolute().h_full().rounded_full().bg(rgba(0xffffff33)).w(relative(f))))
                    .child(div().absolute().h_full().rounded_full().bg(t().accent()).w(relative(progress))),
            )
            // knob
            .child(
                div()
                    .absolute()
                    .left(relative(progress))
                    .ml(px(-6.))
                    .size(px(12.))
                    .rounded_full()
                    .bg(rgb(0xffffff))
                    .when(self.seek_hover.is_none() && !self.dragging, |d| d.invisible()),
            )
            // hover time label
            .children(hover_frac.filter(|_| dur > 0.0).map(|f| {
                div()
                    .absolute()
                    .bottom(px(20.))
                    .left(relative(f))
                    .ml(px(-24.))
                    .w(px(48.))
                    .py_0p5()
                    .rounded_sm()
                    .bg(rgba(0x000000cc))
                    .text_xs()
                    .text_center()
                    .child(fmt_time(f as f64 * dur))
            }));

        // --- volume: 10 clickable segments, scroll to adjust ---
        let vol_level = if muted { 0 } else { (volume / 10.0).round() as usize };
        let volume_bar = div()
            .id("volume")
            .flex()
            .items_end()
            .gap(px(2.))
            .h(px(18.)) // same height as the icons so the meter sits on the icon line
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, _| {
                let dy = f32::from(e.delta.pixel_delta(px(20.)).y);
                this.mpv.cmd(&["add", "volume", if dy > 0.0 { "5" } else { "-5" }]);
            }))
            .children((1..=10).map(|i| {
                div()
                    .id(("vol", i))
                    .w(px(4.))
                    .h(px(4. + i as f32 * 1.2))
                    .rounded_sm()
                    .cursor_pointer()
                    .bg(if i <= vol_level { rgb(0xf0f0f0).into() } else { rgba(0xffffff40) })
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.mpv.cmd(&["set", "mute", "no"]);
                        this.mpv.cmd(&["set", "volume", &(i * 10).to_string()]);
                    }))
            }));

        let controls = div()
            .flex()
            .items_center()
            .gap_1()
            .child(icon_button("prev", "").child(icon("prev")).on_click(cx.listener(|this, _, _, _| this.mpv.cmd(&["playlist-prev"]))))
            .child(
                icon_button("play", "").child(icon(if paused { "play" } else { "pause" }))
                    .on_click(cx.listener(|this, _, _, _| this.play_pause())),
            )
            .child(
                // Stop = end playback completely (playlist cleared).
                icon_button("stop", "").child(icon("stop")).on_click(cx.listener(|this, _, _, _| this.stop())),
            )
            .child(icon_button("back", "").child(icon("rewind")).on_click(cx.listener(|this, _, _, _| this.mpv.cmd(&["seek", "-10"]))))
            .child(icon_button("fwd", "").child(icon("forward")).on_click(cx.listener(|this, _, _, _| this.mpv.cmd(&["seek", "10"]))))
            .child(icon_button("next", "").child(icon("next")).on_click(cx.listener(|this, _, _, _| this.mpv.cmd(&["playlist-next"]))))
            .child(
                div()
                    .ml_2()
                    .text_color(rgb(0xd0d0d0))
                    .child(format!("{} / {}", fmt_time(pos), fmt_time(dur))),
            )
            .child(div().flex_1())
            .child(
                icon_button("mute", "").child(icon(if muted { "volume-mute" } else { "volume" }))
                    .on_click(cx.listener(|this, _, _, _| this.mpv.cmd(&["cycle", "mute"]))),
            )
            .child(volume_bar)
            .child(div().w_2())
            .child(
                icon_button("explore", "")
                    .child(icon("explore"))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_explorer(cx))),
            )
            .child(
                icon_button("pl", "")
                    .child(icon("playlist"))
                    .when(self.panel == Panel::Playlist, |d| d.bg(rgba(0xffffff22)))
                    .on_click(cx.listener(|this, _, _, _| this.toggle_panel(Panel::Playlist))),
            )
            .child(
                icon_button("settings", "")
                    .child(icon("settings"))
                    .when(self.pop.is_some(), |d| d.bg(rgba(0xffffff22)))
                    .on_click(cx.listener(|this, _, _, _| this.toggle_pop())),
            )
            .child(
                icon_button("fs", "")
                    .child(icon(if window.is_fullscreen() { "fullscreen-exit" } else { "fullscreen" }))
                    .on_click(|_, window, _| window.toggle_fullscreen()),
            );

        let bottom = div()
            .id("bottom")
            .opacity(ui_alpha)
            .text_color(rgb(0xf0f0f0)) // always on video: fixed light-on-dark
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .px(px(PAD))
            .pt_8()
            .pb_2()
            .flex()
            .flex_col()
            .gap_1()
            .bg(linear_gradient(0., linear_color_stop(rgba(0x000000d0), 0.), linear_color_stop(rgba(0x00000000), 1.)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(seek)
            .child(controls);

        let top = div()
            .id("top")
            .opacity(ui_alpha)
            .text_color(rgb(0xf0f0f0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .px(px(PAD))
            .pt_3()
            .pb_8()
            .bg(linear_gradient(180., linear_color_stop(rgba(0x000000b0), 0.), linear_color_stop(rgba(0x00000000), 1.)))
            .flex()
            .items_start()
            .gap_2()
            // Windowed, the OS title bar already shows it; only fullscreen needs it here.
            .child(div().flex_1().min_w_0().pt_1().overflow_hidden().text_ellipsis().whitespace_nowrap().when(window.is_fullscreen(), |d| d.child(self.title.clone())))
            .children(self.info_button(cx));

        let count = self.mpv.get_i64("playlist-count").unwrap_or(0) as usize;
        let shown = if self.panel == Panel::Playlist { self.playlist() } else { vec![] }; // only built when visible
        let playlist = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .px_3()
                    .py_1()
                    .gap_1()
                    .child(div().flex_1().text_color(t().muted()).child("NOW PLAYING"))
                    .when(count > 0, |d| {
                        d.child(chip("pl-clear", "Clear", false).on_click(cx.listener(|this, _, _, _| this.stop())))
                    })
                    // Saving, saved playlists and editing them: the large Playlists manager.
                    .child(chip("pl-manage", "Manage…", false).on_click(cx.listener(|this, _, _, _| this.open_playlists())))
                    .child(
                        icon_button("pl-add", "+")
                            .text_color(t().fg())
                            .text_lg()
                            .when(self.add_popup, |d| d.bg(t().pressed()))
                            .on_click(cx.listener(|this, _, _, _| {
                                this.add_popup = !this.add_popup;
                                this.editing = this.add_popup.then_some(Field::AddUrl);
                            })),
                    ),
            )
            .child(
                div().id("pl-items").flex_1().overflow_y_scroll().children(shown.into_iter().enumerate().map(
                    |(i, (name, current))| {
                        div()
                            .id(("pl-item", i))
                            .group("plrow")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_1p5()
                            .cursor_pointer()
                            .hover(|s| s.bg(t().hover()))
                            .when(current, |d| d.bg(t().accent_a(0x30)).text_color(t().accent()))
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.mpv.cmd(&["playlist-play-index", &i.to_string()])
                            }))
                            .child(div().w(px(20.)).text_color(t().dim()).child(format!("{}", i + 1)))
                            .child(div().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
                            .children(playlists::move_buttons("pl", i, count, cx, Player::queue_move))
                            .child(
                                div()
                                    .id(("pl-rm", i))
                                    .px_1()
                                    .rounded_sm()
                                    .text_color(t().muted())
                                    .invisible()
                                    .group_hover("plrow", |s| s.visible())
                                    .hover(|s| s.bg(t().pressed()))
                                    .child("✕")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.mpv.cmd(&["playlist-remove", &i.to_string()]);
                                        cx.stop_propagation(); // don't also play the row
                                    })),
                            )
                    },
                )),
            ).child(div().px_3().py_2().text_xs().text_color(t().dim()).child("+ to add a URL or files · or drop files, folders, .m3u"));

        // "+" dialog, centered over the window: paste a URL, or pick files/folder. Backdrop click closes.
        let add_dialog = div()
            .id("add-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .bg(rgba(0x00000099))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, _| {
                this.add_popup = false;
                this.editing = None;
            }));
        let add_popup = div()
            .id("pl-popup")
            .w(px(480.))
            .max_w(relative(0.9))
            .p_4()
            .rounded_xl()
            .bg(t().surface_solid())
            .border_1()
            .border_color(t().border())
            .shadow_lg()
            .text_color(t().fg())
            .text_sm()
            .flex()
            .flex_col()
            .gap_2()
            .on_click(|_, _, cx| cx.stop_propagation()) // clicks inside don't close
            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("Add to playlist"))
            .child(div().text_xs().text_color(t().muted()).child("Stream or file URL"))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(self.text_box(Field::AddUrl, &self.add_url, "https://…  (Ctrl+V)", false, cx)))
                    .child(panel::button("add-url", "Add", !self.add_url.trim().is_empty()).on_click(cx.listener(|this, _, _, cx| this.submit_field(Field::AddUrl, cx)))),
            )
            .child(div().h(px(1.)).bg(t().border()))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(chip("add-files", "Choose files…", false).on_click(cx.listener(|this, _, _, cx| {
                        this.add_popup = false;
                        this.editing = None;
                        this.open_dialog(OpenKind::Media, cx);
                    })))
                    .child(chip("add-folder", "Choose folder…", false).on_click(cx.listener(|this, _, _, cx| {
                        this.add_popup = false;
                        this.editing = None;
                        this.open_dialog(OpenKind::Folder, cx);
                    }))),
            );
        let add_dialog = add_dialog.child(add_popup);

        div()
            .id("root")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .bg(rgb(0x000000))
            .text_color(t().fg())
            .text_sm()
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, _| {
                this.last_move = Instant::now();
                if this.dragging {
                    this.seek_to(Self::seek_frac(e.position.x.into(), window));
                }
                if let Some(sl) = this.slider_drag {
                    this.set_slider(&sl, e.position.x.into(), window);
                }
                if let Some(id) = this.panel_drag {
                    this.drag_panel_edge(id, e.position.x.into(), window);
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| {
                this.dragging = false;
                this.slider_drag = None;
                this.text_drag = false;
                this.end_panel_drag();
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, e: &MouseDownEvent, _, _| this.open_menu(e.position)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| {
                this.dragging = false;
                this.slider_drag = None;
                this.text_drag = false;
                this.end_panel_drag();
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                for p in paths.paths() {
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                    let path = p.to_string_lossy();
                    if ext == "zym" {
                        this.open_plugin_package(p.clone(), cx); // a content plugin: offer to install it
                    } else if SUB_EXTS.contains(&ext.as_str()) {
                        this.mpv.cmd(&["sub-add", &path, "select"]);
                    } else {
                        this.mpv.load(&path, "append-play");
                    }
                }
            }))
            .drag_over::<ExternalPaths>(|s, _, _, _| s.border_2().border_color(t().accent()))
            .child(video)
            .when(self.panel == Panel::Playlist, |d| d.child(side_panel(window).child(playlist)))
            .when(show_ui, |d| d.child(top).child(bottom))
            .children(self.pop.map(|page| self.quick_settings(page, window, cx)))
            .when(self.explorer.open, |d| d.child(self.render_explorer(window, cx)))
            .children(self.settings.as_ref().map(|s| self.render_settings(s, window, cx)))
            .when(self.pl.open, |d| d.child(self.render_playlists(window, cx)))
            // Fades with the controls; stays open (shown again on mouse move) until closed.
            .when(self.info_open && show_ui, |d| d.children(self.render_info_card(ui_alpha, window, cx)))
            .when(self.add_popup, |d| d.child(add_dialog))
            .children(self.menu.as_ref().map(|state| {
                let items = match &state.field {
                    Some((f, secret)) => self.field_menu(f, *secret),
                    None => self.build_menu(window),
                };
                self.render_menu(state, &items, window, cx)
            }))
            .when(self.about, |d| d.child(self.about_dialog(cx)))
            .children(self.install_dialog(cx))
    }
}

/// Icons baked into the binary; GPUI's svg() loads through an AssetSource.
struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        macro_rules! icons {
            ($($name:literal),*) => {
                match path {
                    $(concat!("icons/", $name, ".svg") => Some(Cow::Borrowed(&include_bytes!(concat!("../assets/icons/", $name, ".svg"))[..])),)*
                    _ => None,
                }
            };
        }
        Ok(icons!("fullscreen", "fullscreen-exit", "prev", "next", "playlist", "settings", "explore", "gear", "stop", "volume", "volume-mute", "play", "pause", "rewind", "forward", "edit", "trash"))
    }

    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    log::init();
    theme::load();
    match files.first().map(String::as_str) {
        Some("-V" | "--version") => return println!("{APP_NAME} {VERSION}"),
        Some("-h" | "--help") => {
            return println!("{APP_NAME} {VERSION}\n\nUsage: zym-media-player [FILE | FOLDER | PLAYLIST.m3u]...\n\n  -h, --help     show this help\n  -V, --version  show version\n\nPlugins: {}", plugins_dir().display());
        }
        _ => {}
    }
    Application::new().with_assets(Assets).with_http_client(Arc::new(sources::ImageFetcher)).run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.), px(650.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("zym-media-player".into()), // matches zym-media-player.desktop -> dock/taskbar icon on Linux
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Player::new(files, window, cx)),
        )
        .unwrap();
        // Closing the window ends the app. Quit on the next loop turn: quitting inside the X11 close
        // handler re-borrows GPUI's X11 state ("RefCell already borrowed" panic).
        cx.on_window_closed(|cx| {
            cx.spawn(async move |cx| {
                cx.update(|cx| cx.quit()).ok();
            })
            .detach();
        })
        .detach();
        if std::env::var_os("ZYM_DEBUG_AUTOPLAY").is_none() {
            cx.activate(true);
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

#[cfg(test)]
mod tests {
    #[test]
    fn decoder_note_explains_fallback() {
        use super::decoder_note as n;
        assert_eq!(n(true, true, None, None), None, "nothing playing");
        assert_eq!(n(true, false, Some("nvdec-copy"), Some("h264")).unwrap(), "Decoding on the GPU (nvdec)");
        assert_eq!(n(false, false, Some("no"), Some("h264")).unwrap(), "Decoding on the CPU");
        assert_eq!(n(true, false, Some("no"), Some("av1 (Alliance for Open Media AV1)")).unwrap(), "Decoding on the CPU: your GPU can't decode AV1 video");
    }

    #[test]
    fn aspect_and_picture() {
        use super::{aspect_ratio as r, picture_args};
        assert!((r("16:9").unwrap() - r("1.777778").unwrap()).abs() < 0.01, "mpv reports 16:9 back as a number");
        assert!((r("2.35:1").unwrap() - 2.35).abs() < 1e-9);
        assert_eq!(r("no"), None);
        assert_eq!(r("-2.000000"), None, "default as mpv reports it");
        assert_eq!(super::eq_filter("Flat"), None);
        assert_eq!(super::eq_filter("Bass boost").unwrap(), "@zymeq:lavfi=[equalizer=f=70:width_type=o:width=1:g=8,equalizer=f=180:width_type=o:width=1:g=6,equalizer=f=320:width_type=o:width=1:g=4,equalizer=f=600:width_type=o:width=1:g=2,alimiter=limit=0.97:level=disabled]");
        let a = picture_args([0, 0, 0, 0, 0]);
        assert_eq!([a[0].1.as_str(), a[1].1.as_str(), a[3].1.as_str(), a[4].1.as_str()], ["0.000", "1.000", "1.000", "0.0"], "0 = unchanged picture");
        let a = picture_args([100, -100, 100, 50, -50]);
        assert_eq!([a[0].1.as_str(), a[1].1.as_str(), a[2].1.as_str(), a[3].1.as_str(), a[4].1.as_str()], ["0.500", "0.000", "2.000", "2.000", "-90.0"]);
    }
}
