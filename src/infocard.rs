//! "What's playing" card: the ⓘ button at the top right of the video opens details of the current
//! item. Every item gets the file facts mpv knows (name, size, length, video/audio); items from a
//! content plugin also get its card (Playback.info, or built from the item they were played from).
//! The card fades with the player controls.

use gpui::{Context, Div, MouseButton, Stateful, Window, div, img, prelude::*, px, rgb, rgba};

use crate::sources::{Detail, Info};
use crate::theme::t;
use crate::{Player, fmt_time, icon_button};

/// Bytes as "734 KB", "1.4 GB".
fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut u) = (bytes as f64, 0);
    while v >= 1024. && u + 1 < UNITS.len() {
        v /= 1024.;
        u += 1;
    }
    if u == 0 || v >= 100. { format!("{v:.0} {}", UNITS[u]) } else { format!("{v:.1} {}", UNITS[u]) }
}

impl Player {
    /// Something is loaded and still playing (not stopped, not finished).
    fn has_media(&self) -> bool {
        !self.mpv.get_flag("idle-active") && !self.mpv.get_flag("eof-reached")
    }

    /// Plugin card for the current playlist entry, if it came from a content plugin.
    pub fn current_info(&self) -> Option<&Info> {
        if !self.has_media() {
            return None;
        }
        let pos = self.mpv.get_i64("playlist-pos")?;
        let (file, _) = self.mpv.playlist_entries().into_iter().nth(usize::try_from(pos).ok()?)?;
        self.infos.get(&file)
    }

    /// File facts from mpv (all observed, so reading them never waits on mpv). Missing ones are skipped.
    fn file_details(&self) -> Vec<Detail> {
        let m = &self.mpv;
        let s = |p: &str| m.get_string(p).filter(|v| !v.is_empty());
        let row = |label: &str, value: Option<String>| value.map(|value| Detail { label: label.into(), value });
        let length = match (m.get_f64("time-pos"), m.get_f64("duration")) {
            (Some(p), Some(d)) => Some(format!("{} / {}", fmt_time(p), fmt_time(d))),
            (None, Some(d)) => Some(fmt_time(d)),
            _ => None,
        };
        let video = match (m.get_i64("width"), m.get_i64("height"), s("video-codec")) {
            (Some(w), Some(h), c) => Some(format!("{w}×{h}{}", c.map(|c| format!(" · {}", short_codec(&c))).unwrap_or_default())),
            (_, _, c) => c.map(|c| short_codec(&c)),
        };
        [
            row("File", s("filename")),
            row("Size", m.get_i64("file-size").filter(|b| *b > 0).map(|b| fmt_size(b as u64))),
            row("Length", length),
            row("Video", video),
            row("Audio", s("audio-codec-name")),
            row("Format", format_name(s("filename").as_deref(), s("file-format").as_deref())),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// ⓘ in the top bar while something plays.
    pub fn info_button(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        self.has_media().then(|| {
            icon_button("info", "")
                .child(
                    div()
                        .size(px(20.))
                        .rounded_full()
                        .border_2()
                        .border_color(rgb(0xf0f0f0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("i"),
                )
                .when(self.info_open, |d| d.bg(rgba(0xffffff22)))
                .on_click(cx.listener(|this, _, _, _| {
                    let open = !this.info_open;
                    this.close_overlays();
                    this.info_open = open;
                }))
        })
    }

    /// The card, faded with the player controls (`alpha`); stays open across fades until closed.
    pub fn render_info_card(&self, alpha: f32, window: &Window, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if !self.has_media() {
            return None;
        }
        let info = self.current_info();
        let title = info.map(|i| i.title.clone()).unwrap_or_else(|| self.title.to_string());
        let max_h = window.viewport_size().height - px(56. + crate::PANEL_V);
        let header = div()
            .flex()
            .gap_4()
            .children(info.and_then(|i| i.poster.clone()).map(|u| {
                div().w(px(96.)).h(px(144.)).flex_none().rounded_md().overflow_hidden().bg(t().field()).child(img(u).size_full().object_fit(gpui::ObjectFit::Cover))
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                    .children(info.and_then(|i| i.subtitle.clone()).map(|s| div().text_color(t().muted()).child(s)))
                    .when(info.is_some_and(|i| !i.badges.is_empty()), |d| {
                        d.child(div().flex().flex_wrap().gap_1().pt_1().children(info.unwrap().badges.iter().map(|b| {
                            div().px_2().py_0p5().rounded_md().text_xs().bg(t().accent_a(0x30)).text_color(t().accent()).child(b.clone())
                        })))
                    }),
            );
        let details = |rows: Vec<Detail>| {
            div().flex().flex_col().gap_1().pt_1().border_t_1().border_color(t().border()).children(rows.into_iter().map(|x| {
                div()
                    .flex()
                    .gap_3()
                    .pt_1()
                    .child(div().w(px(70.)).flex_none().text_color(t().muted()).child(x.label))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().child(x.value))
            }))
        };
        let plugin_details = info.map(|i| i.details.clone()).unwrap_or_default();
        Some(
            div()
                .id("info-card")
                .absolute()
                .top(px(56.))
                .right(px(crate::PANEL_GAP))
                .w(crate::panel::panel_width(window, 0., 420., 420.))
                .max_h(max_h)
                .overflow_y_scroll()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .rounded_xl()
                .shadow_lg()
                .bg(t().surface())
                .border_1()
                .border_color(t().border())
                .text_color(t().fg())
                .text_sm()
                .opacity(alpha)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()) // never reach the video
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(header)
                .children(info.and_then(|i| i.description.clone()).map(|d| div().line_height(px(20.)).child(d)))
                .when(!plugin_details.is_empty(), |d| d.child(details(plugin_details)))
                .child(details(self.file_details()))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .child(crate::chip("info-close", "Close", false).on_click(cx.listener(|this, _, _, _| this.info_open = false))),
                ),
        )
    }
}

/// Container as people know it: the file's extension ("MP4"), else ffmpeg's first demuxer name
/// ("hls"); its raw list ("mov,mp4,m4a,3gp,3g2,mj2") reads as noise.
fn format_name(filename: Option<&str>, demuxer: Option<&str>) -> Option<String> {
    let ext = filename.and_then(|f| f.rsplit_once('.')).map(|(_, e)| e).filter(|e| (1..=5).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()));
    ext.map(str::to_uppercase).or_else(|| demuxer.and_then(|d| d.split(',').next()).map(str::to_string))
}

/// mpv's codec description ("h264 (H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10)") -> "H.264".
fn short_codec(c: &str) -> String {
    let inner = c.split_once('(').and_then(|(_, r)| r.split(['/', ')']).next()).map(str::trim).filter(|s| !s.is_empty());
    inner.unwrap_or(c.split_whitespace().next().unwrap_or(c)).to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes_and_codecs() {
        assert_eq!(super::fmt_size(512), "512 B");
        assert_eq!(super::fmt_size(1536), "1.5 KB");
        assert_eq!(super::fmt_size(4_113_136), "3.9 MB");
        assert_eq!(super::fmt_size(5 * 1024 * 1024 * 1024), "5.0 GB");
        assert_eq!(super::fmt_size(300 * 1024 * 1024), "300 MB");
        assert_eq!(super::short_codec("h264 (H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10)"), "H.264");
        assert_eq!(super::short_codec("vp9"), "vp9");
        assert_eq!(super::format_name(Some("movie.mkv"), Some("matroska,webm")).as_deref(), Some("MKV"));
        assert_eq!(super::format_name(Some("playlist.m3u8?token=1"), Some("hls")).as_deref(), Some("hls"), "query string isn't an extension");
        assert_eq!(super::format_name(Some("stream"), Some("mov,mp4,m4a")).as_deref(), Some("mov"));
    }
}
