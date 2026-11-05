//! Sharing this PC: joins the Windows parts (capture.rs, nvenc.rs, input.rs, audio_in.rs) to the
//! protocol host in core/host.rs. Runs inside the bb1 app; "Share this PC" in the notification-area
//! icon's menu (win/tray.rs) starts and stops it.

use crate::core::host::{Encoded, Host, HostConfig, HostEvents, InputBackend, VideoBackend};
use crate::core::proto::*;
use crate::core::store;
use crate::host::audio_in::Loopback;
use crate::host::capture::Capture;
use crate::host::input::Injector;
use crate::host::nvenc::{Codec, Encoder, EncoderConfig};
use crate::logi;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};

// MARK: Video: one capture, an NVENC encoder per viewer

struct NvidiaVideo {
    capture: Capture,
    encoders: HashMap<u32, Encoder>,
    latest: Option<ID3D11Texture2D>,
}

// The D3D11 device is multithread-protected (capture.rs) and only the video thread uses this.
unsafe impl Send for NvidiaVideo {}

impl VideoBackend for NvidiaVideo {
    fn size(&self) -> (u32, u32) {
        self.capture.size()
    }

    fn wait_frame(&mut self, timeout: Duration) -> Result<bool, String> {
        match self.capture.next(timeout.as_millis() as u32)? {
            Some(texture) => {
                self.latest = Some(texture);
                Ok(true)
            }
            None => {
                if self.latest.is_none() {
                    self.latest = self.capture.latest();
                }
                Ok(false)
            }
        }
    }

    fn open_viewer(&mut self, id: u32, fps: u32, bitrate_kbps: u32) -> Result<(), String> {
        let (width, height) = self.capture.size();
        let device = self.capture.device();
        let encoder = Encoder::new(&device, EncoderConfig { width, height, fps, bitrate_kbps, codec: Codec::H264 })?;
        self.encoders.insert(id, encoder);
        Ok(())
    }

    fn close_viewer(&mut self, id: u32) {
        self.encoders.remove(&id);
    }

    fn encode(&mut self, id: u32, force_keyframe: bool) -> Result<Encoded, String> {
        let texture = self.latest.clone().or_else(|| self.capture.latest()).ok_or("no picture captured yet")?;
        let encoder = self.encoders.get_mut(&id).ok_or("no encoder for this viewer")?;
        let f = encoder.encode(&texture, force_keyframe)?;
        Ok(Encoded { keyframe: f.keyframe, parameter_sets: f.parameter_sets, nals: f.nals })
    }

    fn set_bitrate(&mut self, id: u32, kbps: u32) {
        if let Some(e) = self.encoders.get_mut(&id) {
            if let Err(err) = e.set_bitrate(kbps) {
                logi!("encoder: couldn't change the bitrate: {err}");
            }
        }
    }

    fn set_cursor_in_video(&mut self, on: bool) {
        self.capture.set_draw_cursor(on);
    }
}

// MARK: Input

struct WindowsInput(Injector);

unsafe impl Send for WindowsInput {}

impl InputBackend for WindowsInput {
    fn key(&mut self, k: &KeyMsg, command_is_windows_key: bool) {
        self.0.set_command_is_windows_key(command_is_windows_key);
        self.0.key(k);
    }
    fn mouse_move(&mut self, m: &MouseMoveMsg) {
        self.0.mouse_move(m);
    }
    fn mouse_button(&mut self, b: &MouseButtonMsg) {
        self.0.mouse_button(b);
    }
    fn scroll(&mut self, s: &ScrollMsg) {
        self.0.scroll(s);
    }
    fn set_caps_lock(&mut self, on: bool) {
        self.0.set_caps_lock(on);
    }
    fn release_all(&mut self) {
        self.0.release_all();
    }
}

// MARK: Sharing

/// This PC being shared. Stop it with `stop`.
pub struct Sharing {
    host: Host,
    audio: Option<Loopback>,
}


impl Sharing {
    /// Start sharing: checks for an NVIDIA encoder, opens the screen, listens for viewers.
    pub fn start(events: Arc<dyn HostEvents>, settings: &store::Settings) -> Result<Sharing, String> {
        let name = store::device_name();
        if !Encoder::is_available() {
            return Err("this PC needs an NVIDIA graphics card with an up-to-date driver to share its screen".into());
        }
        let capture = Capture::new(0).map_err(|e| format!("couldn't capture the screen: {e}"))?;
        let video = NvidiaVideo { capture, encoders: HashMap::new(), latest: None };
        let mut cfg = HostConfig::new(&name, &store::device_id());
        cfg.allow_tailscale = settings.allow_tailscale;
        cfg.allow_pairing = settings.allow_pairing;
        let host = Host::start(cfg, Box::new(video), Box::new(WindowsInput(Injector::new())), events)?;
        unsafe {
            // 1 ms timer resolution: packet pacing sleeps for fractions of a frame.
            timeBeginPeriod(1);
        }
        let h = host.clone();
        let audio = match Loopback::start(move |pcm, us| h.audio_chunk(pcm, us)) {
            Ok(a) => Some(a),
            Err(e) => {
                logi!("host: no sound: {e}");
                None
            }
        };
        logi!("host: sharing {name}");
        let sharing = Sharing { host, audio };
        sharing.apply_settings(settings);
        Ok(sharing)
    }

    /// Who can connect, and which apps' sound is left out.
    pub fn apply_settings(&self, settings: &store::Settings) {
        self.host.set_policy(settings.allow_tailscale, settings.allow_pairing);
        if let Some(a) = &self.audio {
            a.set_excluded(crate::win::apps::exe_names_for(&settings.muted_apps));
        }
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn stop(self) {
        self.host.stop();
        unsafe {
            timeEndPeriod(1);
        }
        logi!("host: stopped sharing");
    }
}
