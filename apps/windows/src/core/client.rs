//! Connection to one Mac: handshake, control channel, video path, heartbeats, stall detection and
//! automatic recovery. A port of the Mac client's ClientConnection (docs/04-reliability.md).
//!
//! Threads: a supervisor (connects, reconnects, runs timers and handles control messages), a TCP
//! reader, a TCP writer (so input can be sent from any thread without waiting), and a UDP receiver
//! that reassembles frames, asks for resends and hands whole frames to the app.

use crate::core::audio::JitterBuffer;
use crate::core::crypto::{self, Ephemeral, SessionKeys, StreamSealer};
use crate::core::proto::*;
use crate::core::store::{self, KnownHost};
use crate::core::video::{Frame, FramePayload, Reassembler};
use crate::core::{CONTROL_PORT, PROTOCOL_VERSION, now_us, tailscale};
use crate::logi;
use crossbeam_channel::{Receiver, Sender, unbounded};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum Auth {
    /// A remembered pairing with this host.
    Device(Vec<u8>),
    /// The host checks with Tailscale that this PC is on the same account.
    Tailscale,
    /// First-time pairing: the host shows a 6-digit code.
    Code,
}

impl Auth {
    pub fn mode(&self) -> &'static str {
        match self {
            Auth::Device(_) => "device",
            Auth::Tailscale => "tailscale",
            Auth::Code => "code",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Target {
    /// "100.80.66.124:47800", "kenobi.local", "[fd7a::1]:47800"
    pub address: String,
    pub label: String,
    pub auth: Auth,
    pub tailscale_ip: Option<String>,
}

impl Target {
    /// Picks how to sign in: a remembered pairing, else Tailscale (on a Tailscale address), else a code.
    pub fn new(address: &str, label: &str) -> Target {
        let address = with_port(address);
        let host = host_part(&address);
        let tailscale_ip = tailscale::is_tailscale_ip(&host).then(|| host.clone());
        let auth = match KnownHost::find(&address, label) {
            Some(k) => Auth::Device(k.secret),
            None if tailscale_ip.is_some() => Auth::Tailscale,
            None => Auth::Code,
        };
        Target { address, label: if label.is_empty() { host } else { label.to_string() }, auth, tailscale_ip }
    }
}

fn with_port(address: &str) -> String {
    let a = address.trim();
    if a.starts_with('[') {
        if a.contains("]:") { a.to_string() } else { format!("{a}:{CONTROL_PORT}") }
    } else if a.matches(':').count() == 1 {
        a.to_string()
    } else if a.contains(':') {
        format!("[{a}]:{CONTROL_PORT}") // bare IPv6
    } else {
        format!("{a}:{CONTROL_PORT}")
    }
}

fn host_part(address: &str) -> String {
    let h = address.rsplit_once(':').map(|(h, _)| h).unwrap_or(address);
    h.trim_start_matches('[').trim_end_matches(']').to_string()
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub config: Option<StreamConfig>,
    pub fps: f64,
    pub mbps: f64,
    pub rtt_ms: f64,
    pub latency_ms: Option<f64>,
    pub latency_max_ms: Option<f64>,
    pub path: String,
    pub connected: bool,
}

impl Stats {
    pub fn summary(&self) -> String {
        let mut s = String::new();
        if let Some(c) = &self.config {
            s += &format!("{}×{} {} · ", c.width, c.height, c.codec_name());
        }
        if !self.path.is_empty() {
            s += &format!("{} · ", self.path);
        }
        s += &format!("{:.0} fps · {:.1} Mbps · RTT {:.1} ms", self.fps, self.mbps, self.rtt_ms);
        if let (Some(l), Some(m)) = (self.latency_ms, self.latency_max_ms) {
            s += &format!(" · capture→shown {l:.0} ms (max {m:.0})");
        }
        s
    }
}

/// What the app hears about. Called from the client's threads; keep each one quick.
pub trait Events: Send + Sync + 'static {
    fn status(&self, text: &str, connected: bool);
    /// The session ended for good (no reconnect).
    fn ended(&self, reason: &str);
    /// Blocks until the user types the 6-digit code shown on the Mac (None = cancel).
    fn ask_code(&self, host: &str) -> Option<String>;
    /// A whole, decrypted frame, in order, ready for the decoder.
    fn video(&self, payload: FramePayload, capture_us: u64);
    /// Frames went into the decoder and nothing came out: start it over.
    fn reset_decoder(&self) {}
    fn welcome(&self, _host_name: &str) {}
    fn stream_config(&self, _config: &StreamConfig) {}
    fn notice(&self, _text: &str) {}
    fn clipboard(&self, _clip: ClipboardMsg) {}
    fn cursor(&self, _cursor: CursorMsg) {}
    fn paired(&self, _host: &KnownHost) {}
    fn stats(&self, _stats: &Stats) {}
}

#[derive(Default)]
pub struct Counters {
    pub received: AtomicU64,
    /// Frames handed to the decoder.
    pub accepted: AtomicU64,
    /// Set by the app.
    pub decoded: AtomicU64,
    pub displayed: AtomicU64,
    pub lost_frames: AtomicU64,
    pub keyframe_requests: AtomicU64,
    pub bytes: AtomicU64,
    pub nacks_sent: AtomicU64,
    pub recovered_by_resend: AtomicU64,
    pub audio_packets: AtomicU64,
}

struct VideoState {
    reassembler: Reassembler,
    pending: HashMap<u32, Frame>,
    gap_since_us: u64,
    waiting_for_keyframe: bool,
    last_frame_id: u32,
    current_epoch: u16,
    last_keyframe_request_us: u64,
    keys: Option<SessionKeys>,
    supports_v2: bool,
}

impl Default for VideoState {
    fn default() -> Self {
        VideoState {
            reassembler: Reassembler::default(),
            pending: HashMap::new(),
            gap_since_us: 0,
            waiting_for_keyframe: true,
            last_frame_id: 0,
            current_epoch: 0,
            last_keyframe_request_us: 0,
            keys: None,
            supports_v2: true,
        }
    }
}

struct Shared {
    target: Mutex<Target>,
    events: Arc<dyn Events>,
    counters: Counters,
    audio: JitterBuffer,
    writer: Mutex<Option<Sender<Msg>>>,
    stopped: AtomicBool,
    video: Mutex<VideoState>,
    rtt_us: AtomicU64,
    clock_offset_us: Mutex<Option<i64>>,
    latency_samples: Mutex<Vec<f64>>,
    udp_generation: AtomicU64,
    sound_wanted: AtomicBool,
    cursor_in_video: AtomicBool,
    path: Mutex<String>,
    tcp: Mutex<Option<TcpStream>>,
}

/// A connection to one Mac. Cheap to clone; `stop()` ends it.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
}

enum End {
    /// Not allowed in; retrying the same way won't help.
    Rejected(String),
    /// Connection lost: reconnect.
    Closed(String),
    /// The host ended the session, or we stopped.
    Ended(String),
}

enum Ev {
    Msg(Msg),
    Closed(String),
    Rejected(String),
}

impl Client {
    pub fn start(target: Target, events: Arc<dyn Events>, sound: bool, cursor_in_video: bool) -> Client {
        let shared = Arc::new(Shared {
            target: Mutex::new(target),
            events,
            counters: Counters::default(),
            audio: JitterBuffer::default(),
            writer: Mutex::new(None),
            stopped: AtomicBool::new(false),
            video: Mutex::new(VideoState::default()),
            rtt_us: AtomicU64::new(0),
            clock_offset_us: Mutex::new(None),
            latency_samples: Mutex::new(Vec::new()),
            udp_generation: AtomicU64::new(0),
            sound_wanted: AtomicBool::new(sound),
            cursor_in_video: AtomicBool::new(cursor_in_video),
            path: Mutex::new(String::new()),
            tcp: Mutex::new(None),
        });
        let s = shared.clone();
        std::thread::Builder::new().name("bb1 connection".into()).spawn(move || supervise(s)).unwrap();
        Client { shared }
    }

    /// Sends a control message (dropped while not connected). Never blocks.
    pub fn send(&self, msg: Msg) {
        if let Some(tx) = self.shared.writer.lock().unwrap().as_ref() {
            let _ = tx.send(msg);
        }
    }

    pub fn stop(&self) {
        if self.shared.stopped.swap(true, Relaxed) {
            return;
        }
        self.send(Msg::Bye(v("client closed".into())));
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            if let Some(tcp) = shared.tcp.lock().unwrap().as_ref() {
                let _ = tcp.shutdown(Shutdown::Both);
            }
        });
    }

    pub fn counters(&self) -> &Counters {
        &self.shared.counters
    }

    /// Where the sound thread reads from.
    pub fn audio(&self) -> &JitterBuffer {
        &self.shared.audio
    }

    pub fn label(&self) -> String {
        self.shared.target.lock().unwrap().label.clone()
    }

    pub fn set_sound(&self, on: bool) {
        self.shared.sound_wanted.store(on, Relaxed);
        if !on {
            self.shared.audio.clear();
        }
        self.send(Msg::SetAudio(v(on)));
    }

    /// The decoder failed on a frame: wait for (and ask for) a keyframe.
    pub fn decoder_failed(&self, reason: &str) {
        let mut vs = self.shared.video.lock().unwrap();
        vs.waiting_for_keyframe = true;
        request_keyframe(&self.shared, &mut vs, reason);
    }

    pub fn mark_decoded(&self) {
        self.shared.counters.decoded.fetch_add(1, Relaxed);
    }

    /// Called once a frame is on screen.
    pub fn mark_displayed(&self, capture_us: u64) {
        self.shared.counters.displayed.fetch_add(1, Relaxed);
        if let Some(offset) = *self.shared.clock_offset_us.lock().unwrap() {
            let latency = (now_us() as i64 + offset - capture_us as i64) as f64 / 1000.0;
            if latency > 0.0 && latency < 5000.0 {
                self.shared.latency_samples.lock().unwrap().push(latency);
            }
        }
    }
}

// MARK: Supervisor

fn supervise(shared: Arc<Shared>) {
    let mut retry = 0.25;
    let mut retried_after_forgotten = false;
    let mut ever_welcomed = false;
    let mut failed_before_welcome = 0;
    loop {
        if shared.stopped.load(Relaxed) {
            shared.events.ended("Disconnected.");
            return;
        }
        let (end, welcomed) = run_session(&shared);
        teardown(&shared);
        if shared.stopped.load(Relaxed) {
            shared.events.ended("Disconnected.");
            return;
        }
        if welcomed {
            retry = 0.25;
            ever_welcomed = true;
        }
        match end {
            End::Ended(reason) => {
                logi!("client: {reason}");
                shared.events.ended(&reason);
                return;
            }
            End::Rejected(why) => {
                let mut t = shared.target.lock().unwrap();
                if matches!(t.auth, Auth::Device(_)) && why.contains("doesn't remember") && !retried_after_forgotten {
                    // The Mac forgot this PC: drop the stale pairing and sign in again the other way.
                    if let Some(k) = KnownHost::find(&t.address, &t.label) {
                        KnownHost::forget(&k.host_id);
                    }
                    t.auth = if t.tailscale_ip.is_some() { Auth::Tailscale } else { Auth::Code };
                    retried_after_forgotten = true;
                    logi!("client: the Mac forgot this PC; signing in again ({})", t.auth.mode());
                    continue;
                }
                let why = if why.ends_with('.') { why } else { format!("{why}.") };
                logi!("client: couldn't sign in: {why}");
                drop(t);
                shared.events.ended(&format!("Couldn't sign in: {why}"));
                return;
            }
            End::Closed(why) => {
                let pairing = matches!(shared.target.lock().unwrap().auth, Auth::Code);
                if pairing && !welcomed {
                    shared.events.ended(&format!("Pairing didn't finish ({why})."));
                    return;
                }
                // Never got in at all: say why instead of retrying forever (a dropped stream still reconnects).
                if !ever_welcomed {
                    failed_before_welcome += 1;
                    if failed_before_welcome >= 3 {
                        let label = shared.target.lock().unwrap().label.clone();
                        logi!("client: couldn't connect to {label}: {why}");
                        shared.events.ended(&format!("Couldn't connect to {label}: {why}."));
                        return;
                    }
                }
                logi!("client: reconnecting: {why}");
                shared.events.status(&format!("Reconnecting… ({why})"), false);
                std::thread::sleep(Duration::from_secs_f64(retry));
                retry = (retry * 2.0).min(3.0);
            }
        }
    }
}

fn teardown(shared: &Shared) {
    crate::core::log::set_sink(None);
    *shared.writer.lock().unwrap() = None;
    if let Some(tcp) = shared.tcp.lock().unwrap().take() {
        let _ = tcp.shutdown(Shutdown::Both);
    }
    shared.udp_generation.fetch_add(1, Relaxed);
    *shared.video.lock().unwrap() = VideoState::default();
    *shared.clock_offset_us.lock().unwrap() = None;
    shared.audio.clear();
}

fn connect_tcp(address: &str) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = address.to_socket_addrs().map_err(|e| format!("can't find {address} ({e})"))?.collect();
    let mut last = format!("can't find {address}");
    // IPv4 first: that's what bb1 hosts listen on for sure.
    let mut sorted = addrs.clone();
    sorted.sort_by_key(|a| a.is_ipv6());
    for a in sorted {
        match TcpStream::connect_timeout(&a, Duration::from_secs(5)) {
            Ok(s) => return Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                last = "bb1 isn't sharing on that Mac (it refused the connection)".into()
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => last = "the Mac didn't answer".into(),
            Err(e) => last = format!("can't reach the Mac ({e})"),
        }
    }
    Err(last)
}

/// A signed-in side connection (purpose "update"): previews and pings, no stream. Code pairing isn't offered.
/// A reader thread receives, so a slow message never gets cut in half by a timeout.
pub struct SideChannel {
    stream: TcpStream,
    sealer: StreamSealer,
    incoming: Receiver<Result<Option<Msg>, String>>,
    /// The host's bb1 build (0 = too old to say).
    pub host_build: i64,
}

impl SideChannel {
    pub fn open(target: &Target, timeout: Duration) -> Result<SideChannel, String> {
        let addr = target.address.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no address")?;
        let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
        let _ = stream.set_nodelay(true);
        let (keys, host) = handshake_with(&mut stream, target, Some("update"), &|_: &str| None).map_err(|e| match e {
            End::Rejected(r) | End::Closed(r) | End::Ended(r) => r,
        })?;
        let _ = stream.set_read_timeout(None);
        let mut reader = stream.try_clone().map_err(|e| e.to_string())?;
        let mut opener = StreamSealer::new(&keys.host_to_client);
        let (tx, incoming) = unbounded();
        std::thread::Builder::new()
            .name("bb1 side channel".into())
            .spawn(move || loop {
                let msg = read_frame(&mut reader)
                    .and_then(|body| opener.open(&body).ok_or_else(|| "message failed authentication".to_string()))
                    .map(|plain| Msg::decode(&plain));
                let failed = msg.is_err();
                if tx.send(msg).is_err() || failed {
                    return;
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(SideChannel { stream, sealer: StreamSealer::new(&keys.client_to_host), incoming, host_build: host.app_build.unwrap_or(0) })
    }

    pub fn send(&mut self, msg: &Msg) -> Result<(), String> {
        let sealed = self.sealer.seal(&msg.encode());
        write_frame(&mut self.stream, &sealed).map_err(|e| e.to_string())
    }

    /// The next message (Ok(None): nothing within `timeout`, or a type this client doesn't know).
    pub fn receive(&mut self, timeout: Duration) -> Result<Option<Msg>, String> {
        match self.incoming.recv_timeout(timeout) {
            Ok(r) => r,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(None),
            Err(_) => Err("connection closed".into()),
        }
    }
}

impl Drop for SideChannel {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn write_frame(stream: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(body);
    stream.write_all(&frame)
}

fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).map_err(|e| describe_read_error(&e))?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len >= 12_000_000 {
        return Err("bad frame length".into());
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).map_err(|e| describe_read_error(&e))?;
    Ok(body)
}

fn describe_read_error(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::UnexpectedEof => "the Mac closed the connection".into(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => "the Mac stopped answering".into(),
        _ => format!("receive failed ({e})"),
    }
}

fn handshake(shared: &Shared, stream: &mut TcpStream) -> Result<(SessionKeys, HostHello), End> {
    let target = shared.target.lock().unwrap().clone();
    handshake_with(stream, &target, None, &|host: &str| shared.events.ask_code(host))
}

/// Version 4 handshake (BB1Core/ControlChannel.swift). Returns the session keys and the host's hello.
/// `purpose` "update" is a side connection (screen previews) that doesn't touch the stream.
fn handshake_with(stream: &mut TcpStream, target: &Target, purpose: Option<&str>,
                  ask_code: &dyn Fn(&str) -> Option<String>) -> Result<(SessionKeys, HostHello), End> {
    let device_id = store::device_id();
    let mode = target.auth.mode();
    let client_nonce = crate::core::random_bytes::<32>().to_vec();
    let eph = Ephemeral::new();
    let hello = ClientHello {
        version: PROTOCOL_VERSION,
        nonce: client_nonce.clone(),
        name: store::device_name(),
        device_id: Some(device_id.clone()),
        mode: Some(mode.into()),
        ecdh_pub: Some(eph.public.to_vec()),
        app_build: None,
        purpose: purpose.map(String::from),
        codecs: Some(vec!["h264".into()]),
    };
    write_frame(stream, &Msg::ClientHello(v(hello)).encode()).map_err(|e| End::Closed(format!("send failed ({e})")))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let body = read_frame(stream).map_err(End::Closed)?;
    let host = match Msg::decode(&body) {
        Some(Msg::HostHello(h)) => h.v,
        Some(Msg::AuthFailed(r)) => return Err(End::Rejected(r.v)),
        _ => return Err(End::Closed("unexpected message during handshake".into())),
    };
    let Some(host_pub) = host.ecdh_pub.clone().filter(|_| host.version >= 4) else {
        return Err(End::Rejected("the Mac runs an older bb1; update it".into()));
    };
    let Some(shared_secret) = eph.shared(&host_pub) else {
        return Err(End::Rejected("bad key exchange".into()));
    };
    let host_id = host.host_id.clone().unwrap_or_default();
    let t = crypto::transcript(&client_nonce, &host.nonce, &eph.public, &host_pub, &device_id, &host_id, mode);
    let secret: Vec<u8> = match &target.auth {
        Auth::Device(s) => s.clone(),
        Auth::Tailscale => Vec::new(),
        Auth::Code => match ask_code(&target.label) {
            Some(code) if code.chars().any(|c| c.is_ascii_digit()) => {
                code.chars().filter(|c| c.is_ascii_digit()).collect::<String>().into_bytes()
            }
            _ => return Err(End::Rejected("pairing cancelled".into())),
        },
    };
    if !secret.is_empty() && !crypto::verify(&secret, "host", &t, &host.proof) {
        return Err(End::Rejected(match target.auth {
            Auth::Code => "That code doesn't match. Try again for a new code.".into(),
            _ => "the Mac didn't recognise this PC".into(),
        }));
    }
    let proof = if secret.is_empty() { Vec::new() } else { crypto::proof(&secret, "client", &t).to_vec() };
    write_frame(stream, &Msg::ClientProof(VData { v: proof }).encode()).map_err(|e| End::Closed(format!("send failed ({e})")))?;
    Ok((crypto::session_keys(&shared_secret, &secret, &t), host))
}

/// One connection, from TCP connect to disconnect. Returns why it ended and whether it got as far as "welcome".
fn run_session(shared: &Arc<Shared>) -> (End, bool) {
    let target = shared.target.lock().unwrap().clone();
    shared.events.status(&format!("Connecting to {}…", target.label), false);
    let connect_started = now_us();
    let mut stream = match connect_tcp(&target.address) {
        Ok(s) => s,
        Err(e) => return (End::Closed(e), false),
    };
    let _ = stream.set_nodelay(true);
    *shared.tcp.lock().unwrap() = stream.try_clone().ok();
    let host_ip = stream.peer_addr().map(|a| a.ip()).ok();

    let (keys, host) = match handshake(shared, &mut stream) {
        Ok(r) => r,
        Err(end) => return (end, false),
    };
    let _ = stream.set_read_timeout(None);
    if shared.rtt_us.load(Relaxed) == 0 {
        shared.rtt_us.store(((now_us() - connect_started) / 2).min(1_000_000), Relaxed);
    }
    {
        let mut vs = shared.video.lock().unwrap();
        vs.keys = Some(keys.clone());
        vs.supports_v2 = host.version >= 2;
    }

    // Reader and writer threads.
    let (ev_tx, ev_rx): (Sender<Ev>, Receiver<Ev>) = unbounded();
    let (msg_tx, msg_rx): (Sender<Msg>, Receiver<Msg>) = unbounded();
    *shared.writer.lock().unwrap() = Some(msg_tx.clone());
    {
        let mut reader = match stream.try_clone() {
            Ok(r) => r,
            Err(e) => return (End::Closed(format!("socket error ({e})")), false),
        };
        let tx = ev_tx.clone();
        let mut opener = StreamSealer::new(&keys.host_to_client);
        std::thread::Builder::new()
            .name("bb1 control in".into())
            .spawn(move || {
                let mut first = true;
                loop {
                    let body = match read_frame(&mut reader) {
                        Ok(b) => b,
                        Err(e) => {
                            let _ = tx.send(Ev::Closed(e));
                            return;
                        }
                    };
                    let Some(plain) = opener.open(&body) else {
                        // The host refused our proof: that reply isn't sealed.
                        if first {
                            if let Some(Msg::AuthFailed(r)) = Msg::decode(&body) {
                                let _ = tx.send(Ev::Rejected(r.v));
                                return;
                            }
                        }
                        let _ = tx.send(Ev::Closed("message failed authentication".into()));
                        return;
                    };
                    first = false;
                    match Msg::decode(&plain) {
                        Some(m) => {
                            if tx.send(Ev::Msg(m)).is_err() {
                                return;
                            }
                        }
                        None => {} // a newer host's message type: skip it
                    }
                }
            })
            .unwrap();
    }
    {
        let mut writer = match stream.try_clone() {
            Ok(w) => w,
            Err(e) => return (End::Closed(format!("socket error ({e})")), false),
        };
        let tx = ev_tx.clone();
        let mut sealer = StreamSealer::new(&keys.client_to_host);
        std::thread::Builder::new()
            .name("bb1 control out".into())
            .spawn(move || {
                for msg in msg_rx {
                    let sealed = sealer.seal(&msg.encode());
                    if let Err(e) = write_frame(&mut writer, &sealed) {
                        let _ = tx.send(Ev::Closed(format!("send failed ({e})")));
                        return;
                    }
                }
            })
            .unwrap();
    }
    drop(ev_tx);

    let mut session = Session {
        shared: shared.clone(),
        keys,
        host_ip,
        config: None,
        udp: None,
        welcomed: false,
        ping_seq: 0,
        last_pong_us: now_us(),
        offset_samples: Vec::new(),
        history: Vec::new(),
        sent_now: 0,
        checks_resume_us: 0,
        udp_reset_us: 0,
        decoder_reset_us: 0,
        last_path_check_us: 0,
        last_stats: (0, 0, 0),
        stats_base: (0, 0, 0, 0, 0, 0),
        warned_codec: false,
    };
    let mut next_ping = now_us();
    let mut next_hello = now_us() + 500_000;
    let mut next_stats = now_us() + 1_000_000;
    loop {
        let now = now_us();
        let wake = next_ping.min(next_hello).min(next_stats);
        let timeout = Duration::from_micros(wake.saturating_sub(now).max(1000));
        match ev_rx.recv_timeout(timeout) {
            Ok(Ev::Msg(m)) => {
                if let Some(end) = session.handle(m) {
                    return (end, session.welcomed);
                }
            }
            Ok(Ev::Closed(why)) => return (End::Closed(why), session.welcomed),
            Ok(Ev::Rejected(why)) => return (End::Rejected(why), session.welcomed),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                return (End::Closed("connection closed".into()), session.welcomed);
            }
        }
        let now = now_us();
        if now >= next_ping {
            next_ping = now + 250_000;
            session.send_ping();
            if let Some(end) = session.watchdog() {
                return (end, session.welcomed);
            }
        }
        if now >= next_hello {
            next_hello = now + 500_000;
            session.send_udp_hello();
        }
        if now >= next_stats {
            next_stats = now + 1_000_000;
            session.publish_stats();
        }
    }
}

struct Sample {
    us: u64,
    sent: u64,
    received: u64,
    accepted: u64,
    decoded: u64,
    displayed: u64,
}

struct Session {
    shared: Arc<Shared>,
    keys: SessionKeys,
    host_ip: Option<IpAddr>,
    config: Option<StreamConfig>,
    udp: Option<Arc<UdpSocket>>,
    welcomed: bool,
    ping_seq: u32,
    last_pong_us: u64,
    offset_samples: Vec<(u64, i64)>,
    history: Vec<Sample>,
    sent_now: u64,
    checks_resume_us: u64,
    udp_reset_us: u64,
    decoder_reset_us: u64,
    last_path_check_us: u64,
    last_stats: (u64, u64, u64),
    stats_base: (u64, u64, u64, u64, u64, u64),
    warned_codec: bool,
}

impl Session {
    fn send(&self, m: Msg) {
        if let Some(tx) = self.shared.writer.lock().unwrap().as_ref() {
            let _ = tx.send(m);
        }
    }

    fn handle(&mut self, m: Msg) -> Option<End> {
        let shared = self.shared.clone();
        match m {
            Msg::Welcome { host_name } => {
                self.welcomed = true;
                logi!("client: connected to {host_name}");
                shared.events.status(&format!("Connected to {host_name}"), true);
                // Send this side's log lines to the host too, so everything is in one place.
                if let Some(tx) = shared.writer.lock().unwrap().clone() {
                    crate::core::log::set_sink(Some(Box::new(move |line: &str| {
                        let _ = tx.send(Msg::ClientLog(v(format!("[windows] {line}"))));
                    })));
                }
                logi!("client: bb1 for Windows {} on {}", env!("CARGO_PKG_VERSION"), store::device_name());
                self.send(Msg::SetCursorInVideo(v(shared.cursor_in_video.load(Relaxed))));
                if shared.sound_wanted.load(Relaxed) {
                    self.send(Msg::SetAudio(v(true)));
                }
                shared.events.welcome(&host_name);
            }
            Msg::StreamConfig(c) => {
                let c = c.v;
                let old = self.config.clone();
                logi!("client: stream {}x{} {}{} {} Mbps, epoch {}", c.width, c.height, c.codec_name(),
                      if c.low_latency_encoder { " low-latency" } else { "" }, c.bitrate_mbps, c.epoch);
                if c.codec != CODEC_H264 && !self.warned_codec {
                    self.warned_codec = true;
                    logi!("client: the host is sending {}, which this PC can't decode", c.codec_name());
                    shared.events.notice("The Mac is sending HEVC video, which Windows can't decode. Update bb1 on the Mac (it then sends H.264 to Windows).");
                }
                self.config = Some(c.clone());
                let reopen = old.as_ref().map(|o| (o.session_id, o.udp_port)) != Some((c.session_id, c.udp_port)) || self.udp.is_none();
                if reopen {
                    self.open_video_path(&c);
                }
                if old.as_ref().map(|o| o.epoch) != Some(c.epoch) {
                    let mut vs = shared.video.lock().unwrap();
                    vs.current_epoch = c.epoch;
                    vs.waiting_for_keyframe = true;
                    self.checks_resume_us = now_us() + 3_000_000;
                }
                shared.events.stream_config(&c);
            }
            Msg::Pong(p) => self.handle_pong(p.v),
            Msg::Notice(t) => {
                logi!("client: host says: {}", t.v);
                shared.events.notice(&t.v);
            }
            Msg::Bye(r) => return Some(End::Ended(format!("The Mac ended the session: {}", r.v))),
            Msg::Clipboard(c) => shared.events.clipboard(c.v),
            Msg::Cursor(c) => shared.events.cursor(c.v),
            Msg::Paired(p) => {
                let p = p.v;
                let mut t = shared.target.lock().unwrap();
                let known = KnownHost {
                    host_id: p.host_id.clone(),
                    name: p.host_name.clone(),
                    secret: p.secret.clone(),
                    tailscale_ip: t.tailscale_ip.clone(),
                    address: Some(t.address.clone()),
                };
                KnownHost::remember(known.clone());
                t.auth = Auth::Device(p.secret);
                drop(t);
                logi!("client: paired with {}; this PC will sign in automatically from now on", p.host_name);
                shared.events.paired(&known);
            }
            _ => {}
        }
        None
    }

    fn open_video_path(&mut self, c: &StreamConfig) {
        let Some(ip) = self.host_ip else {
            logi!("client: can't work out the Mac's address for video");
            return;
        };
        let generation = self.shared.udp_generation.fetch_add(1, Relaxed) + 1;
        self.shared.video.lock().unwrap().reassembler.reset();
        let addr = SocketAddr::new(ip, c.udp_port);
        let socket = match make_udp(addr) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                logi!("client: couldn't open the video path: {e}");
                return;
            }
        };
        self.udp = Some(socket.clone());
        self.checks_resume_us = now_us() + 3_000_000;
        let hello = crypto::udp_hello(c.session_id, &self.keys.video);
        let shared = self.shared.clone();
        std::thread::Builder::new()
            .name("bb1 video in".into())
            .spawn(move || udp_loop(shared, socket, generation, hello))
            .unwrap();
        logi!("client: video path to {addr}");
    }

    fn send_udp_hello(&self) {
        if let (Some(udp), Some(c)) = (self.udp.as_ref(), self.config.as_ref()) {
            let _ = udp.send(&crypto::udp_hello(c.session_id, &self.keys.video));
        }
    }

    fn send_ping(&mut self) {
        self.ping_seq = self.ping_seq.wrapping_add(1);
        let c = &self.shared.counters;
        self.send(Msg::Ping(v(PingMsg {
            seq: self.ping_seq,
            client_us: now_us(),
            received: c.received.load(Relaxed),
            decoded: c.decoded.load(Relaxed),
            displayed: c.displayed.load(Relaxed),
        })));
    }

    fn handle_pong(&mut self, p: PongMsg) {
        let now = now_us();
        self.last_pong_us = now;
        let rtt = now.saturating_sub(p.client_us);
        let old = self.shared.rtt_us.load(Relaxed);
        let new = if old == 0 { rtt } else { (old as f64 * 0.8 + rtt as f64 * 0.2) as u64 };
        self.shared.rtt_us.store(new, Relaxed);
        // Clock offset from the lowest-RTT sample among recent ones (least queueing noise).
        self.offset_samples.push((rtt, p.host_us as i64 - (p.client_us + rtt / 2) as i64));
        if self.offset_samples.len() > 40 {
            self.offset_samples.remove(0);
        }
        if let Some(best) = self.offset_samples.iter().min_by_key(|s| s.0) {
            *self.shared.clock_offset_us.lock().unwrap() = Some(best.1);
        }
        self.sent_now = p.sent;
        let c = &self.shared.counters;
        self.history.push(Sample {
            us: now,
            sent: p.sent,
            received: c.received.load(Relaxed),
            accepted: c.accepted.load(Relaxed),
            decoded: c.decoded.load(Relaxed),
            displayed: c.displayed.load(Relaxed),
        });
        self.history.retain(|s| now - s.us <= 3_000_000);
    }

    fn watchdog(&mut self) -> Option<End> {
        let now = now_us();
        let rtt = self.shared.rtt_us.load(Relaxed);
        if now - self.last_pong_us > 2_000_000 {
            return Some(End::Closed("no heartbeat from the Mac for 2 s".into()));
        }
        if now - self.last_path_check_us > 15_000_000 {
            self.last_path_check_us = now;
            self.check_path();
        }
        {
            let mut vs = self.shared.video.lock().unwrap();
            vs.reassembler.expire(now, (rtt * 4 + 100_000).max(250_000));
            if vs.waiting_for_keyframe && now - vs.last_keyframe_request_us > (rtt * 3 / 2 + 100_000).max(300_000) {
                request_keyframe(&self.shared, &mut vs, "still waiting for keyframe");
            }
        }
        // Compare counters now vs ~1 s ago to find a stalled stage.
        if now <= self.checks_resume_us {
            return None;
        }
        let latest = self.history.last()?;
        let past = self.history.iter().rev().find(|s| latest.us - s.us >= 900_000)?;
        if latest.us - past.us >= 2_000_000 {
            return None;
        }
        let sent = latest.sent.wrapping_sub(past.sent);
        let received = latest.received.wrapping_sub(past.received);
        let accepted = latest.accepted.wrapping_sub(past.accepted);
        let decoded = latest.decoded.wrapping_sub(past.decoded);
        let displayed = latest.displayed.wrapping_sub(past.displayed);
        if sent >= 3 && received == 0 {
            if self.udp_reset_us == 0 || now - self.udp_reset_us > 6_000_000 {
                logi!("client: host sent {sent} frames but none arrived; reopening the video path");
                self.udp_reset_us = now;
                if let Some(c) = self.config.clone() {
                    self.open_video_path(&c);
                }
                let mut vs = self.shared.video.lock().unwrap();
                vs.waiting_for_keyframe = true;
                request_keyframe(&self.shared, &mut vs, "video path reopened");
                drop(vs);
                self.history.clear();
            } else if now - self.udp_reset_us > 2_500_000 {
                self.udp_reset_us = 0;
                return Some(End::Closed("video still not arriving after reopening the video path".into()));
            }
        } else if ((accepted >= 3 && decoded == 0) || (decoded >= 3 && displayed == 0)) && now - self.decoder_reset_us > 2_000_000 {
            logi!("client: {accepted} frames went to the decoder, {decoded} came out, {displayed} shown; resetting the decoder");
            self.decoder_reset_us = now;
            self.checks_resume_us = now + 3_000_000;
            self.shared.events.reset_decoder();
            let mut vs = self.shared.video.lock().unwrap();
            vs.waiting_for_keyframe = true;
            request_keyframe(&self.shared, &mut vs, "decoder stalled");
        }
        None
    }

    fn check_path(&self) {
        let ip = self.shared.target.lock().unwrap().tailscale_ip.clone();
        let shared = self.shared.clone();
        match ip {
            None => {
                let mut p = shared.path.lock().unwrap();
                if p.is_empty() {
                    *p = "home network".into();
                    logi!("client: connected over the home network");
                }
            }
            Some(ip) => {
                std::thread::spawn(move || {
                    let d = format!("Tailscale · {}", tailscale::path_description(&ip).unwrap_or_else(|| "connected".into()));
                    let mut p = shared.path.lock().unwrap();
                    if *p != d {
                        logi!("client: connected via {d}");
                        *p = d;
                    }
                });
            }
        }
    }

    fn publish_stats(&mut self) {
        let now = now_us();
        let c = &self.shared.counters;
        let displayed = c.displayed.load(Relaxed);
        let bytes = c.bytes.load(Relaxed);
        let mut s = Stats {
            config: self.config.clone(),
            rtt_ms: self.shared.rtt_us.load(Relaxed) as f64 / 1000.0,
            path: self.shared.path.lock().unwrap().clone(),
            connected: self.welcomed,
            ..Default::default()
        };
        if self.last_stats.0 > 0 {
            let secs = (now - self.last_stats.0) as f64 / 1e6;
            s.fps = displayed.wrapping_sub(self.last_stats.1) as f64 / secs;
            s.mbps = bytes.wrapping_sub(self.last_stats.2) as f64 * 8.0 / 1e6 / secs;
        }
        self.last_stats = (now, displayed, bytes);
        let samples = std::mem::take(&mut *self.shared.latency_samples.lock().unwrap());
        if !samples.is_empty() {
            s.latency_ms = Some(samples.iter().sum::<f64>() / samples.len() as f64);
            s.latency_max_ms = samples.iter().cloned().reduce(f64::max);
        }
        let lost = c.lost_frames.load(Relaxed);
        let nacks = c.nacks_sent.load(Relaxed);
        let recovered = c.recovered_by_resend.load(Relaxed);
        let kf = c.keyframe_requests.load(Relaxed);
        let audio = c.audio_packets.swap(0, Relaxed);
        let (concealed, underruns, buffer_ms) = self.shared.audio.take_stats();
        if self.welcomed {
            let b = self.stats_base;
            logi!(
                "stats: shown {:.0} fps · {:.1} Mbps · RTT {:.1} ms · capture→shown avg {} max {} ms · lost {} · resend asks {}, recovered {} · keyframe req {} · sound {} pkts, {} gaps, {} dry, buffer {} ms",
                s.fps, s.mbps, s.rtt_ms,
                s.latency_ms.map(|l| format!("{l:.0}")).unwrap_or("-".into()),
                s.latency_max_ms.map(|l| format!("{l:.0}")).unwrap_or("-".into()),
                lost - b.0, nacks - b.1, recovered - b.2, kf - b.3, audio, concealed, underruns, buffer_ms
            );
        }
        self.stats_base = (lost, nacks, recovered, kf, 0, 0);
        self.shared.events.stats(&s);
    }
}

fn make_udp(addr: SocketAddr) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    // Room for a whole keyframe burst while this thread is busy.
    let _ = socket.set_recv_buffer_size(8 << 20);
    let bind: SocketAddr = if addr.is_ipv6() { "[::]:0".parse().unwrap() } else { "0.0.0.0:0".parse().unwrap() };
    socket.bind(&bind.into())?;
    socket.connect(&addr.into())?;
    Ok(socket.into())
}

fn request_keyframe(shared: &Shared, vs: &mut VideoState, reason: &str) {
    let now = now_us();
    let rtt = shared.rtt_us.load(Relaxed);
    // One request per round trip at most: asking again before the keyframe could arrive just adds big frames.
    if now - vs.last_keyframe_request_us <= (rtt * 6 / 5).max(200_000) {
        return;
    }
    vs.last_keyframe_request_us = now;
    shared.counters.keyframe_requests.fetch_add(1, Relaxed);
    if let Some(tx) = shared.writer.lock().unwrap().as_ref() {
        let _ = tx.send(Msg::RequestKeyframe { reason: reason.into() });
    }
}

// MARK: Video path

fn udp_loop(shared: Arc<Shared>, socket: Arc<UdpSocket>, generation: u64, hello: Vec<u8>) {
    let started = now_us();
    let mut hellos_sent = 0usize;
    let mut buf = vec![0u8; 2048];
    let mut last_resend_check = 0u64;
    let _ = socket.send(&hello);
    loop {
        if shared.udp_generation.load(Relaxed) != generation || shared.stopped.load(Relaxed) {
            return;
        }
        // A few hellos in quick succession: the first one can arrive before the host is ready for it.
        let since = now_us() - started;
        let schedule = [50_000u64, 150_000, 300_000];
        if hellos_sent < schedule.len() && since >= schedule[hellos_sent] {
            hellos_sent += 1;
            let _ = socket.send(&hello);
        }
        let busy = {
            let vs = shared.video.lock().unwrap();
            vs.reassembler.has_pending() || vs.gap_since_us > 0
        };
        let _ = socket.set_read_timeout(Some(Duration::from_millis(if busy { 3 } else { 50 })));
        match socket.recv(&mut buf) {
            Ok(n) => handle_datagram(&shared, &buf[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            // Windows reports an ICMP "port unreachable" from an earlier send as an error here; keep going.
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
        let now = now_us();
        if busy && now - last_resend_check >= 3_000 {
            last_resend_check = now;
            ask_for_resends(&shared);
        }
    }
}

fn handle_datagram(shared: &Shared, data: &[u8]) {
    shared.counters.bytes.fetch_add(data.len() as u64, Relaxed);
    let mut vs = shared.video.lock().unwrap();
    if data.first() == Some(&crypto::AUDIO_TYPE) {
        if let Some(keys) = vs.keys.as_ref() {
            if let Some((seq, _, pcm)) = crypto::open_audio(data, &keys.video) {
                shared.counters.audio_packets.fetch_add(1, Relaxed);
                if shared.sound_wanted.load(Relaxed) {
                    shared.audio.write(seq, &pcm);
                }
            }
        }
        return;
    }
    if let Some(frame) = vs.reassembler.add(data, now_us()) {
        shared.counters.received.fetch_add(1, Relaxed);
        process(shared, &mut vs, frame);
    }
}

/// Puts frames back in order. After a gap, later frames wait (briefly) for the missing one to be resent.
fn process(shared: &Shared, vs: &mut VideoState, frame: Frame) {
    if vs.keys.is_none() {
        return;
    }
    if frame.was_retransmitted {
        shared.counters.recovered_by_resend.fetch_add(1, Relaxed);
    }
    if vs.last_frame_id != 0 && frame.frame_id <= vs.last_frame_id {
        return;
    }
    if frame.is_keyframe() {
        if frame.epoch.wrapping_sub(vs.current_epoch) > 0x8000 {
            return; // keyframe from an older encoder
        }
        vs.pending.retain(|&id, _| id >= frame.frame_id);
        deliver(shared, vs, frame);
        drain_pending(shared, vs);
        return;
    }
    if vs.waiting_for_keyframe {
        request_keyframe(shared, vs, "waiting for keyframe");
        return;
    }
    if frame.frame_id == vs.last_frame_id.wrapping_add(1) && frame.epoch == vs.current_epoch {
        deliver(shared, vs, frame);
        drain_pending(shared, vs);
    } else if frame.epoch != vs.current_epoch {
        give_up_on_gap(shared, vs, "frame from a restarted encoder");
    } else {
        vs.pending.insert(frame.frame_id, frame);
        if vs.gap_since_us == 0 {
            vs.gap_since_us = now_us();
        }
        if !vs.supports_v2 {
            give_up_on_gap(shared, vs, "lost frame");
        }
    }
}

fn drain_pending(shared: &Shared, vs: &mut VideoState) {
    while !vs.waiting_for_keyframe {
        let Some(next) = vs.pending.remove(&vs.last_frame_id.wrapping_add(1)) else { break };
        if next.epoch != vs.current_epoch && !next.is_keyframe() {
            give_up_on_gap(shared, vs, "frame from a restarted encoder");
            return;
        }
        deliver(shared, vs, next);
    }
    vs.gap_since_us = if vs.pending.is_empty() { 0 } else { now_us() };
}

/// The missing frame isn't coming: start over from a keyframe.
fn give_up_on_gap(shared: &Shared, vs: &mut VideoState, reason: &str) {
    let first_pending = vs.pending.keys().min().copied().unwrap_or(vs.last_frame_id.wrapping_add(2));
    let missing = if first_pending > vs.last_frame_id { (first_pending - vs.last_frame_id - 1) as u64 } else { 1 };
    shared.counters.lost_frames.fetch_add(missing.max(1), Relaxed);
    vs.pending.clear();
    vs.gap_since_us = 0;
    vs.waiting_for_keyframe = true;
    request_keyframe(shared, vs, reason);
}

fn deliver(shared: &Shared, vs: &mut VideoState, frame: Frame) {
    let payload = vs
        .keys
        .as_ref()
        .and_then(|k| crypto::FrameCrypto::open(&frame.sealed, &k.video, frame.frame_id, &frame.aad()))
        .and_then(|plain| FramePayload::decode(&plain));
    let Some(payload) = payload else {
        give_up_on_gap(shared, vs, "frame failed to decrypt");
        return;
    };
    vs.last_frame_id = frame.frame_id;
    vs.current_epoch = frame.epoch;
    vs.waiting_for_keyframe = false;
    vs.reassembler.set_floor(frame.frame_id);
    shared.counters.accepted.fetch_add(1, Relaxed);
    shared.events.video(payload, frame.capture_us);
}

/// Ask the host to resend missing packets, and give up on gaps that took too long.
fn ask_for_resends(shared: &Shared) {
    let mut vs = shared.video.lock().unwrap();
    if vs.keys.is_none() || !vs.supports_v2 {
        return;
    }
    let now = now_us();
    let rtt = shared.rtt_us.load(Relaxed);
    let retry = (rtt * 3 / 2).max(8_000);
    let last = vs.last_frame_id;
    let requests = vs.reassembler.requests(now, 3_000, retry, 4);
    for r in requests.into_iter().filter(|r| r.frame_id > last) {
        shared.counters.nacks_sent.fetch_add(1, Relaxed);
        if let Some(tx) = shared.writer.lock().unwrap().as_ref() {
            let _ = tx.send(Msg::Nack(v(NackMsg { frame_id: r.frame_id, indices: r.indices })));
        }
    }
    if vs.gap_since_us > 0 && now - vs.gap_since_us > (rtt * 4).max(80_000) {
        give_up_on_gap(shared, &mut vs, "lost frame (resend didn't arrive in time)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_get_the_default_port() {
        assert_eq!(with_port("100.80.66.124"), "100.80.66.124:47800");
        assert_eq!(with_port("kenobi.local:5000"), "kenobi.local:5000");
        assert_eq!(with_port("fd7a:115c:a1e0::1"), "[fd7a:115c:a1e0::1]:47800");
        assert_eq!(host_part("[fd7a:115c:a1e0::1]:47800"), "fd7a:115c:a1e0::1");
        assert_eq!(host_part("100.80.66.124:47800"), "100.80.66.124");
    }
}
