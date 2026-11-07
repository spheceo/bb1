//! Headless test client: signs in to a host, receives video and checks it, then reports.
//! Runs on any platform, so the Windows client's protocol code is tested on a Mac against a real host:
//!   bb1-probe --host 127.0.0.1:47960 [--code-file F | --code 123456] [--seconds 6] [--out stream.h264]
//!             [--screen '\\.\DISPLAY2']   (ask the host to share that screen, and check it did)
//!             [--wiggle]   (move the host's pointer in a small circle: checks input, and that frames flow)

use bb1::core::client::{Client, Events, Stats, Target, Auth};
use bb1::core::proto::{CursorMsg, ClipboardMsg, DisplaysMsg, Msg, StreamConfig};
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
    displays: Mutex<Vec<DisplaysMsg>>,
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
    fn displays(&self, d: DisplaysMsg) {
        let list: Vec<String> = d.displays.iter().map(|x| format!("{} \"{}\" {}x{}{}", x.id, x.name, x.width, x.height, if x.primary { " main" } else { "" })).collect();
        eprintln!("displays: [{}] sharing {}", list.join(", "), d.current);
        self.displays.lock().unwrap().push(d);
    }
}

fn main() {
    bb1::core::log::setup(std::env::temp_dir(), "bb1-probe");
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let host = arg("--host").expect("--host address");
    if args.iter().any(|a| a == "--preview") {
        // The Macs list's side connection: sign in quietly, ask for a picture and a ping.
        let target = Target::new(&host, "");
        let mut ch = bb1::core::client::SideChannel::open(&target, Duration::from_secs(3)).unwrap_or_else(|e| { println!("FAIL {e}"); std::process::exit(1) });
        ch.send(&bb1::core::proto::Msg::Ping(bb1::core::proto::v(bb1::core::proto::PingMsg { seq: 1, client_us: bb1::core::now_us(), received: 0, decoded: 0, displayed: 0, rtt_ms: None }))).unwrap();
        ch.send(&bb1::core::proto::Msg::RequestPreview { max_width: 960 }).unwrap();
        let (mut pong, mut preview) = (None, None);
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < until && (pong.is_none() || preview.is_none()) {
            match ch.receive(Duration::from_millis(500)) {
                Ok(Some(bb1::core::proto::Msg::Pong(p))) => pong = Some((bb1::core::now_us() - p.v.client_us) as f64 / 1000.0),
                Ok(Some(bb1::core::proto::Msg::Preview(p))) => preview = Some(p.v),
                Ok(_) => {}
                Err(e) => { println!("FAIL {e}"); std::process::exit(1) }
            }
        }
        match (pong, preview) {
            (Some(ms), Some(p)) => println!("OK preview: host build {}, ping {ms:.1} ms, picture {}", ch.host_build,
                p.jpeg.map(|j| format!("{} KB JPEG", j.len() / 1000)).unwrap_or("none (no Screen Recording permission)".into())),
            other => { println!("FAIL pong {:?} preview {}", other.0, other.1.is_some()); std::process::exit(1) }
        }
        return;
    }
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
        displays: Mutex::new(Vec::new()),
    });
    let screen = arg("--screen");
    // (epoch when asked, frames when asked)
    let mut asked: Option<(i64, u64)> = None;
    // --wiggle: the pointer drawn into the video, so its moves show up as frames.
    let client = Client::start(target, probe.clone(), true, args.iter().any(|a| a == "--wiggle"));
    let _ = probe.client.set(client.clone());
    // --gamepads (Windows): send this PC's controllers too, as if a stream window were in front.
    #[cfg(windows)]
    if args.iter().any(|a| a == "--gamepads") {
        unsafe { std::env::set_var("BB1_GAMEPADS_ALWAYS", "1") };
        bb1::win::gamepad::start(client.clone(), None);
    }
    let wiggle = args.iter().any(|a| a == "--wiggle");
    for tick in 0..seconds * 10 {
        std::thread::sleep(Duration::from_millis(100));
        if wiggle && *probe.connected.lock().unwrap() {
            for step in 0..10 {
                let a = (tick * 10 + step) as f64 * 0.06;
                client.send(Msg::MouseMove(bb1::core::proto::v(bb1::core::proto::MouseMoveMsg { x: 0.5 + 0.04 * a.cos(), y: 0.5 + 0.08 * a.sin() })));
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if probe.ended.lock().unwrap().is_some() { break; }
        // Once the stream runs and the host has listed its screens, ask for the other one.
        if let Some(id) = &screen
            && asked.is_none() && probe.frames.load(Relaxed) > 30 && !probe.displays.lock().unwrap().is_empty()
        {
            let epoch = probe.config.lock().unwrap().as_ref().map(|c| c.epoch as i64).unwrap_or(-1);
            eprintln!("asking for screen {id} (epoch now {epoch})");
            client.send(Msg::SelectDisplay { id: id.clone() });
            asked = Some((epoch, probe.frames.load(Relaxed)));
        }
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
    if let Some(id) = &screen {
        let lists = probe.displays.lock().unwrap().clone();
        let epoch = config.as_ref().map(|c| c.epoch as i64).unwrap_or(-1);
        match (asked, lists.last()) {
            (Some((before, frames_then)), Some(last)) if last.current == *id && epoch != before && frames > frames_then => {
                println!("OK screen {id}: sharing it now ({} lists received), epoch {before} → {epoch}, {} frames since", lists.len(), frames - frames_then);
            }
            (asked, last) => {
                println!("FAIL screen {id}: asked {}, last list {:?}, epoch {epoch}", asked.is_some(), last);
                std::process::exit(1);
            }
        }
    } else if let Some(last) = probe.displays.lock().unwrap().last() {
        println!("screens: {} (sharing {})", last.displays.len(), last.current);
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
