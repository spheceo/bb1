#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    bb1::win::app::run();
    #[cfg(not(windows))]
    eprintln!("bb1 for Windows only runs on Windows. On other systems use bb1-probe for testing.");
}
