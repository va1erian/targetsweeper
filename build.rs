//! Embeds `app.rc` (the application icon) into the executable. No manifest is
//! embedded: `Win32Backend` sets per-monitor-v2 DPI itself.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/cargo-sweep.ico");
    embed_resource::compile("app.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("compile the application resources");
}
