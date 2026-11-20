//! Settings › mpv: common mpv options as switches, choices and text fields, so nobody needs to
//! write mpv.conf. A table: adding an option is one entry. Only options that work with the
//! player's software renderer are listed (GPU-renderer options like deband or scalers do nothing).
//! Values are saved in settings.json ("player"."mpv"), only when they differ from the default.

use std::collections::BTreeMap;

use gpui::{Context, Div, SharedString, div, prelude::*};

use crate::Player;
use crate::panel::button;
use crate::textbox::Field;
use crate::theme::t;

pub enum Kind {
    /// "yes" / "no".
    Toggle,
    /// (label, value).
    Choice(&'static [(&'static str, &'static str)]),
    /// Free text; the &str is the placeholder.
    Text(&'static str),
}

pub struct Opt {
    /// mpv option name.
    pub key: &'static str,
    pub label: &'static str,
    pub desc: &'static str,
    pub kind: Kind,
    /// mpv's default (what's shown when the user never changed it).
    pub default: &'static str,
}

const fn opt(key: &'static str, label: &'static str, desc: &'static str, kind: Kind, default: &'static str) -> Opt {
    Opt { key, label, desc, kind, default }
}

pub const SECTIONS: &[(&str, &[Opt])] = &[
    ("PLAYBACK", &[
        opt("save-position-on-quit", "Resume where you left off", "Remember the position of each file when you quit or press Stop.", Kind::Toggle, "no"),
    ]),
    ("AUDIO", &[
        opt("volume-max", "Volume boost limit", "How far the volume can go above 100%.", Kind::Choice(&[("100%", "100"), ("130%", "130"), ("150%", "150"), ("200%", "200")]), "130"),
        opt("audio-pitch-correction", "Keep voice pitch when changing speed", "Off sounds like a tape played faster or slower.", Kind::Toggle, "yes"),
        opt("alang", "Preferred audio languages", "Language codes in order, e.g. en,eng,hi. Picked when a file has several.", Kind::Text("e.g. en,eng"), ""),
    ]),
    ("SUBTITLES", &[
        opt("slang", "Preferred subtitle languages", "Language codes in order, e.g. en,eng.", Kind::Text("e.g. en,eng"), ""),
        opt("sub-auto", "Load subtitle files next to the video", "Which .srt/.ass files in the video's folder are loaded.", Kind::Choice(&[("Off", "no"), ("Same name", "exact"), ("Similar name", "fuzzy"), ("All", "all")]), "exact"),
    ]),
    ("STREAMING", &[
        opt("cache-secs", "Buffer ahead", "How much of a network stream to load in advance.", Kind::Choice(&[("Default", ""), ("30 s", "30"), ("2 min", "120"), ("10 min", "600")]), ""),
        opt("demuxer-max-bytes", "Buffer memory", "Upper limit for the buffer above.", Kind::Choice(&[("150 MB", "150MiB"), ("500 MB", "500MiB"), ("1 GB", "1GiB"), ("2 GB", "2GiB")]), "150MiB"),
    ]),
    ("SCREENSHOTS", &[
        opt("screenshot-format", "Format", "", Kind::Choice(&[("JPEG", "jpg"), ("PNG", "png"), ("WebP", "webp")]), "jpg"),
        opt("screenshot-dir", "Folder", "Where Tools › Screenshot saves (~ is your home folder).", Kind::Text("~/Pictures"), "~/Pictures"),
    ]),
    ("ON-SCREEN TEXT", &[
        opt("osd-font-size", "Size of mpv's on-screen messages", "Seek times, volume and script messages drawn on the video.", Kind::Choice(&[("Small", "40"), ("Normal", "55"), ("Large", "70")]), "55"),
    ]),
];

pub fn find(key: &str) -> Option<&'static Opt> {
    SECTIONS.iter().flat_map(|(_, opts)| opts.iter()).find(|o| o.key == key)
}

/// Value in effect for `key`: the saved one, else mpv's default.
pub fn value<'a>(saved: &'a BTreeMap<String, String>, o: &'a Opt) -> &'a str {
    saved.get(o.key).map(String::as_str).unwrap_or(o.default)
}

/// Store `v` for `key`; a value equal to the default is removed, so settings.json only holds changes.
pub fn set(saved: &mut BTreeMap<String, String>, key: &str, v: &str) {
    match find(key) {
        Some(o) if v.trim() == o.default => {
            saved.remove(key);
        }
        Some(_) => {
            saved.insert(key.to_string(), v.trim().to_string());
        }
        None => {}
    }
}

/// Apply every known option to mpv (startup and after a change). Unchanged ones get their default,
/// which also undoes an option switched back to default at runtime. Unknown keys are ignored.
pub fn apply(mpv: &crate::Mpv, saved: &BTreeMap<String, String>) {
    for (_, opts) in SECTIONS {
        for o in *opts {
            mpv.cmd(&["set", o.key, value(saved, o)]);
        }
    }
}

impl Player {
    fn set_mpv_opt(&mut self, key: &'static str, v: String) {
        self.update_prefs(|p| set(&mut p.mpv, key, &v));
    }

    /// Enter / Save in one of the text options.
    pub fn save_mpv_text(&mut self, key: &'static str) {
        let v = self.settings.as_ref().and_then(|s| s.mpv_text.get(key).cloned()).unwrap_or_default();
        self.set_mpv_opt(key, v);
        self.editing = None;
    }

    pub fn mpv_options(&self, saved: &BTreeMap<String, String>, drafts: &BTreeMap<&'static str, String>, cx: &mut Context<Self>) -> Div {
        div().children(SECTIONS.iter().map(|(title, opts)| {
            div().child(crate::settings::heading(title)).children(opts.iter().map(|o| {
                let current = value(saved, o).to_string();
                let key = o.key;
                let desc = (!o.desc.is_empty()).then_some(o.desc);
                match &o.kind {
                    Kind::Toggle => {
                        let on = current == "yes";
                        crate::settings::switch_row(format!("mo-{key}"), o.label, desc, on)
                            .on_click(cx.listener(move |this, _, _, _| this.set_mpv_opt(key, if on { "no" } else { "yes" }.into())))
                            .into_any_element()
                    }
                    Kind::Choice(choices) => block(o.label, desc)
                        .child(div().flex().flex_wrap().gap_2().pt_1().children(choices.iter().map(|(label, v)| {
                            crate::chip(SharedString::from(format!("mo-{key}-{v}")), *label, current == *v)
                                .on_click(cx.listener(move |this, _, _, _| this.set_mpv_opt(key, v.to_string())))
                        })))
                        .into_any_element(),
                    Kind::Text(placeholder) => {
                        let draft = drafts.get(key).cloned().unwrap_or(current.clone());
                        let changed = draft.trim() != current;
                        block(o.label, desc)
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .pt_1()
                                    .child(div().flex_1().min_w_0().child(self.text_box(Field::MpvOpt(key), &draft, placeholder, false, cx)))
                                    .child(button(SharedString::from(format!("mo-save-{key}")), "Save", changed).on_click(cx.listener(move |this, _, _, _| this.save_mpv_text(key)))),
                            )
                            .into_any_element()
                    }
                }
            }))
        }))
    }
}

/// Label + optional description, padded like a settings row.
fn block(label: &str, desc: Option<&str>) -> Div {
    div()
        .mx_2()
        .px_2()
        .py_2()
        .child(label.to_string())
        .children(desc.map(|d| div().text_xs().text_color(t().dim()).child(d.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_consistent() {
        let mut keys = vec![];
        for (_, opts) in SECTIONS {
            for o in *opts {
                assert!(!keys.contains(&o.key), "duplicate option {}", o.key);
                keys.push(o.key);
                match &o.kind {
                    Kind::Toggle => assert!(matches!(o.default, "yes" | "no"), "{}", o.key),
                    Kind::Choice(c) => assert!(c.iter().any(|(_, v)| *v == o.default), "{}: default must be a choice", o.key),
                    Kind::Text(_) => {}
                }
            }
        }
    }

    #[test]
    fn only_changes_are_saved() {
        let mut saved = BTreeMap::new();
        set(&mut saved, "volume-max", "200");
        assert_eq!(value(&saved, find("volume-max").unwrap()), "200");
        set(&mut saved, "volume-max", "130");
        assert!(saved.is_empty(), "back to default -> removed");
        set(&mut saved, "alang", "  en,eng ");
        assert_eq!(saved.get("alang").map(String::as_str), Some("en,eng"), "trimmed");
        set(&mut saved, "not-an-option", "x");
        assert!(!saved.contains_key("not-an-option"), "only listed options");
    }
}
