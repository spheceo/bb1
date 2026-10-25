//! Headless test client: signs in to a host, receives video and checks it, then reports.
//! Runs on any platform, so the Windows client's protocol code is tested on a Mac against a real host:
//!   bb1-probe --host 127.0.0.1:47960 [--code-file F | --code 123456] [--seconds 6] [--out stream.h264]

use bb1::core::client::{Client, Events, Stats, Target, Auth};
use bb1::core::proto::{CursorMsg, ClipboardMsg, StreamConfig};
use bb1::core::video::FramePayload;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

struct Probe {
    client: OnceLock<Client>,
    code_file: Option<String>,
    code: Option<String>,
    out: Mutex<Option<std::fs::File>>,
    frames: AtomicU64,
    keyframes: AtomicU64,
    bad: AtomicU64,
    ended: Mutex<Option<String>>,
    config: Mutex<Option<StreamConfig>>,
    connected: Mutex<bool>,
}

impl Events for Probe {
    fn status(&self, text: &str, connected: bool) {
        eprintln!("status: {text}");
        if connected { *self.connected.lock().unwrap() = true; }
    }
    fn ended(&self, reason: &str) {
        eprintln!("ended: {reason}");
        *self.ended.lock().unwrap() = Some(reason.to_string());
    }
    fn ask_code(&self, host: &str) -> Option<String> {
        if let Some(c) = &self.code { return Some(c.clone()); }
        let file = self.code_file.as_ref()?;
        eprintln!("waiting for the pairing code for {host} in {file}");
        for _ in 0..200 {
            if let Ok(c) = std::fs::read_to_string(file) {
                if !c.trim().is_empty() { return Some(c.trim().to_string()); }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }
    fn video(&self, payload: FramePayload, capture_us: u64) {
        let Some(annex_b) = payload.annex_b() else { self.bad.fetch_add(1, Relaxed); return; };
        if payload.codec != 1 { self.bad.fetch_add(1, Relaxed); }
        if !payload.parameter_sets.is_empty() { self.keyframes.fetch_add(1, Relaxed); }
        self.frames.fetch_add(1, Relaxed);
        if let Some(f) = self.out.lock().unwrap().as_mut() { let _ = f.write_all(&annex_b); }
        if let Some(c) = self.client.get() { c.mark_decoded(); c.mark_displayed(capture_us); }
    }
    fn stream_config(&self, c: &StreamConfig) { *self.config.lock().unwrap() = Some(c.clone()); }
    fn notice(&self, text: &str) { eprintln!("notice: {text}"); }
    fn clipboard(&self, _c: ClipboardMsg) {}
    fn cursor(&self, c: CursorMsg) { eprintln!("cursor: {}x{} png {} bytes", c.width, c.height, c.png.len()); }
    fn stats(&self, s: &Stats) { if s.connected { eprintln!("stats: {}", s.summary()); } }
}

fn main() {
    bb1::core::log::setup(std::env::temp_dir(), "bb1-probe");
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let host = arg("--host").expect("--host address");
    let seconds: u64 = arg("--seconds").and_then(|s| s.parse().ok()).unwrap_or(6);
    let mut target = Target::new(&host, "");
    if args.iter().any(|a| a == "--pair") || arg("--code").is_some() || arg("--code-file").is_some() {
        if !matches!(target.auth, Auth::Device(_)) { target.auth = Auth::Code; }
    }
    eprintln!("signing in to {} with {}", target.address, target.auth.mode());
    let probe = Arc::new(Probe {
        client: OnceLock::new(),
        code_file: arg("--code-file"),
        code: arg("--code"),
        out: Mutex::new(arg("--out").and_then(|p| std::fs::File::create(p).ok())),
        frames: AtomicU64::new(0), keyframes: AtomicU64::new(0), bad: AtomicU64::new(0),
        ended: Mutex::new(None), config: Mutex::new(None), connected: Mutex::new(false),
    });
    let client = Client::start(target, probe.clone(), true, false);
    let _ = probe.client.set(client.clone());
    for _ in 0..seconds * 10 {
        std::thread::sleep(Duration::from_millis(100));
        if probe.ended.lock().unwrap().is_some() { break; }
    }
    let audio = client.counters().audio_packets.load(Relaxed);
    client.stop();
    std::thread::sleep(Duration::from_millis(400));
    let frames = probe.frames.load(Relaxed);
    let config = probe.config.lock().unwrap().clone();
    let ended = probe.ended.lock().unwrap().clone();
    if let Some(e) = ended.filter(|e| e != "Disconnected.") {
        println!("ended: {e}");
        std::process::exit(1);
    }
    if frames > 0 && probe.bad.load(Relaxed) == 0 {
        let c = config.unwrap();
        println!("OK {frames} frames ({} with parameter sets) {}x{} {} · audio packets seen {audio}",
                 probe.keyframes.load(Relaxed), c.width, c.height, c.codec_name());
    } else {
        println!("FAIL frames {frames}, bad {}, connected {}", probe.bad.load(Relaxed), probe.connected.lock().unwrap());
        std::process::exit(1);
    }
}
