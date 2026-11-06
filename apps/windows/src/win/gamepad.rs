//! Game controllers on this PC → the host, with SDL3 (DualSense, DualShock 4, Xbox, Switch Pro;
//! DualShock 3 with Sony's driver or DsHidMini). SDL3.dll ships next to bb1.exe (the installer puts
//! it there) and is loaded when the first stream opens; without it controllers are just not sent.
//!
//! One thread polls at ~250 Hz while a stream is open. Each controller gets a pad number 0…3 in
//! the order it was connected (`core::gamepad::Slots`); a GamepadMsg goes out whenever its state
//! changes, `connected: false` when it's unplugged. Only while the stream window is in front: when
//! it isn't, the pads are sent centred with nothing pressed, so a controller on this PC doesn't play
//! the host's game from the background. Rumble from the host keeps the motors going until it
//! changes (refreshed every half second, so it stops within a second if the connection goes).

use crate::core::client::Client;
use crate::core::gamepad::{self, SDL_AXES, Slots};
use crate::core::proto::{GamepadMsg, Msg, RumbleMsg, v};
use crate::logi;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer, TIMER_ALL_ACCESS, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows::core::PCSTR;

struct Active {
    client: Client,
    /// The stream window: controllers count only while it's the foreground window.
    hwnd: isize,
}

static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
static RUMBLE: Mutex<Vec<RumbleMsg>> = Mutex::new(Vec::new());
static THREAD: Once = Once::new();

/// A stream opened (UI thread): send this PC's controllers to it while `stream_window` is in front.
pub fn start(client: Client, stream_window: Option<HWND>) {
    RUMBLE.lock().unwrap().clear();
    *ACTIVE.lock().unwrap() = Some(Active { client, hwnd: stream_window.map(|h| h.0 as isize).unwrap_or(0) });
    THREAD.call_once(|| {
        let _ = std::thread::Builder::new().name("bb1 controllers".into()).spawn(run);
    });
}

/// The stream closed.
pub fn stop() {
    *ACTIVE.lock().unwrap() = None;
}

/// From the connection: the host's game set a controller's motors.
pub fn rumble(r: RumbleMsg) {
    let mut q = RUMBLE.lock().unwrap();
    q.retain(|o| o.pad != r.pad);
    q.push(r);
}

// MARK: SDL3, loaded at run time (only what's used; SDL3 3.2 ABI)

const SDL_INIT_GAMEPAD: u32 = 0x0000_2000;
const SDL_EVENT_GAMEPAD_ADDED: u32 = 0x653;
const SDL_EVENT_GAMEPAD_REMOVED: u32 = 0x654;

/// SDL_Event: a 128-byte union whose first field is the event type.
#[repr(C, align(8))]
struct SdlEvent {
    kind: u32,
    _rest: [u8; 124],
}

type Gamepad = *mut c_void;

#[allow(non_snake_case)]
struct Sdl {
    SDL_SetHint: unsafe extern "C" fn(*const c_char, *const c_char) -> bool,
    SDL_Init: unsafe extern "C" fn(u32) -> bool,
    SDL_GetError: unsafe extern "C" fn() -> *const c_char,
    SDL_PollEvent: unsafe extern "C" fn(*mut SdlEvent) -> bool,
    SDL_GetGamepads: unsafe extern "C" fn(*mut c_int) -> *mut u32,
    SDL_free: unsafe extern "C" fn(*mut c_void),
    SDL_OpenGamepad: unsafe extern "C" fn(u32) -> Gamepad,
    SDL_CloseGamepad: unsafe extern "C" fn(Gamepad),
    SDL_GetGamepadName: unsafe extern "C" fn(Gamepad) -> *const c_char,
    SDL_GetGamepadButton: unsafe extern "C" fn(Gamepad, c_int) -> bool,
    SDL_GetGamepadAxis: unsafe extern "C" fn(Gamepad, c_int) -> i16,
    SDL_RumbleGamepad: unsafe extern "C" fn(Gamepad, u16, u16, u32) -> bool,
}

impl Sdl {
    /// SDL3.dll from bb1.exe's folder (never from the search path).
    fn load() -> Result<Sdl, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let path = exe.with_file_name("SDL3.dll");
        if !path.exists() {
            return Err(format!("{} isn't there", path.display()));
        }
        let module = unsafe { LoadLibraryW(&windows::core::HSTRING::from(path.as_os_str())) }.map_err(|e| e.to_string())?;
        macro_rules! sym {
            ($name:ident) => {{
                let f = unsafe { GetProcAddress(module, PCSTR(concat!(stringify!($name), "\0").as_ptr())) }
                    .ok_or(concat!("SDL3.dll has no ", stringify!($name)))?;
                unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, _>(f) }
            }};
        }
        Ok(Sdl {
            SDL_SetHint: sym!(SDL_SetHint),
            SDL_Init: sym!(SDL_Init),
            SDL_GetError: sym!(SDL_GetError),
            SDL_PollEvent: sym!(SDL_PollEvent),
            SDL_GetGamepads: sym!(SDL_GetGamepads),
            SDL_free: sym!(SDL_free),
            SDL_OpenGamepad: sym!(SDL_OpenGamepad),
            SDL_CloseGamepad: sym!(SDL_CloseGamepad),
            SDL_GetGamepadName: sym!(SDL_GetGamepadName),
            SDL_GetGamepadButton: sym!(SDL_GetGamepadButton),
            SDL_GetGamepadAxis: sym!(SDL_GetGamepadAxis),
            SDL_RumbleGamepad: sym!(SDL_RumbleGamepad),
        })
    }

    fn error(&self) -> String {
        unsafe {
            let e = (self.SDL_GetError)();
            if e.is_null() { String::new() } else { CStr::from_ptr(e).to_string_lossy().into_owned() }
        }
    }

    fn hint(&self, name: &CStr, value: &CStr) {
        unsafe {
            (self.SDL_SetHint)(name.as_ptr(), value.as_ptr());
        }
    }

    /// The controllers SDL sees now (SDL_JoystickID each).
    fn gamepads(&self) -> Vec<u32> {
        unsafe {
            let mut n: c_int = 0;
            let list = (self.SDL_GetGamepads)(&mut n);
            if list.is_null() {
                return Vec::new();
            }
            let ids = std::slice::from_raw_parts(list, n.max(0) as usize).to_vec();
            (self.SDL_free)(list as *mut c_void);
            ids
        }
    }
}

/// One controller this PC has open.
struct Open {
    id: u32,
    handle: Gamepad,
    pad: usize,
    name: String,
    /// What the host last got (None: nothing yet this session).
    sent: Option<GamepadMsg>,
    /// Motors as the host last set them (SDL's 0…65535), whether that's applied yet, and when it was.
    motors: (u16, u16),
    motors_new: bool,
    motors_at: Instant,
}

const RUMBLE_MS: u32 = 1000;

fn run() {
    let sdl = match Sdl::load() {
        Ok(s) => s,
        Err(e) => {
            logi!("controllers: no SDL3 ({e}); this PC's controllers won't be sent");
            return;
        }
    };
    // HIDAPI drivers read PlayStation and Switch pads directly, rumble included (enhanced reports
    // turn on Bluetooth rumble for PlayStation pads). PS3 pads work with Sony's driver or DsHidMini.
    for (name, value) in [
        (c"SDL_JOYSTICK_HIDAPI", c"1"),
        (c"SDL_JOYSTICK_HIDAPI_PS3", c"1"),
        (c"SDL_JOYSTICK_HIDAPI_PS3_SIXAXIS_DRIVER", c"1"),
        (c"SDL_JOYSTICK_HIDAPI_PS4", c"1"),
        (c"SDL_JOYSTICK_HIDAPI_PS5", c"1"),
        (c"SDL_JOYSTICK_HIDAPI_SWITCH", c"1"),
        (c"SDL_JOYSTICK_ENHANCED_REPORTS", c"1"),
        // bb1 decides when controllers count (the stream window in front), not SDL.
        (c"SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", c"1"),
    ] {
        sdl.hint(name, value);
    }
    if !unsafe { (sdl.SDL_Init)(SDL_INIT_GAMEPAD) } {
        logi!("controllers: SDL couldn't start ({}); this PC's controllers won't be sent", sdl.error());
        return;
    }
    logi!("controllers: SDL3 ready");
    let timer = unsafe { CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0) }.ok();
    let mut open: Vec<Open> = Vec::new();
    let mut slots = Slots::default();
    let mut session = 0u64;
    let mut scanned_at: Option<Instant> = None;
    loop {
        // Copied out first so the lock is let go at once: matching on the guard itself would hold it
        // through the 100 ms sleep below, and this loop takes it again straight after, so the UI
        // thread's `start` (the next stream) could wait on it forever (bb1 not responding).
        let active = ACTIVE.lock().unwrap().as_ref().map(|a| (a.client.clone(), a.hwnd));
        let (client, hwnd) = match active {
            Some(a) => a,
            None => {
                // No stream: let go of the controllers (other apps get them back whole).
                for o in open.drain(..) {
                    unsafe {
                        (sdl.SDL_RumbleGamepad)(o.handle, 0, 0, 0);
                        (sdl.SDL_CloseGamepad)(o.handle);
                    }
                }
                slots = Slots::default();
                scanned_at = None;
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        // Pump SDL (this reads the controllers too) and notice plugging and unplugging.
        let mut rescan = scanned_at.is_none_or(|t| t.elapsed() > Duration::from_secs(1));
        let mut ev = SdlEvent { kind: 0, _rest: [0; 124] };
        while unsafe { (sdl.SDL_PollEvent)(&mut ev) } {
            if ev.kind == SDL_EVENT_GAMEPAD_ADDED || ev.kind == SDL_EVENT_GAMEPAD_REMOVED {
                rescan = true;
            }
        }

        let now_session = client.session();
        if now_session != session {
            // A new session: the host has none of our controllers yet.
            session = now_session;
            for o in &mut open {
                o.sent = None;
            }
        }

        if rescan {
            scanned_at = Some(Instant::now());
            let ids = sdl.gamepads();
            open.retain(|o| {
                if ids.contains(&o.id) {
                    return true;
                }
                logi!("controllers: {} (pad {}) unplugged", o.name, o.pad);
                slots.release(o.id);
                unsafe { (sdl.SDL_CloseGamepad)(o.handle) };
                if o.sent.is_some() && session != 0 {
                    client.send(Msg::Gamepad(v(GamepadMsg { pad: o.pad as i64, connected: false, ..Default::default() })));
                }
                false
            });
            for id in ids {
                if open.iter().any(|o| o.id == id) {
                    continue;
                }
                let Some(pad) = slots.assign(id) else { continue };
                let handle = unsafe { (sdl.SDL_OpenGamepad)(id) };
                if handle.is_null() {
                    slots.release(id);
                    logi!("controllers: couldn't open a controller ({})", sdl.error());
                    continue;
                }
                let name = unsafe {
                    let n = (sdl.SDL_GetGamepadName)(handle);
                    if n.is_null() { "Controller".to_string() } else { CStr::from_ptr(n).to_string_lossy().into_owned() }
                };
                // A friend's controller this PC is sharing to (a virtual Xbox 360 one) isn't ours to send.
                if crate::host::input::VIRTUAL_PADS.load(std::sync::atomic::Ordering::Relaxed) > 0 && name == "Xbox 360 Controller" {
                    unsafe { (sdl.SDL_CloseGamepad)(handle) };
                    slots.release(id);
                    continue;
                }
                logi!("controllers: {name} is pad {pad}");
                open.push(Open { id, handle, pad, name, sent: None, motors: (0, 0), motors_new: false, motors_at: Instant::now() });
            }
        }

        if session != 0 {
            // BB1_GAMEPADS_ALWAYS (tests, bb1-probe --gamepads): send without a stream window.
            let front = (hwnd != 0 && unsafe { GetForegroundWindow() }.0 as isize == hwnd)
                || std::env::var_os("BB1_GAMEPADS_ALWAYS").is_some();
            for o in &mut open {
                let state = if front {
                    let handle = o.handle;
                    let mut axes = [0i16; SDL_AXES];
                    for (i, a) in axes.iter_mut().enumerate() {
                        *a = unsafe { (sdl.SDL_GetGamepadAxis)(handle, i as c_int) };
                    }
                    gamepad::from_sdl(o.pad, |b| unsafe { (sdl.SDL_GetGamepadButton)(handle, b) }, axes)
                } else {
                    gamepad::neutral(o.pad)
                };
                let changed = o.sent.as_ref().is_none_or(|s| !gamepad::same_state(s, &state));
                if changed {
                    let first = o.sent.is_none();
                    let mut msg = state.clone();
                    if first {
                        msg.name = Some(o.name.clone());
                    }
                    client.send(Msg::Gamepad(v(msg)));
                    o.sent = Some(state);
                }
            }
        }

        // Rumble: apply what the host asked for, and keep it going.
        for r in std::mem::take(&mut *RUMBLE.lock().unwrap()) {
            if let Some(o) = open.iter_mut().find(|o| o.pad as i64 == r.pad) {
                o.motors = (gamepad::motor(r.large), gamepad::motor(r.small));
                o.motors_new = true;
            }
        }
        for o in &mut open {
            let running = o.motors != (0, 0);
            if o.motors_new || (running && o.motors_at.elapsed() > Duration::from_millis(500)) {
                unsafe { (sdl.SDL_RumbleGamepad)(o.handle, o.motors.0, o.motors.1, if running { RUMBLE_MS } else { 0 }) };
                o.motors_new = false;
                o.motors_at = Instant::now();
            }
        }

        wait_4ms(timer);
    }
}

/// ~4 ms (250 Hz). A high-resolution timer when Windows has them (10 1803+): a plain sleep would
/// round up to the 15.6 ms system tick.
fn wait_4ms(timer: Option<HANDLE>) {
    if let Some(t) = timer {
        let due = -40_000i64; // 100 ns units, negative = relative
        unsafe {
            if SetWaitableTimer(t, &due, 0, None, None, false).is_ok() {
                WaitForSingleObject(t, INFINITE);
                return;
            }
        }
    }
    std::thread::sleep(Duration::from_millis(4));
}
