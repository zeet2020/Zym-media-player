//! Explorer: browse content fed by source plugins (docs/plugins.md). Plugin calls run on the
//! background executor; results land back on the UI thread and are dropped if the user navigated away.

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{Context, Div, MouseButton, SharedString, Stateful, Window, div, img, prelude::*, px, rgb};

use crate::panel::{confirm_button, row_icon, sidebar, sidebar_close, sidebar_item};
use crate::sources::{self, Item, Page, Source};
use crate::textbox::Field;
use crate::theme::t;
use crate::{Player, chip, icon, icon_button};

pub struct Explorer {
    pub open: bool,
    /// Installed plugins (sidebar groups).
    installed: Vec<Arc<sources::Installed>>,
    /// All connections of all plugins, grouped by plugin in sidebar order.
    pub sources: Vec<Arc<Source>>,
    pub loaded: bool,
    sel: usize,
    stack: Vec<Nav>,
    /// Settings form draft for the selected connection (open = Some).
    form: Option<BTreeMap<String, String>>,
    /// Connection-name draft shown above the plugin's settings.
    form_name: String,
    /// Selected plugin tab (index into `installed`).
    sel_plugin: usize,
    /// Gear page: list of this plugin's connections + "Add".
    managing: bool,
    /// Header connection dropdown is open.
    conn_menu: bool,
    /// Last browsed connection per plugin id.
    last_conn: std::collections::HashMap<String, String>,
    /// Row whose trash icon was clicked once; a second click removes it.
    confirm_remove: Option<usize>,
    pub search: String,
    /// "Opening <title>…" / "Couldn't play: …" banner.
    notice: Option<(String, bool)>,
    next_token: u64,
}

struct Nav {
    title: String,
    req: Req,
    state: Load,
    token: u64,
    loading_more: bool,
    /// The item whose route opened this page (a movie for its stream list): describes what plays from here.
    from: Option<Item>,
}

#[derive(Clone)]
enum Req {
    Home,
    Browse(String),
    Search(String),
}

enum Load {
    Loading,
    Ready(Page),
    Failed(String),
}

impl Explorer {
    /// Text behind the Explorer's own fields (search, settings form, connection name).
    pub fn field_text(&mut self, f: &Field) -> Option<&mut String> {
        match f {
            Field::Search => Some(&mut self.search),
            Field::Setting(k) => self.form.as_mut().map(|form| form.entry(k.clone()).or_default()),
            Field::ConnName => Some(&mut self.form_name),
            _ => None,
        }
    }

    pub fn new() -> Self {
        Self {
            open: false,
            installed: vec![],
            sources: vec![],
            loaded: false,
            sel: 0,
            stack: vec![],
            form: None,
            form_name: String::new(),
            sel_plugin: 0,
            managing: false,
            conn_menu: false,
            last_conn: Default::default(),
            confirm_remove: None,
            search: String::new(),
            notice: None,
            next_token: 0,
        }
    }

    fn plugin(&self) -> Option<Arc<sources::Installed>> {
        self.installed.get(self.sel_plugin).cloned()
    }

    /// The selected connection, only if it belongs to the selected plugin tab.
    fn source(&self) -> Option<Arc<Source>> {
        let p = self.plugin()?;
        self.sources.get(self.sel).filter(|s| Arc::ptr_eq(&s.installed, &p)).cloned()
    }

    /// (index into `sources`, connection) for the selected plugin.
    fn plugin_conns(&self) -> Vec<(usize, Arc<Source>)> {
        let Some(p) = self.plugin() else { return vec![] };
        self.sources.iter().enumerate().filter(|(_, s)| Arc::ptr_eq(&s.installed, &p)).map(|(i, s)| (i, s.clone())).collect()
    }
}

impl Player {
    pub fn toggle_explorer(&mut self, cx: &mut Context<Self>) {
        let open = !self.explorer.open;
        self.close_overlays();
        if !open {
            return;
        }
        self.explorer.open = true;
        if !self.explorer.loaded {
            let off = crate::settings::disabled("content");
            self.explorer.installed = sources::load_all().into_iter().filter(|p| !off.contains(&p.manifest.id)).collect();
            self.explorer.sources = self.explorer.installed.iter().flat_map(|p| p.connections()).collect();
            self.explorer.loaded = true;
            // Start on the first plugin that has a connection (else the first plugin).
            let ex = &self.explorer;
            let first = ex.installed.iter().position(|p| ex.sources.iter().any(|s| Arc::ptr_eq(&s.installed, p))).unwrap_or(0);
            self.select_plugin(first, cx);
        }
    }

    /// Sidebar tab: browse this plugin's last-used connection, or its manage page if it has none.
    fn select_plugin(&mut self, pi: usize, cx: &mut Context<Self>) {
        let ex = &mut self.explorer;
        ex.sel_plugin = pi;
        ex.conn_menu = false;
        ex.form = None;
        self.editing = None;
        let Some(p) = ex.plugin() else { return };
        let last = ex.last_conn.get(&p.manifest.id).cloned();
        let conns = ex.plugin_conns();
        let pick = conns.iter().find(|(_, s)| Some(&s.conn_id) == last.as_ref()).or(conns.first()).map(|(i, _)| *i);
        match pick {
            Some(i) => self.select_source(i, cx),
            None => {
                ex.managing = true;
                ex.stack.clear();
                ex.notice = None;
            }
        }
    }

    fn select_source(&mut self, i: usize, cx: &mut Context<Self>) {
        let ex = &mut self.explorer;
        ex.sel = i;
        ex.managing = false;
        ex.conn_menu = false;
        if let Some(s) = ex.sources.get(i).cloned() {
            ex.sel_plugin = ex.installed.iter().position(|p| Arc::ptr_eq(p, &s.installed)).unwrap_or(ex.sel_plugin);
            ex.last_conn.insert(s.manifest.id.clone(), s.conn_id.clone());
        }
        ex.stack.clear();
        ex.notice = None;
        ex.search.clear();
        ex.form = None;
        let Some(src) = ex.source() else { return };
        if src.needs_setup() {
            self.open_settings_form();
        } else {
            self.push(Req::Home, src.name(), cx);
        }
    }

    /// "+" on a plugin header: new unsaved connection, selected, settings form open.
    fn add_connection(&mut self, plugin: Arc<sources::Installed>) {
        let ex = &mut self.explorer;
        let existing = ex.sources.iter().filter(|s| Arc::ptr_eq(&s.installed, &plugin)).count();
        let src = plugin.new_connection(existing);
        // Insert after this plugin's last connection to keep the sidebar grouped.
        let at = ex.sources.iter().rposition(|s| Arc::ptr_eq(&s.installed, &plugin)).map_or(ex.sources.len(), |i| i + 1);
        ex.sources.insert(at, src);
        ex.sel = at;
        ex.stack.clear();
        ex.notice = None;
        self.open_settings_form();
    }

    /// Delete the selected connection (or discard it if never saved).
    fn remove_connection(&mut self) {
        let ex = &mut self.explorer;
        let Some(src) = ex.source() else { return };
        if let Err(e) = src.remove() {
            ex.notice = Some((e, true));
            return;
        }
        ex.sources.remove(ex.sel);
        ex.sel = usize::MAX; // nothing selected until the user picks a connection
        ex.form = None;
        ex.stack.clear();
        ex.managing = true;
        self.editing = None;
    }

    fn open_settings_form(&mut self) {
        let Some(src) = self.explorer.source() else { return };
        let mut draft = src.settings();
        for s in &src.manifest.settings {
            if let (false, Some(d)) = (draft.contains_key(&s.key), s.default.as_ref()) {
                draft.insert(s.key.clone(), d.as_str().map(str::to_string).unwrap_or_else(|| d.to_string()));
            }
        }
        self.explorer.form = Some(draft);
        self.explorer.form_name = src.name();
        self.explorer.managing = true; // the form is shown inline on the manage page
        self.editing = src.manifest.settings.iter().find(|s| matches!(s.kind.as_str(), "string" | "secret")).map(|s| Field::Setting(s.key.clone()));
    }

    fn save_settings_form(&mut self, cx: &mut Context<Self>) {
        let (Some(src), Some(form)) = (self.explorer.source(), self.explorer.form.take()) else { return };
        self.editing = None;
        let was_new = !src.is_saved();
        if let Err(e) = src.save(self.explorer.form_name.clone(), form) {
            self.explorer.notice = Some((e, true));
            return;
        }
        self.explorer.last_conn.insert(src.manifest.id.clone(), src.conn_id.clone());
        if !was_new {
            return; // edited: stay on the list (now showing the new name/URL)
        }
        self.explorer.stack.clear();
        self.explorer.managing = false; // new connection: start browsing it
        self.push(Req::Home, src.name(), cx);
    }

    /// Cancel in the settings form: discard a never-saved connection, then back to the manage list.
    fn cancel_settings_form(&mut self) {
        if self.explorer.source().is_some_and(|s| !s.is_saved()) {
            return self.remove_connection();
        }
        self.explorer.form = None;
        self.explorer.managing = true;
        self.editing = None;
    }

    fn push(&mut self, req: Req, title: String, cx: &mut Context<Self>) {
        let token = self.explorer.next_token;
        self.explorer.next_token += 1;
        self.explorer.stack.push(Nav { title, req: req.clone(), state: Load::Loading, token, loading_more: false, from: None });
        self.fetch(req, None, token, cx);
    }

    /// Run a page request in the background; apply it to the Nav with `token` if it still exists.
    fn fetch(&mut self, req: Req, cursor: Option<String>, token: u64, cx: &mut Context<Self>) {
        let Some(src) = self.explorer.source() else { return };
        let task = cx.background_executor().spawn(async move {
            match (&req, cursor.as_deref()) {
                (Req::Home, None) => src.home(),
                (Req::Home, c) => src.browse("", c),
                (Req::Browse(r), c) => src.browse(r, c),
                (Req::Search(q), c) => src.search(q, c),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |p, cx| {
                let Some(nav) = p.explorer.stack.iter_mut().find(|n| n.token == token) else { return };
                let appending = nav.loading_more;
                nav.loading_more = false;
                match (result, &mut nav.state, appending) {
                    // "Load more": append into existing sections.
                    (Ok(more), Load::Ready(page), true) => {
                        for (i, s) in more.sections.into_iter().enumerate() {
                            match page.sections.get_mut(i) {
                                Some(dst) => dst.items.extend(s.items),
                                None => page.sections.push(s),
                            }
                        }
                        page.next = more.next;
                        page.dropped += more.dropped;
                    }
                    (Ok(page), _, _) => {
                        if nav.title.is_empty() && !page.title.is_empty() {
                            nav.title = page.title.clone();
                        }
                        nav.state = Load::Ready(page);
                    }
                    (Err(e), _, true) => p.explorer.notice = Some((e, true)),
                    (Err(e), state, false) => *state = Load::Failed(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(nav) = self.explorer.stack.last_mut() else { return };
        let Load::Ready(page) = &nav.state else { return };
        let (Some(cursor), false) = (page.next.clone(), nav.loading_more) else { return };
        nav.loading_more = true;
        let (req, token) = (nav.req.clone(), nav.token);
        self.fetch(req, Some(cursor), token, cx);
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        let Some(nav) = self.explorer.stack.last_mut() else { return };
        nav.state = Load::Loading;
        let (req, token) = (nav.req.clone(), nav.token);
        self.fetch(req, None, token, cx);
    }

    /// Play (append = false) or add to the playlist (append = true) a playable item; open a folder otherwise.
    fn activate(&mut self, item: Item, append: bool, cx: &mut Context<Self>) {
        if item.playable {
            let Some(src) = self.explorer.source() else { return };
            self.explorer.notice = Some((format!("{} {}…", if append { "Adding" } else { "Opening" }, item.title), false));
            let id = item.id.clone();
            let parent = self.explorer.stack.last().and_then(|n| n.from.clone());
            let task = cx.background_executor().spawn(async move { src.resolve(&id) });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                this.update(cx, |p, cx| {
                    if let Ok(pb) = &result {
                        let info = pb.info.clone().unwrap_or_else(|| sources::Info::from_item(&item, parent.as_ref()));
                        p.infos.insert(pb.url.clone(), info);
                    }
                    match result {
                        Ok(pb) if append => {
                            p.mpv.append_stream(&pb);
                            p.explorer.notice = Some((format!("Added to playlist: {}", item.title), false));
                        }
                        Ok(pb) => {
                            p.mpv.play_stream(&pb);
                            p.explorer.notice = None;
                            p.explorer.open = false; // collapse so the video is visible
                            p.editing = None;
                        }
                        Err(e) => p.explorer.notice = Some((format!("Couldn't play {}: {e}", item.title), true)),
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        } else if let Some(route) = item.route.clone() {
            self.push(Req::Browse(route), item.title.clone(), cx);
            if let Some(nav) = self.explorer.stack.last_mut() {
                nav.from = Some(item);
            }
        }
    }

    /// "+" on a row or card. A playable item is added as it is. A movie/episode leads to a page of
    /// stream choices, so its first stream (the addon's top pick) is added, described by the movie.
    fn add_item(&mut self, item: Item, cx: &mut Context<Self>) {
        if item.playable {
            return self.activate(item, true, cx);
        }
        let (Some(src), Some(route)) = (self.explorer.source(), item.route.clone()) else { return };
        self.explorer.notice = Some((format!("Adding {}…", item.title), false));
        let task = cx.background_executor().spawn(async move {
            let page = src.browse(&route, None)?;
            let stream = page.sections.into_iter().flat_map(|s| s.items).find(|i| i.playable).ok_or("no streams found")?;
            let pb = src.resolve(&stream.id)?;
            Ok::<_, String>((stream, pb))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |p, cx| {
                p.explorer.notice = Some(match result {
                    Ok((stream, pb)) => {
                        let info = pb.info.clone().unwrap_or_else(|| sources::Info::from_item(&stream, Some(&item)));
                        p.infos.insert(pb.url.clone(), info);
                        p.mpv.append_stream(&pb);
                        let pick = [Some(stream.title.clone()), stream.subtitle.clone()].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                        (format!("Added to playlist: {} ({pick})", item.title), false)
                    }
                    Err(e) => (format!("Couldn't add {}: {e}", item.title), true),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Debug (ZYM_DEBUG_AUTOPLAY): play an item of the selected source exactly as a click would.
    /// Comma-separated: "id" plays, "+id" appends, "@route" browses to a route.
    pub fn debug_play(&mut self, ids: String, cx: &mut Context<Self>) {
        for id in ids.split(',') {
            if let Some(route) = id.strip_prefix('@') {
                self.push(Req::Browse(route.into()), route.into(), cx);
                continue;
            }
            let (append, id) = match id.strip_prefix('+') { Some(rest) => (true, rest), None => (false, id) };
            let item = Item { id: id.into(), kind: "live".into(), title: id.into(), playable: true, ..Default::default() };
            self.activate(item, append, cx);
        }
    }

    pub fn submit_search(&mut self, cx: &mut Context<Self>) {
        let q = self.explorer.search.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.editing = None;
        // Replace a previous search result instead of stacking searches.
        if matches!(self.explorer.stack.last().map(|n| &n.req), Some(Req::Search(_))) {
            self.explorer.stack.pop();
        }
        self.push(Req::Search(q.clone()), format!("“{q}”"), cx);
    }

    /// Enter in one of the Explorer's text fields.
    pub fn submit_explorer_field(&mut self, f: &Field, cx: &mut Context<Self>) {
        match f {
            Field::Search => self.submit_search(cx),
            _ => self.save_settings_form(cx),
        }
    }

    // ---------------------------------------------------------------- render

    pub fn render_explorer(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let ex = &self.explorer;
        let sidebar = sidebar("PLUGINS")
            .children(ex.installed.iter().enumerate().map(|(pi, plugin)| {
                let n = ex.sources.iter().filter(|s| Arc::ptr_eq(&s.installed, plugin)).count();
                sidebar_item(("plugin", pi), pi == ex.sel_plugin)
                    .child(plugin.manifest.name.clone())
                    .child(div().text_xs().text_color(t().dim()).child(match n {
                        0 => "Not set up".to_string(),
                        1 => "1 connection".to_string(),
                        n => format!("{n} connections"),
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_plugin(pi, cx)))
            }))
            .when(ex.installed.is_empty(), |d| {
                d.child(div().px_4().text_sm().text_color(t().muted()).child(format!(
                    "No content plugins installed.\n\nPut a plugin folder (manifest.json + plugin.wasm) in\n{}",
                    sources::sources_dir().display()
                )))
            })
            .child(div().flex_1())
            .child(sidebar_close("ex-close").on_click(cx.listener(|this, _, _, cx| this.toggle_explorer(cx))));

        self.big_panel("explorer", window, cx).child(sidebar).child(self.render_main(window, cx))
    }

    fn render_main(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let ex = &self.explorer;
        let Some(plugin) = ex.plugin() else { return div().flex_1().min_w_0() };
        let src = ex.source();
        let conns = ex.plugin_conns();
        let nav = ex.stack.last();
        let browsing = !ex.managing && ex.form.is_none() && src.is_some();
        let title = match (&src, ex.form.is_some(), ex.managing) {
            (_, _, true) | (None, _, _) => format!("{} connections", plugin.manifest.name),
            _ => nav.map(|n| n.title.clone()).unwrap_or_default(),
        };

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(56.))
            .px_4()
            .border_b_1()
            .border_color(t().hover())
            .when(browsing && ex.stack.len() > 1, |d| {
                d.child(
                    chip("ex-back", "‹ Back", false).on_click(cx.listener(|this, _, _, cx| {
                        this.explorer.stack.pop();
                        this.explorer.notice = None;
                        cx.notify();
                    })),
                )
            })
            // Which connection we're browsing: dropdown.
            .children(src.clone().filter(|_| browsing).map(|s| {
                chip("ex-conn", format!("{}  ▾", s.name()), ex.conn_menu).on_click(cx.listener(|this, _, _, cx| {
                    this.explorer.conn_menu = !this.explorer.conn_menu;
                    cx.notify();
                }))
            }))
            .child(div().flex_1().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
            .when(browsing && src.as_ref().is_some_and(|s| s.has("search")), |d| {
                d.child(div().w(px(220.)).min_w(px(90.)).flex_shrink().child(self.text_box(Field::Search, &ex.search, "Search…  (Enter)", false, cx)))
            })
            // Gear: manage this plugin's connections (list + add); toggles back to browsing.
            .child(
                icon_button("ex-settings", "")
                    .child(icon("gear").text_color(t().fg()))
                    .when(!browsing, |d| d.bg(t().pressed()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let ex = &mut this.explorer;
                        ex.conn_menu = false;
                        if ex.form.is_some() {
                            this.cancel_settings_form();
                        } else if ex.managing && ex.source().is_some() {
                            ex.managing = false; // back to browsing the current connection
                            if ex.stack.is_empty() {
                                let name = ex.source().map(|s| s.name()).unwrap_or_default();
                                this.push(Req::Home, name, cx);
                            }
                        } else {
                            ex.managing = true;
                        }
                        cx.notify();
                    })),
            );

        // Manage page: all connections of this plugin + add.
        let mut manage = || {
            div()
                .p_4()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().pb_2().text_color(t().muted()).child(plugin.manifest.description.clone()))
                .children(conns.iter().map(|(i, c)| {
                    let i = *i;
                    let detail = c.settings().into_iter().find(|(k, _)| k.contains("url")).map(|(_, v)| v).unwrap_or_default();
                    let current = ex.sel == i;
                    let editing_this = current && ex.form.is_some();
                    div()
                        .id(("conn", i))
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .hover(|st| st.bg(t().hover()))
                        .when(editing_this, |d| d.bg(t().accent_a(0x20)))
                        .cursor_pointer()
                        // Clicking the row opens (browses) the connection; the icons stop the click.
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.explorer.confirm_remove = None;
                            this.select_source(i, cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .child(div().when(current, |d| d.text_color(t().accent())).child(c.name()))
                                .child(div().text_xs().text_color(t().dim()).whitespace_nowrap().text_ellipsis().child(detail)),
                        )
                        .child(row_icon(format!("conn-edit-{i}"), "edit").on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.explorer.confirm_remove = None;
                            this.explorer.sel = i;
                            this.open_settings_form();
                            cx.notify();
                        })))
                        .child(if ex.confirm_remove == Some(i) {
                            // Second click confirms: deletes this connection's saved settings (incl. password).
                            confirm_button(SharedString::from(format!("conn-rm-ok-{i}")), "Remove?")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.explorer.confirm_remove = None;
                                    this.explorer.sel = i;
                                    this.remove_connection();
                                    cx.notify();
                                }))
                        } else {
                            row_icon(format!("conn-rm-{i}"), "trash").on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.explorer.confirm_remove = Some(i);
                                cx.notify();
                            }))
                        })
                }))
                .when(conns.is_empty() && ex.form.is_none(), |d| d.child(div().px_3().py_2().text_color(t().muted()).child("No connections yet.")))
                .children(src.clone().filter(|_| ex.form.is_some()).map(|s| self.render_settings_form(&s, cx)))
                .when(ex.form.is_none(), |d| d.child(div().pt_2().child(chip("conn-add", "+ Add connection", true).on_click({
                    let plugin = plugin.clone();
                    cx.listener(move |this, _, _, cx| {
                        this.add_connection(plugin.clone());
                        cx.notify();
                    })
                }))))
        };

        let body: gpui::AnyElement = if ex.managing || src.is_none() || ex.form.is_some() {
            div().id("ex-manage").absolute().inset_0().overflow_y_scroll().child(manage()).into_any_element()
        } else {
            let src = src.clone().unwrap();
            match nav.map(|n| &n.state) {
                None | Some(Load::Loading) => center(div().text_color(t().muted()).child("Loading…")).into_any_element(),
                Some(Load::Failed(e)) => center(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_3()
                        .max_w(px(480.))
                        .child(div().text_color(t().error()).child("This plugin couldn't load the page"))
                        .child(div().text_color(t().muted()).text_center().child(e.clone()))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(chip("ex-retry", "Retry", true).on_click(cx.listener(|this, _, _, cx| this.retry(cx))))
                                .when(!src.manifest.settings.is_empty(), |d| {
                                    d.child(chip("ex-fix", "Open settings", false).on_click(cx.listener(|this, _, _, _| this.open_settings_form())))
                                }),
                        ),
                )
                .into_any_element(),
                Some(Load::Ready(page)) => self.render_page(page, nav.unwrap().loading_more, window, cx).into_any_element(),
            }
        };

        div()
            .flex_1()
            .min_w_0() // shrink with the panel; otherwise header icons are pushed out of view
            .h_full()
            .flex()
            .flex_col()
            .relative()
            .child(header)
            .child(div().flex_1().relative().child(body))
            .when(ex.conn_menu && browsing, |d| {
                d.child(
                    div()
                        .id("conn-menu")
                        .absolute()
                        .top(px(50.))
                        .left_4()
                        .w(px(260.))
                        .py_1()
                        .rounded_lg()
                        .bg(t().surface_solid())
                        .border_1()
                        .border_color(t().border())
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .children(conns.iter().map(|(i, c)| {
                            let i = *i;
                            div()
                                .id(("conn-pick", i))
                                .flex()
                                .items_center()
                                .gap_2()
                                .mx_1()
                                .px_2()
                                .py_1p5()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|st| st.bg(t().hover()))
                                .child(div().w(px(14.)).text_color(t().accent()).child(if ex.sel == i { "✓" } else { "" }))
                                .child(div().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(c.name()))
                                .on_click(cx.listener(move |this, _, _, cx| this.select_source(i, cx)))
                        }))
                        .child(div().h(px(1.)).my_1().bg(t().border()))
                        .child(
                            div()
                                .id("conn-manage")
                                .mx_1()
                                .px_2()
                                .py_1p5()
                                .rounded_md()
                                .cursor_pointer()
                                .text_color(t().muted())
                                .hover(|st| st.bg(t().hover()))
                                .child("Manage connections…")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.explorer.conn_menu = false;
                                    this.explorer.managing = true;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .children(ex.notice.as_ref().map(|(msg, is_err)| {
                div()
                    .absolute()
                    .bottom_4()
                    .left_4()
                    .right_4()
                    .px_4()
                    .py_2()
                    .rounded_lg()
                    .shadow_lg()
                    // Fixed dark toast in both themes, so the text must be fixed too (theme fg is dark in light mode).
                    .bg(if *is_err { rgb(0x7a2424) } else { rgb(0x1f2a40) })
                    .text_color(rgb(0xffffff))
                    .child(msg.clone())
            }))
    }

    fn render_page(&self, page: &Page, loading_more: bool, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let grid = matches!(page.layout.as_str(), "grid" | "rows");
        let empty = page.sections.iter().all(|s| s.items.is_empty());
        div()
            .id("ex-page")
            .absolute()
            .inset_0()
            .overflow_y_scroll()
            .p_4()
            .when(empty, |d| d.child(div().text_color(t().muted()).child("Nothing here.")))
            .children(page.sections.iter().enumerate().map(|(si, sec)| {
                let items = sec.items.iter().enumerate().map(|(ii, it)| {
                    let id = SharedString::from(format!("it-{si}-{ii}"));
                    let item = it.clone();
                    let add = addable(it).then(|| {
                        let item = it.clone();
                        let btn = icon_button_small(SharedString::from(format!("add-{si}-{ii}")), "+");
                        // On a poster: a round dark badge so it reads over any art (fixed bg -> fixed text).
                        let btn = if grid { btn.rounded_full().bg(gpui::rgba(0x000000b0)).text_color(rgb(0xffffff)).hover(|s| s.bg(gpui::rgba(0x000000e0))) } else { btn };
                        btn.on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation(); // don't also open/play the item
                            this.add_item(item.clone(), cx);
                        }))
                        .into_any_element()
                    });
                    let el = if grid { poster_card(id, it, add) } else { list_row(id, it, add) };
                    el.on_click(cx.listener(move |this, _, _, cx| this.activate(item.clone(), false, cx)))
                });
                div()
                    .mb_4()
                    .children(sec.title.clone().map(|title| div().mb_2().text_color(t().muted()).font_weight(gpui::FontWeight::SEMIBOLD).child(title)))
                    .child(if grid { div().flex().flex_wrap().gap_3().children(items) } else { div().flex().flex_col().children(items) })
            }))
            .when(page.next.is_some(), |d| {
                d.child(
                    div().flex().justify_center().py_3().child(
                        chip("ex-more", if loading_more { "Loading…" } else { "Load more" }, false)
                            .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                    ),
                )
            })
            .when(page.dropped > 0, |d| {
                d.child(div().text_xs().text_color(t().dim()).child(format!("{} invalid item(s) from this plugin were hidden.", page.dropped)))
            })
    }

    /// Compact inline form (manage page): one row per field — label | control, hint underneath.
    fn render_settings_form(&self, src: &Source, cx: &mut Context<Self>) -> Div {
        let form = self.explorer.form.clone().unwrap_or_default();
        let row = |label: String, control: gpui::AnyElement, hint: Option<String>| {
            div()
                .flex()
                .items_start()
                .gap_3()
                .child(div().w(px(110.)).flex_none().pt(px(7.)).text_color(t().muted()).child(label))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(control)
                        .children(hint.map(|h| div().text_xs().text_color(t().dim()).child(h))),
                )
        };
        div()
            .mt_2()
            .p_3()
            .rounded_lg()
            .bg(t().field())
            .border_1()
            .border_color(t().border())
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(if src.is_saved() { "Edit connection" } else { "New connection" }))
            .child(row("Name".into(), self.text_box(Field::ConnName, &self.explorer.form_name, "e.g. Home NAS", false, cx).into_any_element(), None))
            .children(group_by_section(&src.manifest.settings).into_iter().flat_map(|(_, fields)| fields).map(|s| {
                let val = form.get(&s.key).cloned().unwrap_or_default();
                let key = s.key.clone();
                let control: gpui::AnyElement = match s.kind.as_str() {
                    "bool" => {
                        let on = val == "true";
                        chip(SharedString::from(format!("set-{key}")), if on { "On" } else { "Off" }, on)
                            .on_click(cx.listener(move |this, _, _, _| {
                                if let Some(f) = this.explorer.form.as_mut() {
                                    f.insert(key.clone(), (!on).to_string());
                                }
                            }))
                            .into_any_element()
                    }
                    "select" => div()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .pt(px(4.))
                        .children(s.options.iter().map(|o| {
                            let (key, o2) = (key.clone(), o.clone());
                            chip(SharedString::from(format!("set-{key}-{o}")), o.clone(), *o == val).on_click(cx.listener(move |this, _, _, _| {
                                if let Some(f) = this.explorer.form.as_mut() {
                                    f.insert(key.clone(), o2.clone());
                                }
                            }))
                        }))
                        .into_any_element(),
                    kind => self.text_box(Field::Setting(key), &val, "", kind == "secret", cx).into_any_element(),
                };
                row(format!("{}{}", s.label, if s.required { " *" } else { "" }), control, s.description.clone())
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pt_1()
                    .child(chip("set-save", "Save", true).on_click(cx.listener(|this, _, _, cx| this.save_settings_form(cx))))
                    .child(chip("set-cancel", "Cancel", false).on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_settings_form();
                        cx.notify();
                    })))
                    .child(div().flex_1().text_xs().text_color(t().dim()).text_right().child(
                        if src.manifest.hosts.iter().any(|h| h == "*") { "Can reach any host".to_string() } else { format!("Reaches: {}", src.manifest.hosts.join(", ")) },
                    ))
                    .when(src.is_saved(), |d| {
                        d.child(chip("set-remove", "Remove", false).on_click(cx.listener(|this, _, _, cx| {
                            this.remove_connection();
                            cx.notify();
                        })))
                    }),
            )
    }
}

/// Settings grouped by `section`, sections in order of first appearance; unsectioned first.
fn group_by_section(settings: &[sources::Setting]) -> Vec<(Option<String>, Vec<&sources::Setting>)> {
    let mut groups: Vec<(Option<String>, Vec<&sources::Setting>)> = vec![(None, vec![])];
    for s in settings {
        match groups.iter_mut().find(|(t, _)| *t == s.section) {
            Some((_, v)) => v.push(s),
            None => groups.push((s.section.clone(), vec![s])),
        }
    }
    groups.retain(|(_, v)| !v.is_empty());
    groups
}

/// Small themed icon button for list rows (edit / remove).

fn center(child: Div) -> Div {
    div().size_full().flex().items_center().justify_center().child(child)
}

fn art_url(it: &Item) -> Option<String> {
    it.art.logo.clone().or_else(|| it.art.thumb.clone()).or_else(|| it.art.poster.clone())
}

/// Logo/thumb or a lettered placeholder.
fn thumb(it: &Item, w: f32, h: f32) -> gpui::AnyElement {
    match art_url(it) {
        Some(url) => img(url).w(px(w)).h(px(h)).rounded_md().object_fit(gpui::ObjectFit::Contain).into_any_element(),
        None => div()
            .w(px(w))
            .h(px(h))
            .rounded_md()
            .bg(t().field())
            .flex()
            .items_center()
            .justify_center()
            .text_color(t().muted())
            .child(it.title.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default())
            .into_any_element(),
    }
}

fn icon_button_small(id: SharedString, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(28.))
        .h(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_lg()
        .text_color(t().fg())
        .hover(|s| s.bg(t().pressed()))
        .child(label)
}

/// `add` = the "+ add to playlist" button for playable rows.
fn list_row(id: SharedString, it: &Item, add: Option<gpui::AnyElement>) -> Stateful<Div> {
    let sub = match (&it.live, &it.subtitle) {
        (Some(l), _) if l.now.is_some() => format!("Now: {}", l.now.as_ref().unwrap().title),
        (_, Some(s)) => s.clone(),
        _ => String::new(),
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .px_2()
        .py_1p5()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(t().hover()))
        .child(thumb(it, 44., 44.))
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .child(div().whitespace_nowrap().text_ellipsis().overflow_hidden().child(it.title.clone()))
                .when(!sub.is_empty(), |d| d.child(div().text_xs().text_color(t().muted()).whitespace_nowrap().child(sub))),
        )
        .children(it.badges.iter().map(|b| div().px_1p5().rounded_sm().bg(t().border()).text_xs().child(b.clone())))
        .children(add)
        .child(div().w(px(20.)).text_center().text_color(t().muted()).child(if it.playable { "▶" } else { "›" }))
}

/// Fixed-size poster card (2:3 art + one title line + one subtitle line), so every card in a row
/// lines up whatever the art's shape, load state or text length.
fn poster_card(id: SharedString, it: &Item, add: Option<gpui::AnyElement>) -> Stateful<Div> {
    const W: f32 = 142.;
    const H: f32 = 213.;
    let url = it.art.poster.clone().or_else(|| art_url(it));
    let line = |text: String| div().h(px(18.)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(text);
    div()
        .id(id)
        .flex_none()
        .w(px(W + 8.))
        .cursor_pointer()
        .rounded_md()
        .group("card")
        .hover(|s| s.bg(t().hover()))
        .p_1()
        .child(
            // Frame holds the size while the image loads, if it fails, or if its aspect differs.
            div()
                .relative()
                .w(px(W))
                .h(px(H))
                .flex_none()
                .rounded_md()
                .overflow_hidden()
                .bg(t().field())
                .children(url.map(|u| img(u).size_full().object_fit(gpui::ObjectFit::Cover)))
                // "+ add to playlist", on hover, top-right of the poster.
                .children(add.map(|a| div().absolute().top_1().right_1().invisible().group_hover("card", |s| s.visible()).child(a))),
        )
        .child(line(it.title.clone()).mt_1().text_sm())
        .child(line(it.subtitle.clone().unwrap_or_default()).text_xs().text_color(t().muted()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(key: &str, section: Option<&str>) -> sources::Setting {
        serde_json::from_value(serde_json::json!({ "key": key, "label": key, "type": "string", "section": section })).unwrap()
    }

    #[test]
    fn sections_keep_first_appearance_order() {
        let s = [setting("a", None), setting("b", Some("Account")), setting("c", Some("Playback")), setting("d", Some("Account")), setting("e", None)];
        let g = group_by_section(&s);
        let shape: Vec<(Option<&str>, Vec<&str>)> = g.iter().map(|(t, v)| (t.as_deref(), v.iter().map(|x| x.key.as_str()).collect())).collect();
        assert_eq!(shape, vec![(None, vec!["a", "e"]), (Some("Account"), vec!["b", "d"]), (Some("Playback"), vec!["c"])]);
        assert!(group_by_section(&[setting("x", Some("Only"))])[0].0.is_some()); // no empty leading group
    }
}

/// Gets a "+": playable items, and movies/episodes whose page lists stream choices (first one is added).
/// Series and folders hold many things, so they don't.
fn addable(it: &Item) -> bool {
    it.playable || (it.route.is_some() && matches!(it.kind.as_str(), "movie" | "episode"))
}

#[cfg(test)]
mod add_tests {
    fn item(v: serde_json::Value) -> super::Item {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn what_can_be_added() {
        assert!(super::addable(&item(serde_json::json!({ "id": "s", "kind": "stream", "title": "1080p", "playable": true }))));
        assert!(super::addable(&item(serde_json::json!({ "id": "m", "kind": "movie", "title": "M", "route": "streams:movie:tt1" }))));
        assert!(super::addable(&item(serde_json::json!({ "id": "e", "kind": "episode", "title": "E", "route": "streams:series:tt1:1:1" }))));
        assert!(!super::addable(&item(serde_json::json!({ "id": "x", "kind": "series", "title": "S", "route": "meta:series:tt1" }))));
        assert!(!super::addable(&item(serde_json::json!({ "id": "f", "kind": "folder", "title": "F", "route": "/a" }))));
    }
}
