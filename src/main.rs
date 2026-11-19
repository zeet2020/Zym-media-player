// No console window next to the player on Windows (logs still go to stderr when run from a terminal).
#![cfg_attr(windows, windows_subsystem = "windows")]

// Zym Player: GPUI window + libmpv software renderer.
// mpv decodes and renders BGRA frames on a worker thread; GPUI shows the latest one as an image.

mod explorer;
mod menu;
mod remote;
mod sources;
mod theme;

use theme::t;

pub const APP_NAME: &str = "Zym Player";
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
use explorer::{Explorer, Field};
use menu::{Item, MenuState, choice, mpv as mcmd, sub, ui};
use libmpv2_sys::*;

// ---- debug watchdog (ZYM_DEBUG): which sync mpv call, if any, the UI thread is stuck in.
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);
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
                eprintln!("WATCHDOG: UI tick stalled; in-flight mpv call: {:?}; main wchan: {wchan}", IN_FLIGHT.lock().unwrap());
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
}
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

impl Mpv {
    fn new() -> Arc<Self> {
        unsafe {
            let h = mpv_create();
            assert!(!h.is_null(), "mpv_create failed");
            let mpv = Arc::new(Mpv(h, Mutex::default()));
            mpv.opt("vo", "libmpv");
            // Decode on GPU, copy back for the sw renderer. "-safe" = only mpv's whitelisted decoders:
            // plain auto-copy picked Vulkan here and hit VK_ERROR_DEVICE_LOST before a crash.
            mpv.opt("hwdec", "auto-copy-safe");
            mpv.opt("keep-open", "yes"); // advance through playlist, hold last frame at the end
            mpv.opt("idle", "yes"); // stay alive with an empty playlist (drop files later)
            mpv.opt("screenshot-dir", "~/Pictures"); // mpv expands ~ on every OS
            assert!(mpv_initialize(h) >= 0, "mpv_initialize failed");
            mpv
        }
    }

    /// Plugins = mpv Lua/JS scripts (files or dirs with main.lua) in the plugins dir.
    fn load_plugins(&self) {
        let dir = plugins_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            let is_script = p.is_dir() || matches!(p.extension().and_then(|x| x.to_str()), Some("lua" | "js"));
            if is_script {
                eprintln!("plugin: {}", p.display());
                self.cmd(&["load-script", &p.to_string_lossy()]);
            }
        }
    }

    fn get_i64(&self, name: &str) -> Option<i64> {
        let _g = InFlight::new(name);
        let n = CString::new(name).unwrap();
        let mut out = 0i64;
        let r = unsafe {
            mpv_get_property(self.0, n.as_ptr(), mpv_format_MPV_FORMAT_INT64, &mut out as *mut _ as *mut c_void)
        };
        (r >= 0).then_some(out)
    }

    fn get_string(&self, name: &str) -> Option<String> {
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

    pub fn cmd(&self, args: &[&str]) {
        if std::env::var_os("ZYM_DEBUG").is_some() {
            eprintln!("cmd {}", redact(&format!("{args:?}")));
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
        *self.1.lock().unwrap() = AfterLoad { subs: pb.subtitles.iter().map(|s| s.url.clone()).collect(), start: pb.start };
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
                _ if v.contains(',') => eprintln!("append: skipped header {k} (contains ',')"),
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
            eprintln!("cmd [\"loadfile\", {}, \"append-play\", -1, {opts:?}]", redact(&pb.url));
        }
        unsafe { mpv_command_node_async(self.0, 0, &mut root) }; // mpv copies args before returning
    }

    fn get_f64(&self, name: &str) -> Option<f64> {
        let _g = InFlight::new(name);
        let n = CString::new(name).unwrap();
        let mut out = 0f64;
        let r = unsafe {
            mpv_get_property(self.0, n.as_ptr(), mpv_format_MPV_FORMAT_DOUBLE, &mut out as *mut _ as *mut c_void)
        };
        (r >= 0).then_some(out)
    }

    fn get_flag(&self, name: &str) -> bool {
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

/// <config>/zym-player/plugins — %APPDATA% on Windows, $XDG_CONFIG_HOME or ~/.config elsewhere.
pub fn plugins_dir() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(Into::into)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("zym-player").join("plugins")
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
/// and prints mpv warnings/errors to stderr.
#[allow(non_upper_case_globals)] // bindgen constants used as match patterns
fn spawn_event_thread(mpv: Arc<Mpv>, size: VideoSize, wake_render: &'static Wake) {
    unsafe {
        let level = if std::env::var_os("ZYM_DEBUG").is_some() { c"v" } else { c"warn" };
        mpv_request_log_messages(mpv.0, level.as_ptr());
        mpv_observe_property(mpv.0, 0, c"dwidth".as_ptr(), mpv_format_MPV_FORMAT_INT64);
        mpv_observe_property(mpv.0, 0, c"dheight".as_ptr(), mpv_format_MPV_FORMAT_INT64);
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
                    eprint!("mpv [{}] {}: {}", s(m.prefix), s(m.level), s(m.text));
                }
                mpv_event_id_MPV_EVENT_FILE_LOADED => {
                    let after = std::mem::take(&mut *mpv.1.lock().unwrap());
                    for url in after.subs {
                        mpv.cmd(&["sub-add", &url, "auto"]);
                    }
                    if let Some(t) = after.start.filter(|t| *t > 0.0) {
                        mpv.cmd(&["seek", &t.to_string(), "absolute"]);
                    }
                }
                mpv_event_id_MPV_EVENT_END_FILE => {
                    let e = &*(ev.data as *const mpv_event_end_file);
                    if e.reason == mpv_end_file_reason_MPV_END_FILE_REASON_ERROR {
                        eprintln!("mpv: file failed: {}", mpv_error_str(e.error));
                    }
                }
                mpv_event_id_MPV_EVENT_PROPERTY_CHANGE => {
                    let p = &*(ev.data as *const mpv_event_property);
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
                eprintln!("render failed: {}", mpv_error_str(r));
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
    pop: Option<Pop>,
    slider_drag: Option<Slider>,
    /// Controls opacity 0..1, eased toward shown/hidden each frame.
    ui_alpha: f32,
    last_frame: Instant,
    pub explorer: Explorer,
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
    Appearance,
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
        cx.spawn_in(window, async move |this, cx| loop {
            Timer::after(Duration::from_millis(16)).await;
            if let Err(e) = this.update_in(cx, |p, window, cx| p.tick(window, cx)) {
                // Only a dropped player ends the loop; anything else is logged and retried.
                if this.upgrade().is_none() {
                    break;
                }
                eprintln!("tick: {e:#}");
            }
        })
        .detach();

        let focus = cx.focus_handle();
        window.focus(&focus);
        let mut player = Self {
            mpv,
            latest,
            frame: None,
            title: SharedString::default(),
            focus,
            last_move: Instant::now(),
            seek_hover: None,
            dragging: false,
            panel: if files.len() > 1 { Panel::Playlist } else { Panel::None },
            menu: None,
            about: false,
            pop: None,
            slider_drag: None,
            ui_alpha: 1.0,
            last_frame: Instant::now(),
            explorer: Explorer::new(),
            editing: None,
            add_url: String::new(),
            add_popup: false,
            stopped_at: None,
        };
        if files.is_empty() {
            player.toggle_explorer(cx); // no files: start on the Explorer (home screen)
        }
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
        if self.mpv.get_flag("idle-active") {
            if let Some(old) = self.frame.take() {
                window.drop_image(old).ok(); // stopped: don't leave the last frame on screen
            }
        }
        let title: SharedString = if self.mpv.get_flag("idle-active") {
            APP_NAME.into() // stopped/empty: never show a stale title
        } else {
            self.mpv.get_string("media-title").unwrap_or_else(|| APP_NAME.into()).into()
        };
        if title != self.title {
            window.set_window_title(&if title.as_ref() == APP_NAME { title.to_string() } else { format!("{title} — {APP_NAME}") });
            self.title = title;
        }
        cx.notify(); // also refreshes time readout + auto-hide
    }

    /// (filename, is_current) for each playlist entry.
    // ponytail: re-reads the whole playlist every frame; cache on `playlist` change events if lists get huge.
    fn playlist(&self) -> Vec<(String, bool)> {
        let n = self.mpv.get_i64("playlist-count").unwrap_or(0);
        let cur = self.mpv.get_i64("playlist-pos").unwrap_or(-1);
        (0..n)
            .map(|i| {
                let title = self.mpv.get_string(&format!("playlist/{i}/title")).filter(|t| !t.is_empty());
                let name = title.unwrap_or_else(|| {
                    let f = self.mpv.get_string(&format!("playlist/{i}/filename")).unwrap_or_default();
                    std::path::Path::new(&f).file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or(f)
                });
                (name, i == cur)
            })
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
                }
                "enter" => self.submit_field(f, cx),
                "backspace" => {
                    self.field_text(&f).map(String::pop);
                }
                "v" if !plain => {
                    let paste = cx.read_from_clipboard().and_then(|c| c.text()).unwrap_or_default();
                    if let Some(t) = self.field_text(&f) {
                        t.push_str(paste.lines().next().unwrap_or("").trim());
                    }
                }
                _ => {
                    if let (true, Some(c)) = (plain, ks.key_char.clone()) {
                        if let Some(t) = self.field_text(&f) {
                            t.push_str(&c);
                        }
                    }
                }
            }
            cx.notify();
            return;
        }
        if ks.key == "escape" && self.explorer.open && self.menu.is_none() && !self.about {
            return self.toggle_explorer(cx);
        }
        let m = &self.mpv;
        if ks.key == "escape" && (self.menu.is_some() || self.about || self.pop.is_some()) {
            self.menu = None;
            self.about = false;
            self.pop = None;
            return;
        }
        if ks.modifiers.control && ks.key == "o" {
            return self.open_dialog(OpenKind::Media, cx);
        }
        if ks.modifiers.control && ks.key == "l" {
            return self.open_url_box();
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
    fn toggle_panel(&mut self, p: Panel) {
        self.panel = if self.panel == p { Panel::None } else { p };
        if self.panel != Panel::None {
            self.explorer.open = false; // one right-side panel at a time
        }
    }

    /// Show the playlist with the "add URL" box focused.
    fn stop(&mut self) {
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
        self.explorer.open = false;
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
        self.pop = if self.pop.is_some() { None } else { Some(Pop::Main) };
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
        Self::pop_row(id)
            .child(div().w(px(16.)).text_color(t().accent()).child(if selected { "✓" } else { "" }))
            .child(div().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label.into()))
            .on_click(cx.listener(move |this, _, _, _| this.mpv.cmd(&cmd.iter().map(String::as_str).collect::<Vec<_>>())))
    }

    /// Label + switch.
    fn toggle_row(&self, label: &str, on: bool, cmd: Vec<String>, cx: &mut Context<Self>) -> Stateful<Div> {
        Self::pop_row(format!("tg-{label}"))
            .child(div().flex_1().child(label.to_string()))
            .child(
                div()
                    .w(px(32.))
                    .h(px(18.))
                    .p(px(2.))
                    .rounded_full()
                    .bg(if on { t().accent() } else { t().dim() })
                    .flex()
                    .when(on, |d| d.justify_end())
                    .child(div().size(px(14.)).rounded_full().bg(rgb(0xffffff))),
            )
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
        let aspect_label = ASPECTS.iter().find(|a| a.1 == aspect || aspect.starts_with(&a.1[..2.min(a.1.len())])).map(|a| a.0).unwrap_or("Custom");

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
                    .child({
                        let p = theme::prefs();
                        let mode = match p.mode { theme::Mode::System => "System", theme::Mode::Dark => "Dark", theme::Mode::Light => "Light" };
                        let accent = theme::ACCENTS.iter().find(|a| a.1 == p.accent).map_or("Custom", |a| a.0);
                        self.nav_row("Appearance", format!("{mode} · {accent}"), Pop::Appearance, cx)
                    })
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
            }
            Pop::Video => {
                let rotate = m.get_i64("video-rotate").unwrap_or(0);
                let hw_on = m.get_string("hwdec").unwrap_or_default() != "no";
                let hw = m.get_string("hwdec-current").filter(|h| h != "no").unwrap_or_else(|| "software".into());
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
                                    .on_click(cx.listener(move |this, _, _, _| this.mpv.cmd(&["set", "video-rotate", &r.to_string()])))
                            })),
                    )
                    .child(self.toggle_row("Deinterlace", m.get_flag("deinterlace"), v(&["cycle", "deinterlace"]), cx))
                    .child(self.toggle_row("Hardware decoding", hw_on, v(&["set", "hwdec", if hw_on { "no" } else { "auto-copy-safe" }]), cx))
                    .child(Self::pop_note(&format!("Decoder: {hw}")))
            }
            Pop::Picture => body
                .child(self.pop_header("Picture", cx))
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
                ),
            Pop::Sync => body
                .child(self.pop_header("Sync", cx))
                .child(self.slider_row(AUDIO_DELAY, cx))
                .child(self.slider_row(SUB_DELAY, cx))
                .child(Self::pop_note("Positive = later. Click a value to reset.")),
            Pop::Appearance => {
                let p = theme::prefs();
                body.child(self.pop_header("Appearance", cx))
                    .child(Self::pop_note("THEME"))
                    .children([("System", theme::Mode::System), ("Dark", theme::Mode::Dark), ("Light", theme::Mode::Light)].map(|(label, mode)| {
                        Self::pop_row(format!("mode-{label}"))
                            .child(div().w(px(16.)).text_color(t().accent()).child(if p.mode == mode { "✓" } else { "" }))
                            .child(div().flex_1().child(label))
                            .on_click(cx.listener(move |_, _, window, cx| {
                                theme::set(theme::Prefs { mode, ..theme::prefs() });
                                theme::resolve(window.appearance());
                                cx.notify();
                            }))
                    }))
                    .child(Self::pop_note("ACCENT"))
                    .child(
                        div().flex().flex_wrap().gap_2().px(px(POP_INSET)).py_2().children(theme::ACCENTS.map(|(name, color)| {
                            let selected = p.accent == color;
                            div()
                                .id(SharedString::from(format!("accent-{name}")))
                                .size(px(26.))
                                .rounded_full()
                                .bg(rgb(color))
                                .cursor_pointer()
                                .border_2()
                                .border_color(if selected { t().fg() } else { gpui::transparent_black().into() })
                                .hover(|s| s.opacity(0.85))
                                .on_click(cx.listener(move |_, _, window, cx| {
                                    theme::set(theme::Prefs { accent: color, ..theme::prefs() });
                                    theme::resolve(window.appearance());
                                    cx.notify();
                                }))
                        })),
                    )
            }
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
                    .child(line("Plugins", plugins_dir().display().to_string()))
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
        let track_items = |kind: &str, prop: &str, with_off: bool| {
            let tracks = self.tracks(kind);
            let mut items: Vec<Item> = Vec::new();
            if with_off {
                items.push(mcmd("Off", &["set", prop, "no"]).checked(!tracks.iter().any(|t| t.2)));
            }
            if tracks.is_empty() && !with_off {
                items.push(ui("No tracks", |_, _, _| {}));
            }
            items.extend(tracks.into_iter().map(|(id, label, sel)| mcmd(label, &["set", prop, &id.to_string()]).checked(sel)));
            items
        };

        let speed = m.get_f64("speed").unwrap_or(1.0);
        let speeds: Vec<Item> = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0]
            .map(|v| mcmd(if v == 1.0 { "Normal".to_string() } else { format!("{v}×") }, &["set", "speed", &v.to_string()]).checked((speed - v).abs() < 0.001))
            .into();

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

        let aspect = s("video-aspect-override");
        let aspect = if aspect.starts_with('-') || aspect.is_empty() { "no".to_string() } else { aspect };
        let rotate = m.get_i64("video-rotate").unwrap_or(0).to_string();
        let hwdec_on = s("hwdec") != "no";

        let mut playback = vec![
            ui(if m.get_flag("pause") || m.get_flag("idle-active") { "Play" } else { "Pause" }, |p, _, _| p.play_pause()).hint("Space"),
            ui("Stop", |p, _, _| p.stop()),
            mcmd("Previous", &["playlist-prev"]).hint("B"),
            mcmd("Next", &["playlist-next"]).hint("N"),
            Item::Sep,
            sub("Speed", speeds),
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
            Item::Sep,
            sub("Playback", playback),
            sub("Audio", vec![
                sub("Audio Track", track_items("audio", "aid", false)),
                Item::Sep,
                mcmd("Mute", &["cycle", "mute"]).hint("M").checked(m.get_flag("mute")),
                mcmd("Volume Up", &["add", "volume", "5"]).hint("↑"),
                mcmd("Volume Down", &["add", "volume", "-5"]).hint("↓"),
                mcmd("Normalize Loudness", &["af", "toggle", "@norm:lavfi=[loudnorm]"]).checked(s("af").contains("loudnorm")),
                Item::Sep,
                ui("Audio Delay…", |p, _, _| p.pop = Some(Pop::Sync)),
            ]),
            sub("Video", vec![
                choice("Aspect Ratio", "video-aspect-override", &aspect, &[("Default", "no"), ("16:9", "16:9"), ("4:3", "4:3"), ("21:9", "2.35:1"), ("1:1", "1:1")]),
                choice("Rotate", "video-rotate", &rotate, &[("0°", "0"), ("90°", "90"), ("180°", "180"), ("270°", "270")]),
                mcmd("Deinterlace", &["cycle", "deinterlace"]).checked(m.get_flag("deinterlace")),
                ui("Hardware Decoding", move |p, _, _| p.mpv.cmd(&["set", "hwdec", if hwdec_on { "no" } else { "auto-copy-safe" }])).checked(hwdec_on),
                Item::Sep,
                ui("Fullscreen", |_, w, _| w.toggle_fullscreen()).hint("F").checked(window.is_fullscreen()),
                ui("Picture & Zoom…", |p, _, _| p.pop = Some(Pop::Picture)),
            ]),
            sub("Subtitles", vec![
                sub("Subtitle Track", track_items("sub", "sid", true)),
                ui("Add Subtitle File…", |p, _, cx| p.open_dialog(OpenKind::Subtitle, cx)),
                Item::Sep,
                ui("Size & Position…", |p, _, _| p.pop = Some(Pop::Subtitles)),
                ui("Subtitle Delay…", |p, _, _| p.pop = Some(Pop::Sync)),
            ]),
            sub("Tools", vec![
                mcmd("Screenshot", &["screenshot"]),
                mcmd("Screenshot (no subtitles)", &["screenshot", "video"]),
                mcmd("Statistics Overlay", &["script-binding", "stats/display-stats-toggle"]),
                mcmd("Shuffle Playlist", &["playlist-shuffle"]),
            ]),
            Item::Sep,
            ui("Playlist", |p, _, _| p.toggle_panel(Panel::Playlist)).hint("P").checked(self.panel == Panel::Playlist),
            ui("Quick Settings", |p, _, _| p.toggle_pop()).hint("S").checked(self.pop.is_some()),
            ui("Appearance…", |p, _, _| p.pop = Some(Pop::Appearance)),
            Item::Sep,
            ui(format!("About {APP_NAME}"), |p, _, _| p.about = true),
            ui("Quit", |_, _, cx| cx.quit()).hint("Q"),
        ]
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

fn fmt_time(s: f64) -> String {
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
fn side_panel() -> Stateful<Div> {
    div()
        .id("side-panel")
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()) // don't reach the video
        // Floating card: inset from the edges, soft corners, shadow.
        .absolute()
        .top(px(PANEL_V))
        .right(px(PANEL_GAP))
        .bottom(px(PANEL_V)) // equal top/bottom = vertically centred, clear of title + seek bar
        .w(px(320.))
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
                let had_overlay = this.pop.take().is_some() | (this.panel != Panel::None) | this.explorer.open;
                if had_overlay {
                    this.panel = Panel::None;
                    this.explorer.open = false;
                    this.add_popup = false;
                    this.editing = None;
                    return;
                }
                this.play_pause();
                if e.click_count() == 2 {
                    window.toggle_fullscreen(); // 2nd pause toggle above undoes the 1st click's
                }
            }))
            .child(match &self.frame {
                Some(f) => img(f.clone()).size_full().into_any_element(),
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
            .child(self.title.clone());

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
                    .child(div().flex_1().text_color(t().muted()).child("PLAYLIST"))
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
                div().id("pl-items").flex_1().overflow_y_scroll().children(self.playlist().into_iter().enumerate().map(
                    |(i, (name, current))| {
                        div()
                            .id(("pl-item", i))
                            .group("item")
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
                            .child(
                                div()
                                    .id(("pl-rm", i))
                                    .px_1()
                                    .rounded_sm()
                                    .text_color(t().muted())
                                    .invisible()
                                    .group_hover("item", |s| s.visible())
                                    .hover(|s| s.bg(t().pressed()))
                                    .child("✕")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.mpv.cmd(&["playlist-remove", &i.to_string()]);
                                        cx.stop_propagation(); // don't also play the row
                                    })),
                            )
                    },
                )),
            )
            .child(div().px_3().py_2().text_xs().text_color(t().dim()).child("+ to add a URL or files · or drop files, folders, .m3u"));

        // "+" popup: paste a URL, or pick files/folder.
        let add_popup = div()
            .id("pl-popup")
            .absolute()
            .top(px(40.))
            .left_2()
            .right_2()
            .p_3()
            .rounded_lg()
            .bg(t().surface_solid())
            .border_1()
            .border_color(t().border())
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_2()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_xs().text_color(t().muted()).child("Stream or file URL"))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(div().flex_1().child(self.text_box(Field::AddUrl, &self.add_url, "https://…  (Ctrl+V)", false, cx)))
                    .child(chip("add-url", "Add", !self.add_url.trim().is_empty()).on_click(cx.listener(|this, _, _, cx| this.submit_field(Field::AddUrl, cx)))),
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
        let playlist = playlist.when(self.add_popup, |d| d.child(add_popup));


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
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| {
                this.dragging = false;
                this.slider_drag = None;
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, e: &MouseDownEvent, _, _| this.open_menu(e.position)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| {
                this.dragging = false;
                this.slider_drag = None;
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, _| {
                for p in paths.paths() {
                    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                    let path = p.to_string_lossy();
                    if SUB_EXTS.contains(&ext.as_str()) {
                        this.mpv.cmd(&["sub-add", &path, "select"]);
                    } else {
                        this.mpv.load(&path, "append-play");
                    }
                }
            }))
            .drag_over::<ExternalPaths>(|s, _, _, _| s.border_2().border_color(t().accent()))
            .child(video)
            .when(self.panel == Panel::Playlist, |d| d.child(side_panel().child(playlist)))
            .when(show_ui, |d| d.child(top).child(bottom))
            .children(self.pop.map(|page| self.quick_settings(page, window, cx)))
            .when(self.explorer.open, |d| d.child(self.render_explorer(window, cx)))
            .children(self.menu.as_ref().map(|state| self.render_menu(state, &self.build_menu(window), window, cx)))
            .when(self.about, |d| d.child(self.about_dialog(cx)))
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
    theme::load();
    match files.first().map(String::as_str) {
        Some("-V" | "--version") => return println!("{APP_NAME} {VERSION}"),
        Some("-h" | "--help") => {
            return println!("{APP_NAME} {VERSION}\n\nUsage: zym-player [FILE | FOLDER | PLAYLIST.m3u]...\n\n  -h, --help     show this help\n  -V, --version  show version\n\nPlugins: {}", plugins_dir().display());
        }
        _ => {}
    }
    Application::new().with_assets(Assets).with_http_client(Arc::new(sources::ImageFetcher)).run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.), px(650.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("zym-player".into()), // matches zym-player.desktop -> dock/taskbar icon on Linux
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
