//! Shared panel chrome for the large panels (Explorer, Settings, Playlists) and the cards/rows in
//! them: the resizable panel frame, sidebar, header, buttons. One place, so every panel matches.

use gpui::{Context, Div, MouseButton, SharedString, Stateful, Window, div, prelude::*, px};

use crate::theme::t;
use crate::{PANEL_GAP, PANEL_V, Player, chip, icon};

/// Narrowest a big panel can be dragged: sidebar + a usable content column.
const MIN_PANEL_W: f32 = 420.;

/// `frac` of the window, at least `min` and at most `max`, but never wider than the window minus
/// the side gaps (CSS min-width would win over max-width and push the panel off-screen).
pub fn panel_width(window: &Window, frac: f32, min: f32, max: f32) -> gpui::Pixels {
    px(fit_width(f32::from(window.viewport_size().width), frac, min, max))
}

fn fit_width(window_w: f32, frac: f32, min: f32, max: f32) -> f32 {
    let room = (window_w - 2. * PANEL_GAP).max(0.);
    (room * frac).clamp(min, max).min(room)
}

/// A dragged width kept inside the window: at least MIN_PANEL_W (if it fits), at most the room.
fn clamp_dragged(w: f32, window_w: f32) -> f32 {
    let room = (window_w - 2. * PANEL_GAP).max(0.);
    w.max(MIN_PANEL_W).min(room)
}

impl Player {
    fn big_panel_width(&self, id: &str, window: &Window) -> f32 {
        let vw = f32::from(window.viewport_size().width);
        match self.panel_w.get(id).copied() {
            Some(w) => clamp_dragged(w, vw),
            None => fit_width(vw, 0.70, 560., 1040.),
        }
    }

    /// Mouse moved while dragging a big panel's left edge (from the root mouse-move handler).
    /// Stored already clamped, so the saved width is one that fit the window it was set in.
    pub fn drag_panel_edge(&mut self, id: &str, x: f32, window: &Window) {
        let vw = f32::from(window.viewport_size().width);
        self.panel_w.insert(id.to_string(), clamp_dragged(vw - PANEL_GAP - x, vw));
    }

    pub fn end_panel_drag(&mut self) {
        if self.panel_drag.take().is_some() {
            crate::settings::put("panel_widths", serde_json::json!(self.panel_w));
        }
    }

    /// Large translucent side panel: video stays visible, controls stay usable. Drag its left
    /// edge (grip) to resize; each panel remembers its own width (Explorer wide, Playlists
    /// narrow, …). Double-click the edge to reset that panel.
    pub fn big_panel(&self, id: &'static str, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let dragging = self.panel_drag == Some(id);
        let edge = div()
            .id("panel-edge")
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(10.))
            .flex()
            .items_center()
            .justify_center()
            .cursor(gpui::CursorStyle::ResizeLeftRight)
            .group("panel-edge")
            // Highlight follows the panel's rounded corners (rounded_xl = 12 px, minus the 1 px
            // border). GPUI caps a radius at half the shape's width, so a 10 px strip can't take
            // 11 px: draw it 22 px wide and let this 10 px edge clip off the right part.
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(22.))
                    .rounded_l(px(11.))
                    .when(dragging, |d| d.bg(t().accent_a(0x40)))
                    .group_hover("panel-edge", |s| s.bg(t().accent_a(0x40))),
            )
            // Grip: always visible, so the edge reads as draggable.
            .child(
                div()
                    .w(px(4.))
                    .h(px(40.))
                    .rounded_full()
                    .bg(if dragging { t().accent() } else { t().dim() })
                    .group_hover("panel-edge", |s| s.bg(t().accent())),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.panel_drag = Some(id);
                if e.click_count >= 2 {
                    this.panel_w.remove(id); // back to the default width
                    this.end_panel_drag();
                }
            }));
        div()
            .id(id)
            .absolute()
            .top(px(48.))
            .right(px(PANEL_GAP))
            .bottom(px(PANEL_V))
            .w(px(self.big_panel_width(id, window)))
            .pl(px(10.)) // the edge's own strip: content starts after it, so its highlight covers nothing
            .flex()
            .rounded_xl()
            .overflow_hidden()
            .shadow_lg()
            .bg(t().surface())
            .border_1()
            .border_color(t().border())
            .text_color(t().fg())
            .text_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()) // never reach the video
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(edge)
    }
}

/// Left tab column with a small caps heading.
pub fn sidebar(heading: &'static str) -> Div {
    div()
        .w(px(170.))
        .h_full()
        .flex()
        .flex_col()
        .bg(t().hover())
        .border_r_1()
        .border_color(t().hover())
        .child(div().px_4().pt_4().pb_2().text_xs().text_color(t().muted()).child(heading))
}

pub fn sidebar_item(id: impl Into<gpui::ElementId>, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .mx_2()
        .px_3()
        .py_2()
        .rounded_md()
        .cursor_pointer()
        .hover(|st| st.bg(t().hover()))
        .when(active, |d| d.bg(t().accent_a(0x30)).text_color(t().accent()))
}

/// Close button at the bottom of a panel sidebar: a bordered button, not just text, so it's found.
pub fn sidebar_close(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .m_2()
        .px_3()
        .h(px(34.))
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .cursor_pointer()
        .bg(t().field())
        .border_1()
        .border_color(t().border())
        .text_color(t().fg())
        .font_weight(gpui::FontWeight::MEDIUM)
        .hover(|st| st.bg(t().pressed()).border_color(t().accent()))
        .child("✕")
        .child(div().flex_1().child("Close"))
        .child(div().text_xs().text_color(t().dim()).child("Esc"))
}

/// 56 px page header: title (ellipsised) on the left, `actions` on the right.
pub fn panel_header(title: impl IntoElement, actions: Div) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .h(px(56.))
        .flex_none()
        .px_4()
        .border_b_1()
        .border_color(t().hover())
        .child(div().flex_1().min_w_0().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
        .child(actions.flex().items_center().gap_1())
}

/// Chip sized to sit next to a 32 px text box.
pub fn button(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, active: bool) -> Stateful<Div> {
    chip(id, label, active).h(px(32.)).px_3().flex().items_center()
}

/// Red "second click confirms" button that replaces a row's trash icon (fixed bg, fixed text).
pub fn confirm_button(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .h(px(28.))
        .flex()
        .items_center()
        .rounded_md()
        .cursor_pointer()
        .bg(gpui::rgb(0xd93a3a))
        .text_color(gpui::rgb(0xffffff))
        .text_xs()
        .child(label.into())
}

/// 28 px icon button for list rows.
pub fn row_icon(id: String, name: &str) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .size(px(28.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(t().pressed()))
        .child(icon(name).size(px(16.)).text_color(t().muted()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn panels_fit_small_windows() {
        let gap = 2. * crate::PANEL_GAP;
        assert_eq!(super::fit_width(1920., 0.7, 560., 1040.), 1040., "capped on big screens");
        assert_eq!(super::fit_width(1000., 0.7, 560., 1040.), (1000. - gap) * 0.7);
        assert_eq!(super::fit_width(700., 0.7, 560., 1040.), 560., "min while it fits");
        assert_eq!(super::fit_width(400., 0.7, 560., 1040.), 400. - gap, "never wider than the window");
        assert_eq!(super::fit_width(300., 0., 320., 320.), 300. - gap, "playlist panel too");
    }

    #[test]
    fn dragged_width_stays_in_window() {
        let gap = 2. * crate::PANEL_GAP;
        assert_eq!(super::clamp_dragged(700., 1600.), 700.);
        assert_eq!(super::clamp_dragged(100., 1600.), super::MIN_PANEL_W, "not narrower than usable");
        assert_eq!(super::clamp_dragged(5000., 1600.), 1600. - gap, "not wider than the window");
        assert_eq!(super::clamp_dragged(700., 380.), 380. - gap, "small window wins over the minimum");
    }
}
