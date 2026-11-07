//! Sharing this Linux computer: joins the desktop portal (portal.rs), PipeWire capture
//! (capture.rs), the encoder (encoder.rs), input (input.rs) and sound (audio_in.rs) to the protocol
//! host in core/host.rs, which Mac, Windows and Linux clients already speak.

use crate::core::host::{Encoded, Host, HostConfig, HostEvents, InputBackend, VideoBackend};
use crate::core::proto::*;
use crate::core::store;
use crate::linux::audio_in::SoundCapture;
use crate::linux::capture::Capture;
use crate::linux::encoder::Encoder;
use crate::linux::input::PortalInput;
use crate::linux::portal::Portal;
use crate::logi;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

// MARK: Video: one capture, an encoder per viewer

struct ScreenVideo {
    capture: Capture,
    encoders: HashMap<u32, Encoder>,
    fps: u32,
    /// The desktop sends the pointer beside the frames, so it can be left out.
    pointer_meta: bool,
}

impl VideoBackend for ScreenVideo {
    fn size(&self) -> (u32, u32) {
        let (w, h) = self.capture.size();
        (w & !1, h & !1)
    }

    fn wait_frame(&mut self, timeout: Duration) -> Result<bool, String> {
        self.capture.wait(timeout)
    }

    fn open_viewer(&mut self, id: u32, fps: u32, bitrate_kbps: u32) -> Result<(), String> {
        let (w, h) = self.capture.size();
        self.fps = fps;
        let rgb = self.capture.picture.rgb;
        self.encoders.insert(id, Encoder::new(w, h, fps, bitrate_kbps, rgb)?);
        Ok(())
    }

    fn close_viewer(&mut self, id: u32) {
        self.encoders.remove(&id);
    }

    fn encode(&mut self, id: u32, force_keyframe: bool) -> Result<Encoded, String> {
        let p = &self.capture.picture;
        if p.data.is_empty() {
            return Err("no picture captured yet".into());
        }
        let encoder = self.encoders.get_mut(&id).ok_or("no encoder for this viewer")?;
        if encoder.width > p.width || encoder.height > p.height {
            return Err("the screen changed size".into());
        }
        let f = encoder.encode(&p.data, p.stride, force_keyframe)?;
        Ok(Encoded { keyframe: f.keyframe, parameter_sets: f.parameter_sets, nals: f.nals })
    }

    fn set_bitrate(&mut self, id: u32, kbps: u32) {
        if let Some(e) = self.encoders.get_mut(&id) {
            e.set_bitrate(kbps);
        }
    }

    fn set_cursor_in_video(&mut self, on: bool) {
        if self.pointer_meta {
            self.capture.draw_pointer = on;
        }
    }
}

// MARK: A test picture (bb1 --host --synthetic): no portal, no screen

struct SyntheticVideo {
    width: u32,
    height: u32,
    data: Vec<u8>,
    encoders: HashMap<u32, Encoder>,
    started: Instant,
    next: Instant,
}

impl SyntheticVideo {
    fn new(width: u32, height: u32) -> SyntheticVideo {
        SyntheticVideo { width, height, data: vec![0; (width * height * 4) as usize], encoders: HashMap::new(), started: Instant::now(), next: Instant::now() }
    }
}

impl VideoBackend for SyntheticVideo {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn wait_frame(&mut self, timeout: Duration) -> Result<bool, String> {
        let now = Instant::now();
        if self.next > now {
            std::thread::sleep((self.next - now).min(timeout));
        }
        self.next += Duration::from_millis(16);
        let t = self.started.elapsed().as_secs_f64();
        let bar = ((t * 300.0) as u32) % self.width;
        for y in 0..self.height {
            for x in 0..self.width {
                let i = ((y * self.width + x) * 4) as usize;
                let on = x.abs_diff(bar) < 40;
                self.data[i] = if on { 255 } else { (x * 255 / self.width) as u8 };
                self.data[i + 1] = (y * 255 / self.height) as u8;
                self.data[i + 2] = if on { 255 } else { 90 };
            }
        }
        Ok(true)
    }
    fn open_viewer(&mut self, id: u32, fps: u32, bitrate_kbps: u32) -> Result<(), String> {
        self.encoders.insert(id, Encoder::new(self.width, self.height, fps, bitrate_kbps, false)?);
        Ok(())
    }
    fn close_viewer(&mut self, id: u32) {
        self.encoders.remove(&id);
    }
    fn encode(&mut self, id: u32, force_keyframe: bool) -> Result<Encoded, String> {
        let e = self.encoders.get_mut(&id).ok_or("no encoder")?;
        let f = e.encode(&self.data, self.width as usize * 4, force_keyframe)?;
        Ok(Encoded { keyframe: f.keyframe, parameter_sets: f.parameter_sets, nals: f.nals })
    }
    fn set_bitrate(&mut self, id: u32, kbps: u32) {
        if let Some(e) = self.encoders.get_mut(&id) {
            e.set_bitrate(kbps);
        }
    }
}

struct NoInput;

impl InputBackend for NoInput {
    fn key(&mut self, _k: &KeyMsg, _c: bool) {}
    fn mouse_move(&mut self, _m: &MouseMoveMsg) {}
    fn mouse_button(&mut self, _b: &MouseButtonMsg) {}
    fn scroll(&mut self, _s: &ScrollMsg) {}
    fn set_caps_lock(&mut self, _on: bool) {}
    fn release_all(&mut self) {}
}

// MARK: Sharing

/// This computer being shared. Stop it with `stop`.
pub struct Sharing {
    host: Host,
    _audio: Option<SoundCapture>,
    _portal: Option<Arc<Portal>>,
}

impl Sharing {
    /// Start sharing: asks the desktop for the screen and remote input (a dialog the first time),
    /// then listens for viewers. `synthetic`: a moving test picture instead (no portal).
    pub fn start(events: Arc<dyn HostEvents>, settings: &store::Settings, synthetic: bool) -> Result<Sharing, String> {
        crate::linux::init_libraries();
        let name = store::device_name();
        let mut cfg = HostConfig::new(&name, &store::device_id());
        cfg.allow_tailscale = settings.allow_tailscale;
        cfg.allow_pairing = settings.allow_pairing;
        if let Some(port) = std::env::var("BB1_PORT").ok().and_then(|p| p.parse().ok()) {
            cfg.control_port = port;
            cfg.video_port = port + 1;
        }
        let (video, input, portal): (Box<dyn VideoBackend>, Box<dyn InputBackend>, Option<Arc<Portal>>) = if synthetic {
            (Box::new(SyntheticVideo::new(1280, 720)), Box::new(NoInput), None)
        } else {
            let portal = Arc::new(Portal::open(Duration::from_secs(300))?);
            let fd = portal.take_fd().ok_or("no PipeWire socket")?;
            let mut capture = Capture::start(fd, portal.node_id).map_err(|e| format!("couldn't capture the screen: {e}"))?;
            let pointer_meta = portal.cursor_mode == crate::linux::portal::CURSOR_METADATA;
            capture.draw_pointer = true;
            let (w, h) = capture.size();
            logi!("host: screen {w}x{h}");
            let video = ScreenVideo { capture, encoders: HashMap::new(), fps: 60, pointer_meta };
            (Box::new(video), Box::new(PortalInput::new(portal.clone())), Some(portal))
        };
        let host = Host::start(cfg, video, input, events)?;
        let h = host.clone();
        let audio = match SoundCapture::start(move |pcm, us| h.audio_chunk(pcm, us)) {
            Ok(a) => Some(a),
            Err(e) => {
                logi!("host: no sound: {e}");
                None
            }
        };
        logi!("host: sharing {name}");
        Ok(Sharing { host, _audio: audio, _portal: portal })
    }

    pub fn apply_settings(&self, settings: &store::Settings) {
        self.host.set_policy(settings.allow_tailscale, settings.allow_pairing);
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn stop(self) {
        self.host.stop();
        logi!("host: stopped sharing");
    }
}
