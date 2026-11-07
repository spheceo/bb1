//! The host side of the protocol: sign-in, sessions, video and sound to each viewer. A port of the
//! Mac host (apps/mac/Sources/bb1/Host/HostController.swift + BB1Core/ControlChannel.swift), so the
//! existing Mac and Windows clients connect to a Windows host unchanged.
//!
//! It builds on any platform: screen capture and encoding sit behind `VideoBackend`, input behind
//! `InputBackend` (the Windows ones are in src/host). Tests run it on a Mac with a recorded H.264
//! file standing in for the encoder (src/bin/bb1-testhost.rs).
//!
//! Unlike the Mac host, every viewer has its own encoder: its own bitrate (following its own
//! connection), keyframes and pacing, so one friend on a slow line doesn't drag the others down
//! (docs/08-gaming-host.md).
//!
//! Threads: one accepts connections; each connection has one for its control channel; one receives
//! UDP hellos; one captures and encodes (it owns the VideoBackend); each viewer has a sender that
//! paces its packets; a watchdog checks heartbeats, logs stats and adapts each viewer's bitrate.

use crate::core::client::{read_frame, write_frame};
use crate::core::crypto::{self, Ephemeral, FrameCrypto, SessionKeys, StreamSealer};
use crate::core::proto::*;
use crate::core::store::support_dir;
use crate::core::video::{KEYFRAME_FLAG, VIDEO_TYPE};
use crate::core::{CONTROL_PORT, MAX_DATAGRAM, PROTOCOL_VERSION, now_us, random_bytes, tailscale};
use crate::logi;
use base64::Engine;
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const VIDEO_PORT: u16 = CONTROL_PORT + 1;
pub const MAX_VIEWERS: usize = 4;
const HEADER: usize = 20;

/// One encoded picture.
#[derive(Clone, Debug, Default)]
pub struct Encoded {
    pub keyframe: bool,
    /// SPS, PPS (HEVC: VPS, SPS, PPS) without start codes; on keyframes.
    pub parameter_sets: Vec<Vec<u8>>,
    /// The picture's NAL units without start codes.
    pub nals: Vec<Vec<u8>>,
}

/// Screen capture + one encoder per viewer. Used only from the video thread.
pub trait VideoBackend: Send + 'static {
    /// Current screen size in pixels.
    fn size(&self) -> (u32, u32);
    /// Wait up to `timeout` for the screen to change. Ok(true) = there's a new picture to encode.
    fn wait_frame(&mut self, timeout: Duration) -> Result<bool, String>;
    /// An encoder for this viewer at the current screen size.
    fn open_viewer(&mut self, id: u32, fps: u32, bitrate_kbps: u32) -> Result<(), String>;
    fn close_viewer(&mut self, id: u32);
    /// Encode the latest picture for this viewer.
    fn encode(&mut self, id: u32, force_keyframe: bool) -> Result<Encoded, String>;
    fn set_bitrate(&mut self, id: u32, kbps: u32);
    /// Mac clients draw their own pointer when they say so; most backends always draw it in.
    fn set_cursor_in_video(&mut self, _on: bool) {}
    /// This computer's screens (empty: the backend can't switch).
    fn displays(&self) -> Vec<DisplayInfo> {
        Vec::new()
    }
    /// The id of the screen being shared.
    fn current_display(&self) -> String {
        String::new()
    }
    /// Share another screen. The size may change; the host then starts new encoders by itself.
    fn select_display(&mut self, _id: &str) -> Result<(), String> {
        Err("this host can't switch screens".into())
    }
    /// Where the shared screen is on the desktop (left, top, width, height), for the mouse.
    fn display_rect(&self) -> Option<(i32, i32, i32, i32)> {
        None
    }
}

/// Keyboard and mouse into this computer. Shared by every viewer.
pub trait InputBackend: Send + 'static {
    fn key(&mut self, k: &KeyMsg, command_is_windows_key: bool);
    fn mouse_move(&mut self, m: &MouseMoveMsg);
    fn mouse_button(&mut self, b: &MouseButtonMsg);
    fn scroll(&mut self, s: &ScrollMsg);
    fn set_caps_lock(&mut self, on: bool);
    fn release_all(&mut self);
    /// The shared screen moved or changed (left, top, width, height on the desktop): mouse positions
    /// (0…1) are on it.
    fn set_screen(&mut self, _left: i32, _top: i32, _width: i32, _height: i32) {}
    /// A viewer's game controller (each viewer's pads are separate virtual controllers).
    fn gamepad(&mut self, _viewer: u32, _g: &GamepadMsg) {}
    /// Let go of everything on this viewer's controllers (its access was turned off).
    fn release_gamepads(&mut self, _viewer: u32) {}
    /// The viewer left: unplug its virtual controllers.
    fn viewer_left(&mut self, _viewer: u32) {}
    /// Where to send rumble for a viewer's controller (set once, at start).
    fn set_rumble_sink(&mut self, _sink: Box<dyn Fn(u32, RumbleMsg) + Send + Sync>) {}
}

/// What the host's window shows.
pub trait HostEvents: Send + Sync + 'static {
    fn pairing_code(&self, device: &str, code: &str);
    fn pairing_finished(&self, paired: bool);
    fn viewers_changed(&self, names: &[String]);
    /// Another computer asked this one to update to `build` (newer than this one). Ok: it's being
    /// installed and this app will quit and come back as that build.
    fn update_requested(&self, _build: i64) -> Result<(), String> {
        Err("update bb1 on the PC from its own window".into())
    }
}

/// What a viewer may do on this computer. All on when it connects; the owner can turn each off live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Mouse,
    Keyboard,
    /// Game controllers (coming: the flag is kept and shown already).
    Controller,
}

impl Access {
    pub fn label(self) -> &'static str {
        match self {
            Access::Mouse => "mouse",
            Access::Keyboard => "keyboard",
            Access::Controller => "controller",
        }
    }
}

/// A connected viewer, for the owner's list.
#[derive(Clone, Debug, PartialEq)]
pub struct Viewer {
    pub id: u32,
    pub name: String,
    pub mouse: bool,
    pub keyboard: bool,
    pub controller: bool,
}

#[derive(Clone)]
pub struct HostConfig {
    pub name: String,
    pub host_id: String,
    pub control_port: u16,
    pub video_port: u16,
    pub fps: u32,
    /// None: by resolution (30 Mbps at 1080p).
    pub bitrate_mbps: Option<u32>,
    pub allow_tailscale: bool,
    pub allow_pairing: bool,
    /// Packets to drop on purpose, in percent (tests).
    pub drop_percent: f64,
}

impl HostConfig {
    pub fn new(name: &str, host_id: &str) -> HostConfig {
        HostConfig {
            name: name.into(),
            host_id: host_id.into(),
            control_port: CONTROL_PORT,
            video_port: VIDEO_PORT,
            fps: 60,
            bitrate_mbps: None,
            allow_tailscale: true,
            allow_pairing: true,
            drop_percent: 0.0,
        }
    }
}

/// Full-quality bitrate for a screen size (the Mac host's numbers: 30 Mbps at 1080p).
pub fn default_bitrate_mbps(width: u32, height: u32) -> u32 {
    let mp = (width * height) as f64 / 1_000_000.0;
    ((mp / 2.07 * 30.0).round() as u32).clamp(10, 80)
}

// MARK: Paired devices (this host remembers who may sign in without a code)

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct PairedDevice {
    id: String,
    name: String,
    secret: String, // base64
    via: String,
}

fn paired_path() -> std::path::PathBuf {
    support_dir().join("paired-devices.json")
}

fn paired_load() -> Vec<PairedDevice> {
    std::fs::read(paired_path()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
}

fn paired_save(list: &[PairedDevice]) {
    if let Ok(d) = serde_json::to_vec_pretty(list) {
        let tmp = paired_path().with_extension("tmp");
        if std::fs::write(&tmp, d).is_ok() {
            let _ = std::fs::rename(tmp, paired_path());
        }
    }
}

/// How many computers sign in here without a code.
pub fn paired_device_count() -> usize {
    paired_load().len()
}

pub fn forget_paired_devices() {
    paired_save(&[]);
}

// MARK: Sessions

struct Counters {
    sent: AtomicU64,
    sent_bytes: AtomicU64,
    encoded: AtomicU64,
    nacks: AtomicU64,
    resent: AtomicU64,
    // Input since the last stats line (counts only, never what was pressed).
    keys: AtomicU64,
    mouse: AtomicU64,
    pads: AtomicU64,
    /// Controller updates that arrived while the owner had this viewer's controller switched off.
    pads_blocked: AtomicU64,
    rumble: AtomicU64,
}

struct Session {
    id: u32,
    name: String,
    peer_id: String,
    /// A Windows client (it says it only decodes H.264): its Windows key arrives as ⌘.
    windows_client: bool,
    keys: SessionKeys,
    writer: Mutex<(TcpStream, StreamSealer)>,
    udp_peer: Mutex<Option<SocketAddr>>,
    last_ping_us: AtomicU64,
    key_requested: AtomicBool,
    audio: AtomicBool,
    audio_seq: AtomicU32,
    recent_audio: Mutex<VecDeque<Vec<u8>>>,
    ended: AtomicBool,
    mouse: AtomicBool,
    keyboard: AtomicBool,
    controller: AtomicBool,
    /// Recently sent frames, for resends.
    recent: Mutex<HashMap<u32, (u64, Arc<Vec<Vec<u8>>>)>>,
    frames: Sender<Arc<Vec<Vec<u8>>>>,
    counters: Counters,
    /// Share of full quality this viewer's connection carries (watchdog thread).
    rate: Mutex<RateState>,
    /// Bytes per second for bursts (pacing).
    pace: AtomicU64,
    /// Video packets sent in the last second (for the resend budget).
    pps: AtomicU64,
    /// Resends allowed right now (a token bucket refilled with ~30% of the stream), and when it was refilled.
    resend_tokens: Mutex<(f64, Instant)>,
    /// Resends skipped since the last stats line: over budget, or for a frame too old to help.
    resends_skipped: AtomicU64,
    /// The viewer's round trips (from its pings), to notice its line backing up before frames are late.
    delay: Mutex<DelayState>,
    baseline: Mutex<(u64, u64, u64, u64)>,
    epoch: AtomicU32,
}

#[derive(Default)]
struct DelayState {
    /// (when, round trip ms), the last 30 s: the lowest is this connection's normal round trip.
    samples: VecDeque<(Instant, f64)>,
    /// Pings in a row with the round trip well above normal.
    high: u32,
    /// When the bitrate was last cut for it (at most one cut a second; no climbing back for 3 s after).
    last_cut: Option<Instant>,
}

#[derive(Clone, Copy)]
struct RateState {
    rate: f64,
    clean_seconds: u32,
}

impl Session {
    fn send(&self, msg: &Msg) {
        if self.ended.load(Relaxed) {
            return;
        }
        let mut w = self.writer.lock().unwrap();
        let (stream, sealer) = &mut *w;
        let sealed = sealer.seal(&msg.encode());
        if write_frame(stream, &sealed).is_err() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

enum VideoCommand {
    /// (screen id, who asked)
    SelectDisplay(String, String),
    Open(Arc<Session>),
    Close(u32),
    Bitrate(u32, u32),
    CursorInVideo(bool),
}

struct Shared {
    cfg: HostConfig,
    events: Arc<dyn HostEvents>,
    input: Mutex<Box<dyn InputBackend>>,
    sessions: Mutex<Vec<Arc<Session>>>,
    udp: UdpSocket,
    video: Sender<VideoCommand>,
    pairing: Mutex<Option<(u64, String)>>, // (connection, device ID or name)
    next_session: AtomicU32,
    next_connection: AtomicU64,
    captured: AtomicU64,
    encode_us: Mutex<(u64, u64, u64)>, // (total, count, max)
    size: Mutex<(u32, u32)>,
    stopped: AtomicBool,
    own_login: Mutex<Option<(String, Instant)>>,
    /// Each computer's last quality (share of full bitrate) when it left, by device ID.
    last_rates: Mutex<HashMap<String, (f64, Instant)>>,
    /// Announces this host on the home network (Bonjour), like the Mac host.
    bonjour: Option<mdns_sd::ServiceDaemon>,
    /// Who may sign in (changeable while sharing).
    allow_tailscale: AtomicBool,
    allow_pairing: AtomicBool,
    /// The screens and which one is shared, as last sent to viewers.
    displays: Mutex<Option<DisplaysMsg>>,
}

/// A running host. Dropping it doesn't stop it; call `stop`.
#[derive(Clone)]
pub struct Host {
    shared: Arc<Shared>,
}

impl Host {
    pub fn start(cfg: HostConfig, video: Box<dyn VideoBackend>, input: Box<dyn InputBackend>, events: Arc<dyn HostEvents>) -> Result<Host, String> {
        let listener = TcpListener::bind(("0.0.0.0", cfg.control_port)).map_err(|e| format!("can't listen on port {} ({e})", cfg.control_port))?;
        let udp = UdpSocket::bind(("0.0.0.0", cfg.video_port)).map_err(|e| format!("can't listen on port {} ({e})", cfg.video_port))?;
        let _ = udp.set_read_timeout(Some(Duration::from_millis(500)));
        let (video_tx, video_rx) = unbounded();
        let size = video.size();
        let shared = Arc::new(Shared {
            cfg: cfg.clone(),
            events,
            input: Mutex::new(input),
            sessions: Mutex::new(Vec::new()),
            udp,
            video: video_tx,
            pairing: Mutex::new(None),
            next_session: AtomicU32::new(u32::from_le_bytes(random_bytes::<4>()) / 2),
            next_connection: AtomicU64::new(1),
            captured: AtomicU64::new(0),
            encode_us: Mutex::new((0, 0, 0)),
            size: Mutex::new(size),
            stopped: AtomicBool::new(false),
            own_login: Mutex::new(None),
            bonjour: advertise(&cfg.name, cfg.control_port),
            allow_tailscale: AtomicBool::new(cfg.allow_tailscale),
            allow_pairing: AtomicBool::new(cfg.allow_pairing),
            displays: Mutex::new(None),
            last_rates: Mutex::new(HashMap::new()),
        });
        // Rumble from the virtual controllers goes back to the viewer that owns them.
        {
            let weak = Arc::downgrade(&shared);
            shared.input.lock().unwrap().set_rumble_sink(Box::new(move |viewer, r| {
                let Some(shared) = weak.upgrade() else { return };
                let s = shared.sessions.lock().unwrap().iter().find(|s| s.id == viewer).cloned();
                if let Some(s) = s {
                    s.counters.rumble.fetch_add(1, Relaxed);
                    s.send(&Msg::Rumble(v(r)));
                }
            }));
        }
        logi!("host: {} sharing a {}x{} screen on ports {} and {}", cfg.name, size.0, size.1, cfg.control_port, cfg.video_port);

        let s = shared.clone();
        spawn("bb1 accept", move || accept_loop(s, listener));
        let s = shared.clone();
        spawn("bb1 udp", move || udp_loop(s));
        let s = shared.clone();
        spawn("bb1 video", move || video_loop(s, video, video_rx));
        let s = shared.clone();
        spawn("bb1 watchdog", move || watchdog(s));
        Ok(Host { shared })
    }

    /// 5 ms of sound (960 bytes: 240 frames, stereo, i16). Sent to viewers that asked for sound.
    /// Digital silence isn't sent (the client plays silence when nothing arrives), but it's counted, so
    /// the client can tell a pause from a lost packet.
    pub fn audio_chunk(&self, pcm: &[u8], capture_us: u64) {
        let silent = pcm.chunks_exact(2).all(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs() <= 2);
        let sessions = self.shared.sessions.lock().unwrap().clone();
        for s in sessions {
            if !s.audio.load(Relaxed) {
                continue;
            }
            let Some(peer) = *s.udp_peer.lock().unwrap() else { continue };
            let seq = s.audio_seq.fetch_add(1, Relaxed).wrapping_add(1);
            let mut recent = s.recent_audio.lock().unwrap();
            if silent {
                recent.clear();
                continue;
            }
            let packet = crypto::seal_audio(seq, capture_us, pcm, &s.keys.video);
            let _ = self.shared.udp.send_to(&packet, peer);
            // Each packet goes out a second time two packets (10 ms) later: one lost packet isn't a gap.
            recent.push_back(packet);
            if recent.len() > 2 {
                if let Some(old) = recent.pop_front() {
                    let _ = self.shared.udp.send_to(&old, peer);
                }
            }
        }
    }

    /// Who may sign in from now on (people already connected stay).
    pub fn set_policy(&self, allow_tailscale: bool, allow_pairing: bool) {
        self.shared.allow_tailscale.store(allow_tailscale, Relaxed);
        self.shared.allow_pairing.store(allow_pairing, Relaxed);
    }

    pub fn viewers(&self) -> Vec<Viewer> {
        self.shared.sessions.lock().unwrap().iter().map(|s| Viewer {
            id: s.id,
            name: s.name.clone(),
            mouse: s.mouse.load(Relaxed),
            keyboard: s.keyboard.load(Relaxed),
            controller: s.controller.load(Relaxed),
        }).collect()
    }

    /// Let a viewer use the mouse, keyboard or controllers, or not. Turning one off lets go of
    /// whatever it was holding, and the viewer is told.
    pub fn set_access(&self, id: u32, what: Access, on: bool) {
        let Some(s) = self.shared.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() else { return };
        let flag = match what {
            Access::Mouse => &s.mouse,
            Access::Keyboard => &s.keyboard,
            Access::Controller => &s.controller,
        };
        if flag.swap(on, Relaxed) == on {
            return;
        }
        logi!("host: {} for {} {}", what.label(), s.name, if on { "allowed" } else { "turned off" });
        if !on {
            let mut input = self.shared.input.lock().unwrap();
            if what == Access::Controller {
                input.release_gamepads(id);
            } else {
                input.release_all();
            }
        }
        let host = &self.shared.cfg.name;
        s.send(&Msg::Notice(v(if on {
            format!("{host} gave you back the {}.", what.label())
        } else {
            format!("{host} turned off your {}.", what.label())
        })));
        let names: Vec<String> = self.shared.sessions.lock().unwrap().iter().map(|s| s.name.clone()).collect();
        self.shared.events.viewers_changed(&names);
    }

    /// Disconnect one viewer (it's told why, so it doesn't reconnect).
    pub fn disconnect(&self, id: u32) {
        let Some(s) = self.shared.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() else { return };
        end_session(&self.shared, &s, "disconnected by the host", Some(&format!("{} disconnected you", self.shared.cfg.name)));
    }

    pub fn cancel_pairing(&self) {
        *self.shared.pairing.lock().unwrap() = None;
    }

    /// Disconnect everyone (they're told why, so they don't reconnect).
    pub fn disconnect_all(&self, why: &str) {
        let sessions = self.shared.sessions.lock().unwrap().clone();
        for s in sessions {
            end_session(&self.shared, &s, why, Some(why));
        }
    }

    pub fn stop(&self) {
        self.disconnect_all("sharing stopped on the host");
        self.shared.stopped.store(true, Relaxed);
        // Wake the accept loop so it lets go of the port (sharing can start again at once).
        let _ = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], self.shared.cfg.control_port)), Duration::from_millis(300));
        if let Some(d) = &self.shared.bonjour {
            let _ = d.shutdown();
        }
    }
}

/// "_bb1._tcp" with this host's name, so clients on the home network list it.
fn advertise(name: &str, port: u16) -> Option<mdns_sd::ServiceDaemon> {
    let daemon = mdns_sd::ServiceDaemon::new().ok()?;
    let host: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let info = mdns_sd::ServiceInfo::new(crate::core::BONJOUR_TYPE, name, &format!("{host}.local."), "", port, None::<HashMap<String, String>>)
        .ok()?
        .enable_addr_auto();
    match daemon.register(info) {
        Ok(()) => Some(daemon),
        Err(e) => {
            logi!("host: couldn't announce on the home network ({e})");
            None
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().name(name.into()).spawn(f).expect("couldn't start a thread");
}

// MARK: Connections and sign-in

fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    for conn in listener.incoming() {
        if shared.stopped.load(Relaxed) {
            return;
        }
        let Ok(stream) = conn else { continue };
        let s = shared.clone();
        spawn("bb1 control", move || {
            let peer = stream.peer_addr().ok();
            if let Err(e) = connection(&s, stream) {
                logi!("host: connection from {} ended: {e}", peer.map(|p| p.to_string()).unwrap_or_default());
            }
        });
    }
}

struct Authed {
    keys: SessionKeys,
    hello: ClientHello,
    mode: String,
}

fn reject(stream: &mut TcpStream, why: &str) -> String {
    let _ = write_frame(stream, &Msg::AuthFailed(v(why.to_string())).encode());
    std::thread::sleep(Duration::from_millis(300));
    why.to_string()
}

/// Version 4 handshake, host side (BB1Core/ControlChannel.swift).
fn handshake(shared: &Arc<Shared>, stream: &mut TcpStream, connection_id: u64) -> Result<Authed, String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let body = read_frame(stream)?;
    let hello = match Msg::decode(&body) {
        Some(Msg::ClientHello(h)) => h.v,
        _ => return Err("expected a hello".into()),
    };
    let Some(client_pub) = hello.ecdh_pub.clone().filter(|_| hello.version >= 4 && hello.nonce.len() == 32) else {
        return Err(reject(stream, &format!("version mismatch (client {}, host {PROTOCOL_VERSION}): update bb1 there", hello.version)));
    };
    let mode = hello.mode.clone().unwrap_or_else(|| "key".into());
    let device_id = hello.device_id.clone().unwrap_or_default();
    let remote = stream.peer_addr().ok().map(|a| a.ip());
    let cfg = &shared.cfg;

    let secret: Vec<u8> = match mode.as_str() {
        "device" => match paired_load().into_iter().find(|d| d.id == device_id) {
            Some(d) => base64::engine::general_purpose::STANDARD.decode(d.secret).unwrap_or_default(),
            None => return Err(reject(stream, &format!("this PC doesn't remember {} any more. Pair it again.", hello.name))),
        },
        "tailscale" => {
            if !shared.allow_tailscale.load(Relaxed) {
                return Err(reject(stream, "Tailscale sign-in is turned off on the host"));
            }
            let Some(ip) = remote.map(|ip| plain_ip(ip).to_string()).filter(|ip| tailscale::is_tailscale_ip(ip)) else {
                return Err(reject(stream, "Tailscale sign-in only works over a Tailscale address"));
            };
            let who = tailscale::whois_login(&ip);
            let me = own_login(shared);
            match (who, me) {
                (Some(who), Some(me)) if who == me => {
                    logi!("host: Tailscale says {} belongs to {who}: letting it in", hello.name);
                    Vec::new()
                }
                (who, me) => {
                    let me = me.map(|m| format!(" ({m})")).unwrap_or_default();
                    return Err(reject(stream, &format!("{} isn't this PC's Tailscale account{me}", who.unwrap_or_else(|| "that device".into()))));
                }
            }
        }
        "code" => {
            if !shared.allow_pairing.load(Relaxed) {
                return Err(reject(stream, "pairing new computers is turned off on the host"));
            }
            {
                let mut p = shared.pairing.lock().unwrap();
                // The same computer trying again replaces its own earlier try.
                let who = if device_id.is_empty() { hello.name.clone() } else { device_id.clone() };
                if p.as_ref().is_some_and(|(c, d)| *c != connection_id && *d != who) {
                    return Err(reject(stream, "another computer is pairing right now; try again in a moment"));
                }
                *p = Some((connection_id, who));
            }
            let n = u32::from_le_bytes(random_bytes::<4>()) % 1_000_000;
            let code = format!("{n:06}");
            logi!("host: {} wants to pair; showing a code on this screen", hello.name);
            shared.events.pairing_code(&hello.name, &code);
            if let Ok(file) = std::env::var("BB1_TEST_PAIRING_CODE_FILE") {
                let _ = std::fs::write(file, &code); // automated tests only
            }
            // Two minutes to type it.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
            code.into_bytes()
        }
        other => return Err(reject(stream, &format!("unknown sign-in method {other}"))),
    };

    let host_nonce = random_bytes::<32>().to_vec();
    let eph = Ephemeral::new();
    let Some(shared_key) = eph.shared(&client_pub) else { return Err(reject(stream, "bad key exchange")) };
    let t = crypto::transcript(&hello.nonce, &host_nonce, &client_pub, &eph.public, &device_id, &cfg.host_id, &mode);
    let proof = if secret.is_empty() { Vec::new() } else { crypto::proof(&secret, "host", &t).to_vec() };
    let reply = HostHello {
        version: PROTOCOL_VERSION,
        nonce: host_nonce,
        proof,
        name: cfg.name.clone(),
        host_id: Some(cfg.host_id.clone()),
        ecdh_pub: Some(eph.public.to_vec()),
        app_build: Some(crate::core::updater::BUILD),
    };
    write_frame(stream, &Msg::HostHello(v(reply)).encode()).map_err(|e| e.to_string())?;
    let result = read_frame(stream);
    let finish_pairing = |paired: bool| {
        let mut p = shared.pairing.lock().unwrap();
        if p.as_ref().is_some_and(|(c, _)| *c == connection_id) {
            *p = None;
            drop(p);
            if !paired {
                shared.events.pairing_finished(false);
            }
        }
    };
    let body = match result {
        Ok(b) => b,
        Err(e) => {
            finish_pairing(false);
            return Err(e);
        }
    };
    let client_proof = match Msg::decode(&body) {
        Some(Msg::ClientProof(p)) => p.v,
        _ => {
            finish_pairing(false);
            return Err("expected a proof".into());
        }
    };
    let ok = if secret.is_empty() {
        mode == "tailscale" && client_proof.is_empty()
    } else {
        crypto::verify(&secret, "client", &t, &client_proof)
    };
    if !ok {
        finish_pairing(false);
        return Err(reject(stream, if mode == "code" { "wrong pairing code" } else { "this device isn't paired with this PC" }));
    }
    let _ = stream.set_read_timeout(None);
    Ok(Authed { keys: crypto::session_keys(&shared_key, &secret, &t), hello, mode })
}

fn plain_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// This computer's Tailscale account, cached for 5 minutes.
fn own_login(shared: &Shared) -> Option<String> {
    if let Some((login, at)) = shared.own_login.lock().unwrap().clone() {
        if at.elapsed() < Duration::from_secs(300) {
            return Some(login);
        }
    }
    let login = tailscale::self_login()?;
    *shared.own_login.lock().unwrap() = Some((login.clone(), Instant::now()));
    Some(login)
}

fn connection(shared: &Arc<Shared>, mut stream: TcpStream) -> Result<(), String> {
    let _ = stream.set_nodelay(true);
    let connection_id = shared.next_connection.fetch_add(1, Relaxed);
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let authed = match handshake(shared, &mut stream, connection_id) {
        Ok(a) => a,
        Err(e) => {
            // A computer's list checking whether bb1 answers connects and hangs up at once: not a rejection.
            if !e.contains("closed the connection") {
                logi!("host: rejected {peer}: {e}");
            }
            return Ok(());
        }
    };
    let reader = stream.try_clone().map_err(|e| e.to_string())?;
    let writer = (stream, StreamSealer::new(&authed.keys.host_to_client));
    let opener = StreamSealer::new(&authed.keys.client_to_host);
    if authed.hello.purpose.as_deref() == Some("update") {
        return update_connection(shared, authed, writer, reader, opener);
    }
    stream_session(shared, authed, writer, reader, opener, connection_id, &peer)
}

/// A quiet side connection: the client checks this host's build, asks for a screen preview, pings.
fn update_connection(shared: &Arc<Shared>, authed: Authed, writer: (TcpStream, StreamSealer), mut reader: TcpStream, mut opener: StreamSealer) -> Result<(), String> {
    let writer = Mutex::new(writer);
    let send = |msg: &Msg| {
        let mut w = writer.lock().unwrap();
        let (stream, sealer) = &mut *w;
        let sealed = sealer.seal(&msg.encode());
        let _ = write_frame(stream, &sealed);
    };
    let _ = reader.set_read_timeout(Some(Duration::from_secs(30)));
    loop {
        let body = read_frame(&mut reader)?;
        let Some(plain) = opener.open(&body) else { return Err("message failed authentication".into()) };
        match Msg::decode(&plain) {
            Some(Msg::Ping(p)) => send(&pong(shared, &p.v, 0, 0)),
            // No screen previews from a Windows host yet: say so, so the list shows the name.
            Some(Msg::RequestPreview { .. }) => send(&Msg::Preview(v(PreviewMsg { jpeg: None, capture_ms: 0.0 }))),
            Some(Msg::UpdateHost { build }) => {
                let done = handle_update_request(shared, &authed.hello.name, build, &send);
                if done {
                    return Ok(());
                }
            }
            Some(Msg::Bye(_)) => return Ok(()),
            _ => {}
        }
    }
}

/// Another computer asked this one to update (like a Mac host does): the app installs it and quits;
/// the asker waits for this host to come back with the new build. Returns true if it's on its way.
fn handle_update_request(shared: &Shared, who: &str, build: i64, send: &dyn Fn(&Msg)) -> bool {
    let status = |phase: &str, detail: Option<String>| {
        send(&Msg::UpdateStatus(v(UpdateStatusMsg { phase: phase.into(), progress: None, detail })));
    };
    if build <= crate::core::updater::BUILD {
        status("upToDate", None);
        return false;
    }
    logi!("host: {who} asked this PC to update from build {} to {build}", crate::core::updater::BUILD);
    status("installing", None);
    match shared.events.update_requested(build) {
        Ok(()) => {
            status("restarting", None);
            true
        }
        Err(e) => {
            logi!("host: couldn't update: {e}");
            status("failed", Some(e));
            false
        }
    }
}

fn pong(shared: &Shared, p: &PingMsg, encoded: u64, sent: u64) -> Msg {
    let (total, count, _) = *shared.encode_us.lock().unwrap();
    Msg::Pong(v(PongMsg {
        seq: p.seq,
        client_us: p.client_us,
        host_us: now_us(),
        captured: shared.captured.load(Relaxed),
        encoded,
        sent,
        encode_avg_us: if count > 0 { total / count } else { 0 },
        host_recoveries: 0,
    }))
}

fn stream_session(shared: &Arc<Shared>, authed: Authed, writer: (TcpStream, StreamSealer), mut reader: TcpStream, mut opener: StreamSealer,
                  connection_id: u64, peer: &str) -> Result<(), String> {
    let hello = &authed.hello;
    let peer_id = hello.device_id.clone().unwrap_or_default();
    // The same computer reconnecting before its old connection timed out: drop the stale one, and
    // tell it why, so a second copy that's still running stops instead of fighting this one.
    let stale: Vec<Arc<Session>> = shared.sessions.lock().unwrap().iter()
        .filter(|s| if peer_id.is_empty() { s.peer_id.is_empty() && s.name == hello.name } else { s.peer_id == peer_id })
        .cloned().collect();
    for old in stale {
        end_session(shared, &old, &format!("{} reconnected", hello.name), Some(&format!("{} connected again", hello.name)));
    }
    let count = shared.sessions.lock().unwrap().len();
    if count >= MAX_VIEWERS {
        logi!("host: turned away {}: {count} computers already connected", hello.name);
        let (mut stream, mut sealer) = writer;
        let _ = write_frame(&mut stream, &sealer.seal(&Msg::Bye(v(format!("{} already has {count} computers connected", shared.cfg.name))).encode()));
        return Ok(());
    }

    let id = shared.next_session.fetch_add(1, Relaxed).wrapping_add(1);
    let (tx, rx) = bounded::<Arc<Vec<Vec<u8>>>>(6);
    let (w, h) = *shared.size.lock().unwrap();
    let bitrate = shared.cfg.bitrate_mbps.unwrap_or_else(|| default_bitrate_mbps(w, h));
    let s = Arc::new(Session {
        id,
        name: hello.name.clone(),
        peer_id: peer_id.clone(),
        windows_client: hello.codecs.as_ref().is_some_and(|c| !c.iter().any(|c| c == "hevc")),
        keys: authed.keys.clone(),
        writer: Mutex::new(writer),
        udp_peer: Mutex::new(None),
        last_ping_us: AtomicU64::new(now_us()),
        key_requested: AtomicBool::new(true),
        audio: AtomicBool::new(false),
        audio_seq: AtomicU32::new(0),
        recent_audio: Mutex::new(VecDeque::new()),
        ended: AtomicBool::new(false),
        mouse: AtomicBool::new(true),
        keyboard: AtomicBool::new(true),
        controller: AtomicBool::new(true),
        recent: Mutex::new(HashMap::new()),
        frames: tx,
        counters: Counters { sent: AtomicU64::new(0), sent_bytes: AtomicU64::new(0), encoded: AtomicU64::new(0), nacks: AtomicU64::new(0), resent: AtomicU64::new(0),
                             keys: AtomicU64::new(0), mouse: AtomicU64::new(0), pads: AtomicU64::new(0), pads_blocked: AtomicU64::new(0), rumble: AtomicU64::new(0) },
        // A computer that just reconnected starts where its last connection left off (for a minute):
        // starting it at full quality in the middle of the congestion that dropped it made things worse.
        rate: Mutex::new(RateState { rate: remembered_rate(shared, &peer_id), clean_seconds: 0 }),
        pps: AtomicU64::new(0),
        resend_tokens: Mutex::new((60.0, Instant::now())),
        resends_skipped: AtomicU64::new(0),
        delay: Mutex::new(DelayState::default()),
        pace: AtomicU64::new(pace_for(bitrate as f64)),
        baseline: Mutex::new((0, 0, 0, 0)),
        epoch: AtomicU32::new(1),
    });
    let others: Vec<String> = shared.sessions.lock().unwrap().iter().map(|s| s.name.clone()).collect();
    shared.sessions.lock().unwrap().push(s.clone());
    logi!("host: {} connected (session {id}) from {peer}{}", s.name,
          if others.is_empty() { String::new() } else { format!("; also connected: {}", others.join(", ")) });
    s.send(&Msg::Welcome { host_name: shared.cfg.name.clone() });
    if let Some(d) = shared.displays.lock().unwrap().clone() {
        s.send(&Msg::Displays(v(d)));
    }
    logi!("host: {} signed in via {}", s.name, authed.mode);
    remember_device(shared, &s, &authed, connection_id);
    {
        let s2 = s.clone();
        let sh = shared.clone();
        spawn("bb1 sender", move || sender(sh, s2, rx));
    }
    let _ = shared.video.send(VideoCommand::Open(s.clone()));
    shared.events.viewers_changed(&shared.sessions.lock().unwrap().iter().map(|s| s.name.clone()).collect::<Vec<_>>());
    if let Some(ip) = reader.peer_addr().ok().map(|a| plain_ip(a.ip()).to_string()).filter(|ip| tailscale::is_tailscale_ip(ip)) {
        let s2 = s.clone();
        let sh = shared.clone();
        spawn("bb1 path", move || {
            if let Some(path) = tailscale::path_description(&ip) {
                logi!("host: {} is connected via Tailscale, {path}", s2.name);
                // A relay drops whatever doesn't fit its short queue: start gently.
                if path.starts_with("relayed") {
                    set_rate(&sh, &s2, 0.4, "Tailscale relay");
                }
            }
        });
    }

    // The control channel, until the client goes away.
    let result: Result<(), String> = loop {
        let body = match read_frame(&mut reader) {
            Ok(b) => b,
            Err(e) => break Err(e),
        };
        let Some(plain) = opener.open(&body) else { break Err("message failed authentication".into()) };
        let Some(msg) = Msg::decode(&plain) else { continue };
        // Count input per viewer for the stats line.
        let counter = match &msg {
            Msg::Key(_) | Msg::CapsLock(_) => Some(&s.counters.keys),
            Msg::MouseMove(_) | Msg::MouseButton(_) | Msg::Scroll(_) => Some(&s.counters.mouse),
            Msg::Gamepad(_) if s.controller.load(Relaxed) => Some(&s.counters.pads),
            Msg::Gamepad(_) => Some(&s.counters.pads_blocked),
            _ => None,
        };
        if let Some(c) = counter {
            c.fetch_add(1, Relaxed);
        }
        match msg {
            Msg::Ping(p) => {
                s.last_ping_us.store(now_us(), Relaxed);
                if let Some(rtt) = p.v.rtt_ms {
                    note_rtt(shared, &s, rtt);
                }
                s.send(&pong(shared, &p.v, s.counters.encoded.load(Relaxed), s.counters.sent.load(Relaxed)));
            }
            Msg::RequestKeyframe { reason } => {
                logi!("host: {} asked for a keyframe ({reason})", s.name);
                s.key_requested.store(true, Relaxed);
            }
            // Input only while the owner allows it for this viewer.
            Msg::Key(k) if s.keyboard.load(Relaxed) => shared.input.lock().unwrap().key(&k.v, s.windows_client),
            Msg::CapsLock(on) if s.keyboard.load(Relaxed) => shared.input.lock().unwrap().set_caps_lock(on.v),
            Msg::MouseMove(m) if s.mouse.load(Relaxed) => shared.input.lock().unwrap().mouse_move(&m.v),
            Msg::MouseButton(b) if s.mouse.load(Relaxed) => shared.input.lock().unwrap().mouse_button(&b.v),
            Msg::Scroll(sc) if s.mouse.load(Relaxed) => shared.input.lock().unwrap().scroll(&sc.v),
            Msg::ReleaseAll {} => shared.input.lock().unwrap().release_all(),
            Msg::Gamepad(g) if s.controller.load(Relaxed) => {
                if std::env::var_os("BB1_GAMEPAD_TRACE").is_some() {
                    let g = &g.v;
                    logi!("gamepad trace: {} pad {} {} buttons {:#06x} lt {} rt {} left {},{} right {},{}", s.name, g.pad,
                          if g.connected { "on" } else { "off" }, g.buttons, g.lt, g.rt, g.lx, g.ly, g.rx, g.ry);
                }
                shared.input.lock().unwrap().gamepad(s.id, &g.v)
            }
            // Anyone who may use the mouse or keyboard may pick the screen.
            Msg::SelectDisplay { id } if s.mouse.load(Relaxed) || s.keyboard.load(Relaxed) => {
                logi!("host: {} asked for screen {id}", s.name);
                let _ = shared.video.send(VideoCommand::SelectDisplay(id, s.name.clone()));
            }
            Msg::SetCursorInVideo(on) => {
                let _ = shared.video.send(VideoCommand::CursorInVideo(on.v));
            }
            Msg::Nack(n) => resend(shared, &s, &n.v),
            Msg::ClientLog(line) => crate::core::log::info(&format!("[{}] {}", s.name, line.v)),
            Msg::SetAudio(on) => {
                logi!("host: sound {} for {} (client's choice)", if on.v { "on" } else { "off" }, s.name);
                s.audio.store(on.v, Relaxed);
            }
            Msg::UpdateHost { build } => {
                let s2 = s.clone();
                handle_update_request(shared, &s.name, build, &move |m: &Msg| s2.send(m));
            }
            Msg::Bye(why) => break Err(format!("client said bye: {}", why.v)),
            _ => {}
        }
    };
    end_session(shared, &s, &result.err().unwrap_or_else(|| "client disconnected".into()), None);
    Ok(())
}

/// After sign-in with Tailscale or a code: give the client a pairing, so next time it signs in by itself.
fn remember_device(shared: &Shared, s: &Session, authed: &Authed, connection_id: u64) {
    let finish = {
        let mut p = shared.pairing.lock().unwrap();
        let mine = p.as_ref().is_some_and(|(c, _)| *c == connection_id);
        if mine {
            *p = None;
        }
        mine
    };
    if finish {
        shared.events.pairing_finished(true);
    }
    if s.peer_id.is_empty() {
        return;
    }
    let mut devices = paired_load();
    if authed.mode == "device" {
        if let Some(d) = devices.iter_mut().find(|d| d.id == s.peer_id) {
            d.name = s.name.clone();
            paired_save(&devices);
        }
        return;
    }
    let secret = random_bytes::<32>().to_vec();
    devices.retain(|d| d.id != s.peer_id);
    devices.push(PairedDevice {
        id: s.peer_id.clone(),
        name: s.name.clone(),
        secret: base64::engine::general_purpose::STANDARD.encode(&secret),
        via: authed.mode.clone(),
    });
    paired_save(&devices);
    s.send(&Msg::Paired(v(PairedMsg { host_id: shared.cfg.host_id.clone(), host_name: shared.cfg.name.clone(), secret })));
    logi!("host: paired with {} (via {}); it won't need to sign in again", s.name, authed.mode);
}

fn end_session(shared: &Shared, s: &Arc<Session>, reason: &str, bye: Option<&str>) {
    {
        let mut list = shared.sessions.lock().unwrap();
        let before = list.len();
        list.retain(|x| !Arc::ptr_eq(x, s));
        if list.len() == before {
            return;
        }
    }
    logi!("host: session {} with {} ended: {reason}", s.id, s.name);
    if !s.peer_id.is_empty() {
        let rate = s.rate.lock().unwrap().rate;
        shared.last_rates.lock().unwrap().insert(s.peer_id.clone(), (rate, Instant::now()));
    }
    if let Some(bye) = bye {
        s.send(&Msg::Bye(v(bye.to_string())));
        std::thread::sleep(Duration::from_millis(150));
    }
    s.ended.store(true, Relaxed);
    let _ = s.writer.lock().unwrap().0.shutdown(Shutdown::Both);
    let _ = shared.video.send(VideoCommand::Close(s.id));
    // Keys or buttons it was holding down (all of them: the injector doesn't know whose they are),
    // and its virtual controllers.
    {
        let mut input = shared.input.lock().unwrap();
        input.release_all();
        input.viewer_left(s.id);
    }
    shared.events.viewers_changed(&shared.sessions.lock().unwrap().iter().map(|s| s.name.clone()).collect::<Vec<_>>());
}

// MARK: Video path

fn udp_loop(shared: Arc<Shared>) {
    let mut buf = [0u8; 2048];
    while !shared.stopped.load(Relaxed) {
        let Ok((n, from)) = shared.udp.recv_from(&mut buf) else { continue };
        let d = &buf[..n];
        let Some(id) = crypto::udp_hello_session(d) else { continue };
        let Some(s) = shared.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned() else { continue };
        if !crypto::udp_hello_valid(d, &s.keys.video) {
            continue;
        }
        let mut peer = s.udp_peer.lock().unwrap();
        if *peer != Some(from) {
            *peer = Some(from);
            logi!("host: video path to {from} ready for session {id}");
            s.key_requested.store(true, Relaxed);
        }
    }
}

/// Captures, encodes for each viewer, and hands the packets to each viewer's sender.
fn video_loop(shared: Arc<Shared>, mut video: Box<dyn VideoBackend>, commands: Receiver<VideoCommand>) {
    let mut viewers: Vec<Arc<Session>> = Vec::new();
    let mut size = video.size();
    let fps = shared.cfg.fps.max(1);
    let mut frame_ids: HashMap<u32, u32> = HashMap::new();
    let mut failing_since: Option<Instant> = None;
    let mut last_display_check = Instant::now() - Duration::from_secs(10);
    publish_displays(&shared, &*video, false);
    while !shared.stopped.load(Relaxed) {
        while let Ok(cmd) = commands.try_recv() {
            match cmd {
                VideoCommand::Open(s) => {
                    if open_viewer(&shared, &mut *video, &s, size, fps) {
                        viewers.push(s);
                    }
                }
                VideoCommand::Close(id) => {
                    viewers.retain(|s| s.id != id);
                    frame_ids.remove(&id);
                    video.close_viewer(id);
                }
                VideoCommand::Bitrate(id, kbps) => video.set_bitrate(id, kbps),
                VideoCommand::CursorInVideo(on) => video.set_cursor_in_video(on),
                VideoCommand::SelectDisplay(id, who) => {
                    let before = video.current_display();
                    match video.select_display(&id) {
                        Ok(()) => {
                            if video.current_display() != before {
                                logi!("capture: now sharing screen {id} (asked by {who})");
                                // A new capture (maybe on another graphics card): new encoders for
                                // everyone, a new epoch and a keyframe, whether or not the size changed.
                                size = video.size();
                                *shared.size.lock().unwrap() = size;
                                for s in viewers.clone() {
                                    video.close_viewer(s.id);
                                    s.epoch.fetch_add(1, Relaxed);
                                    if !open_viewer(&shared, &mut *video, &s, size, fps) {
                                        viewers.retain(|x| x.id != s.id);
                                    }
                                }
                            }
                            publish_displays(&shared, &*video, true);
                        }
                        Err(e) => {
                            logi!("capture: couldn't switch to screen {id}: {e}");
                            let sessions = shared.sessions.lock().unwrap().clone();
                            for s in sessions.iter().filter(|s| s.name == who) {
                                s.send(&Msg::Notice(v(format!("{} couldn't share that screen: {e}", shared.cfg.name))));
                            }
                        }
                    }
                }
            }
        }
        // Screens plugged in or out, or the shared one changed: tell everyone (cheap; checked often).
        if last_display_check.elapsed() > Duration::from_secs(2) {
            last_display_check = Instant::now();
            publish_displays(&shared, &*video, false);
        }
        if viewers.is_empty() {
            // Nobody watching: don't capture at all.
            match commands.recv_timeout(Duration::from_millis(200)) {
                Ok(VideoCommand::Open(s)) => {
                    size = video.size();
                    *shared.size.lock().unwrap() = size;
                    if open_viewer(&shared, &mut *video, &s, size, fps) {
                        viewers.push(s);
                    }
                }
                Ok(VideoCommand::CursorInVideo(on)) => video.set_cursor_in_video(on),
                _ => {}
            }
            continue;
        }
        let new_frame = match video.wait_frame(Duration::from_millis(1000 / fps as u64 + 4)) {
            Ok(n) => {
                failing_since = None;
                n
            }
            Err(e) => {
                if failing_since.is_none() {
                    logi!("capture: {e}");
                    failing_since = Some(Instant::now());
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if new_frame {
            shared.captured.fetch_add(1, Relaxed);
        }
        // The screen changed size (a game switched resolution): new encoders, a new epoch.
        let now_size = video.size();
        if now_size != size {
            logi!("capture: screen is now {}x{}", now_size.0, now_size.1);
            size = now_size;
            *shared.size.lock().unwrap() = size;
            for s in viewers.clone() {
                video.close_viewer(s.id);
                s.epoch.fetch_add(1, Relaxed);
                if !open_viewer(&shared, &mut *video, &s, size, fps) {
                    viewers.retain(|x| x.id != s.id);
                }
            }
        }
        for s in &viewers {
            if s.udp_peer.lock().unwrap().is_none() {
                continue;
            }
            let key = s.key_requested.swap(false, Relaxed);
            if !new_frame && !key {
                continue;
            }
            let started = Instant::now();
            let frame = match video.encode(s.id, key) {
                Ok(f) => f,
                Err(e) => {
                    logi!("encoder: {} for {}", e, s.name);
                    s.key_requested.store(true, Relaxed);
                    continue;
                }
            };
            let us = started.elapsed().as_micros() as u64;
            {
                let mut e = shared.encode_us.lock().unwrap();
                e.0 += us;
                e.1 += 1;
                e.2 = e.2.max(us);
            }
            s.counters.encoded.fetch_add(1, Relaxed);
            let id = frame_ids.entry(s.id).or_insert(1);
            let frame_id = *id;
            *id = id.wrapping_add(1);
            let packets = packetize(s, &frame, frame_id, now_us());
            s.recent.lock().unwrap().insert(frame_id, (now_us(), packets.clone()));
            // A viewer that can't keep up loses frames here rather than building a queue.
            if s.frames.try_send(packets).is_err() {
                s.key_requested.store(true, Relaxed);
            }
        }
    }
}

/// Send viewers the list of screens when it changed (or `force`), and point the mouse at the shared one.
fn publish_displays(shared: &Shared, video: &dyn VideoBackend, force: bool) {
    let list = video.displays();
    if list.is_empty() {
        return;
    }
    let msg = DisplaysMsg { displays: list, current: video.current_display() };
    {
        let mut last = shared.displays.lock().unwrap();
        if !force && last.as_ref() == Some(&msg) {
            return;
        }
        *last = Some(msg.clone());
    }
    if let Some((l, t, w, h)) = video.display_rect() {
        shared.input.lock().unwrap().set_screen(l, t, w, h);
    }
    let sessions = shared.sessions.lock().unwrap().clone();
    for s in sessions {
        s.send(&Msg::Displays(v(msg.clone())));
    }
}

fn open_viewer(shared: &Shared, video: &mut dyn VideoBackend, s: &Arc<Session>, size: (u32, u32), fps: u32) -> bool {
    let full = shared.cfg.bitrate_mbps.unwrap_or_else(|| default_bitrate_mbps(size.0, size.1));
    let rate = s.rate.lock().unwrap().rate;
    let mbps = ((full as f64 * rate).round() as u32).max(3);
    if let Err(e) = video.open_viewer(s.id, fps, mbps * 1000) {
        logi!("encoder: couldn't start one for {}: {e}", s.name);
        s.send(&Msg::Notice(v(format!("The PC couldn't start a video encoder: {e}"))));
        return false;
    }
    let config = StreamConfig {
        width: size.0 as i64,
        height: size.1 as i64,
        codec: CODEC_H264,
        fps: fps as i64,
        bitrate_mbps: mbps as i64,
        low_latency_encoder: true,
        udp_port: shared.cfg.video_port,
        session_id: s.id,
        epoch: s.epoch.load(Relaxed) as u16,
    };
    logi!("encoder: H.264 {}x{} {fps} fps {mbps} Mbps for {}, epoch {}", size.0, size.1, s.name, config.epoch);
    s.send(&Msg::StreamConfig(v(config)));
    s.key_requested.store(true, Relaxed);
    true
}

/// Frame payload (codec, parameter sets, AVCC NAL units), sealed, split into ≤1200-byte datagrams.
fn packetize(s: &Session, frame: &Encoded, frame_id: u32, capture_us: u64) -> Arc<Vec<Vec<u8>>> {
    let mut payload = Vec::with_capacity(frame.nals.iter().map(|n| n.len() + 4).sum::<usize>() + 64);
    payload.push(CODEC_H264);
    let sets: &[Vec<u8>] = if frame.keyframe { &frame.parameter_sets } else { &[] };
    payload.push(sets.len() as u8);
    for p in sets {
        payload.extend_from_slice(&(p.len() as u16).to_le_bytes());
        payload.extend_from_slice(p);
    }
    for n in &frame.nals {
        payload.extend_from_slice(&(n.len() as u32).to_be_bytes());
        payload.extend_from_slice(n);
    }
    let flags = if frame.keyframe { KEYFRAME_FLAG } else { 0 };
    let epoch = s.epoch.load(Relaxed) as u16;
    let aad = crate::core::video::aad(flags, epoch, frame_id, capture_us);
    let sealed = FrameCrypto::seal(&payload, &s.keys.video, frame_id, &aad);
    let chunk = MAX_DATAGRAM - HEADER;
    let count = sealed.len().div_ceil(chunk);
    let packets = sealed
        .chunks(chunk)
        .enumerate()
        .map(|(i, part)| {
            let mut p = Vec::with_capacity(HEADER + part.len());
            p.push(VIDEO_TYPE);
            p.push(flags);
            p.extend_from_slice(&epoch.to_le_bytes());
            p.extend_from_slice(&frame_id.to_le_bytes());
            p.extend_from_slice(&(i as u16).to_le_bytes());
            p.extend_from_slice(&(count as u16).to_le_bytes());
            p.extend_from_slice(&capture_us.to_le_bytes());
            p.extend_from_slice(part);
            p
        })
        .collect();
    Arc::new(packets)
}

/// Bursts at several times the bitrate, at least 60 Mbps: a frame leaves quickly, but not as one
/// burst that overflows a Wi-Fi hop or the viewer's router.
fn pace_for(bitrate_mbps: f64) -> u64 {
    ((bitrate_mbps * 4.0).max(60.0) * 125_000.0) as u64
}

/// Sends one viewer's frames, paced.
fn sender(shared: Arc<Shared>, s: Arc<Session>, frames: Receiver<Arc<Vec<Vec<u8>>>>) {
    while !s.ended.load(Relaxed) {
        let Ok(packets) = frames.recv_timeout(Duration::from_millis(250)) else { continue };
        let Some(peer) = *s.udp_peer.lock().unwrap() else { continue };
        let pace = s.pace.load(Relaxed) as f64;
        let start = Instant::now();
        let mut bytes = 0usize;
        for (i, p) in packets.iter().enumerate() {
            if shared.cfg.drop_percent > 0.0 {
                if (u64::from_le_bytes(random_bytes::<8>()) % 10_000) as f64 / 100.0 < shared.cfg.drop_percent {
                    continue;
                }
            }
            let _ = shared.udp.send_to(p, peer);
            bytes += p.len();
            if packets.len() > 8 && i % 8 == 7 {
                let target = Duration::from_secs_f64(bytes as f64 / pace);
                let elapsed = start.elapsed();
                if target > elapsed {
                    std::thread::sleep(target - elapsed);
                }
            }
        }
        s.counters.sent.fetch_add(1, Relaxed);
        s.counters.sent_bytes.fetch_add(bytes as u64, Relaxed);
    }
}

/// The quality a computer had when it last left, if that was under a minute ago.
fn remembered_rate(shared: &Shared, peer_id: &str) -> f64 {
    if peer_id.is_empty() {
        return 1.0;
    }
    match shared.last_rates.lock().unwrap().get(peer_id) {
        Some((rate, at)) if at.elapsed() < Duration::from_secs(60) => {
            if *rate < 1.0 {
                logi!("host: starting this computer at {:.0}% of full quality, where its last connection left off", rate * 100.0);
            }
            *rate
        }
        _ => 1.0,
    }
}

/// Resend packets the viewer says it didn't get, within a budget. On a connection that's suddenly
/// slow (the viewer's line queuing up), every late frame looks lost; resending them all doubled the
/// traffic into that queue (2026-11-05: 560 resent packets a second on a 450-a-second stream, then the
/// connection dropped). So: at most ~30% on top of the stream, and nothing older than 0.6 s.
fn resend(shared: &Shared, s: &Session, n: &NackMsg) {
    s.counters.nacks.fetch_add(1, Relaxed);
    let Some(peer) = *s.udp_peer.lock().unwrap() else { return };
    let Some((sent_us, packets)) = s.recent.lock().unwrap().get(&n.frame_id).cloned() else { return };
    let indices: Vec<usize> = match &n.indices {
        Some(list) => list.iter().map(|&i| i as usize).filter(|&i| i < packets.len()).collect(),
        None => (0..packets.len()).collect(),
    };
    if now_us().saturating_sub(sent_us) > 600_000 {
        s.resends_skipped.fetch_add(indices.len() as u64, Relaxed);
        return;
    }
    {
        let per_second = (s.pps.load(Relaxed) as f64 * 0.3).max(60.0);
        let mut b = s.resend_tokens.lock().unwrap();
        let elapsed = b.1.elapsed().as_secs_f64();
        b.0 = (b.0 + elapsed * per_second).min(per_second);
        b.1 = Instant::now();
        if b.0 < indices.len() as f64 {
            drop(b);
            s.resends_skipped.fetch_add(indices.len() as u64, Relaxed);
            return;
        }
        b.0 -= indices.len() as f64;
    }
    for &i in &indices {
        let _ = shared.udp.send_to(&packets[i], peer);
    }
    s.counters.resent.fetch_add(indices.len() as u64, Relaxed);
}

// MARK: Health

fn watchdog(shared: Arc<Shared>) {
    let mut tick = 0u64;
    while !shared.stopped.load(Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
        tick += 1;
        let sessions = shared.sessions.lock().unwrap().clone();
        for s in &sessions {
            if now_us().saturating_sub(s.last_ping_us.load(Relaxed)) > 3_000_000 {
                end_session(&shared, s, "no heartbeat from the client for 3 s", None);
            }
            // Frames older than 2 s can't usefully be resent (long enough for a far-away viewer).
            let now = now_us();
            s.recent.lock().unwrap().retain(|_, (at, _)| now - *at < 2_000_000);
        }
        if tick % 4 == 0 && !sessions.is_empty() {
            log_stats(&shared, &sessions);
        }
    }
}

fn log_stats(shared: &Shared, sessions: &[Arc<Session>]) {
    let (total, count, max) = std::mem::replace(&mut *shared.encode_us.lock().unwrap(), (0, 0, 0));
    let captured = shared.captured.swap(0, Relaxed);
    let mut lines = Vec::new();
    for s in sessions {
        let now = (s.counters.sent.load(Relaxed), s.counters.sent_bytes.load(Relaxed), s.counters.nacks.load(Relaxed), s.counters.resent.load(Relaxed));
        let b = std::mem::replace(&mut *s.baseline.lock().unwrap(), now);
        let (sent, bytes, nacks, resent) = (now.0 - b.0, now.1 - b.1, now.2 - b.2, now.3 - b.3);
        let take = |c: &AtomicU64| c.swap(0, Relaxed);
        let (keys, mouse, pads, blocked, rumble) =
            (take(&s.counters.keys), take(&s.counters.mouse), take(&s.counters.pads), take(&s.counters.pads_blocked), take(&s.counters.rumble));
        let mut line = format!("{} {sent} fps {:.1} Mbps nacks {nacks} resent {resent} · input {keys} keys {mouse} mouse {pads} pad",
                               s.name, bytes as f64 * 8.0 / 1e6);
        if blocked > 0 {
            line.push_str(&format!(" ({blocked} pad ignored: controller off)"));
        }
        if rumble > 0 {
            line.push_str(&format!(" · rumble {rumble}"));
        }
        lines.push(line);
        s.pps.store(bytes / 1200, Relaxed);
        let skipped = s.resends_skipped.swap(0, Relaxed);
        if skipped > 0 {
            if let Some(last) = lines.last_mut() {
                last.push_str(&format!(" · {skipped} resends skipped (late or over budget)"));
            }
        }
        // Skipped resends were asked for too: they count as loss for the bitrate.
        adapt_rate(shared, s, bytes / 1200, resent + skipped);
    }
    logi!("stats: captured {captured} fps · encode avg {:.1} max {:.1} ms · {}",
          if count > 0 { total as f64 / count as f64 / 1000.0 } else { 0.0 }, max as f64 / 1000.0, lines.join(" · "));
}

/// Lost packets mean we're sending more than this viewer's path carries: back off fast, then creep
/// back up while nothing is lost. Only this viewer's encoder changes.
fn adapt_rate(shared: &Shared, s: &Arc<Session>, packets: u64, resent: u64) {
    let mut r = *s.rate.lock().unwrap();
    let old = r.rate;
    // Back off only on heavy loss: at least 5% of the second's packets, and at least 12 of them. A
    // friend in another city (2026-11-05) had a steady ~2% that the viewer recovered every time and that
    // didn't shrink at a lower bitrate: line noise, not congestion. Backing off for it kept him at a sixth
    // of full quality, and a game starting then took ~20 s to look sharp again.
    if resent >= 12 && resent * 20 >= packets {
        r.rate = (r.rate * 0.7).max(0.15);
        r.clean_seconds = 0;
    } else {
        // Not clean while the viewer's line was backing up in the last 3 s.
        let backing_up = s.delay.lock().unwrap().last_cut.is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        r.clean_seconds = if backing_up { 0 } else { r.clean_seconds + 1 };
        // Climb back sooner: after 2 clean seconds, a quarter more each clean second.
        if r.clean_seconds >= 2 && r.rate < 1.0 {
            r.rate = (r.rate * 1.25).min(1.0);
        }
    }
    s.rate.lock().unwrap().clean_seconds = r.clean_seconds;
    if r.rate != old {
        set_rate(shared, s, r.rate, &if r.rate < old { format!("{resent} packets resent") } else { "no loss".into() });
    }
}

/// The viewer's round trip, four times a second. When it climbs well above this connection's normal
/// (the lowest in the last 30 s), the viewer's line is queuing up: lower the bitrate now, before
/// frames arrive late and the picture freezes. Loss only shows up after the queue is full.
fn note_rtt(shared: &Shared, s: &Arc<Session>, rtt: f64) {
    let cut = {
        let mut d = s.delay.lock().unwrap();
        let now = Instant::now();
        d.samples.push_back((now, rtt));
        while d.samples.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(30)) {
            d.samples.pop_front();
        }
        let normal = d.samples.iter().map(|(_, r)| *r).fold(f64::MAX, f64::min);
        let queued = rtt - normal;
        // Well above normal: 50 ms more, or double a slow connection's normal.
        let threshold = normal.max(50.0);
        if queued > threshold {
            d.high += 1;
        } else {
            d.high = 0;
        }
        if d.high >= 2 && d.last_cut.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            d.last_cut = Some(now);
            Some((queued > threshold * 4.0, normal, rtt))
        } else {
            None
        }
    };
    if let Some((severe, normal, rtt)) = cut {
        let mut r = *s.rate.lock().unwrap();
        r.rate = (r.rate * if severe { 0.5 } else { 0.75 }).max(0.15);
        r.clean_seconds = 0;
        *s.rate.lock().unwrap() = r;
        set_rate(shared, s, r.rate, &format!("round trip {rtt:.0} ms, usually {normal:.0}: the line is backing up"));
    }
}

fn set_rate(shared: &Shared, s: &Arc<Session>, rate: f64, why: &str) {
    s.rate.lock().unwrap().rate = rate;
    let (w, h) = *shared.size.lock().unwrap();
    let full = shared.cfg.bitrate_mbps.unwrap_or_else(|| default_bitrate_mbps(w, h)) as f64;
    let mbps = (full * rate).max(3.0);
    s.pace.store(pace_for(mbps), Relaxed);
    let _ = shared.video.send(VideoCommand::Bitrate(s.id, (mbps * 1000.0) as u32));
    logi!("host: sending {} at {:.0}% of full quality, {:.0} Mbps ({why})", s.name, rate * 100.0, mbps);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_by_resolution() {
        assert_eq!(default_bitrate_mbps(1920, 1080), 30);
        assert_eq!(default_bitrate_mbps(1280, 720), 13);
        assert_eq!(default_bitrate_mbps(3840, 2160), 80);
    }
}
