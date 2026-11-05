//! A bb1 host on any computer, for testing the host protocol (src/core/host.rs) without Windows or a
//! GPU: it "encodes" by replaying a recorded H.264 stream (Annex B, e.g. `bb1-probe --out`), each
//! viewer from its own position (a forced keyframe starts that viewer over), and logs input instead
//! of injecting it.
//!
//!   bb1-testhost --video /tmp/s.h264 [--port 47960] [--fps 60] [--drop 5]
//!
//! BB1_TEST_PAIRING_CODE_FILE gets the pairing code, like the Mac host.

use bb1::core::host::{Encoded, Host, HostConfig, HostEvents, InputBackend, VideoBackend};
use bb1::core::proto::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Replay {
    frames: Vec<Encoded>,
    size: (u32, u32),
    fps: u32,
    next_tick: Instant,
    positions: HashMap<u32, usize>,
}

impl VideoBackend for Replay {
    fn size(&self) -> (u32, u32) {
        self.size
    }
    fn wait_frame(&mut self, _timeout: Duration) -> Result<bool, String> {
        let now = Instant::now();
        if self.next_tick > now {
            std::thread::sleep(self.next_tick - now);
        }
        self.next_tick += Duration::from_secs_f64(1.0 / self.fps as f64);
        if self.next_tick < Instant::now() {
            self.next_tick = Instant::now();
        }
        Ok(true)
    }
    fn open_viewer(&mut self, id: u32, _fps: u32, _kbps: u32) -> Result<(), String> {
        self.positions.insert(id, 0);
        Ok(())
    }
    fn close_viewer(&mut self, id: u32) {
        self.positions.remove(&id);
    }
    fn encode(&mut self, id: u32, force_keyframe: bool) -> Result<Encoded, String> {
        let pos = self.positions.entry(id).or_insert(0);
        if force_keyframe {
            *pos = 0;
        }
        let frame = self.frames[*pos].clone();
        *pos = (*pos + 1) % self.frames.len();
        Ok(frame)
    }
    fn set_bitrate(&mut self, id: u32, kbps: u32) {
        eprintln!("testhost: viewer {id} bitrate → {kbps} kbps");
    }
}

struct LogInput;

impl InputBackend for LogInput {
    fn key(&mut self, k: &KeyMsg, command_is_windows_key: bool) {
        eprintln!("input: key {} {} (⌘ is Windows key: {command_is_windows_key})", k.code, if k.down { "down" } else { "up" });
    }
    fn mouse_move(&mut self, _m: &MouseMoveMsg) {}
    fn mouse_button(&mut self, b: &MouseButtonMsg) {
        eprintln!("input: button {} {}", b.button, if b.down { "down" } else { "up" });
    }
    fn scroll(&mut self, _s: &ScrollMsg) {}
    fn set_caps_lock(&mut self, _on: bool) {}
    fn release_all(&mut self) {}
}

struct PrintEvents;

impl HostEvents for PrintEvents {
    fn pairing_code(&self, device: &str, code: &str) {
        println!("pairing code for {device}: {code}");
    }
    fn pairing_finished(&self, paired: bool) {
        println!("pairing {}", if paired { "done" } else { "cancelled" });
    }
    fn viewers_changed(&self, names: &[String]) {
        println!("viewers: {}", names.join(", "));
    }
}

/// Annex B → frames: a frame starts at each picture slice (types 1 and 5); parameter sets ride along.
fn load(path: &str) -> Result<(Vec<Encoded>, (u32, u32)), String> {
    let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let mut nals: Vec<&[u8]> = Vec::new();
    let mut i = 0;
    let mut start: Option<usize> = None;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                let mut end = i;
                if end > s && data[end - 1] == 0 {
                    end -= 1;
                }
                nals.push(&data[s..end]);
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        nals.push(&data[s..]);
    }
    let mut frames = Vec::new();
    let mut sets: Vec<Vec<u8>> = Vec::new();
    for n in nals.into_iter().filter(|n| !n.is_empty()) {
        match n[0] & 0x1F {
            7 | 8 => sets.push(n.to_vec()),
            5 => frames.push(Encoded { keyframe: true, parameter_sets: std::mem::take(&mut sets), nals: vec![n.to_vec()] }),
            1 => frames.push(Encoded { keyframe: false, parameter_sets: Vec::new(), nals: vec![n.to_vec()] }),
            _ => {}
        }
    }
    let first = frames.iter().position(|f| f.keyframe && !f.parameter_sets.is_empty()).ok_or("no keyframe with SPS/PPS in the file")?;
    frames.drain(..first);
    Ok((frames, (1920, 1080)))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let video = arg("--video").expect("--video file.h264");
    let port: u16 = arg("--port").and_then(|p| p.parse().ok()).unwrap_or(47960);
    let fps: u32 = arg("--fps").and_then(|p| p.parse().ok()).unwrap_or(60);
    bb1::core::log::setup(std::env::temp_dir(), "bb1-testhost");
    if args.iter().any(|a| a == "--serve-logs") {
        bb1::core::logserve::start(std::env::temp_dir(), "bb1 test host".into());
    }
    let (frames, size) = load(&video).unwrap_or_else(|e| panic!("{e}"));
    eprintln!("testhost: {} frames from {video}", frames.len());
    let mut cfg = HostConfig::new(&std::env::var("BB1_HOST_NAME").unwrap_or_else(|_| "Test PC".into()), &bb1::core::store::device_id());
    cfg.control_port = port;
    cfg.video_port = port + 1;
    cfg.fps = fps;
    cfg.drop_percent = arg("--drop").and_then(|p| p.parse().ok()).unwrap_or(0.0);
    let replay = Replay { frames, size, fps, next_tick: Instant::now(), positions: HashMap::new() };
    let _host = Host::start(cfg, Box::new(replay), Box::new(LogInput), Arc::new(PrintEvents)).unwrap_or_else(|e| panic!("{e}"));
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
