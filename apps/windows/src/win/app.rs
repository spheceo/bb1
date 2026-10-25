//! Start-up, the UI thread's event queue, and connecting/disconnecting.
//!
//! The connection's threads never touch windows: they push a `UiEvent` and post WM_APP_EVENT to
//! the host list window, and the UI thread handles it. Video frames skip the UI thread and go
//! straight to the stream window's video thread.

use crate::core::client::{Client, Events, Stats, Target};
use crate::core::proto::{ClipboardMsg, CursorMsg, StreamConfig};
use crate::core::store::{self, KnownHost, Settings};
use crate::core::video::FramePayload;
use crate::logi;
use crate::win::render::VideoCmd;
use crate::win::{connect, stream};
use crossbeam_channel::{Sender, bounded};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Media::MediaFoundation::{MFSTARTUP_FULL, MFStartup};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_APP_EVENT: u32 = WM_APP + 1;
const MF_VERSION: u32 = 0x0002_0070;

pub enum UiEvent {
    Status(u64, String, bool),
    Ended(u64, String),
    AskCode(String, Sender<Option<String>>),
    Notice(String),
    Clipboard(ClipboardMsg),
    Cursor(CursorMsg),
    Stats(Stats),
    Welcome(String),
    Config(StreamConfig),
    Paired(String),
    HostsChanged,
}

static QUEUE: Mutex<VecDeque<UiEvent>> = Mutex::new(VecDeque::new());
/// The host list window: it receives WM_APP_EVENT.
static EVENT_HWND: AtomicIsize = AtomicIsize::new(0);

/// From any thread.
pub fn post(ev: UiEvent) {
    QUEUE.lock().unwrap().push_back(ev);
    let hwnd = EVENT_HWND.load(Relaxed);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as _)), WM_APP_EVENT, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn set_event_window(hwnd: HWND) {
    EVENT_HWND.store(hwnd.0 as isize, Relaxed);
}

/// The host list window (lives as long as the app; owns the clipboard when we write to it).
pub fn event_window() -> Option<HWND> {
    let hwnd = EVENT_HWND.load(Relaxed);
    (hwnd != 0).then(|| HWND(hwnd as _))
}

/// The connection's events, for one stream window.
struct AppEvents {
    video: Sender<VideoCmd>,
    /// Which connection this is: events from an older one are ignored.
    generation: u64,
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Events for AppEvents {
    fn status(&self, text: &str, connected: bool) {
        post(UiEvent::Status(self.generation, text.into(), connected));
    }
    fn ended(&self, reason: &str) {
        post(UiEvent::Ended(self.generation, reason.into()));
    }
    fn ask_code(&self, host: &str) -> Option<String> {
        let (tx, rx) = bounded(1);
        post(UiEvent::AskCode(host.into(), tx));
        rx.recv().ok().flatten()
    }
    fn video(&self, payload: FramePayload, capture_us: u64) {
        let _ = self.video.send(VideoCmd::Frame(payload, capture_us));
    }
    fn reset_decoder(&self) {
        let _ = self.video.send(VideoCmd::ResetDecoder);
    }
    fn welcome(&self, host_name: &str) {
        post(UiEvent::Welcome(host_name.into()));
    }
    fn stream_config(&self, config: &StreamConfig) {
        let _ = self.video.send(VideoCmd::Size(config.width as u32, config.height as u32));
        post(UiEvent::Config(config.clone()));
    }
    fn notice(&self, text: &str) {
        post(UiEvent::Notice(text.into()));
    }
    fn clipboard(&self, clip: ClipboardMsg) {
        post(UiEvent::Clipboard(clip));
    }
    fn cursor(&self, cursor: CursorMsg) {
        post(UiEvent::Cursor(cursor));
    }
    fn paired(&self, host: &KnownHost) {
        post(UiEvent::Paired(host.name.clone()));
    }
    fn stats(&self, stats: &Stats) {
        post(UiEvent::Stats(stats.clone()));
    }
}

/// UI thread state.
pub struct App {
    pub settings: Settings,
    pub client: Option<Client>,
}

thread_local! {
    pub static APP: RefCell<App> = RefCell::new(App { settings: Settings::load(), client: None });
}

pub fn settings() -> Settings {
    APP.with(|a| a.borrow().settings.clone())
}

pub fn update_settings(f: impl FnOnce(&mut Settings)) {
    APP.with(|a| {
        let mut a = a.borrow_mut();
        f(&mut a.settings);
        a.settings.save();
    });
}

pub fn client() -> Option<Client> {
    APP.with(|a| a.borrow().client.clone())
}

pub fn run() {
    crate::core::log::setup(store::log_dir(), "client");
    logi!("bb1 for Windows {} starting on {}", env!("CARGO_PKG_VERSION"), store::device_name());
    std::panic::set_hook(Box::new(|info| {
        logi!("CRASH: {info}");
    }));
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            logi!("app: Media Foundation didn't start ({e}); video can't be decoded");
        }
    }
    let main = connect::create();
    set_event_window(main);
    // Started with an address (bb1.exe 100.80.66.124): connect straight away.
    if let Some(address) = std::env::args().nth(1).filter(|a| !a.starts_with('-')) {
        connect_to(&address, "");
    }
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if connect::is_dialog_message(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if let Some(c) = client() {
        c.stop(); // says bye, so the Mac lets go of the keys and ends the session at once
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    logi!("bb1 for Windows quit");
    // Don't wait for background work (Tailscale check, previews, network listeners) to wind down.
    std::process::exit(0);
}

/// Opens a stream window and connects to `address` ("100.80.66.124", "kenobi.local:47800").
pub fn connect_to(address: &str, label: &str) {
    if client().is_some() {
        return;
    }
    let target = Target::new(address, label);
    logi!("app: connecting to {} at {} (sign-in: {})", target.label, target.address, target.auth.mode());
    update_settings(|s| s.last_address = Some(address.to_string()));
    let video = stream::open(&target.label);
    let settings = settings();
    let generation = GENERATION.fetch_add(1, Relaxed) + 1;
    let events = std::sync::Arc::new(AppEvents { video, generation });
    let client = Client::start(target, events, settings.sound, false);
    stream::attach(client.clone());
    APP.with(|a| a.borrow_mut().client = Some(client));
    connect::hide();
}

/// The stream window was closed (or Disconnect chosen): back to the host list right away.
pub fn disconnect() {
    GENERATION.fetch_add(1, Relaxed); // whatever the old connection still says is stale now
    if let Some(c) = APP.with(|a| a.borrow_mut().client.take()) {
        c.stop();
    }
    stream::close();
    connect::show("");
}

/// UI thread: handle everything the connection posted.
pub fn drain_events() {
    loop {
        let Some(ev) = QUEUE.lock().unwrap().pop_front() else { break };
        match ev {
            UiEvent::Status(generation, text, connected) => {
                if generation != GENERATION.load(Relaxed) {
                    continue;
                }
                stream::status(&text, connected);
                connect::set_status(&text);
            }
            UiEvent::Ended(generation, reason) => {
                if generation != GENERATION.load(Relaxed) {
                    continue;
                }
                logi!("app: session ended: {reason}");
                if let Some(c) = APP.with(|a| a.borrow_mut().client.take()) {
                    c.stop();
                }
                stream::close();
                connect::show(if reason == "Disconnected." { "" } else { &reason });
            }
            UiEvent::AskCode(host, reply) => connect::ask_code(&host, reply),
            UiEvent::Notice(text) => stream::notice(&text),
            UiEvent::Clipboard(clip) => {
                if settings().clipboard {
                    crate::win::clipboard::apply(&clip);
                }
            }
            UiEvent::Cursor(c) => stream::set_cursor(&c),
            UiEvent::Stats(s) => stream::stats(&s),
            UiEvent::Welcome(host) => stream::welcome(&host),
            UiEvent::Config(c) => stream::config(&c),
            UiEvent::Paired(name) => connect::set_status(&format!("Paired with {name}.")),
            UiEvent::HostsChanged => connect::refresh(),
        }
    }
}
