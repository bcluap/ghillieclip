//! Embeds the application icon in the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico").set("ProductName", "GhillieClip").set("FileDescription", "GhillieClip");
        res.compile().expect("embedding Windows icon");
    }
}
