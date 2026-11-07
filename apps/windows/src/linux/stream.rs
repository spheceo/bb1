//! The stream window (`bb1 --stream ADDRESS`): the other computer's screen, with this computer's
//! keyboard, mouse, sound, clipboard and game controllers. SDL3, so it runs on Wayland and X11.
//!
//! Control follows the pointer, like the Windows client: while this window is in front and the
//! pointer is on the picture (or the window is full screen), the keyboard is grabbed and every key
//! goes to the other computer, Super and Alt+Tab included (GNOME asks once whether bb1 may do that).
//! Move the pointer off the picture and the keyboard is this computer's again.
//! - Ctrl+Alt+Enter: full screen. Ctrl+Alt+→ / ←: the host's next / previous screen.
//!
//! It runs as its own process so the main window (ui.rs) stays responsive. It talks to that window
//! over stdout (lines starting with "@": status, ended, ask-code, …) and reads pairing codes from
//! stdin. Started from a terminal, it asks for the code there.

use crate::core::client::{Client, Events, Stats, Target};
use crate::core::gamepad::{self, Slots};
use crate::core::keymap::{self, MAC_CAPS_LOCK, ModifierLayout};
use crate::core::proto::*;
use crate::core::store::{KnownHost, Settings};
use crate::core::video::FramePayload;
use crate::linux::decoder::{Decoder, Layout, Picture};
use crate::logi;
use crossbeam_channel::{Receiver, Sender, unbounded};
use sdl3_sys::everything::*;
use std::collections::HashSet;
use std::ffi::{CStr, CString, c_void};
use std::io::{BufRead, IsTerminal, Write};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// At most one pointer move per this long (125 a second, about what a Mac sends).
const MOVE_EVERY: Duration = Duration::from_millis(8);

/// One line to the main window.
fn tell(kind: &str, text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "@{kind} {}", text.replace('\n', " "));
    let _ = out.flush();
}

enum Ui {
    Status(String, bool),
    Ended(String),
    Welcome(String),
    Notice(String),
    Cursor(CursorMsg),
    Clipboard(ClipboardMsg),
    Stats(Stats),
    Displays(DisplaysMsg),
    Rumble(RumbleMsg),
    Paired(String),
}

enum VideoCmd {
    Frame(FramePayload, u64),
    Reset,
}

/// The newest decoded picture, waiting for the main thread.
#[derive(Default)]
struct Slot {
    picture: Mutex<Option<(Picture, u64)>>,
    wake_pending: AtomicBool,
}

struct StreamEvents {
    ui: Sender<Ui>,
    video: Sender<VideoCmd>,
    wake_type: u32,
    slot: Arc<Slot>,
}

fn wake(kind: u32) {
    unsafe {
        let mut ev: SDL_Event = std::mem::zeroed();
        ev.r#type = kind;
        SDL_PushEvent(&mut ev);
    }
}

impl StreamEvents {
    fn post(&self, u: Ui) {
        let _ = self.ui.send(u);
        wake(self.wake_type);
    }
}

impl Events for StreamEvents {
    fn status(&self, text: &str, connected: bool) {
        self.post(Ui::Status(text.into(), connected));
    }
    fn ended(&self, reason: &str) {
        self.post(Ui::Ended(reason.into()));
    }
    fn ask_code(&self, host: &str) -> Option<String> {
        self.post(Ui::Status(format!("Type the 6-digit code shown on {host}"), false));
        tell("ask-code", host);
        if std::io::stdin().is_terminal() {
            eprint!("Type the 6-digit code shown on {host}: ");
        }
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).ok()?;
        let code: String = line.chars().filter(|c| c.is_ascii_digit()).collect();
        (!code.is_empty()).then_some(code)
    }
    fn video(&self, payload: FramePayload, capture_us: u64) {
        let _ = self.video.send(VideoCmd::Frame(payload, capture_us));
    }
    fn reset_decoder(&self) {
        let _ = self.video.send(VideoCmd::Reset);
    }
    fn welcome(&self, host_name: &str) {
        self.post(Ui::Welcome(host_name.into()));
    }
    fn notice(&self, text: &str) {
        self.post(Ui::Notice(text.into()));
    }
    fn clipboard(&self, clip: ClipboardMsg) {
        self.post(Ui::Clipboard(clip));
    }
    fn cursor(&self, cursor: CursorMsg) {
        self.post(Ui::Cursor(cursor));
    }
    fn paired(&self, host: &KnownHost) {
        self.post(Ui::Paired(host.name.clone()));
    }
    fn stats(&self, stats: &Stats) {
        self.post(Ui::Stats(stats.clone()));
    }
    fn displays(&self, d: DisplaysMsg) {
        self.post(Ui::Displays(d));
    }
    fn rumble(&self, r: RumbleMsg) {
        self.post(Ui::Rumble(r));
    }
}

/// Decodes on its own thread; the main thread shows the newest picture.
fn decode_loop(rx: Receiver<VideoCmd>, slot: Arc<Slot>, client: Arc<Mutex<Option<Client>>>, wake_type: u32) {
    let mut decoder: Option<Decoder> = None;
    let mut failures = 0u32;
    let client = || client.lock().unwrap().clone();
    while let Ok(cmd) = rx.recv() {
        let (payload, capture_us) = match cmd {
            VideoCmd::Frame(p, us) => (p, us),
            VideoCmd::Reset => {
                decoder = None;
                continue;
            }
        };
        let Some(annex_b) = payload.annex_b() else { continue };
        if decoder.as_ref().is_some_and(|d| d.codec() != payload.codec) {
            decoder = None;
        }
        if decoder.is_none() {
            match Decoder::new(payload.codec) {
                Ok(d) => decoder = Some(d),
                Err(e) => {
                    logi!("decoder: {e}");
                    if let Some(c) = client() {
                        c.decoder_failed(&e);
                    }
                    continue;
                }
            }
        }
        let d = decoder.as_mut().unwrap();
        match d.decode(&annex_b) {
            Ok(Some(picture)) => {
                failures = 0;
                if let Some(c) = client() {
                    c.mark_decoded();
                }
                *slot.picture.lock().unwrap() = Some((picture, capture_us));
                if !slot.wake_pending.swap(true, Relaxed) {
                    wake(wake_type);
                }
            }
            Ok(None) => {}
            Err(e) => {
                failures += 1;
                logi!("decoder: {e}");
                if failures >= 2 {
                    // Start over at the next keyframe.
                    decoder = None;
                    failures = 0;
                }
                if let Some(c) = client() {
                    c.decoder_failed(&e);
                }
            }
        }
    }
}

struct Pad {
    id: SDL_JoystickID,
    handle: *mut SDL_Gamepad,
    pad: usize,
    name: String,
    sent: Option<GamepadMsg>,
}

struct Window {
    window: *mut SDL_Window,
    renderer: *mut SDL_Renderer,
    texture: *mut SDL_Texture,
    texture_key: (u32, u32, Option<Layout>),
    client: Client,
    label: String,
    settings: Settings,
    layout: ModifierLayout,
    /// The picture's place in the window, in pixels.
    picture_rect: SDL_FRect,
    has_picture: bool,
    focused: bool,
    pointer_inside: bool,
    fullscreen: bool,
    grabbed: bool,
    keys_down: HashSet<u16>,
    /// The Enter of Ctrl+Alt+Enter / the arrow of Ctrl+Alt+→: their repeats and key-up stay here.
    local_key: Option<SDL_Scancode>,
    caps_on: bool,
    buttons_held: u32,
    move_pending: Option<(f64, f64)>,
    move_last: Option<(f64, f64)>,
    move_at: Instant,
    status: String,
    status_until: Option<Instant>,
    connected: bool,
    cursor: *mut SDL_Cursor,
    clipboard_last: Option<String>,
    displays: Option<DisplaysMsg>,
    pads: Vec<Pad>,
    slots: Slots,
}

pub fn run(address: &str, label: &str) {
    crate::linux::init_libraries();
    let settings = Settings::load();
    let target = Target::new(address, label);
    let label = target.label.clone();
    logi!("stream: connecting to {label} ({}, {})", target.address, target.auth.mode());
    unsafe {
        SDL_SetHint(SDL_HINT_APP_ID, c"bb1".as_ptr());
        SDL_SetHint(SDL_HINT_AUDIO_DEVICE_SAMPLE_FRAMES, c"256".as_ptr());
        SDL_SetHint(SDL_HINT_MOUSE_FOCUS_CLICKTHROUGH, c"1".as_ptr());
        if !SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO | SDL_INIT_GAMEPAD) {
            let e = CStr::from_ptr(SDL_GetError()).to_string_lossy().into_owned();
            tell("ended", &format!("Couldn't open a window: {e}"));
            return;
        }
    }
    let wake_type = unsafe { SDL_RegisterEvents(1) };
    let (ui_tx, ui_rx) = unbounded::<Ui>();
    let (video_tx, video_rx) = unbounded::<VideoCmd>();
    let slot = Arc::new(Slot::default());
    let client_cell: Arc<Mutex<Option<Client>>> = Arc::default();
    {
        let (slot, cell) = (slot.clone(), client_cell.clone());
        std::thread::Builder::new().name("bb1 decoder".into()).spawn(move || decode_loop(video_rx, slot, cell, wake_type)).unwrap();
    }
    let events = Arc::new(StreamEvents { ui: ui_tx, video: video_tx, wake_type, slot: slot.clone() });
    let _ = &events.slot;
    let client = Client::start(target, events, settings.sound, false);
    *client_cell.lock().unwrap() = Some(client.clone());

    let (window, renderer) = unsafe {
        // 80% of the screen at 16:10.
        let mut w = 1280;
        let mut h = 800;
        let display = SDL_GetPrimaryDisplay();
        let mut bounds = std::mem::zeroed::<SDL_Rect>();
        if SDL_GetDisplayUsableBounds(display, &mut bounds) {
            w = bounds.w * 8 / 10;
            h = w * 10 / 16;
            if h > bounds.h * 88 / 100 {
                h = bounds.h * 88 / 100;
                w = h * 16 / 10;
            }
        }
        let title = CString::new(format!("{label} — bb1")).unwrap();
        let window = SDL_CreateWindow(title.as_ptr(), w, h, SDL_WINDOW_RESIZABLE | SDL_WINDOW_HIGH_PIXEL_DENSITY);
        if window.is_null() {
            let e = CStr::from_ptr(SDL_GetError()).to_string_lossy().into_owned();
            tell("ended", &format!("Couldn't open a window: {e}"));
            client.stop();
            return;
        }
        let renderer = SDL_CreateRenderer(window, null());
        if renderer.is_null() {
            let e = CStr::from_ptr(SDL_GetError()).to_string_lossy().into_owned();
            tell("ended", &format!("Couldn't draw in the window: {e}"));
            client.stop();
            return;
        }
        // Show each picture as soon as it's decoded.
        SDL_SetRenderVSync(renderer, 0);
        let name = SDL_GetRendererName(renderer);
        if !name.is_null() {
            logi!("stream: drawing with {}", CStr::from_ptr(name).to_string_lossy());
        }
        (window, renderer)
    };

    let audio = if settings.sound { open_audio(&client) } else { null_mut() };
    let mut w = Window {
        window,
        renderer,
        texture: null_mut(),
        texture_key: (0, 0, None),
        client: client.clone(),
        label: label.clone(),
        layout: ModifierLayout::from_id(&settings.modifier_layout),
        settings,
        picture_rect: SDL_FRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
        has_picture: false,
        focused: true,
        pointer_inside: false,
        fullscreen: false,
        grabbed: false,
        keys_down: HashSet::new(),
        local_key: None,
        caps_on: false,
        buttons_held: 0,
        move_pending: None,
        move_last: None,
        move_at: Instant::now(),
        status: format!("Connecting to {label}…"),
        status_until: None,
        connected: false,
        cursor: null_mut(),
        clipboard_last: None,
        displays: None,
        pads: Vec::new(),
        slots: Slots::default(),
    };
    w.render();
    let mut ended: Option<String> = None;
    'main: loop {
        let mut ev: SDL_Event = unsafe { std::mem::zeroed() };
        let got = unsafe { SDL_WaitEventTimeout(&mut ev, 8) };
        if got {
            loop {
                if !w.handle(&ev, wake_type) {
                    break 'main;
                }
                if !unsafe { SDL_PollEvent(&mut ev) } {
                    break;
                }
            }
        }
        while let Ok(u) = ui_rx.try_recv() {
            if let Some(reason) = w.ui(u) {
                ended = Some(reason);
                break 'main;
            }
        }
        if slot.wake_pending.swap(false, Relaxed) || slot.picture.lock().unwrap().is_some() {
            let taken = slot.picture.lock().unwrap().take();
            if let Some((picture, capture_us)) = taken {
                w.show(&picture);
                w.client.mark_displayed(capture_us);
            }
        }
        w.flush_move(false);
        if w.status_until.is_some_and(|t| Instant::now() > t) {
            w.status_until = None;
            w.render();
        }
    }
    let reason = ended.unwrap_or_else(|| "Disconnected.".into());
    w.release_all();
    client.stop();
    tell("ended", &reason);
    logi!("stream: {reason}");
    unsafe {
        if !audio.is_null() {
            SDL_DestroyAudioStream(audio);
        }
        for p in &w.pads {
            SDL_CloseGamepad(p.handle);
        }
        if !w.texture.is_null() {
            SDL_DestroyTexture(w.texture);
        }
        SDL_DestroyRenderer(renderer);
        SDL_DestroyWindow(window);
        SDL_Quit();
    }
    // The client's threads may still be finishing a bye; don't wait for them.
    std::process::exit(0);
}

/// Sound: the client's jitter buffer, pulled by SDL as the device needs it.
fn open_audio(client: &Client) -> *mut SDL_AudioStream {
    unsafe extern "C" fn pull(user: *mut c_void, stream: *mut SDL_AudioStream, additional: i32, _total: i32) {
        if additional <= 0 {
            return;
        }
        let client = unsafe { &*(user as *const Client) };
        let mut buf = vec![0i16; (additional as usize).div_ceil(4) * 2];
        client.audio().read(&mut buf);
        unsafe { SDL_PutAudioStreamData(stream, buf.as_ptr() as *const c_void, (buf.len() * 2) as i32) };
    }
    unsafe {
        let spec = SDL_AudioSpec { format: SDL_AUDIO_S16LE, channels: 2, freq: 48_000 };
        let user = Box::into_raw(Box::new(client.clone())) as *mut c_void;
        let stream = SDL_OpenAudioDeviceStream(SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK, &spec, Some(pull), user);
        if stream.is_null() {
            logi!("stream: no sound ({})", CStr::from_ptr(SDL_GetError()).to_string_lossy());
            drop(Box::from_raw(user as *mut Client));
            return null_mut();
        }
        SDL_ResumeAudioStreamDevice(stream);
        stream
    }
}

/// The PNG clipboard picture offered to other apps (SDL asks for it when they paste).
static CLIP_PNG: Mutex<Vec<u8>> = Mutex::new(Vec::new());

unsafe extern "C" fn clip_png(_user: *mut c_void, _mime: *const std::ffi::c_char, size: *mut usize) -> *const c_void {
    let png = CLIP_PNG.lock().unwrap();
    unsafe { *size = png.len() };
    // Lives in the static until the next picture replaces it.
    png.as_ptr() as *const c_void
}

impl Window {
    fn send(&self, m: Msg) {
        self.client.send(m);
    }

    /// Keys and buttons go to the other computer.
    fn in_control(&self) -> bool {
        self.connected && self.focused && (self.fullscreen || self.pointer_inside)
    }

    fn update_grab(&mut self) {
        let want = self.in_control();
        if want != self.grabbed {
            self.grabbed = want;
            unsafe { SDL_SetWindowKeyboardGrab(self.window, want) };
            if want {
                self.caps_on = unsafe { SDL_GetModState() }.0 & SDL_KMOD_CAPS.0 != 0;
                self.send(Msg::CapsLock(v(self.caps_on)));
            } else {
                self.release_all();
            }
        }
    }

    fn release_all(&mut self) {
        if !self.keys_down.is_empty() || self.buttons_held > 0 {
            self.keys_down.clear();
            self.buttons_held = 0;
            self.send(Msg::ReleaseAll {});
        }
    }

    fn pixel_scale(&self) -> f32 {
        unsafe {
            let (mut ww, mut wh, mut pw, mut ph) = (0, 0, 0, 0);
            SDL_GetWindowSize(self.window, &mut ww, &mut wh);
            SDL_GetWindowSizeInPixels(self.window, &mut pw, &mut ph);
            if ww > 0 { pw as f32 / ww as f32 } else { 1.0 }
        }
    }

    /// Window point → 0…1 on the picture (None: off it).
    fn on_picture(&self, x: f32, y: f32) -> Option<(f64, f64)> {
        let s = self.pixel_scale();
        let r = self.picture_rect;
        if r.w <= 0.0 || r.h <= 0.0 {
            return None;
        }
        let (px, py) = (x * s, y * s);
        let nx = ((px - r.x) / r.w) as f64;
        let ny = ((py - r.y) / r.h) as f64;
        ((0.0..=1.0).contains(&nx) && (0.0..=1.0).contains(&ny)).then_some((nx, ny))
    }

    fn clamped(&self, x: f32, y: f32) -> (f64, f64) {
        let s = self.pixel_scale();
        let r = self.picture_rect;
        let nx = ((x * s - r.x) / r.w.max(1.0)) as f64;
        let ny = ((y * s - r.y) / r.h.max(1.0)) as f64;
        (nx.clamp(0.0, 1.0), ny.clamp(0.0, 1.0))
    }

    fn queue_move(&mut self, x: f64, y: f64) {
        if self.move_last == Some((x, y)) && self.move_pending.is_none() {
            return;
        }
        self.move_pending = Some((x, y));
        self.flush_move(false);
    }

    fn flush_move(&mut self, now: bool) {
        let Some(p) = self.move_pending else { return };
        if now || self.move_at.elapsed() >= MOVE_EVERY {
            self.move_pending = None;
            self.move_last = Some(p);
            self.move_at = Instant::now();
            self.send(Msg::MouseMove(v(MouseMoveMsg { x: p.0, y: p.1 })));
        }
    }

    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        unsafe { SDL_SetWindowFullscreen(self.window, self.fullscreen) };
        self.update_grab();
    }

    fn next_screen(&mut self, step: i32) {
        let Some(d) = &self.displays else { return };
        if d.displays.len() < 2 {
            return;
        }
        let i = d.displays.iter().position(|x| x.id == d.current).unwrap_or(0) as i32;
        let n = d.displays.len() as i32;
        let next = d.displays[((i + step).rem_euclid(n)) as usize].clone();
        self.flash(format!("Showing {}", next.name));
        self.send(Msg::SelectDisplay { id: next.id.clone() });
    }

    fn flash(&mut self, text: String) {
        self.status = text;
        self.status_until = Some(Instant::now() + Duration::from_secs(3));
        self.render();
    }

    /// Ctrl and Alt are held (here, or on the other computer through us).
    fn ctrl_alt(&self) -> bool {
        let m = unsafe { SDL_GetModState() }.0;
        m & SDL_KMOD_CTRL.0 != 0 && m & SDL_KMOD_ALT.0 != 0
    }

    fn key(&mut self, scancode: SDL_Scancode, down: bool, repeat: bool) {
        if let Some(k) = self.local_key {
            if k == scancode {
                if !down {
                    self.local_key = None;
                }
                return;
            }
        }
        let Some((scan, ext)) = keymap::scan_for_hid(scancode.0 as u32) else { return };
        let Some(code) = keymap::mac_key(scan, ext, self.layout) else { return };
        let held = self.keys_down.contains(&code);
        if code == MAC_CAPS_LOCK {
            if !down {
                self.keys_down.remove(&code);
            } else if !held && self.in_control() {
                self.keys_down.insert(code);
                self.caps_on = unsafe { SDL_GetModState() }.0 & SDL_KMOD_CAPS.0 != 0;
                self.send(Msg::CapsLock(v(self.caps_on)));
            }
            return;
        }
        // A key that went down there goes up there too (and repeats there), wherever the pointer is.
        if held {
            if !down {
                self.keys_down.remove(&code);
            }
            self.send(Msg::key(code, down, down));
            return;
        }
        if !down || repeat || !self.connected {
            return;
        }
        if self.focused && self.ctrl_alt() {
            if scancode == SDL_SCANCODE_RETURN {
                self.local_key = Some(scancode);
                self.toggle_fullscreen();
                return;
            }
            if scancode == SDL_SCANCODE_RIGHT || scancode == SDL_SCANCODE_LEFT {
                if self.displays.as_ref().is_some_and(|d| d.displays.len() > 1) {
                    self.local_key = Some(scancode);
                    self.next_screen(if scancode == SDL_SCANCODE_RIGHT { 1 } else { -1 });
                    return;
                }
            }
        }
        if !self.in_control() {
            return;
        }
        self.keys_down.insert(code);
        self.send(Msg::key(code, true, false));
    }

    /// Handles one SDL event. false: the window was closed.
    fn handle(&mut self, ev: &SDL_Event, wake_type: u32) -> bool {
        let kind = unsafe { ev.r#type };
        if kind == wake_type {
            return true;
        }
        match SDL_EventType(kind) {
            SDL_EVENT_QUIT | SDL_EVENT_WINDOW_CLOSE_REQUESTED => return false,
            SDL_EVENT_WINDOW_FOCUS_GAINED => {
                self.focused = true;
                self.update_grab();
                self.offer_clipboard();
            }
            SDL_EVENT_WINDOW_FOCUS_LOST => {
                self.focused = false;
                self.update_grab();
                self.release_all();
                self.neutral_pads();
            }
            SDL_EVENT_WINDOW_MOUSE_ENTER => {
                self.pointer_inside = true;
            }
            SDL_EVENT_WINDOW_MOUSE_LEAVE => {
                self.pointer_inside = false;
                self.update_grab();
            }
            SDL_EVENT_WINDOW_RESIZED | SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED | SDL_EVENT_WINDOW_EXPOSED => self.render(),
            SDL_EVENT_WINDOW_ENTER_FULLSCREEN => {
                self.fullscreen = true;
                self.update_grab();
            }
            SDL_EVENT_WINDOW_LEAVE_FULLSCREEN => {
                self.fullscreen = false;
                self.update_grab();
            }
            SDL_EVENT_KEY_DOWN | SDL_EVENT_KEY_UP => {
                let k = unsafe { ev.key };
                self.key(k.scancode, k.down, k.repeat);
            }
            SDL_EVENT_MOUSE_MOTION => {
                let m = unsafe { ev.motion };
                let on = self.on_picture(m.x, m.y);
                let inside = on.is_some() || self.fullscreen;
                if inside != self.pointer_inside {
                    self.pointer_inside = inside;
                    self.update_grab();
                }
                if self.connected && self.focused {
                    if let Some((x, y)) = on {
                        self.queue_move(x, y);
                    } else if self.buttons_held > 0 {
                        // Dragging off the picture: keep the pointer at its edge there.
                        let (x, y) = self.clamped(m.x, m.y);
                        self.queue_move(x, y);
                    }
                }
            }
            SDL_EVENT_MOUSE_BUTTON_DOWN | SDL_EVENT_MOUSE_BUTTON_UP => {
                let b = unsafe { ev.button };
                let button = match b.button as i32 {
                    1 => 0,
                    3 => 1,
                    2 => 2,
                    4 => 3,
                    5 => 4,
                    _ => return true,
                };
                if b.down {
                    let Some((x, y)) = self.on_picture(b.x, b.y) else { return true };
                    if !self.connected {
                        return true;
                    }
                    self.flush_move(true);
                    self.buttons_held += 1;
                    self.send(Msg::MouseButton(v(MouseButtonMsg { button, down: true, clicks: b.clicks.max(1) as i64, x, y })));
                } else if self.buttons_held > 0 {
                    self.buttons_held -= 1;
                    self.flush_move(true);
                    let (x, y) = self.clamped(b.x, b.y);
                    self.send(Msg::MouseButton(v(MouseButtonMsg { button, down: false, clicks: b.clicks.max(1) as i64, x, y })));
                }
            }
            SDL_EVENT_MOUSE_WHEEL => {
                let wh = unsafe { ev.wheel };
                if !self.connected || self.on_picture(wh.mouse_x, wh.mouse_y).is_none() {
                    return true;
                }
                self.flush_move(true);
                let flip = if wh.direction == SDL_MOUSEWHEEL_FLIPPED { -1.0 } else { 1.0 };
                let (x, y) = (wh.x as f64 * flip, wh.y as f64 * flip);
                // Whole clicks are a mouse wheel (lines); anything finer is a touchpad (pixels).
                let precise = x.fract() != 0.0 || y.fract() != 0.0;
                let per = if precise { 30.0 } else { self.settings.wheel_lines };
                // SDL: y > 0 away from you (up), x > 0 right. Mac: dy > 0 up, dx > 0 left.
                self.send(Msg::Scroll(v(ScrollMsg { dx: -x * per, dy: y * per, precise, phase: 0, momentum: 0 })));
            }
            SDL_EVENT_CLIPBOARD_UPDATE => {
                let c = unsafe { ev.clipboard };
                if !c.owner {
                    self.offer_clipboard();
                }
            }
            SDL_EVENT_GAMEPAD_ADDED => {
                let id = unsafe { ev.gdevice }.which;
                self.pad_added(id);
            }
            SDL_EVENT_GAMEPAD_REMOVED => {
                let id = unsafe { ev.gdevice }.which;
                self.pad_removed(id);
            }
            SDL_EVENT_GAMEPAD_AXIS_MOTION | SDL_EVENT_GAMEPAD_BUTTON_DOWN | SDL_EVENT_GAMEPAD_BUTTON_UP => {
                let id = if SDL_EventType(kind) == SDL_EVENT_GAMEPAD_AXIS_MOTION { unsafe { ev.gaxis }.which } else { unsafe { ev.gbutton }.which };
                self.pad_changed(id);
            }
            _ => {}
        }
        true
    }

    /// Something from the connection. Some(reason): the session ended.
    fn ui(&mut self, u: Ui) -> Option<String> {
        match u {
            Ui::Status(text, connected) => {
                let was = self.connected;
                self.connected = connected;
                tell("status", &text);
                if !connected {
                    self.status = text;
                    self.status_until = None;
                } else if !was {
                    self.status.clear();
                }
                self.update_grab();
                self.render();
            }
            Ui::Ended(reason) => return Some(reason),
            Ui::Welcome(name) => {
                tell("connected", &name);
                self.label = name;
            }
            Ui::Notice(text) => {
                tell("notice", &text);
                self.flash(text);
            }
            Ui::Paired(name) => tell("paired", &name),
            Ui::Stats(s) => {
                if s.connected {
                    let lat = s.latency_ms.map(|l| format!(" · {l:.0} ms behind")).unwrap_or_default();
                    let title = format!("{} — {:.0} fps · {:.1} Mbps · RTT {:.0} ms{lat} — bb1", self.label, s.fps, s.mbps, s.rtt_ms);
                    let t = CString::new(title).unwrap_or_default();
                    unsafe { SDL_SetWindowTitle(self.window, t.as_ptr()) };
                }
            }
            Ui::Displays(d) => self.displays = Some(d),
            Ui::Cursor(c) => self.set_cursor(&c),
            Ui::Clipboard(c) => self.receive_clipboard(c),
            Ui::Rumble(r) => {
                if let Some(p) = self.pads.iter().find(|p| p.pad as i64 == r.pad) {
                    unsafe { SDL_RumbleGamepad(p.handle, gamepad::motor(r.large), gamepad::motor(r.small), 5000) };
                }
            }
        }
        None
    }

    fn show(&mut self, p: &Picture) {
        unsafe {
            let key = (p.width, p.height, Some(p.layout));
            if self.texture.is_null() || self.texture_key != key {
                if !self.texture.is_null() {
                    SDL_DestroyTexture(self.texture);
                }
                let props = SDL_CreateProperties();
                let format = if p.layout == Layout::Nv12 { SDL_PIXELFORMAT_NV12 } else { SDL_PIXELFORMAT_IYUV };
                SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_FORMAT_NUMBER, format.0 as i64);
                SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_ACCESS_NUMBER, SDL_TEXTUREACCESS_STREAMING.0 as i64);
                SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_WIDTH_NUMBER, p.width as i64);
                SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_HEIGHT_NUMBER, p.height as i64);
                // The hosts encode BT.709, limited range.
                SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_COLORSPACE_NUMBER, SDL_COLORSPACE_BT709_LIMITED.0 as i64);
                self.texture = SDL_CreateTextureWithProperties(self.renderer, props);
                SDL_DestroyProperties(props);
                if self.texture.is_null() {
                    logi!("stream: no texture: {}", CStr::from_ptr(SDL_GetError()).to_string_lossy());
                    return;
                }
                self.texture_key = key;
                logi!("stream: picture {}x{} {:?}", p.width, p.height, p.layout);
            }
            match p.layout {
                Layout::Nv12 => {
                    SDL_UpdateNVTexture(self.texture, null(), p.planes[0].as_ptr(), p.strides[0] as i32, p.planes[1].as_ptr(), p.strides[1] as i32);
                }
                Layout::I420 => {
                    SDL_UpdateYUVTexture(
                        self.texture,
                        null(),
                        p.planes[0].as_ptr(),
                        p.strides[0] as i32,
                        p.planes[1].as_ptr(),
                        p.strides[1] as i32,
                        p.planes[2].as_ptr(),
                        p.strides[2] as i32,
                    );
                }
            }
        }
        if !self.has_picture {
            self.has_picture = true;
        }
        self.render();
    }

    fn render(&mut self) {
        unsafe {
            let r = self.renderer;
            SDL_SetRenderDrawColor(r, 0, 0, 0, 255);
            SDL_RenderClear(r);
            let (mut ow, mut oh) = (0, 0);
            SDL_GetCurrentRenderOutputSize(r, &mut ow, &mut oh);
            if !self.texture.is_null() && self.has_picture {
                let (tw, th) = (self.texture_key.0 as f32, self.texture_key.1 as f32);
                let scale = (ow as f32 / tw).min(oh as f32 / th);
                let (dw, dh) = (tw * scale, th * scale);
                self.picture_rect = SDL_FRect { x: ((ow as f32 - dw) / 2.0).floor(), y: ((oh as f32 - dh) / 2.0).floor(), w: dw.round(), h: dh.round() };
                SDL_RenderTexture(r, self.texture, null(), &self.picture_rect);
            }
            let show_status = !self.status.is_empty() && (!self.connected || !self.has_picture || self.status_until.is_some());
            if show_status {
                let text = CString::new(self.status.clone()).unwrap_or_default();
                let zoom = (ow as f32 / 640.0).clamp(1.0, 3.0).floor();
                let width = self.status.chars().count() as f32 * 8.0 * zoom;
                let (x, y) = (((ow as f32 - width) / 2.0).max(8.0), oh as f32 / 2.0 - 4.0 * zoom);
                SDL_SetRenderDrawColor(r, 20, 20, 20, 220);
                let pad = 10.0 * zoom;
                let bg = SDL_FRect { x: x - pad, y: y - pad, w: width + pad * 2.0, h: 8.0 * zoom + pad * 2.0 };
                SDL_SetRenderDrawBlendMode(r, SDL_BLENDMODE_BLEND);
                SDL_RenderFillRect(r, &bg);
                SDL_SetRenderScale(r, zoom, zoom);
                SDL_SetRenderDrawColor(r, 235, 235, 235, 255);
                SDL_RenderDebugText(r, x / zoom, y / zoom, text.as_ptr());
                SDL_SetRenderScale(r, 1.0, 1.0);
            }
            SDL_RenderPresent(r);
        }
    }

    /// The host's pointer shape (a PNG sized in points) as this window's cursor.
    fn set_cursor(&mut self, c: &CursorMsg) {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(&c.png));
        decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
        let Ok(mut reader) = decoder.read_info() else { return };
        let Some(size) = reader.output_buffer_size() else { return };
        let mut buf = vec![0; size];
        let Ok(info) = reader.next_frame(&mut buf) else { return };
        let (w, h) = (info.width as usize, info.height as usize);
        let rgba: Vec<u8> = match info.color_type {
            png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
            png::ColorType::GrayscaleAlpha => buf[..w * h * 2].chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
            _ => return,
        };
        // Points → this screen's pixels; the PNG may be drawn at 2x.
        let scale = self.pixel_scale() as f64;
        let dw = ((c.width * scale).round() as i32).clamp(1, 256);
        let dh = ((c.height * scale).round() as i32).clamp(1, 256);
        unsafe {
            let src = SDL_CreateSurfaceFrom(w as i32, h as i32, SDL_PIXELFORMAT_RGBA32, rgba.as_ptr() as *mut c_void, (w * 4) as i32);
            if src.is_null() {
                return;
            }
            let surface = if (dw as usize, dh as usize) != (w, h) { SDL_ScaleSurface(src, dw, dh, SDL_SCALEMODE_LINEAR) } else { src };
            let hx = ((c.hot_x * scale).round() as i32).clamp(0, dw - 1);
            let hy = ((c.hot_y * scale).round() as i32).clamp(0, dh - 1);
            let cursor = SDL_CreateColorCursor(surface, hx, hy);
            if surface != src {
                SDL_DestroySurface(surface);
            }
            SDL_DestroySurface(src);
            if !cursor.is_null() {
                SDL_SetCursor(cursor);
                if !self.cursor.is_null() {
                    SDL_DestroyCursor(self.cursor);
                }
                self.cursor = cursor;
            }
        }
    }

    fn receive_clipboard(&mut self, c: ClipboardMsg) {
        if !self.settings.clipboard {
            return;
        }
        unsafe {
            if let Some(text) = c.text {
                if let Ok(t) = CString::new(text.clone()) {
                    SDL_SetClipboardText(t.as_ptr());
                    self.clipboard_last = Some(text);
                }
            } else if let Some(png) = c.png {
                *CLIP_PNG.lock().unwrap() = png;
                let mimes = [c"image/png".as_ptr()];
                SDL_SetClipboardData(Some(clip_png), None, null_mut(), mimes.as_ptr(), 1);
                self.clipboard_last = None;
            }
        }
    }

    /// This computer's clipboard → the other computer, when it changed (not passwords).
    fn offer_clipboard(&mut self) {
        if !self.settings.clipboard || !self.connected {
            return;
        }
        unsafe {
            if SDL_HasClipboardData(c"x-kde-passwordManagerHint".as_ptr()) {
                return;
            }
            if SDL_HasClipboardText() {
                let p = SDL_GetClipboardText();
                if p.is_null() {
                    return;
                }
                let text = CStr::from_ptr(p).to_string_lossy().into_owned();
                SDL_free(p as *mut c_void);
                if !text.is_empty() && self.clipboard_last.as_deref() != Some(text.as_str()) {
                    self.clipboard_last = Some(text.clone());
                    self.send(Msg::Clipboard(v(ClipboardMsg { text: Some(text), png: None })));
                }
            } else if SDL_HasClipboardData(c"image/png".as_ptr()) {
                let mut size = 0usize;
                let p = SDL_GetClipboardData(c"image/png".as_ptr(), &mut size);
                if p.is_null() {
                    return;
                }
                let png = std::slice::from_raw_parts(p as *const u8, size).to_vec();
                SDL_free(p);
                if size > 0 && size < 16_000_000 && *CLIP_PNG.lock().unwrap() != png {
                    *CLIP_PNG.lock().unwrap() = png.clone();
                    self.clipboard_last = None;
                    self.send(Msg::Clipboard(v(ClipboardMsg { text: None, png: Some(png) })));
                }
            }
        }
    }

    // MARK: Game controllers

    fn pad_added(&mut self, id: SDL_JoystickID) {
        if self.pads.iter().any(|p| p.id == id) {
            return;
        }
        let Some(pad) = self.slots.assign(id.0) else { return };
        let handle = unsafe { SDL_OpenGamepad(id) };
        if handle.is_null() {
            self.slots.release(id.0);
            return;
        }
        let name = unsafe {
            let n = SDL_GetGamepadName(handle);
            if n.is_null() { "Controller".to_string() } else { CStr::from_ptr(n).to_string_lossy().into_owned() }
        };
        logi!("stream: controller {pad}: {name}");
        self.pads.push(Pad { id, handle, pad, name, sent: None });
        self.pad_changed(id);
    }

    fn pad_removed(&mut self, id: SDL_JoystickID) {
        if let Some(i) = self.pads.iter().position(|p| p.id == id) {
            let p = self.pads.remove(i);
            unsafe { SDL_CloseGamepad(p.handle) };
            self.slots.release(id.0);
            self.send(Msg::Gamepad(v(GamepadMsg { pad: p.pad as i64, connected: false, ..Default::default() })));
        }
    }

    fn pad_changed(&mut self, id: SDL_JoystickID) {
        if !self.connected || !self.focused {
            return;
        }
        let Some(i) = self.pads.iter().position(|p| p.id == id) else { return };
        let p = &self.pads[i];
        let mut axes = [0i16; gamepad::SDL_AXES];
        for (a, value) in axes.iter_mut().enumerate() {
            *value = unsafe { SDL_GetGamepadAxis(p.handle, SDL_GamepadAxis(a as i32)) };
        }
        let handle = p.handle;
        let mut msg = gamepad::from_sdl(p.pad, |b| unsafe { SDL_GetGamepadButton(handle, SDL_GamepadButton(b)) }, axes);
        if p.sent.as_ref().is_some_and(|s| gamepad::same_state(s, &msg)) {
            return;
        }
        if p.sent.is_none() {
            msg.name = Some(p.name.clone());
        }
        self.pads[i].sent = Some(msg.clone());
        self.send(Msg::Gamepad(v(msg)));
    }

    /// The window went to the back: the host sees every controller centred with nothing pressed.
    fn neutral_pads(&mut self) {
        let pads: Vec<usize> = self.pads.iter().filter(|p| p.sent.is_some()).map(|p| p.pad).collect();
        for p in &mut self.pads {
            p.sent = None;
        }
        for pad in pads {
            self.send(Msg::Gamepad(v(gamepad::neutral(pad))));
        }
    }
}

