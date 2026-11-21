//! Install a content plugin from a `.zym` package (docs/plugins.md §1): Settings › Plugins ›
//! Install plugin…, or drop the file on the player. The package is fully checked before anything
//! is written; only manifest.json, plugin.wasm and icon.png are taken from it.

use std::io::Read;
use std::path::{Path, PathBuf};

use gpui::{Context, Div, SharedString, Stateful, div, prelude::*, px, rgba};

use crate::sources::{API, Manifest, sources_dir};
use crate::theme::t;
use crate::{Player, chip};

const MAX_PACKAGE: u64 = 80 << 20;
const MAX_MANIFEST: u64 = 256 << 10;
const MAX_WASM: u64 = 64 << 20;
const MAX_ICON: u64 = 2 << 20;
/// Files a package may install; everything else in the archive is ignored.
const FILES: [&str; 3] = ["manifest.json", "plugin.wasm", "icon.png"];

/// A package that passed every check, ready to install.
pub struct Package {
    pub manifest: Manifest,
    /// Version already installed by the user (an upgrade), if any.
    pub replaces: Option<String>,
    files: Vec<(&'static str, Vec<u8>)>,
}

/// "org.example.iptv": dotted lowercase words. Also what keeps the install folder inside sources/.
fn valid_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    parts.len() >= 2 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'))
}

/// The plugin's own checks: manifest fields and the exports the player calls.
fn check_manifest(m: &Manifest) -> Result<(), String> {
    if !valid_id(&m.id) {
        return Err(format!("invalid plugin id “{}”", m.id));
    }
    if m.name.trim().is_empty() || m.version.trim().is_empty() {
        return Err("the manifest has no name or version".into());
    }
    if m.api != API {
        return Err(format!("made for plugin API {}, this player supports API {API}. Update the player or get a matching plugin.", m.api));
    }
    if !m.capabilities.iter().any(|c| c == "browse") {
        return Err("the manifest doesn't declare the “browse” capability".into());
    }
    Ok(())
}

/// Read and check a `.zym` file. Err = a message for the user.
pub fn read(path: &Path) -> Result<Package, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_PACKAGE {
        return Err("the file is too large to be a plugin".into());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| "not a plugin package (.zym files are zip archives)".to_string())?;
    let mut files = Vec::new();
    for (name, max) in FILES.into_iter().zip([MAX_MANIFEST, MAX_WASM, MAX_ICON]) {
        let Ok(entry) = zip.by_name(name) else {
            if name == "icon.png" {
                continue;
            }
            return Err(format!("{name} is missing from the package"));
        };
        // The size in the header can lie (zip bomb): stop reading past the limit either way.
        let mut data = Vec::new();
        entry.take(max + 1).read_to_end(&mut data).map_err(|e| format!("{name}: {e}"))?;
        if data.len() as u64 > max {
            return Err(format!("{name} is too large"));
        }
        files.push((name, data));
    }
    let manifest: Manifest = serde_json::from_slice(&files[0].1).map_err(|e| format!("manifest.json isn't a valid plugin manifest: {e}"))?;
    check_manifest(&manifest)?;
    let wasm = &files[1].1;
    if !wasm.starts_with(b"\0asm\x01\0\0\0") {
        return Err("plugin.wasm isn't a WebAssembly module".into());
    }
    // Load it in the sandbox (no network) and check the functions the player calls are there.
    let m = extism::Manifest::new([extism::Wasm::data(wasm.clone())]);
    let plugin = extism::Plugin::new(&m, [crate::remote::host_function(Vec::new())], true).map_err(|e| format!("plugin.wasm doesn't load as a plugin: {e:#}"))?;
    let mut needed = vec!["home", "browse", "resolve"];
    if manifest.capabilities.iter().any(|c| c == "search") {
        needed.push("search");
    }
    if let Some(f) = needed.iter().find(|f| !plugin.function_exists(f)) {
        return Err(format!("plugin.wasm doesn't provide the “{f}” function"));
    }
    let replaces = std::fs::read_to_string(user_dir(&manifest.id).join("manifest.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Manifest>(&t).ok())
        .map(|m| m.version);
    Ok(Package { manifest, replaces, files })
}

fn user_dir(id: &str) -> PathBuf {
    sources_dir().join(id)
}

/// Write the package to `<config>/sources/<id>/`. Connections and settings already there are kept,
/// so an upgrade (or a user copy of a bundled plugin) keeps working as before.
pub fn install(pkg: &Package) -> Result<(), String> {
    let dir = user_dir(&pkg.manifest.id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // plugin.wasm last: until it's written, a half-installed folder isn't loaded as a plugin.
    let _ = std::fs::remove_file(dir.join("plugin.wasm"));
    for (name, data) in pkg.files.iter().filter(|(n, _)| *n != "plugin.wasm").chain(pkg.files.iter().filter(|(n, _)| *n == "plugin.wasm")) {
        let tmp = dir.join(format!("{name}.part"));
        std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, dir.join(name))).map_err(|e| format!("{name}: {e}"))?;
    }
    if !pkg.files.iter().any(|(n, _)| *n == "icon.png") {
        let _ = std::fs::remove_file(dir.join("icon.png")); // an old version's icon
    }
    Ok(())
}

/// The user's folder holding this plugin. Usually `sources/<id>/`, but a plugin copied in by hand
/// can sit in a folder with any name (the loader accepts it), so fall back to matching manifest ids.
fn user_folder(id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    let is_plugin = |d: &PathBuf| d.join("plugin.wasm").is_file();
    let named = user_dir(id);
    if is_plugin(&named) {
        return Some(named);
    }
    std::fs::read_dir(sources_dir()).ok()?.flatten().map(|e| e.path()).find(|d| {
        is_plugin(d)
            && std::fs::read(d.join("manifest.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .is_some_and(|m| m["id"] == id)
    })
}

/// Installed by the user (removable), as opposed to bundled with the player.
pub fn user_installed(id: &str) -> bool {
    user_folder(id).is_some()
}

/// Remove a user-installed plugin and its connections.
pub fn remove(id: &str) -> Result<(), String> {
    let dir = user_folder(id).ok_or("only plugins you installed can be removed")?;
    std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ UI

/// The install dialog's state (over everything, like About).
pub enum Dialog {
    Checking,
    Ready(Package),
    Failed(String),
    Done(String),
}

impl Player {
    /// Settings › Plugins › Install plugin…: pick a .zym file.
    pub fn pick_plugin_package(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Install".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            if let Some(p) = paths.into_iter().next() {
                this.update(cx, |p2, cx| p2.open_plugin_package(p, cx)).ok();
            }
        })
        .detach();
    }

    /// Check a package (in the background: loading the wasm takes a moment), then ask to install.
    pub fn open_plugin_package(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.install = Some(Dialog::Checking);
        let task = cx.background_executor().spawn(async move { read(&path) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |p, cx| {
                if let Err(e) = &result {
                    crate::log::warn("install", e);
                }
                p.install = Some(match result {
                    Ok(pkg) => Dialog::Ready(pkg),
                    Err(e) => Dialog::Failed(e),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn confirm_install(&mut self) {
        let Some(Dialog::Ready(pkg)) = self.install.take() else { return };
        self.install = Some(match install(&pkg) {
            Ok(()) => {
                crate::log::info("install", &format!("installed {} {}", pkg.manifest.id, pkg.manifest.version));
                self.plugins_changed();
                Dialog::Done(format!("{} {} is installed. Find it in the Explorer.", pkg.manifest.name, pkg.manifest.version))
            }
            Err(e) => Dialog::Failed(format!("Couldn't install: {e}")),
        });
    }

    /// Settings › Plugins › remove (second click confirms).
    pub fn remove_plugin(&mut self, id: &str) {
        match remove(id) {
            Ok(()) => crate::log::info("install", &format!("removed {id}")),
            Err(e) => crate::log::error("install", &format!("removing {id}: {e}")),
        }
        self.plugins_changed();
    }

    /// Re-read plugins: the Explorer on next open, the Settings list now.
    fn plugins_changed(&mut self) {
        self.explorer.loaded = false;
        if let Some(s) = self.settings.as_mut() {
            s.reload_content();
        }
    }

    pub fn install_dialog(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let d = self.install.as_ref()?;
        let close = |label: &'static str, primary: bool| chip(SharedString::from(format!("inst-{label}")), label, primary).on_click(cx.listener(|this, _, _, _| this.install = None));
        let body = div().flex().flex_col().gap_2();
        let (title, body, buttons): (String, Div, Vec<Stateful<Div>>) = match d {
            Dialog::Checking => ("Checking plugin…".into(), body, vec![close("Cancel", false)]),
            Dialog::Failed(e) => ("Can't install this file".into(), body.child(div().text_color(t().error()).child(e.clone())), vec![close("Close", true)]),
            Dialog::Done(msg) => ("Installed".into(), body.child(msg.clone()), vec![close("Done", true)]),
            Dialog::Ready(pkg) => {
                let m = &pkg.manifest;
                let any_host = m.hosts.iter().any(|h| h == "*");
                let hosts = if m.hosts.is_empty() { "nothing (works offline)".to_string() } else if any_host { "any website".to_string() } else { m.hosts.join(", ") };
                let version = match &pkg.replaces {
                    Some(old) if *old == m.version => format!("Version {} (reinstall)", m.version),
                    Some(old) => format!("Version {} (replaces {old}; your connections are kept)", m.version),
                    None => format!("Version {}", m.version),
                };
                (
                    format!("Install “{}”?", m.name),
                    body.child(div().text_color(t().muted()).child(version))
                        .when(!m.description.is_empty(), |b| b.child(m.description.clone()))
                        .child(div().pt_2().text_color(t().muted()).child("This plugin talks to:"))
                        .child(div().when(any_host, |d| d.text_color(t().error())).child(hosts))
                        .when(any_host, |b| b.child(div().text_xs().text_color(t().dim()).child("It may contact any site. Install only if you trust where it came from."))),
                    vec![close("Cancel", false), chip("inst-install", "Install", true).on_click(cx.listener(|this, _, _, _| this.confirm_install()))],
                )
            }
        };
        Some(
            div()
                .id("install-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .bg(rgba(0x00000099))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("install")
                        .w(px(420.))
                        .max_w_full()
                        .p_6()
                        .rounded_xl()
                        .bg(t().surface_solid())
                        .border_1()
                        .border_color(t().border())
                        .shadow_lg()
                        .text_color(t().fg())
                        .text_sm()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                        .child(body)
                        .child(div().pt_2().flex().justify_end().gap_2().children(buttons)),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn package(dir: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let p = dir.join(name);
        let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        for (n, data) in files {
            z.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
        p
    }

    #[test]
    fn rejects_what_isnt_a_plugin() {
        let d = std::env::temp_dir().join(format!("zym-install-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let err = |p: PathBuf| read(&p).err().unwrap();

        std::fs::write(d.join("random.zym"), b"hello").unwrap();
        assert!(err(d.join("random.zym")).contains("not a plugin package"));
        assert!(err(package(&d, "empty.zym", &[("readme.txt", b"hi")])).contains("manifest.json is missing"));
        let good = br#"{"id":"org.test.demo","name":"Demo","version":"1.0.0","api":1,"capabilities":["browse"]}"#;
        assert!(err(package(&d, "nowasm.zym", &[("manifest.json", good)])).contains("plugin.wasm is missing"));
        assert!(err(package(&d, "badjson.zym", &[("manifest.json", b"{"), ("plugin.wasm", b"\0asm\x01\0\0\0")])).contains("valid plugin manifest"));
        let evil = br#"{"id":"../../evil","name":"x","version":"1","api":1,"capabilities":["browse"]}"#;
        assert!(err(package(&d, "evil.zym", &[("manifest.json", evil), ("plugin.wasm", b"\0asm\x01\0\0\0")])).contains("invalid plugin id"));
        let future = br#"{"id":"org.test.demo","name":"x","version":"1","api":9,"capabilities":["browse"]}"#;
        assert!(err(package(&d, "api.zym", &[("manifest.json", future), ("plugin.wasm", b"\0asm\x01\0\0\0")])).contains("plugin API 9"));
        assert!(err(package(&d, "notwasm.zym", &[("manifest.json", good), ("plugin.wasm", b"MZ not wasm")])).contains("isn't a WebAssembly module"));
        // Valid wasm, but none of the functions the player calls.
        let empty_module = b"\0asm\x01\0\0\0";
        assert!(err(package(&d, "noexports.zym", &[("manifest.json", good), ("plugin.wasm", empty_module)])).contains("“home” function"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The real bundled plugins (built by CI before the tests; skipped when not built locally).
    #[test]
    fn accepts_the_bundled_plugins() {
        let d = std::env::temp_dir().join(format!("zym-install-ok-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for (name, krate) in [("m3u", "zym_source_m3u"), ("stremio", "zym_source_stremio"), ("webdav", "zym_source_webdav"), ("ftp", "zym_source_ftp")] {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sources").join(name);
            let Ok(wasm) = std::fs::read(root.join(format!("target/wasm32-unknown-unknown/release/{krate}.wasm"))) else {
                eprintln!("skip {name}: not built");
                continue;
            };
            let manifest = std::fs::read(root.join("manifest.json")).unwrap();
            let pkg = read(&package(&d, &format!("{name}.zym"), &[("manifest.json", &manifest), ("plugin.wasm", &wasm), ("notes.txt", b"ignored")])).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(pkg.files.iter().all(|(n, _)| *n != "notes.txt"), "only the plugin's own files are taken");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ids() {
        assert!(valid_id("org.zym.stremio") && valid_id("com.example.my-plugin_2"));
        for bad in ["", "single", "org..x", "../x", "org/zym", "Org.Zym", "org.zym.", "a.b c"] {
            assert!(!valid_id(bad), "{bad}");
        }
    }
}
