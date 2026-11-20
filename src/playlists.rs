//! Playlists manager: the queue ("Now Playing") plus saved playlists, one .m3u per playlist in
//! <config>/zym-player/playlists/ (VLC/mpv open them too; mpv expands .m3u natively, so playing one
//! is just `loadfile`). A large panel like Explorer/Settings; the small playlist panel stays for
//! quick queue use while watching.

use std::path::{Path, PathBuf};

use gpui::{Context, Div, SharedString, Stateful, div, prelude::*, px};

use crate::panel::{button, confirm_button, panel_header, row_icon, sidebar, sidebar_close, sidebar_item};
use crate::textbox::Field;
use crate::theme::t;
use crate::{Panel, Player, chip};

/// (title, file) — title may be empty.
pub type Entry = (String, String);

// ------------------------------------------------------------------ files

pub fn dir() -> PathBuf {
    crate::config_dir().join("playlists")
}

/// Names without the .m3u extension, sorted case-insensitively.
fn list_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("m3u")))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    out.sort_by_key(|n| n.to_lowercase());
    out
}

/// A name becomes a file name: no path separators, nothing hidden, not empty.
fn check_name(name: &str) -> Result<&str, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("Type a name first.".into());
    }
    if n.starts_with('.') || n.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
        return Err("Names can't start with '.' or contain / \\ : * ? \" < > |".into());
    }
    Ok(n)
}

/// Write `entries` as an extended M3U (overwrites).
fn save_in(dir: &Path, name: &str, entries: &[Entry]) -> Result<(), String> {
    let name = check_name(name)?;
    let mut text = String::from("#EXTM3U\n");
    for (title, file) in entries {
        if !title.is_empty() {
            text.push_str(&format!("#EXTINF:-1,{}\n", title.replace('\n', " ")));
        }
        text.push_str(file);
        text.push('\n');
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{name}.m3u")), text).map_err(|e| e.to_string())
}

/// Entries of an .m3u: `#EXTINF:<dur>,<title>` names the next line; other `#` lines are skipped.
fn parse_m3u(text: &str) -> Vec<Entry> {
    let mut out = vec![];
    let mut title = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            title = info.split_once(',').map(|(_, t)| t.trim().to_string()).unwrap_or_default();
        } else if !line.starts_with('#') {
            out.push((std::mem::take(&mut title), line.to_string()));
        }
    }
    out
}

fn read_in(dir: &Path, name: &str) -> Vec<Entry> {
    std::fs::read_to_string(dir.join(format!("{name}.m3u"))).map(|t| parse_m3u(&t)).unwrap_or_default()
}

fn rename_in(dir: &Path, old: &str, new: &str) -> Result<String, String> {
    let new = check_name(new)?.to_string();
    if new == old {
        return Ok(new);
    }
    let to = dir.join(format!("{new}.m3u"));
    if to.exists() {
        return Err(format!("A playlist called “{new}” already exists."));
    }
    std::fs::rename(dir.join(format!("{old}.m3u")), to).map_err(|e| e.to_string())?;
    Ok(new)
}

fn path(name: &str) -> PathBuf {
    dir().join(format!("{name}.m3u"))
}

/// Local paths must be absolute in a saved file; URLs stay as they are.
fn absolute(file: &str) -> String {
    if file.contains("://") || Path::new(file).is_absolute() {
        return file.to_string();
    }
    std::fs::canonicalize(file).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| file.to_string())
}

pub fn display_name((title, file): &Entry) -> String {
    if !title.is_empty() {
        return title.clone();
    }
    Path::new(file).file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|| file.clone())
}

// ------------------------------------------------------------------ state

#[derive(Clone, PartialEq, Default)]
pub enum Sel {
    #[default]
    Queue,
    Saved(String),
}

#[derive(Default)]
pub struct PlaylistUi {
    pub open: bool,
    sel: Sel,
    /// Entries of the selected saved playlist (re-read after every change).
    items: Vec<Entry>,
    /// Saved playlist names (folder snapshot).
    names: Vec<String>,
    /// "Save queue as" box on Now Playing.
    save_as: String,
    /// "New playlist" box in the sidebar.
    new_name: String,
    /// Rename draft for the selected saved playlist.
    renaming: Option<String>,
    /// "Add a URL" box on a saved playlist.
    add_url: String,
    confirm_delete: bool,
    /// (message, is_error)
    notice: Option<(String, bool)>,
}

impl PlaylistUi {
    pub fn field_text(&mut self, f: &Field) -> Option<&mut String> {
        match f {
            Field::PlSaveAs => Some(&mut self.save_as),
            Field::PlRename => self.renaming.as_mut(),
            Field::PlNewName => Some(&mut self.new_name),
            Field::PlAddUrl => Some(&mut self.add_url),
            _ => None,
        }
    }
}

impl Player {
    /// Enter in one of the Playlists manager's text fields.
    pub fn submit_playlist_field(&mut self, f: &Field) {
        match f {
            Field::PlSaveAs => self.save_queue_as(),
            Field::PlRename => self.finish_rename(),
            Field::PlNewName => self.create_playlist(),
            _ => self.add_url_to_saved(),
        }
    }

    /// Move queue entry `i` one step (up = towards the top). mpv's playlist-move puts the item
    /// *before* the target index, so moving down targets two past it.
    pub fn queue_move(&mut self, i: usize, up: bool) {
        let len = self.mpv.playlist_entries().len();
        if let Some(j) = move_target(i, up, len) {
            let to = if up { j } else { j + 1 };
            self.mpv.cmd(&["playlist-move", &i.to_string(), &to.to_string()]);
        }
    }

    fn saved_move(&mut self, i: usize, up: bool) {
        let len = self.pl.items.len();
        if let Some(j) = move_target(i, up, len) {
            self.edit_saved(|v| v.swap(i, j));
        }
    }

    pub fn open_playlists(&mut self) {
        self.close_overlays();
        self.pl.open = true;
        self.pl.names = list_in(&dir());
        if let Sel::Saved(n) = &self.pl.sel {
            if !self.pl.names.contains(n) {
                self.pl.sel = Sel::Queue;
            }
        }
        self.select(self.pl.sel.clone());
    }

    /// After loading a saved playlist: close the manager and show it in the small playlist panel.
    fn back_to_queue(&mut self) {
        self.close_overlays();
        self.panel = Panel::Playlist;
    }

    pub fn close_playlists(&mut self) {
        self.pl.open = false;
        self.editing = None;
    }

    fn select(&mut self, sel: Sel) {
        let pl = &mut self.pl;
        pl.items = match &sel {
            Sel::Saved(n) => read_in(&dir(), n),
            Sel::Queue => vec![],
        };
        pl.sel = sel;
        pl.renaming = None;
        pl.confirm_delete = false;
        pl.notice = None;
        pl.add_url.clear();
        self.editing = None;
    }

    fn notice(&mut self, r: Result<String, String>) {
        self.pl.notice = Some(match r {
            Ok(m) => (m, false),
            Err(e) => (e, true),
        });
    }

    fn queue_entries(&self) -> Vec<Entry> {
        self.mpv.playlist_entries().into_iter().map(|(file, title)| (title, absolute(&file))).collect()
    }

    /// "Save queue as": write the queue under that name (replaces a playlist of the same name).
    pub fn save_queue_as(&mut self) {
        let name = self.pl.save_as.trim().to_string();
        let entries = self.queue_entries();
        if entries.is_empty() {
            return self.notice(Err("The queue is empty: add something first.".into()));
        }
        match save_in(&dir(), &name, &entries) {
            Ok(()) => {
                self.pl.save_as.clear();
                self.pl.names = list_in(&dir());
                self.select(Sel::Saved(name.clone()));
                self.notice(Ok(format!("Saved the queue as “{name}” ({} items).", entries.len())));
            }
            Err(e) => self.notice(Err(e)),
        }
    }

    /// "New playlist": an empty playlist, opened for adding.
    pub fn create_playlist(&mut self) {
        let name = self.pl.new_name.trim().to_string();
        if self.pl.names.contains(&name) {
            return self.notice(Err(format!("“{name}” already exists.")));
        }
        match save_in(&dir(), &name, &[]) {
            Ok(()) => {
                self.pl.new_name.clear();
                self.pl.names = list_in(&dir());
                self.select(Sel::Saved(name.clone()));
                self.notice(Ok(format!("Created “{name}”. Add files, a URL or the queue below.")));
            }
            Err(e) => self.notice(Err(e)),
        }
    }

    pub fn finish_rename(&mut self) {
        let (Sel::Saved(old), Some(new)) = (self.pl.sel.clone(), self.pl.renaming.clone()) else { return };
        match rename_in(&dir(), &old, &new) {
            Ok(new) => {
                self.pl.names = list_in(&dir());
                self.select(Sel::Saved(new));
            }
            Err(e) => self.notice(Err(e)),
        }
    }

    /// Change the selected saved playlist's entries and write it back.
    fn edit_saved(&mut self, f: impl FnOnce(&mut Vec<Entry>)) {
        let Sel::Saved(name) = self.pl.sel.clone() else { return };
        f(&mut self.pl.items);
        if let Err(e) = save_in(&dir(), &name, &self.pl.items) {
            self.notice(Err(e));
        }
        self.pl.items = read_in(&dir(), &name);
    }

    pub fn add_url_to_saved(&mut self) {
        let url = self.pl.add_url.trim().to_string();
        if url.is_empty() {
            return;
        }
        self.edit_saved(|v| v.push((String::new(), absolute(&url))));
        self.pl.add_url.clear();
    }

    fn add_files_to_saved(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            this.update(cx, |p, cx| {
                p.edit_saved(|v| v.extend(paths.iter().map(|x| (String::new(), x.to_string_lossy().into_owned()))));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---------------------------------------------------------------- render

    pub fn render_playlists(&self, window: &gpui::Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let pl = &self.pl;
        let count = self.mpv.get_i64("playlist-count").unwrap_or(0);
        let side = sidebar("PLAYLISTS")
            .child(
                sidebar_item("pls-queue", pl.sel == Sel::Queue)
                    .child("Now Playing")
                    .child(div().text_xs().text_color(t().dim()).child(items_label(count as usize)))
                    .on_click(cx.listener(|this, _, _, _| this.select(Sel::Queue))),
            )
            .child(div().h(px(1.)).mx_3().my_2().bg(t().border()))
            .child(
                div().id("pls-names").flex_1().overflow_y_scroll().children(pl.names.iter().map(|name| {
                    let n = name.clone();
                    sidebar_item(SharedString::from(format!("pls-n-{name}")), pl.sel == Sel::Saved(name.clone()))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(name.clone())
                        .on_click(cx.listener(move |this, _, _, _| this.select(Sel::Saved(n.clone()))))
                })),
            )
            .child(div().px_2().pt_2().child(self.text_box(Field::PlNewName, &pl.new_name, "New playlist…  (Enter)", false, cx)))
            .child(sidebar_close("pls-close").on_click(cx.listener(|this, _, _, _| this.close_playlists())));

        let main = match &pl.sel {
            Sel::Queue => self.queue_page(count, cx),
            Sel::Saved(name) => self.saved_page(name, cx),
        };
        self.big_panel("playlists", window, cx).child(side).child(main)
    }


    fn notice_line(&self) -> Option<Div> {
        self.pl.notice.as_ref().map(|(msg, err)| {
            div().px_4().pt_2().text_xs().text_color(if *err { t().error() } else { t().muted() }).child(msg.clone())
        })
    }

    fn queue_page(&self, count: i64, cx: &mut Context<Self>) -> Div {
        let cur = self.mpv.get_i64("playlist-pos").unwrap_or(-1);
        let rows = self.mpv.playlist_entries().into_iter().enumerate().map(|(i, (file, title))| {
            item_row(format!("q-{i}"), i, &display_name(&(title, file)), i as i64 == cur)
                .on_click(cx.listener(move |this, _, _, _| this.mpv.cmd(&["playlist-play-index", &i.to_string()])))
                .children(move_buttons("q", i, count as usize, cx, Player::queue_move))
                .child(row_icon(format!("q-rm-{i}"), "trash").on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.mpv.cmd(&["playlist-remove", &i.to_string()]);
                })))
        }).collect::<Vec<_>>();

        let actions = div()
            .when(count > 1, |d| d.child(chip("q-shuffle", "Shuffle", false).on_click(cx.listener(|this, _, _, _| this.mpv.cmd(&["playlist-shuffle"])))))
            .when(count > 0, |d| d.child(chip("q-clear", "Clear", false).on_click(cx.listener(|this, _, _, _| this.stop()))));

        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(panel_header("Now Playing", actions))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .px_4()
                    .pt_3()
                    .child(div().flex_1().min_w_0().child(self.text_box(Field::PlSaveAs, &self.pl.save_as, "Save queue as…", false, cx)))
                    .child(button("q-save", "Save", !self.pl.save_as.trim().is_empty() && count > 0).on_click(cx.listener(|this, _, _, _| this.save_queue_as()))),
            )
            .children(self.notice_line())
            .child(
                div()
                    .id("q-items")
                    .flex_1()
                    .overflow_y_scroll()
                    .py_2()
                    .children(rows)
                    .when(count == 0, |d| d.child(empty("The queue is empty. Open files, drop them on the window, or play a saved playlist."))),
            )
    }

    fn saved_page(&self, name: &str, cx: &mut Context<Self>) -> Div {
        let pl = &self.pl;
        let n = pl.items.len();
        let rows = pl.items.iter().enumerate().map(|(i, e)| {
            item_row(format!("s-{i}"), i, &display_name(e), false)
                .on_click(cx.listener(move |this, _, _, _| {
                    if let Sel::Saved(name) = this.pl.sel.clone() {
                        this.mpv.load_from(&path(&name).to_string_lossy(), i as i64);
                        this.back_to_queue();
                    }
                }))
                .children(move_buttons("s", i, n, cx, Player::saved_move))
                .child(row_icon(format!("s-rm-{i}"), "trash").on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.edit_saved(|v| {
                        v.remove(i);
                    });
                })))
        }).collect::<Vec<_>>();

        let title: Div = match &pl.renaming {
            Some(draft) => div()
                .flex()
                .gap_1()
                .child(div().w(px(260.)).child(self.text_box(Field::PlRename, draft, "New name", false, cx)))
                .child(button("s-rn-ok", "OK", true).on_click(cx.listener(|this, _, _, _| this.finish_rename()))),
            None => div().child(name.to_string()),
        };
        let actions = div()
            .child(chip("s-load", "Load", true).on_click(cx.listener(move |this, _, _, _| {
                if let Sel::Saved(name) = this.pl.sel.clone() {
                    this.mpv.load(&path(&name).to_string_lossy(), "replace");
                    this.back_to_queue();
                }
            })))
            .child(chip("s-queue", "Add to queue", false).on_click(cx.listener(|this, _, _, _| {
                if let Sel::Saved(name) = this.pl.sel.clone() {
                    this.mpv.load(&path(&name).to_string_lossy(), "append-play");
                    this.notice(Ok(format!("Added “{name}” to the queue.")));
                }
            })))
            .child(row_icon("s-rename".into(), "edit").on_click(cx.listener(|this, _, _, _| {
                if let Sel::Saved(name) = this.pl.sel.clone() {
                    this.pl.renaming = Some(name);
                    this.editing = Some(Field::PlRename);
                }
            })))
            .child(if pl.confirm_delete {
                confirm_button("s-del-ok", "Delete playlist?")
                    .on_click(cx.listener(|this, _, _, _| {
                        if let Sel::Saved(name) = this.pl.sel.clone() {
                            let r = std::fs::remove_file(path(&name)).map(|_| format!("Deleted “{name}”.")).map_err(|e| e.to_string());
                            this.pl.names = list_in(&dir());
                            this.select(Sel::Queue);
                            this.notice(r);
                        }
                    }))
            } else {
                row_icon("s-del".into(), "trash").on_click(cx.listener(|this, _, _, _| this.pl.confirm_delete = true))
            });

        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(panel_header(title, actions))
            .children(self.notice_line())
            .child(
                div()
                    .id("s-items")
                    .flex_1()
                    .overflow_y_scroll()
                    .py_2()
                    .children(rows)
                    .when(n == 0, |d| d.child(empty("This playlist is empty. Add files, a URL, or the current queue below."))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(t().hover())
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(div().flex_1().min_w_0().child(self.text_box(Field::PlAddUrl, &pl.add_url, "Add a URL…  (Enter)", false, cx)))
                            .child(button("s-add-url", "Add", !pl.add_url.trim().is_empty()).on_click(cx.listener(|this, _, _, _| this.add_url_to_saved()))),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(chip("s-add-files", "Add files…", false).on_click(cx.listener(|this, _, _, cx| this.add_files_to_saved(cx))))
                            .child(chip("s-add-queue", "Add current queue", false).on_click(cx.listener(|this, _, _, _| {
                                let q = this.queue_entries();
                                let k = q.len();
                                this.edit_saved(|v| v.extend(q));
                                this.notice(Ok(format!("Added {k} items from the queue.")));
                            }))),
                    ),
            )
    }
}

fn items_label(n: usize) -> String {
    match n {
        1 => "1 item".into(),
        n => format!("{n} items"),
    }
}

fn empty(text: &str) -> Div {
    div().px_4().py_3().text_color(t().muted()).child(text.to_string())
}


/// "  3  Title ............ [actions]" — actions show on hover.
fn item_row(id: String, i: usize, name: &str, current: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .group("plrow")
        .flex()
        .items_center()
        .gap_1()
        .mx_2()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(t().hover()))
        .when(current, |d| d.bg(t().accent_a(0x30)).text_color(t().accent()))
        .child(div().w(px(28.)).flex_none().text_color(t().dim()).child(format!("{}", i + 1)))
        .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(name.to_string()))
}

/// Index to swap row `i` with when moving it one step, or None at the edge.
pub fn move_target(i: usize, up: bool, len: usize) -> Option<usize> {
    if up { i.checked_sub(1) } else { (i + 1 < len).then_some(i + 1) }
}

/// ↑ and ↓ for row `i` of `len`, shown on row hover (group "plrow"). `on_move(player, i, up)`
/// is only attached where a move exists, so an edge row's hidden arrow does nothing.
pub fn move_buttons(prefix: &str, i: usize, len: usize, cx: &mut Context<Player>, on_move: fn(&mut Player, usize, bool)) -> [Stateful<Div>; 2] {
    [(true, "↑"), (false, "↓")].map(|(up, arrow)| {
        let btn = div()
            .id(SharedString::from(format!("{prefix}-mv-{arrow}-{i}")))
            .size(px(28.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_color(t().muted())
            .invisible()
            .child(arrow);
        if move_target(i, up, len).is_none() {
            return btn;
        }
        btn.group_hover("plrow", |s| s.visible())
            .cursor_pointer()
            .hover(|s| s.bg(t().pressed()))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation(); // not the row's own click
                on_move(this, i, up);
            }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_read_rename() {
        let d = std::env::temp_dir().join(format!("zym-pl-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let items: Vec<Entry> = vec![("Song".into(), "/music/a.mp3".into()), (String::new(), "https://x/b.m3u8".into())];

        assert!(save_in(&d, "", &items).is_err(), "empty name");
        assert!(save_in(&d, "../evil", &items).is_err(), "no path separators");
        save_in(&d, "Mix", &items).unwrap();
        save_in(&d, "abc", &[]).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("Mix.m3u")).unwrap(), "#EXTM3U\n#EXTINF:-1,Song\n/music/a.mp3\nhttps://x/b.m3u8\n");
        assert_eq!(read_in(&d, "Mix"), items, "round trip");
        assert!(read_in(&d, "abc").is_empty());
        assert_eq!(list_in(&d), ["abc", "Mix"], "case-insensitive order");

        assert!(rename_in(&d, "Mix", "abc").is_err(), "name taken");
        assert_eq!(rename_in(&d, "Mix", " Party ").unwrap(), "Party");
        assert_eq!(list_in(&d), ["abc", "Party"]);
        std::fs::remove_dir_all(&d).unwrap();

        // Other players' m3u: #EXTINF with duration, comments, blank lines, CRLF.
        let ext = parse_m3u("#EXTM3U\r\n#EXTINF:123,Artist - Track\r\n\r\nsong.mp3\r\n# comment\r\nhttp://radio\r\n");
        assert_eq!(ext, [("Artist - Track".to_string(), "song.mp3".to_string()), (String::new(), "http://radio".to_string())]);
        assert_eq!(display_name(&(String::new(), "/a/b/c.mkv".into())), "c.mkv");
        assert_eq!((move_target(0, true, 3), move_target(0, false, 3)), (None, Some(1)), "top row only moves down");
        assert_eq!((move_target(2, true, 3), move_target(2, false, 3)), (Some(1), None), "bottom row only moves up");
    }
}
