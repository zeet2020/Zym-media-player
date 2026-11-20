//! Settings panel (Appearance · Plugins · Player) and the settings.json store every pref shares:
//! <config>/zym-player/settings.json, one top-level key per area ("appearance", "player", "disabled").

use std::path::PathBuf;

use gpui::{Context, Div, SharedString, Stateful, Window, div, prelude::*, px, rgb};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::panel::{button, panel_header, sidebar, sidebar_close, sidebar_item};
use crate::textbox::Field;
use crate::theme::{self, t};
use crate::{Player, chip, sources, switch};

// ------------------------------------------------------------------ store

fn path() -> PathBuf {
    crate::config_dir().join("settings.json")
}

fn root() -> Value {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

pub fn get(key: &str) -> Option<Value> {
    root().get(key).cloned()
}

/// Replace one top-level key; the others survive.
pub fn put(key: &str, v: Value) {
    let mut r = root();
    r[key] = v;
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&r).unwrap()) {
        crate::log::error("settings", &format!("couldn't save {}: {e}", path.display()));
    }
}

// ------------------------------------------------------------------ player prefs

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct PlayerPrefs {
    #[serde(default = "yes")]
    pub hwdec: bool,
    /// Play web pages (YouTube, …) through mpv's ytdl_hook + yt-dlp.
    #[serde(default = "yes")]
    pub ytdl: bool,
    /// Key of YTDL_QUALITIES.
    #[serde(default = "best")]
    pub ytdl_quality: String,
    /// yt-dlp executable; empty = the one on PATH.
    #[serde(default)]
    pub ytdl_path: String,
    /// Let mpv read <config>/zym-player/mpv/mpv.conf (any mpv option). Applies at startup.
    #[serde(default)]
    pub mpv_conf: bool,
    /// Settings › mpv option values that differ from mpv's defaults (see mpvopts.rs).
    #[serde(default)]
    pub mpv: std::collections::BTreeMap<String, String>,
}

fn yes() -> bool {
    true
}

fn best() -> String {
    "best".into()
}

impl Default for PlayerPrefs {
    fn default() -> Self {
        Self { hwdec: yes(), ytdl: yes(), ytdl_quality: best(), ytdl_path: String::new(), mpv_conf: false, mpv: Default::default() }
    }
}

/// (key, label, ytdl-format). `?` keeps formats without height info eligible.
pub const YTDL_QUALITIES: [(&str, &str, &str); 5] = [
    ("best", "Best", "bestvideo+bestaudio/best"),
    ("1080", "1080p", "bestvideo[height<=?1080]+bestaudio/best[height<=?1080]/best"),
    ("720", "720p", "bestvideo[height<=?720]+bestaudio/best[height<=?720]/best"),
    ("480", "480p", "bestvideo[height<=?480]+bestaudio/best[height<=?480]/best"),
    ("audio", "Audio only", "bestaudio/best"),
];

pub fn ytdl_format(quality: &str) -> &'static str {
    YTDL_QUALITIES.iter().find(|q| q.0 == quality).unwrap_or(&YTDL_QUALITIES[0]).2
}

/// mpv option values for the yt-dlp prefs: (ytdl, ytdl-format, script-opts entry). The path goes
/// into a key-value list, so it's set with `-append` (one pair, no escaping: commas in paths are fine).
pub fn ytdl_options(p: &PlayerPrefs) -> [(&'static str, String); 3] {
    [
        ("ytdl", if p.ytdl { "yes" } else { "no" }.into()),
        ("ytdl-format", ytdl_format(&p.ytdl_quality).into()),
        ("script-opts-append", format!("ytdl_hook-ytdl_path={}", p.ytdl_path.trim())),
    ]
}

/// Push player prefs into a running mpv (startup, after initialize, and on every change).
/// script-opts must be set at runtime: the `-append` form isn't accepted before initialize, and
/// ytdl_hook re-reads its options whenever they change.
pub fn apply_prefs(mpv: &crate::Mpv, p: &PlayerPrefs) {
    mpv.cmd(&["set", "hwdec", hwdec_value(p.hwdec)]);
    crate::mpvopts::apply(mpv, &p.mpv);
    for (k, v) in ytdl_options(p) {
        match k.strip_suffix("-append") {
            Some(list) => mpv.cmd(&["change-list", list, "append", &v]),
            None => mpv.cmd(&["set", k, &v]),
        }
    }
}

/// mpv's own config folder for the player: <config>/zym-player/mpv (mpv.conf, input.conf, scripts/).
pub fn mpv_config_dir() -> PathBuf {
    crate::config_dir().join("mpv")
}

const MPV_CONF_TEMPLATE: &str = "\
# mpv options for Zym Media Player: one per line, same syntax as mpv's own mpv.conf.
# Reference: https://mpv.io/manual/stable/#options
#
# Read only when Settings > mpv > \"Load mpv.conf\" is on; changes apply after restarting the player.
# The player's own settings (hardware decoding, yt-dlp) and vo / video-rotate are applied
# after this file and win; everything else here is used as written.
#
# Examples:
# volume-max=150
# sub-font-size=44
# cache-secs=60
# demuxer-max-bytes=200MiB
";

/// mpv.conf path, created with a commented template the first time it's asked for.
pub fn ensure_mpv_conf() -> std::io::Result<PathBuf> {
    let dir = mpv_config_dir();
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("mpv.conf");
    if !file.exists() {
        std::fs::write(&file, MPV_CONF_TEMPLATE)?;
    }
    Ok(file)
}

/// file:// URL for opening a local path with the desktop's default app.
fn file_url(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') { format!("file://{s}") } else { format!("file:///{s}") }
}

/// `<yt-dlp> --version` (blocking: call off the UI thread). Ok("2026.10.26 · /usr/bin/yt-dlp").
pub fn probe_ytdlp(path: &str) -> Result<String, String> {
    let exe = if path.trim().is_empty() { "yt-dlp" } else { path.trim() };
    let out = std::process::Command::new(exe).arg("--version").output().map_err(|_| format!("{exe} not found"))?;
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || version.is_empty() {
        return Err(format!("{exe} didn't report a version"));
    }
    let resolved = if exe.contains(std::path::MAIN_SEPARATOR) { exe.to_string() } else { which(exe).unwrap_or_else(|| exe.to_string()) };
    Ok(format!("{version} · {resolved}"))
}

/// First `name` on PATH (also tries `.exe` on Windows).
fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|d| [d.join(name), d.join(format!("{name}.exe"))])
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

pub fn player_prefs() -> PlayerPrefs {
    get("player").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

pub fn hwdec_value(on: bool) -> &'static str {
    // Explicit copy-back decoders, never Vulkan: auto-copy-safe still tried Vulkan first on NVIDIA,
    // and on older cards (GTX 970, driver 580) Vulkan decode faulted the GPU copy engine
    // (kernel "NVRM: Xid 31 ... ENGINE CE2", or VK_ERROR_DEVICE_LOST). APIs that don't exist on
    // this OS are skipped; if none fits the codec, mpv decodes on the CPU.
    if on { HWDEC } else { "no" }
}

pub const HWDEC: &str = "nvdec-copy,vaapi-copy,d3d11va-copy,videotoolbox-copy";

// ------------------------------------------------------------------ enabled / disabled plugins

/// Names switched off in Settings. `kind`: "scripts" (file names) or "content" (manifest ids).
pub fn disabled(kind: &str) -> Vec<String> {
    get("disabled").and_then(|d| d.get(kind).cloned()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// `list` with `name` switched off (present once) or on (absent).
fn toggled(mut list: Vec<String>, name: &str, off: bool) -> Vec<String> {
    list.retain(|n| n != name);
    if off {
        list.push(name.to_string());
    }
    list
}

/// Save a plugin's on/off state; returns the new disabled list for `kind`.
fn set_disabled(kind: &str, name: &str, off: bool) -> Vec<String> {
    let list = toggled(disabled(kind), name, off);
    let mut d = get("disabled").filter(Value::is_object).unwrap_or_else(|| json!({}));
    d[kind] = json!(list);
    put("disabled", d);
    list
}

/// mpv Lua/JS scripts in the plugins dir: .lua/.js files, or folders (main.lua inside).
pub fn scripts() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(crate::plugins_dir()) else { return vec![] };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() || matches!(p.extension().and_then(|x| x.to_str()), Some("lua" | "js")))
        .collect();
    out.sort();
    out
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// ------------------------------------------------------------------ panel

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Appearance,
    Plugins,
    Mpv,
    Logs,
}

/// Everything the panel shows is read once when it opens (and updated on each toggle), so
/// rendering never touches the disk.
pub struct SettingsPanel {
    pub tab: Tab,
    scripts: Vec<String>,
    /// (manifest id, name)
    content: Vec<(String, String)>,
    off_scripts: Vec<String>,
    off_content: Vec<String>,
    prefs: PlayerPrefs,
    /// yt-dlp path box draft (saved on Enter / Save).
    pub ytdl_path: String,
    /// Drafts of the mpv text options, by option name (saved on Enter / Save).
    pub mpv_text: std::collections::BTreeMap<&'static str, String>,
    /// None = still checking.
    ytdlp: Option<Result<String, String>>,
    /// Logs tab: most verbose level shown.
    log_filter: crate::log::Level,
    /// Script toggles only take effect on restart (mpv can't unload a script).
    restart_needed: bool,
}

impl SettingsPanel {
    /// Text behind the Settings fields (yt-dlp path, mpv text options); a draft starts as the saved value.
    pub fn field_text(&mut self, f: &Field) -> Option<&mut String> {
        match f {
            Field::YtdlPath => Some(&mut self.ytdl_path),
            Field::MpvOpt(key) => {
                let saved = crate::mpvopts::find(key).map(|o| crate::mpvopts::value(&self.prefs.mpv, o).to_string()).unwrap_or_default();
                Some(self.mpv_text.entry(key).or_insert(saved))
            }
            _ => None,
        }
    }
}

impl Player {
    pub fn open_settings(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.close_overlays();
        self.settings = Some(SettingsPanel {
            tab,
            scripts: scripts().iter().map(|p| file_name(p)).collect(),
            content: sources::load_all().iter().map(|p| (p.manifest.id.clone(), p.manifest.name.clone())).collect(),
            off_scripts: disabled("scripts"),
            off_content: disabled("content"),
            ytdl_path: player_prefs().ytdl_path,
            mpv_text: Default::default(),
            ytdlp: None,
            log_filter: crate::log::Level::Debug,
            prefs: player_prefs(),
            restart_needed: false,
        });
        self.probe_ytdlp(cx);
    }

    /// Check the configured yt-dlp in the background; the result shows under the yt-dlp settings.
    fn probe_ytdlp(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.settings.as_mut() else { return };
        s.ytdlp = None;
        let path = s.prefs.ytdl_path.clone();
        let task = cx.background_executor().spawn(async move { probe_ytdlp(&path) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |p, cx| {
                if let Some(s) = p.settings.as_mut() {
                    s.ytdlp = Some(r);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Change, save and apply player prefs (the one writer of "player").
    pub fn update_prefs(&mut self, f: impl FnOnce(&mut PlayerPrefs)) -> PlayerPrefs {
        let mut prefs = self.settings.as_ref().map(|s| s.prefs.clone()).unwrap_or_else(player_prefs);
        f(&mut prefs);
        put("player", serde_json::to_value(&prefs).unwrap());
        apply_prefs(&self.mpv, &prefs);
        if let Some(s) = self.settings.as_mut() {
            s.prefs = prefs.clone();
        }
        prefs
    }

    /// Save the yt-dlp path box (Enter or Save) and re-check it.
    pub fn save_ytdl_path(&mut self, cx: &mut Context<Self>) {
        let path = self.settings.as_ref().map(|s| s.ytdl_path.trim().to_string()).unwrap_or_default();
        self.update_prefs(|p| p.ytdl_path = path);
        self.editing = None;
        self.probe_ytdlp(cx);
    }

    /// Hardware decoding on/off: applied now and saved. The one setter for every switch of it.
    pub fn set_hwdec(&mut self, on: bool) {
        self.update_prefs(|p| p.hwdec = on);
    }

    pub fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            self.settings = None;
        } else {
            self.open_settings(Tab::Appearance, cx);
        }
    }

    pub fn render_settings(&self, s: &SettingsPanel, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let tabs = [(Tab::Appearance, "Appearance"), (Tab::Plugins, "Plugins"), (Tab::Mpv, "mpv"), (Tab::Logs, "Logs")];
        let side = sidebar("SETTINGS")
            .children(tabs.map(|(tab, label)| {
                sidebar_item(SharedString::from(format!("set-tab-{label}")), s.tab == tab).child(label).on_click(cx.listener(
                    move |this, _, _, _| {
                        if let Some(s) = this.settings.as_mut() {
                            s.tab = tab;
                        }
                    },
                ))
            }))
            .child(div().flex_1())
            .child(sidebar_close("set-close").on_click(cx.listener(|this, _, _, _| this.settings = None)));

        let title = tabs.iter().find(|t| t.0 == s.tab).map_or("", |t| t.1);
        let body = match s.tab {
            Tab::Appearance => self.appearance_tab(cx),
            Tab::Plugins => self.plugins_tab(s, cx),
            Tab::Mpv => self.mpv_tab(s, cx),
            Tab::Logs => self.logs_tab(s, cx),
        };

        self.big_panel("settings", window, cx).child(side).child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(panel_header(title, div()))
                .child(div().id("settings-body").flex_1().overflow_y_scroll().pb_4().child(body)),
        )
    }

    fn appearance_tab(&self, cx: &mut Context<Self>) -> Div {
        let p = theme::prefs();
        let set = |p: theme::Prefs, window: &mut Window| {
            theme::set(p);
            theme::resolve(window.appearance());
        };
        div()
            .child(heading("THEME"))
            .child(div().flex().gap_2().px_4().children(
                [("System", theme::Mode::System), ("Dark", theme::Mode::Dark), ("Light", theme::Mode::Light)].map(|(label, mode)| {
                    chip(SharedString::from(format!("mode-{label}")), label, p.mode == mode)
                        .on_click(cx.listener(move |_, _, window, _| set(theme::Prefs { mode, ..theme::prefs() }, window)))
                }),
            ))
            .child(heading("ACCENT"))
            .child(div().flex().flex_wrap().gap_2().px_4().children(theme::ACCENTS.map(|(name, color)| {
                div()
                    .id(SharedString::from(format!("accent-{name}")))
                    .size(px(28.))
                    .rounded_full()
                    .bg(rgb(color))
                    .cursor_pointer()
                    .border_2()
                    .border_color(if p.accent == color { t().fg() } else { gpui::transparent_black().into() })
                    .hover(|s| s.opacity(0.85))
                    .on_click(cx.listener(move |_, _, window, _| set(theme::Prefs { accent: color, ..theme::prefs() }, window)))
            })))
    }

    fn plugins_tab(&self, s: &SettingsPanel, cx: &mut Context<Self>) -> Div {
        let (off_content, off_scripts) = (&s.off_content, &s.off_scripts);
        div()
            .child(heading("CONTENT PLUGINS"))
            .child(note("Feed the Explorer. A disabled plugin is hidden there; its connections are kept."))
            .children(s.content.iter().map(|(id, name)| {
                let on = !off_content.contains(id);
                let id = id.clone();
                switch_row(format!("content-{id}"), name, None, on).on_click(cx.listener(move |this, _, _, _| {
                    let list = set_disabled("content", &id, on);
                    if let Some(s) = this.settings.as_mut() {
                        s.off_content = list;
                    }
                    this.explorer.loaded = false; // re-read the plugin list next time the Explorer opens
                }))
            }))
            .when(s.content.is_empty(), |d| d.child(note("None installed.")))
            .child(heading("SCRIPTS"))
            .child(note(&format!("mpv Lua/JS scripts from {}", crate::plugins_dir().display())))
            .children(s.scripts.iter().map(|name| {
                let on = !off_scripts.contains(name);
                let name = name.clone();
                switch_row(format!("script-{name}"), &name, None, on).on_click(cx.listener(move |this, _, _, _| {
                    let list = set_disabled("scripts", &name, on);
                    if let Some(s) = this.settings.as_mut() {
                        s.off_scripts = list;
                        s.restart_needed = true;
                    }
                }))
            }))
            .when(s.scripts.is_empty(), |d| d.child(note("None installed.")))
            .when(s.restart_needed, |d| d.child(div().px_4().pt_2().text_color(t().accent()).child(format!("Restart {} to apply script changes.", crate::APP_NAME))))
    }

    fn logs_tab(&self, s: &SettingsPanel, cx: &mut Context<Self>) -> Div {
        use crate::log::{self, Level};
        let filter = s.log_filter;
        let verbose = self.mpv_verbose;
        let lines = log::recent(filter, 400); // newest first; rendering more is slow and rarely useful
        let set_filter = |lv: Level| move |this: &mut Player, _: &gpui::ClickEvent, _: &mut Window, _: &mut Context<Player>| {
            if let Some(s) = this.settings.as_mut() {
                s.log_filter = lv;
            }
        };
        div()
            .flex()
            .flex_col()
            .child(heading("LOGS"))
            .child(note(&format!("Messages from mpv and the player, newest first. This run is also saved to {} (rotated at 5 MB; the run before is previous.log).", log::file_path().display())))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .pb_2()
                    .child(chip("log-all", "All", filter == Level::Debug).on_click(cx.listener(set_filter(Level::Debug))))
                    .child(chip("log-warn", "Warnings", filter == Level::Warn).on_click(cx.listener(set_filter(Level::Warn))))
                    .child(chip("log-err", "Errors", filter == Level::Error).on_click(cx.listener(set_filter(Level::Error))))
                    .child(div().flex_1())
                    .child(chip("log-copy", "Copy", false).on_click(cx.listener(move |_, _, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(log::text(filter))))))
                    .child(chip("log-clear", "Clear", false).on_click(cx.listener(|_, _, _, _| log::clear())))
                    .child(chip("log-open", "Open log file", false).on_click(cx.listener(|_, _, _, cx| cx.open_url(&file_url(&log::file_path()))))),
            )
            .child(
                switch_row("log-verbose".into(), "Verbose mpv log", Some("Everything mpv reports, for tracking down a problem. Off again after a restart."), verbose)
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.mpv.set_log_level(!verbose);
                        this.mpv_verbose = !verbose;
                    })),
            )
            .child(
                div()
                    .mx_4()
                    .mt_2()
                    .p_2()
                    .rounded_md()
                    .bg(t().field())
                    .font_family("monospace")
                    .text_xs()
                    .when(lines.is_empty(), |d| d.child(div().text_color(t().muted()).child("Nothing logged yet.")))
                    .children(lines.into_iter().map(|l| {
                        let color = match l.level {
                            Level::Error => t().error(),
                            Level::Warn => t().fg(),
                            _ => t().muted(),
                        };
                        div().text_color(color).child(l.format())
                    })),
            )
    }

    /// Everything mpv: decoding, yt-dlp, common options, and mpv.conf for the rest.
    fn mpv_tab(&self, s: &SettingsPanel, cx: &mut Context<Self>) -> Div {
        div()
            .child(self.decoding_and_ytdl(s, cx))
            .child(self.mpv_options(&s.prefs.mpv, &s.mpv_text, cx))
            .child(self.mpv_conf_section(s, cx))
    }

    fn mpv_conf_section(&self, s: &SettingsPanel, cx: &mut Context<Self>) -> Div {
        let on = s.prefs.mpv_conf;
        let file = mpv_config_dir().join("mpv.conf");
        div()
            .child(heading("ADVANCED: MPV.CONF"))
            .child(
                switch_row("mpv-conf".into(), "Load mpv.conf", Some("Any of mpv's ~900 options, in mpv's own config format. Applies after restarting the player."), on)
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.update_prefs(|p| p.mpv_conf = !on);
                        if let Some(s) = this.settings.as_mut() {
                            s.restart_needed = true;
                        }
                    })),
            )
            .child(note(&file.display().to_string()).pt_2())
            .child(
                div()
                    .flex()
                    .gap_2()
                    .px_4()
                    .child(chip("mpv-open", "Open mpv.conf", true).on_click(cx.listener(|this, _, _, cx| match ensure_mpv_conf() {
                        Ok(f) => cx.open_url(&file_url(&f)),
                        Err(e) => this.mpv_conf_error = Some(e.to_string()),
                    })))
                    .child(chip("mpv-dir", "Open folder", false).on_click(cx.listener(|this, _, _, cx| match ensure_mpv_conf() {
                        Ok(_) => cx.open_url(&file_url(&mpv_config_dir())),
                        Err(e) => this.mpv_conf_error = Some(e.to_string()),
                    }))),
            )
            .children(self.mpv_conf_error.clone().map(|e| div().px_4().pt_2().text_xs().text_color(t().error()).child(e)))
            .child(note("input.conf (key bindings) and a scripts/ folder in the same place are read too. The settings above are applied after mpv.conf and win; vo and video-rotate are managed by the player.").pt_3())
            .when(s.restart_needed, |d| d.child(div().px_4().pt_2().text_color(t().accent()).child(format!("Restart {} to apply.", crate::APP_NAME))))
    }

    fn decoding_and_ytdl(&self, s: &SettingsPanel, cx: &mut Context<Self>) -> Div {
        let on = s.prefs.hwdec;
        let ytdl = s.prefs.ytdl;
        let status = match &s.ytdlp {
            None => div().text_color(t().muted()).child("Checking yt-dlp…"),
            Some(Ok(v)) => div().text_color(t().muted()).child(format!("✓ yt-dlp {v}")),
            Some(Err(e)) => div().text_color(t().error()).child(format!("{e}. Install yt-dlp, or set its path above.")),
        };
        div()
            .child(heading("DECODING"))
            .child(
                switch_row("hwdec".into(), "Hardware decoding", Some("Decode video on the GPU. Turn off if video shows glitches or green frames."), on)
                    .on_click(cx.listener(move |this, _, _, _| this.set_hwdec(!on))),
            )
            .child(heading("ONLINE VIDEO (YT-DLP)"))
            .child(
                switch_row("ytdl".into(), "Play links from YouTube and other sites", Some("Pages are opened through yt-dlp. Changes apply to the next video."), ytdl)
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.update_prefs(|p| p.ytdl = !ytdl);
                    })),
            )
            .when(ytdl, |d| {
                d.child(
                    div().px_4().pt_2().flex().flex_col().gap_2().child("Maximum quality").child(div().flex().flex_wrap().gap_2().children(YTDL_QUALITIES.map(|(key, label, _)| {
                        chip(SharedString::from(format!("ytq-{key}")), label, s.prefs.ytdl_quality == key).on_click(cx.listener(move |this, _, _, _| {
                            this.update_prefs(|p| p.ytdl_quality = key.into());
                        }))
                    }))),
                )
                .child(
                    div()
                        .px_4()
                        .pt_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child("yt-dlp program")
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(div().flex_1().min_w_0().child(self.text_box(Field::YtdlPath, &s.ytdl_path, "yt-dlp  (found on PATH)", false, cx)))
                                .child(button("ytdl-path-save", "Save", s.ytdl_path.trim() != s.prefs.ytdl_path).on_click(cx.listener(|this, _, _, cx| this.save_ytdl_path(cx)))),
                        )
                        .child(status.text_xs())
                        .child(note("A current yt-dlp matters: sites change often and old versions fail (e.g. HTTP 403).").px_0()),
                )
            })
    }
}

pub fn heading(text: &str) -> Div {
    div().px_4().pt_5().pb_2().text_xs().text_color(t().muted()).child(text.to_string())
}

fn note(text: &str) -> Div {
    div().px_4().pb_2().text_xs().text_color(t().dim()).child(text.to_string())
}

/// Full-width "label (+ description) ...... switch" row; the caller adds on_click.
pub fn switch_row(id: String, label: &str, desc: Option<&str>, on: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .flex()
        .items_center()
        .gap_4()
        .mx_2()
        .px_2()
        .py_2()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(t().hover()))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(label.to_string())
                .children(desc.map(|d| div().text_xs().text_color(t().dim()).child(d.to_string()))),
        )
        .child(switch(on))
}

#[cfg(test)]
mod tests {
    #[test]
    fn prefs_defaults() {
        let p: super::PlayerPrefs = serde_json::from_str("{}").unwrap();
        assert!(p.hwdec, "missing hwdec -> on");
        assert_eq!(super::hwdec_value(false), "no");
        assert!(!super::hwdec_value(true).contains("vulkan") && !super::hwdec_value(true).contains("auto"));
    }

    #[test]
    fn ytdl_prefs() {
        let p: super::PlayerPrefs = serde_json::from_str(r#"{"hwdec": false}"#).unwrap();
        assert!(p.ytdl && p.ytdl_quality == "best" && p.ytdl_path.is_empty(), "old settings files get yt-dlp defaults");
        let p = super::PlayerPrefs { ytdl: false, ytdl_quality: "720".into(), ytdl_path: " /opt/yt dlp,v2/yt-dlp ".into(), ..p };
        let o = super::ytdl_options(&p);
        assert_eq!(o[0], ("ytdl", "no".to_string()));
        assert!(o[1].1.contains("height<=?720"));
        assert_eq!(o[2], ("script-opts-append", "ytdl_hook-ytdl_path=/opt/yt dlp,v2/yt-dlp".to_string()), "trimmed, commas kept");
        assert_eq!(super::ytdl_format("nonsense"), super::YTDL_QUALITIES[0].2, "unknown -> best");
        assert!(super::probe_ytdlp("/nonexistent/yt-dlp").is_err());
        assert!(!super::PlayerPrefs::default().mpv_conf, "mpv.conf is opt-in");
        assert_eq!(super::file_url(std::path::Path::new("/a b/mpv.conf")), "file:///a b/mpv.conf");
    }

    #[test]
    fn toggling_plugins() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::toggled(l(&[]), "a.lua", true), l(&["a.lua"]));
        assert_eq!(super::toggled(l(&["a.lua"]), "a.lua", true), l(&["a.lua"]), "no duplicates");
        assert_eq!(super::toggled(l(&["a.lua", "b.lua"]), "a.lua", false), l(&["b.lua"]));
    }
}
