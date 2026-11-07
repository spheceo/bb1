#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    // `bb1.exe --host` also starts sharing this PC, in the background (docs/08-gaming-host.md).
    // --apply-update and --relaunch-after-update are the update task and its helper (win/selfupdate.rs).
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == "--apply-update") {
            bb1::win::selfupdate::apply_update(&args);
        } else if args.iter().any(|a| a == "--relaunch-after-update") {
            bb1::win::selfupdate::relaunch(&args);
        } else {
            bb1::win::app::run();
        }
    }
    #[cfg(target_os = "linux")]
    bb1::linux::app::main();
    #[cfg(not(any(windows, target_os = "linux")))]
    eprintln!("bb1 runs on Windows and Linux. On other systems use bb1-probe for testing.");
}
