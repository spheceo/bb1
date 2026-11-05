#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    // `bb1.exe --host` also starts sharing this PC, in the background (docs/08-gaming-host.md).
    #[cfg(windows)]
    bb1::win::app::run();
    #[cfg(not(windows))]
    eprintln!("bb1 for Windows only runs on Windows. On other systems use bb1-probe for testing.");
}
