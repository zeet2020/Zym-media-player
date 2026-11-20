//! Cascading context menu (right-click / ⋮ button), mpv/VLC style.
//! The menu tree is rebuilt every frame from live mpv state, so checkmarks are always current.

use std::rc::Rc;

use gpui::{
    Context, Div, MouseButton, Pixels, Point, SharedString, Window, div, point, prelude::*, px,
};

use crate::Player;
use crate::theme::t;

pub type Act = Rc<dyn Fn(&mut Player, &mut Window, &mut Context<Player>)>;

pub enum Item {
    Action { label: SharedString, hint: Option<SharedString>, check: Option<bool>, act: Act },
    Sub { label: SharedString, items: Vec<Item> },
    Sep,
}

impl Item {
    pub fn hint(mut self, h: &'static str) -> Self {
        if let Item::Action { hint, .. } = &mut self {
            *hint = Some(h.into());
        }
        self
    }

    /// Show a checkmark column; `true` draws the ✓.
    pub fn checked(mut self, on: bool) -> Self {
        if let Item::Action { check, .. } = &mut self {
            *check = Some(on);
        }
        self
    }

    fn height(&self) -> f32 {
        if matches!(self, Item::Sep) { SEP_H } else { ITEM_H }
    }
}

/// Action item running a UI closure.
pub fn ui(label: impl Into<SharedString>, f: impl Fn(&mut Player, &mut Window, &mut Context<Player>) + 'static) -> Item {
    Item::Action { label: label.into(), hint: None, check: None, act: Rc::new(f) }
}

/// Action item running an mpv command.
pub fn mpv(label: impl Into<SharedString>, cmd: &[&str]) -> Item {
    let cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
    ui(label, move |p, _, _| p.mpv.cmd(&cmd.iter().map(String::as_str).collect::<Vec<_>>()))
}

pub fn sub(label: impl Into<SharedString>, items: Vec<Item>) -> Item {
    Item::Sub { label: label.into(), items }
}

pub struct MenuState {
    pub origin: Point<Pixels>,
    /// open[d] = index of the submenu expanded at depth d.
    pub open: Vec<usize>,
    /// Some = text-box menu (cut/copy/paste) for this field; bool = secret (no cut/copy).
    pub field: Option<(crate::textbox::Field, bool)>,
}

const W: f32 = 240.;
const ITEM_H: f32 = 28.;
const SEP_H: f32 = 9.;
const PAD: f32 = 4.;

impl Player {
    pub fn open_menu(&mut self, at: Point<Pixels>) {
        self.menu = Some(MenuState { origin: at, open: vec![], field: None });
    }

    pub fn render_menu(&self, state: &MenuState, items: &[Item], window: &Window, cx: &mut Context<Self>) -> Div {
        let vw = f32::from(window.viewport_size().width);
        let vh = f32::from(window.viewport_size().height);
        let col_h = |items: &[Item]| items.iter().map(Item::height).sum::<f32>() + 2. * PAD;

        // Backdrop: click anywhere outside closes; right-click re-opens at the new spot.
        let mut root = div().absolute().inset_0().child(
            div()
                .id("menu-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.menu = None))
                .on_mouse_down(MouseButton::Right, cx.listener(|this, e: &gpui::MouseDownEvent, _, _| this.open_menu(e.position))),
        );

        // Walk the open path, laying out one column per depth.
        let mut col_items = items;
        let mut x = f32::from(state.origin.x).min(vw - W).max(0.);
        let mut y = f32::from(state.origin.y);
        for depth in 0..=state.open.len() {
            y = y.min(vh - col_h(col_items)).max(0.);
            root = root.child(self.render_column(depth, col_items, state.open.get(depth).copied(), point(px(x), px(y)), cx));

            let Some(&i) = state.open.get(depth) else { break };
            let Some(Item::Sub { items, .. }) = col_items.get(i) else { break };
            y += col_items[..i].iter().map(Item::height).sum::<f32>(); // submenu's first row lines up with its parent row
            x = if x + 2. * W <= vw { x + W - 4. } else { (x - W + 4.).max(0.) }; // flip left at the edge
            col_items = items;
        }
        root
    }

    fn render_column(&self, depth: usize, items: &[Item], open: Option<usize>, at: Point<Pixels>, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("menu-col-{depth}")))
            .absolute()
            .left(at.x)
            .top(at.y)
            .w(px(W))
            .py(px(PAD))
            .occlude()
            .rounded_lg()
            .border_1()
            .border_color(t().border())
            .bg(t().surface_solid())
            .shadow_lg()
            .text_sm()
            .children(items.iter().enumerate().map(|(i, item)| {
                let id = SharedString::from(format!("menu-{depth}-{i}"));
                let row = div()
                    .id(id)
                    .h(px(ITEM_H))
                    .mx_1()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(t().accent_a(0x40)))
                    .when(open == Some(i), |d| d.bg(t().border()));
                let is_sub = matches!(item, Item::Sub { .. });
                // Hovering a row collapses deeper columns and, for submenus, opens this one.
                let row = row.on_hover(cx.listener(move |this, hovered: &bool, _, _| {
                    if let (true, Some(m)) = (*hovered, this.menu.as_mut()) {
                        m.open.truncate(depth);
                        if is_sub {
                            m.open.push(i);
                        }
                    }
                }));
                match item {
                    Item::Sep => div().h(px(SEP_H)).flex().items_center().child(div().h(px(1.)).w_full().bg(t().border())).into_any_element(),
                    Item::Sub { label, .. } => row
                        .child(div().w(px(14.)))
                        .child(div().flex_1().child(label.clone()))
                        .child(div().text_color(t().muted()).child("›"))
                        .into_any_element(),
                    Item::Action { label, hint, check, act } => {
                        let act = act.clone();
                        row.child(div().w(px(14.)).text_color(t().accent()).child(if *check == Some(true) { "✓" } else { "" }))
                            .child(div().flex_1().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label.clone()))
                            .children(hint.clone().map(|h| div().text_xs().text_color(t().dim()).child(h)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.menu = None;
                                act(this, window, cx);
                            }))
                            .into_any_element()
                    }
                }
            }))
    }
}
