//! Single-line text box, used by every text field in the app (GPUI ships no text input).
//! Mouse: click places the caret, drag selects, Shift+click extends, double-click selects a word,
//! triple-click selects all. Keys (routed from Player::on_key): ←/→/Home/End (+Shift selects),
//! Backspace/Delete, Ctrl+A/C/X/V; typing or pasting replaces the selection. Right-click: menu.

use gpui::{Context, Div, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, Stateful, Window, div, prelude::*, px};

use crate::Player;
use crate::menu::{Item, ui};
use crate::theme::t;

/// Per text box (by field): text start x, font and size from the last paint.
pub type TextGeo = std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, (gpui::Pixels, gpui::Font, gpui::Pixels)>>>;

/// Which text box has keyboard focus. Each variant's text lives with the panel that owns it
/// (`field_text` dispatches); adding a field = a variant + its owner's two match arms.
#[derive(Clone, PartialEq, Debug)]
pub enum Field {
    /// Playlist "+" dialog URL.
    AddUrl,
    // Explorer
    Search,
    Setting(String),
    ConnName,
    // Playlists manager
    PlSaveAs,
    PlRename,
    PlNewName,
    PlAddUrl,
    /// Settings › mpv: yt-dlp program path.
    YtdlPath,
    /// Settings › mpv: a text option (mpv option name).
    MpvOpt(&'static str),
}

impl Player {
    /// Mutable text behind a field.
    pub fn field_text(&mut self, f: &Field) -> Option<&mut String> {
        match f {
            Field::AddUrl => Some(&mut self.add_url),
            Field::Search | Field::Setting(_) | Field::ConnName => self.explorer.field_text(f),
            Field::PlSaveAs | Field::PlRename | Field::PlNewName | Field::PlAddUrl => self.pl.field_text(f),
            Field::YtdlPath | Field::MpvOpt(_) => self.settings.as_mut()?.field_text(f),
        }
    }

    /// Enter in a text field.
    pub fn submit_field(&mut self, f: Field, cx: &mut Context<Self>) {
        match f {
            Field::AddUrl => {
                let url = self.add_url.trim().to_string();
                if !url.is_empty() {
                    self.mpv.load(&url, "append-play");
                    self.add_url.clear();
                }
                self.editing = None;
                self.add_popup = false;
            }
            Field::Search | Field::Setting(_) | Field::ConnName => self.submit_explorer_field(&f, cx),
            Field::PlSaveAs | Field::PlRename | Field::PlNewName | Field::PlAddUrl => self.submit_playlist_field(&f),
            Field::YtdlPath => self.save_ytdl_path(cx),
            Field::MpvOpt(key) => self.save_mpv_text(key),
        }
    }
}

/// Caret and selection of the focused field, as char indices. Selection = anchor..pos when they differ.
#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    pub field: Field,
    pub pos: usize,
    pub anchor: usize,
}

// ------------------------------------------------------------------ pure editing

fn byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map_or(s.len(), |(b, _)| b)
}

fn range(pos: usize, anchor: usize) -> Option<(usize, usize)> {
    (pos != anchor).then(|| (pos.min(anchor), pos.max(anchor)))
}

fn selected(text: &str, pos: usize, anchor: usize) -> Option<String> {
    range(pos, anchor).map(|(lo, hi)| text.chars().skip(lo).take(hi - lo).collect())
}

/// The word (letters, digits, `_`) around char index `at`; a single char if it isn't a word char.
fn word_at(text: &str, at: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let at = at.min(chars.len().saturating_sub(1));
    if chars.is_empty() || !is_word(chars[at]) {
        return (at, (at + 1).min(chars.len()));
    }
    let lo = (0..at).rev().take_while(|&i| is_word(chars[i])).last().unwrap_or(at);
    let hi = (at..chars.len()).take_while(|&i| is_word(chars[i])).last().map_or(at, |i| i + 1);
    (lo, hi)
}

/// One editing step. `insert` types/pastes over the selection; otherwise `key` is
/// left/right/home/end (extend selection with `shift`), backspace/delete or "all".
/// False for any other key.
pub fn apply(text: &mut String, pos: &mut usize, anchor: &mut usize, key: &str, shift: bool, insert: Option<&str>) -> bool {
    let len = text.chars().count();
    *pos = (*pos).min(len);
    *anchor = (*anchor).min(len);
    let delete_selection = |text: &mut String, pos: &mut usize, anchor: &mut usize| {
        let Some((lo, hi)) = range(*pos, *anchor) else { return false };
        text.replace_range(byte(text, lo)..byte(text, hi), "");
        (*pos, *anchor) = (lo, lo);
        true
    };
    if let Some(ins) = insert {
        delete_selection(text, pos, anchor);
        text.insert_str(byte(text, *pos), ins);
        *pos += ins.chars().count();
        *anchor = *pos;
        return true;
    }
    match key {
        "left" | "right" if !shift && range(*pos, *anchor).is_some() => {
            let (lo, hi) = range(*pos, *anchor).unwrap();
            *pos = if key == "left" { lo } else { hi }; // collapse to that side
        }
        "left" => *pos = pos.saturating_sub(1),
        "right" => *pos = (*pos + 1).min(len),
        "home" => *pos = 0,
        "end" => *pos = len,
        "all" => {
            (*anchor, *pos) = (0, len);
            return true;
        }
        "backspace" | "delete" if delete_selection(text, pos, anchor) => return true,
        "backspace" if *pos > 0 => {
            text.remove(byte(text, *pos - 1));
            *pos -= 1;
        }
        "delete" if *pos < len => {
            text.remove(byte(text, *pos));
        }
        "backspace" | "delete" => {}
        _ => return false,
    }
    if !shift || matches!(key, "backspace" | "delete") {
        *anchor = *pos;
    }
    true
}

// ------------------------------------------------------------------ player glue

impl Player {
    /// (pos, anchor) in field `f`; a field just focused has the caret at its end.
    fn cursor_in(&self, f: &Field, len: usize) -> (usize, usize) {
        match &self.caret {
            Some(c) if &c.field == f => (c.pos.min(len), c.anchor.min(len)),
            _ => (len, len),
        }
    }

    /// Apply an editing key (or insert text) in field `f`. False if `key` isn't an editing key.
    pub fn field_edit(&mut self, f: &Field, key: &str, shift: bool, insert: Option<&str>) -> bool {
        let len = self.field_text(f).map(|t| t.chars().count());
        let Some(len) = len else { return false };
        let (mut pos, mut anchor) = self.cursor_in(f, len);
        let t = self.field_text(f).expect("checked above");
        let handled = apply(t, &mut pos, &mut anchor, key, shift, insert);
        self.caret = Some(Cursor { field: f.clone(), pos, anchor });
        handled
    }

    /// First line of the clipboard, trimmed, typed over the selection (Ctrl+V and the Paste item).
    pub fn paste_into(&mut self, f: &Field, cx: &mut Context<Self>) {
        let paste = cx.read_from_clipboard().and_then(|c| c.text()).unwrap_or_default();
        self.field_edit(f, "", false, Some(paste.lines().next().unwrap_or("").trim()));
    }

    /// Ctrl+C / Ctrl+X: copy (and cut) the selection; nothing selected = nothing happens.
    pub fn copy_selection(&mut self, f: &Field, cut: bool, cx: &mut Context<Self>) {
        let text = self.field_text(f).cloned().unwrap_or_default();
        let (pos, anchor) = self.cursor_in(f, text.chars().count());
        if let Some(s) = selected(&text, pos, anchor) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(s));
            if cut {
                self.field_edit(f, "backspace", false, None);
            }
        }
    }

    /// Char index under window x in field `f`, from the text geometry recorded at its last paint.
    fn index_at(&mut self, f: &Field, secret: bool, x: Pixels, window: &mut Window) -> Option<usize> {
        let (x0, font, size) = self.text_geo.borrow().get(&format!("{f:?}")).cloned()?;
        let text = self.field_text(f).cloned().unwrap_or_default();
        let shown = if secret { "•".repeat(text.chars().count()) } else { text };
        let run = gpui::TextRun { len: shown.len(), font, color: gpui::black(), background_color: None, underline: None, strikethrough: None };
        let line = window.text_system().shape_line(shown.clone().into(), size, &[run], None);
        let b = line.closest_index_for_x(x - x0);
        Some(shown.get(..b).map_or(0, |s| s.chars().count()))
    }

    fn text_mouse_down(&mut self, f: &Field, secret: bool, e: &MouseDownEvent, window: &mut Window) {
        let len = self.field_text(f).map_or(0, |t| t.chars().count());
        let at = self.index_at(f, secret, e.position.x, window).unwrap_or(len);
        let same = self.editing.as_ref() == Some(f);
        let (pos, anchor) = self.cursor_in(f, len);
        let (pos, anchor) = match e.click_count {
            3.. => (len, 0),
            2 => {
                let text = self.field_text(f).cloned().unwrap_or_default();
                let (lo, hi) = if secret { (0, len) } else { word_at(&text, at) };
                (hi, lo)
            }
            _ if e.modifiers.shift && same => (at, if pos == anchor { pos } else { anchor }),
            _ => (at, at),
        };
        self.editing = Some(f.clone());
        self.caret = Some(Cursor { field: f.clone(), pos, anchor });
        self.text_drag = e.click_count == 1;
        self.menu = None;
    }

    // ---------------------------------------------------------------- render

    /// Right-click menu. Cut/Copy take the selection, or the whole text when nothing is selected.
    pub fn field_menu(&self, f: &Field, secret: bool) -> Vec<Item> {
        let mut items = vec![];
        if !secret {
            let (a, b) = (f.clone(), f.clone());
            items.push(
                ui("Cut", move |p, _, cx| {
                    p.select_all_if_none(&a);
                    p.copy_selection(&a, true, cx);
                })
                .hint("Ctrl+X"),
            );
            items.push(
                ui("Copy", move |p, _, cx| {
                    p.select_all_if_none(&b);
                    p.copy_selection(&b, false, cx);
                })
                .hint("Ctrl+C"),
            );
        }
        let (c, d, e) = (f.clone(), f.clone(), f.clone());
        items.push(ui("Paste", move |p, _, cx| p.paste_into(&c, cx)).hint("Ctrl+V"));
        items.push(Item::Sep);
        items.push(ui("Select All", move |p, _, _| { p.field_edit(&d, "all", false, None); }).hint("Ctrl+A"));
        items.push(ui("Clear", move |p, _, _| {
            if let Some(t) = p.field_text(&e) {
                t.clear();
            }
        }));
        items
    }

    fn select_all_if_none(&mut self, f: &Field) {
        let len = self.field_text(f).map_or(0, |t| t.chars().count());
        let (pos, anchor) = self.cursor_in(f, len);
        if pos == anchor {
            self.field_edit(f, "all", false, None);
        }
    }

    /// Single-line text box. Keys are handled in Player::on_key.
    pub fn text_box(&self, f: Field, value: &str, placeholder: &str, secret: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let focused = self.editing.as_ref() == Some(&f);
        let shown = if secret { "•".repeat(value.chars().count()) } else { value.to_string() };
        let len = shown.chars().count();
        let (pos, anchor) = self.cursor_in(&f, len);
        let (lo, hi) = range(pos, anchor).filter(|_| focused).unwrap_or((pos, pos));
        let part = |a: usize, b: usize| shown.chars().skip(a).take(b - a).collect::<String>();
        let key = format!("{f:?}");

        // While painting: record where the text starts and its (inherited) font, so clicks map to
        // characters; while this box is being drag-selected, follow the mouse anywhere in the window.
        let probe = {
            let (geo, key, field) = (self.text_geo.clone(), key.clone(), f.clone());
            let dragging = focused && self.text_drag;
            let this = cx.entity().downgrade();
            gpui::canvas(
                move |bounds, window, _| {
                    let style = window.text_style();
                    geo.borrow_mut().insert(key, (bounds.origin.x, style.font(), style.font_size.to_pixels(window.rem_size())));
                },
                move |_, _, window, _| {
                    if !dragging {
                        return;
                    }
                    let (this2, field2) = (this.clone(), field.clone());
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                        if phase != gpui::DispatchPhase::Bubble || e.pressed_button != Some(MouseButton::Left) {
                            return;
                        }
                        this2.update(cx, |p, cx| {
                            if let Some(at) = p.index_at(&field2, secret, e.position.x, window)
                                && let Some(c) = p.caret.as_mut().filter(|c| c.field == field2) {
                                    c.pos = at;
                                    cx.notify();
                                }
                        })
                        .ok();
                    });
                    let this3 = this.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                        if phase == gpui::DispatchPhase::Bubble {
                            this3.update(cx, |p, _| p.text_drag = false).ok();
                        }
                    });
                },
            )
            .absolute()
            .inset_0()
        };

        let (fm, fr) = (f.clone(), f.clone());
        let caret_bar = || div().w_0().h(px(16.)).relative().child(div().absolute().left(px(-0.75)).w(px(1.5)).h(px(16.)).bg(t().accent()));
        let text = div()
            .relative()
            .flex()
            .items_center()
            .flex_shrink_0()
            .whitespace_nowrap()
            .when(!focused, |d| d.child(shown.clone()))
            .when(focused, |d| {
                // The caret is absolutely placed, so it never shifts the text (click mapping stays exact).
                d.child(part(0, lo))
                    .when(pos == lo, |d| d.child(caret_bar()))
                    .when(hi > lo, |d| d.child(div().bg(t().accent_a(0x55)).rounded_sm().child(part(lo, hi))))
                    .when(pos == hi && hi > lo, |d| d.child(caret_bar()))
                    .child(part(hi, len))
            })
            .child(probe);

        div()
            .id(SharedString::from(format!("field-{key}")))
            .flex()
            .items_center()
            .h(px(32.))
            .w_full()
            .min_w_0() // long text must not widen the box (or its popup) past the parent
            .px_2()
            .rounded_md()
            .overflow_hidden()
            // Short text sits left (the spacer below eats free space); text longer than the box
            // overflows to the left instead, so the end stays visible.
            .justify_end()
            .bg(t().hover())
            .border_1()
            .border_color(if focused { t().accent() } else { t().field() })
            .cursor_text()
            // Mouse-down, not on_click: stopping propagation (so the click never reaches the video)
            // would also cancel this element's own on_click.
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.text_mouse_down(&fm, secret, e, window);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation(); // not the player menu
                this.editing = Some(fr.clone());
                this.menu = Some(crate::menu::MenuState { origin: e.position, open: vec![], field: Some((fr.clone(), secret)) });
            }))
            .when(shown.is_empty() && !focused, |d| d.child(div().flex_shrink_0().text_color(t().dim()).whitespace_nowrap().child(placeholder.to_string())))
            .when(!shown.is_empty() || focused, |d| d.child(text))
            .child(div().flex_1())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, pos: usize, anchor: usize, steps: &[(&str, bool, Option<&str>)]) -> (String, usize, usize) {
        let (mut t, mut p, mut a) = (text.to_string(), pos, anchor);
        for (k, shift, ins) in steps {
            apply(&mut t, &mut p, &mut a, k, *shift, *ins);
        }
        (t, p, a)
    }

    #[test]
    fn caret_and_selection() {
        // Caret moves and inserts mid-text (multi-byte chars).
        assert_eq!(run("héllo", 5, 5, &[("left", false, None), ("left", false, None), ("", false, Some("<>"))]), ("hél<>lo".into(), 5, 5));
        // Shift+arrows select; typing replaces the selection.
        assert_eq!(run("abcdef", 1, 1, &[("right", true, None), ("right", true, None), ("", false, Some("X"))]), ("aXdef".into(), 2, 2));
        // Backspace deletes the selection only.
        assert_eq!(run("abcdef", 4, 1, &[("backspace", false, None)]), ("aef".into(), 1, 1));
        // Plain arrow collapses a selection to that side.
        assert_eq!(run("abcdef", 4, 1, &[("left", false, None)]), ("abcdef".into(), 1, 1));
        assert_eq!(run("abcdef", 1, 4, &[("right", false, None)]), ("abcdef".into(), 4, 4));
        // Select all, Shift+Home, edges.
        assert_eq!(run("abc", 1, 1, &[("all", false, None)]), ("abc".into(), 3, 0));
        assert_eq!(run("abc", 3, 3, &[("home", true, None)]), ("abc".into(), 0, 3));
        assert_eq!(run("abc", 0, 0, &[("backspace", false, None), ("left", false, None)]), ("abc".into(), 0, 0));
        assert_eq!(selected("abcdef", 4, 1).as_deref(), Some("bcd"));
        assert_eq!(selected("abcdef", 2, 2), None);
        let mut t = String::from("x");
        assert!(!apply(&mut t, &mut 0, &mut 0, "f5", false, None));
    }

    #[test]
    fn word_selection() {
        let s = "https://cdn.example.com/a_b.m3u8";
        assert_eq!(word_at(s, 10), (8, 11)); // "cdn"
        assert_eq!(word_at(s, 25), (24, 27)); // "a_b"
        assert_eq!(word_at(s, 5), (5, 6)); // ':' alone
        assert_eq!(word_at("", 0), (0, 0));
    }
}
