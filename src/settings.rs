//! Settings panel (Appearance · Plugins · Player) and the settings.json store every pref shares:
//! <config>/zym-player/settings.json, one top-level key per area ("appearance", "player", "disabled").

use std::path::PathBuf;

use gpui::{Context, Div, SharedString, Stateful, Window, div, prelude::*, px, rgb};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::panel::{panel_header, sidebar, sidebar_close, sidebar_item};
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
        eprintln!("settings: couldn't save {}: {e}", path.display());
    }
}

// ------------------------------------------------------------------ player prefs

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct PlayerPrefs {
    #[serde(default = "yes")]
    pub hwdec: bool,
}

fn yes() -> bool {
    true
}

impl Default for PlayerPrefs {
    fn default() -> Self {
        Self { hwdec: yes() }
    }
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
    Player,
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
    /// Script toggles only take effect on restart (mpv can't unload a script).
    restart_needed: bool,
}

impl Player {
    pub fn open_settings(&mut self, tab: Tab) {
        self.close_overlays();
        self.settings = Some(SettingsPanel {
            tab,
            scripts: scripts().iter().map(|p| file_name(p)).collect(),
            content: sources::load_all().iter().map(|p| (p.manifest.id.clone(), p.manifest.name.clone())).collect(),
            off_scripts: disabled("scripts"),
            off_content: disabled("content"),
            prefs: player_prefs(),
            restart_needed: false,
        });
    }

    /// Hardware decoding on/off: applied now and saved. The one setter for every switch of it.
    pub fn set_hwdec(&mut self, on: bool) {
        let prefs = PlayerPrefs { hwdec: on };
        put("player", serde_json::to_value(prefs).unwrap());
        self.mpv.cmd(&["set", "hwdec", hwdec_value(on)]);
        if let Some(s) = self.settings.as_mut() {
            s.prefs = prefs;
        }
    }

    pub fn toggle_settings(&mut self) {
        if self.settings.is_some() {
            self.settings = None;
        } else {
            self.open_settings(Tab::Appearance);
        }
    }

    pub fn render_settings(&self, s: &SettingsPanel, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let tabs = [(Tab::Appearance, "Appearance"), (Tab::Plugins, "Plugins"), (Tab::Player, "Player")];
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
            Tab::Player => self.player_tab(s, cx),
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

    fn player_tab(&self, s: &SettingsPanel, cx: &mut Context<Self>) -> Div {
        let on = s.prefs.hwdec;
        div().child(heading("PLAYBACK")).child(
            switch_row("hwdec".into(), "Hardware decoding", Some("Decode video on the GPU. Turn off if video shows glitches or green frames."), on)
                .on_click(cx.listener(move |this, _, _, _| this.set_hwdec(!on))),
        )
    }
}

fn heading(text: &str) -> Div {
    div().px_4().pt_5().pb_2().text_xs().text_color(t().muted()).child(text.to_string())
}

fn note(text: &str) -> Div {
    div().px_4().pb_2().text_xs().text_color(t().dim()).child(text.to_string())
}

/// Full-width "label (+ description) ...... switch" row; the caller adds on_click.
fn switch_row(id: String, label: &str, desc: Option<&str>, on: bool) -> Stateful<Div> {
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
    fn toggling_plugins() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::toggled(l(&[]), "a.lua", true), l(&["a.lua"]));
        assert_eq!(super::toggled(l(&["a.lua"]), "a.lua", true), l(&["a.lua"]), "no duplicates");
        assert_eq!(super::toggled(l(&["a.lua", "b.lua"]), "a.lua", false), l(&["b.lua"]));
    }
}
