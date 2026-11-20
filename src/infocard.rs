//! "What's playing" card: the ⓘ button at the top right of the video opens details a content
//! plugin gave for the current item (Playback.info, or built from the item it was played from).

use gpui::{Context, Div, MouseButton, Stateful, Window, div, img, prelude::*, px, rgb, rgba};

use crate::sources::Info;
use crate::theme::t;
use crate::{Player, icon_button};

impl Player {
    /// Card for the current playlist entry, if it came from a content plugin.
    pub fn current_info(&self) -> Option<&Info> {
        // Finished or stopped: like the title, the card goes with the item.
        if self.mpv.get_flag("eof-reached") || self.mpv.get_flag("idle-active") {
            return None;
        }
        let pos = self.mpv.get_i64("playlist-pos")?;
        let (file, _) = self.mpv.playlist_entries().into_iter().nth(usize::try_from(pos).ok()?)?;
        self.infos.get(&file)
    }

    /// ⓘ in the top bar; None when there's nothing to show.
    pub fn info_button(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        self.current_info()?;
        Some(
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
                })),
        )
    }

    pub fn render_info_card(&self, window: &Window, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let info = self.current_info()?;
        let max_h = window.viewport_size().height - px(56. + crate::PANEL_V);
        let header = div()
            .flex()
            .gap_4()
            .children(info.poster.clone().map(|u| {
                div().w(px(96.)).h(px(144.)).flex_none().rounded_md().overflow_hidden().bg(t().field()).child(img(u).size_full().object_fit(gpui::ObjectFit::Cover))
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(info.title.clone()))
                    .children(info.subtitle.clone().map(|s| div().text_color(t().muted()).child(s)))
                    .when(!info.badges.is_empty(), |d| {
                        d.child(div().flex().flex_wrap().gap_1().pt_1().children(info.badges.iter().map(|b| {
                            div().px_2().py_0p5().rounded_md().text_xs().bg(t().accent_a(0x30)).text_color(t().accent()).child(b.clone())
                        })))
                    }),
            );
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
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()) // never reach the video
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(header)
                .children(info.description.clone().map(|d| div().text_color(t().fg()).line_height(px(20.)).child(d)))
                .when(!info.details.is_empty(), |d| {
                    d.child(div().flex().flex_col().gap_1().pt_1().border_t_1().border_color(t().border()).children(info.details.iter().map(|x| {
                        div()
                            .flex()
                            .gap_3()
                            .pt_1()
                            .child(div().w(px(90.)).flex_none().text_color(t().muted()).child(x.label.clone()))
                            .child(div().flex_1().min_w_0().child(x.value.clone()))
                    })))
                })
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .child(crate::chip("info-close", "Close", false).on_click(cx.listener(|this, _, _, _| this.info_open = false))),
                ),
        )
    }
}
