// Windows: build the icon and file details into the exe, so Explorer, "Open with", the taskbar
// and Alt-Tab show the player's icon and name. Nothing to do on other platforms.
fn main() {
    println!("cargo:rerun-if-changed=packaging/zym-media-player.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/zym-media-player.ico");
        res.set("FileDescription", "Zym Media Player");
        res.set("ProductName", "Zym Media Player");
        res.compile().expect("Windows resources (icon)");
    }
}
