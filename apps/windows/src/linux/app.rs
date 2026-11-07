//! `bb1` on Linux:
//!   bb1                         the main window: your computers, connect, share this computer
//!   bb1 --host [--synthetic]    share this computer without a window (pairing codes in the terminal
//!                               and as notifications); --synthetic: a test picture, no portal
//!   bb1 --stream ADDRESS [--label NAME]   the stream window (the main window starts it)

use crate::core::host::HostEvents;
use crate::core::store::{self, Settings};
use crate::logi;
use std::sync::Arc;

pub fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let has = |name: &str| args.iter().any(|a| a == name);
    if has("--version") {
        println!("bb1 {}", crate::core::updater::build_name(crate::core::updater::BUILD));
        return;
    }
    if has("--host") {
        crate::core::log::setup(store::log_dir(), "host");
        logi!("bb1 {} (Linux) sharing from the terminal", crate::core::updater::build_name(crate::core::updater::BUILD));
        host_headless(has("--synthetic"));
    } else if let Some(address) = arg("--stream") {
        crate::core::log::setup(store::log_dir(), "client");
        let label = arg("--label").unwrap_or_default();
        crate::linux::stream::run(&address, &label);
    } else {
        crate::core::log::setup(store::log_dir(), "client");
        logi!("bb1 {} (Linux)", crate::core::updater::build_name(crate::core::updater::BUILD));
        crate::linux::ui::run();
    }
}

/// Shows a desktop notification (best effort).
pub fn notify(title: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=bb1", "--urgency=critical", "--expire-time=60000", title, body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

struct TerminalEvents;

impl HostEvents for TerminalEvents {
    fn pairing_code(&self, device: &str, code: &str) {
        println!("\n  {device} wants to connect. Type this code on it:  {code}\n");
        notify(&format!("{device} wants to connect"), &format!("Type this code on it: {code}"));
    }
    fn pairing_finished(&self, paired: bool) {
        println!("{}", if paired { "  Paired." } else { "  Pairing ended." });
    }
    fn viewers_changed(&self, names: &[String]) {
        if names.is_empty() {
            println!("  Nobody watching.");
        } else {
            println!("  Watching: {}", names.join(", "));
        }
    }
}

fn host_headless(synthetic: bool) {
    let settings = Settings::load();
    match crate::linux::sharing::Sharing::start(Arc::new(TerminalEvents), &settings, synthetic) {
        Ok(_sharing) => {
            println!("Sharing {} — connect from bb1 on your other computers. Ctrl+C stops.", store::device_name());
            loop {
                std::thread::park();
            }
        }
        Err(e) => {
            eprintln!("Couldn't share this computer: {e}");
            std::process::exit(1);
        }
    }
}
