fn main() {
    // The build number (commit count, as on the Mac), for updates. BB1_BUILD overrides it.
    println!("cargo:rerun-if-env-changed=BB1_BUILD");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    let build = std::env::var("BB1_BUILD").ok().or_else(|| {
        let out = std::process::Command::new("git").args(["rev-list", "--count", "HEAD"]).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    });
    if let Some(b) = build.filter(|b| !b.is_empty() && b.chars().all(|c| c.is_ascii_digit())) {
        println!("cargo:rustc-env=BB1_BUILD={b}");
    }
    // Icon, manifest (modern controls, per-monitor DPI) and version info for bb1.exe.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=res");
        embed_resource::compile("res/bb1.rc", embed_resource::NONE).manifest_required().unwrap();
    }
}
