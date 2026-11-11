//! Embeds the app icon and version info in `cue.exe` on Windows. GPUI loads icon resource
//! 1 for every window, so this also sets the title bar, taskbar and Alt+Tab icon.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/cue.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("../../assets/cue.ico")
            .set("ProductName", "Cue")
            .set("FileDescription", "Cue")
            .set(
                "LegalCopyright",
                "Copyright © 2026 Tugay Ersoy. MIT License.",
            );
        resource
            .compile()
            .expect("failed to embed the Windows icon");
    }
}
