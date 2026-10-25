fn main() {
    // Icon, manifest (modern controls, per-monitor DPI) and version info for bb1.exe.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=res");
        embed_resource::compile("res/bb1.rc", embed_resource::NONE).manifest_required().unwrap();
    }
}
