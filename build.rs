//! Embeds the app icon into the Windows executable (Explorer, taskbar, shortcuts).

fn main() {
    println!("cargo:rerun-if-changed=assets/icons/rustcut.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icons/rustcut.ico");
        res.set("ProductName", "RustCut");
        res.set("FileDescription", "RustCut video editor");
        if let Err(e) = res.compile() {
            // Builds without the Windows SDK still work, just without an embedded icon.
            println!("cargo:warning=could not embed the app icon: {e}");
        }
    }
}
