//! Keyboard and mouse from viewers into this PC, with SendInput.
//!
//! Keys arrive as macOS virtual key codes (both clients send those) and go back to PC scan codes
//! with the inverse of `core/keymap.rs`, sent with KEYEVENTF_SCANCODE so games that read DirectInput
//! or raw input see real key presses (and the PC's keyboard layout decides what they type).
//!
//! Modifiers on a Windows host:
//! - ⌥ Option → Alt. ⌃ Control → Ctrl.
//! - ⌘ Command → Ctrl by default, so ⌘C / ⌘V from a Mac copy and paste here.
//! - `set_command_is_windows_key(true)` makes ⌘ the Windows key instead: for Windows clients, whose
//!   default layout sends their Windows key as ⌘ (so Ctrl stays Ctrl and the Windows key opens Start).
//!
//! Two Mac keys can land on the same PC key (⌘ and ⌃ both on Ctrl); the PC key goes down with the
//! first and up with the last. Caps Lock is a state, not a key (`set_caps_lock`); key code 57 is
//! ignored.
//!
//! Mouse: positions are 0..1 of the shared monitor (`set_screen_rect`, primary monitor by default),
//! sent as absolute coordinates over the whole virtual desktop. Scrolling: wheel lines become
//! wheel notches (`SPI_GETWHEELSCROLLLINES` lines per notch), touchpad pixels become fractions of a
//! notch (60 px per notch, the inverse of what the Windows client sends), so a Windows client's
//! wheel and touchpad arrive exactly as they left.
//!
//! Windows doesn't let a normal program send input to a program running as administrator (UIPI):
//! bb1 must run as admin too to control those (a game launcher, Task Manager).
//!
//! Game controllers: each viewer's pads become virtual Xbox 360 controllers through the ViGEmBus
//! driver (`Gamepads`), so every friend is a separate player; games' rumble goes back to that
//! viewer. Without the driver (Settings offers to install it) controllers are ignored.

use crate::core::gamepad::{self, MAX_PADS};
use crate::core::proto::{GamepadMsg, KeyMsg, MouseButtonMsg, MouseMoveMsg, RumbleMsg, ScrollMsg};
use crate::logi;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SPI_GETWHEELSCROLLLINES,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW, WHEEL_DELTA, XBUTTON1, XBUTTON2,
};

// Mac modifier key codes, for the tests (the mapping itself is `keymap::scan_for`).
#[cfg(test)]
const CMD_L: u16 = 55;
#[cfg(test)]
const CMD_R: u16 = 54;
#[cfg(test)]
const CTRL_L: u16 = 59;
#[cfg(test)]
const OPT_L: u16 = 58;
#[cfg(test)]
const OPT_R: u16 = 61;
const SCAN_CAPS_LOCK: u16 = 0x3A;
/// NSEvent.Phase.began: a new touchpad gesture, so leftover fractions of a notch are dropped.
const PHASE_BEGAN: u64 = 1;

pub(crate) use crate::core::keymap::scan_for;
#[cfg(test)]
use crate::core::keymap::{KEYS, MAC_CAPS_LOCK};

/// Pixel `p` of a `size`-pixel span starting at `origin` → SendInput's 0..65535 absolute units
/// (aimed at the pixel's centre, so rounding either way lands on it).
pub(crate) fn normalize(p: i32, origin: i32, size: i32) -> i32 {
    if size <= 1 {
        return 0;
    }
    ((((p - origin) as f64 + 0.5) * 65536.0 / size as f64) as i32).clamp(0, 65535)
}

/// 0..1 of a `size`-pixel span starting at `start` → the pixel.
pub(crate) fn pixel(v: f64, start: i32, size: i32) -> i32 {
    let size = size.max(1);
    start + ((v.clamp(0.0, 1.0) * size as f64) as i32).min(size - 1)
}

/// A scroll message → (vertical, horizontal) wheel units (120 = one notch).
/// Mac: dy > 0 scrolls up (content moves down), dx > 0 scrolls left. Windows: wheel forward (> 0)
/// scrolls up, tilt right (> 0) scrolls right.
pub(crate) fn wheel_units(dx: f64, dy: f64, precise: bool, lines_per_notch: f64) -> (f64, f64) {
    let per = if precise { 2.0 } else { WHEEL_DELTA as f64 / lines_per_notch.max(1.0) };
    (dy * per, -dx * per)
}

pub struct Injector {
    command_is_windows_key: bool,
    /// Mac code, and the PC key it pressed (kept, so a mapping change doesn't strand a key).
    held_keys: Vec<(u16, u16, bool)>,
    held_buttons: Vec<i64>,
    /// The shared monitor: left, top, width, height in virtual-desktop pixels.
    screen: (i32, i32, i32, i32),
    /// Wheel units not sent yet (vertical, horizontal).
    scroll_left: (f64, f64),
    wheel_lines: f64,
    /// The Caps Lock state we last set, and when: GetKeyState can lag right after injecting.
    caps_sent: Option<(bool, Instant)>,
    refused_logged: bool,
    /// Viewers' game controllers.
    pub gamepads: Gamepads,
}

impl Default for Injector {
    fn default() -> Self {
        Self::new()
    }
}

impl Injector {
    pub fn new() -> Injector {
        let screen = unsafe {
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(monitor, &mut info).as_bool() {
                let r = info.rcMonitor;
                (r.left, r.top, r.right - r.left, r.bottom - r.top)
            } else {
                (0, 0, GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN), GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN))
            }
        };
        let mut lines = 3u32;
        unsafe {
            let _ = SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, Some(&mut lines as *mut u32 as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        }
        // 0 = wheel off, u32::MAX = a page per notch: neither means anything for lines.
        let wheel_lines = if lines == 0 || lines > 100 { 3.0 } else { lines as f64 };
        Injector {
            command_is_windows_key: false,
            held_keys: Vec::new(),
            held_buttons: Vec::new(),
            screen,
            scroll_left: (0.0, 0.0),
            wheel_lines,
            caps_sent: None,
            refused_logged: false,
            gamepads: Gamepads::default(),
        }
    }

    /// The monitor viewers see, in virtual-desktop pixels (for MouseMoveMsg's 0..1).
    pub fn set_screen_rect(&mut self, left: i32, top: i32, width: i32, height: i32) {
        self.screen = (left, top, width.max(1), height.max(1));
    }

    /// ⌘ → Windows key (true, for Windows clients) or Ctrl (false, the default, for Macs).
    pub fn set_command_is_windows_key(&mut self, on: bool) {
        self.command_is_windows_key = on;
    }

    pub fn key(&mut self, k: &KeyMsg) {
        let Some((scan, ext)) = scan_for(k.code, self.command_is_windows_key) else { return };
        let held = self.held_keys.iter().position(|h| h.0 == k.code);
        if k.down {
            if let Some(i) = held {
                // A repeat: Windows repeats only real keyboards' keys, so send it on.
                let (_, s, e) = self.held_keys[i];
                self.send_key(s, e, true);
                return;
            }
            let shared = self.held_keys.iter().any(|h| (h.1, h.2) == (scan, ext));
            self.held_keys.push((k.code, scan, ext));
            if !shared {
                self.send_key(scan, ext, true);
            }
        } else {
            let (scan, ext) = match held {
                Some(i) => {
                    let h = self.held_keys.remove(i);
                    (h.1, h.2)
                }
                None => (scan, ext),
            };
            if !self.held_keys.iter().any(|h| (h.1, h.2) == (scan, ext)) {
                self.send_key(scan, ext, false);
            }
        }
    }

    pub fn mouse_move(&mut self, m: &MouseMoveMsg) {
        let (dx, dy) = self.absolute(m.x, m.y);
        self.send_mouse(dx, dy, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, 0);
    }

    pub fn mouse_button(&mut self, b: &MouseButtonMsg) {
        let (flags, data) = match (b.button, b.down) {
            (0, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (0, false) => (MOUSEEVENTF_LEFTUP, 0),
            (1, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (1, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (2, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (2, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (3, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as u32),
            (3, false) => (MOUSEEVENTF_XUP, XBUTTON1 as u32),
            (4, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as u32),
            (4, false) => (MOUSEEVENTF_XUP, XBUTTON2 as u32),
            _ => return,
        };
        if b.down {
            if !self.held_buttons.contains(&b.button) {
                self.held_buttons.push(b.button);
            }
        } else {
            self.held_buttons.retain(|&x| x != b.button);
        }
        // Move there and press in one event, so the click lands where the viewer clicked.
        let (dx, dy) = self.absolute(b.x, b.y);
        self.send_mouse(dx, dy, flags | MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, data);
    }

    pub fn scroll(&mut self, s: &ScrollMsg) {
        if s.phase & PHASE_BEGAN != 0 {
            self.scroll_left = (0.0, 0.0);
        }
        let (v, h) = wheel_units(s.dx, s.dy, s.precise, self.wheel_lines);
        self.scroll_left.0 += v;
        self.scroll_left.1 += h;
        let v = self.scroll_left.0.trunc();
        let h = self.scroll_left.1.trunc();
        self.scroll_left.0 -= v;
        self.scroll_left.1 -= h;
        if v != 0.0 {
            self.send_mouse(0, 0, MOUSEEVENTF_WHEEL, v as i32 as u32);
        }
        if h != 0.0 {
            self.send_mouse(0, 0, MOUSEEVENTF_HWHEEL, h as i32 as u32);
        }
    }

    /// Turns Caps Lock on or off (presses it only if it isn't that way already).
    pub fn set_caps_lock(&mut self, on: bool) {
        let current = match self.caps_sent {
            Some((state, at)) if at.elapsed() < Duration::from_millis(500) => state,
            _ => (unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1) != 0,
        };
        if current != on {
            self.send_key(SCAN_CAPS_LOCK, false, true);
            self.send_key(SCAN_CAPS_LOCK, false, false);
            self.caps_sent = Some((on, Instant::now()));
        }
    }

    /// Lets go of every key and button this injector holds down.
    pub fn release_all(&mut self) {
        let mut released: Vec<(u16, bool)> = Vec::new();
        for (_, scan, ext) in std::mem::take(&mut self.held_keys) {
            if !released.contains(&(scan, ext)) {
                released.push((scan, ext));
                self.send_key(scan, ext, false);
            }
        }
        for b in std::mem::take(&mut self.held_buttons) {
            let (flags, data) = match b {
                0 => (MOUSEEVENTF_LEFTUP, 0),
                1 => (MOUSEEVENTF_RIGHTUP, 0),
                2 => (MOUSEEVENTF_MIDDLEUP, 0),
                3 => (MOUSEEVENTF_XUP, XBUTTON1 as u32),
                _ => (MOUSEEVENTF_XUP, XBUTTON2 as u32),
            };
            self.send_mouse(0, 0, flags, data);
        }
        self.scroll_left = (0.0, 0.0);
    }

    /// 0..1 of the shared monitor → absolute units over the virtual desktop.
    fn absolute(&self, x: f64, y: f64) -> (i32, i32) {
        let (left, top, width, height) = self.screen;
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        (normalize(pixel(x, left, width), vx, vw), normalize(pixel(y, top, height), vy, vh))
    }

    fn send_key(&mut self, scan: u16, extended: bool, down: bool) {
        let mut flags = KEYEVENTF_SCANCODE;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        };
        self.send(&input);
    }

    fn send_mouse(&mut self, dx: i32, dy: i32, flags: MOUSE_EVENT_FLAGS, data: u32) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        };
        self.send(&input);
    }

    fn send(&mut self, input: &INPUT) {
        let sent = unsafe { SendInput(std::slice::from_ref(input), std::mem::size_of::<INPUT>() as i32) };
        if sent == 0 && !self.refused_logged {
            self.refused_logged = true;
            logi!(
                "input: Windows refused injected input ({}): an admin window has focus (run bb1 as admin to control it), or the secure desktop is up",
                windows::core::Error::from_thread()
            );
        } else if sent != 0 {
            self.refused_logged = false;
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_all();
    }
}

// MARK: Game controllers (ViGEmBus)

pub type RumbleSink = Arc<dyn Fn(u32, RumbleMsg) + Send + Sync>;

/// One viewer's pad: a virtual Xbox 360 controller, plugged in while the pad is.
struct Pad {
    target: vigem_client::Xbox360Wired<Arc<vigem_client::Client>>,
    /// The last report it took (None: none yet).
    last: Option<gamepad::XReport>,
}

/// Every viewer's virtual controllers. Connects to ViGEmBus when the first controller arrives (and
/// again every few seconds while it isn't there, so installing the driver works without restarting).
#[derive(Default)]
pub struct Gamepads {
    bus: Option<Arc<vigem_client::Client>>,
    bus_tried: Option<Instant>,
    missing_logged: bool,
    pads: HashMap<(u32, i64), Pad>,
    sink: Option<RumbleSink>,
}

/// How many virtual controllers this process has plugged in (the client side skips them, so a PC
/// that shares and watches at once doesn't send its friends' controllers back out as its own).
pub static VIRTUAL_PADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Gamepads {
    fn count_changed(&self) {
        VIRTUAL_PADS.store(self.pads.len(), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_rumble_sink(&mut self, sink: RumbleSink) {
        self.sink = Some(sink);
    }

    /// A viewer's controller changed: plug it in on its first state, update it, unplug on connected = false.
    pub fn update(&mut self, viewer: u32, g: &GamepadMsg) {
        if g.pad < 0 || g.pad >= MAX_PADS as i64 {
            return;
        }
        let key = (viewer, g.pad);
        if !g.connected {
            if self.pads.remove(&key).is_some() {
                logi!("controllers: viewer {viewer}'s pad {} unplugged", g.pad);
                self.count_changed();
            }
            return;
        }
        let report = gamepad::report(g);
        if !self.pads.contains_key(&key) {
            let Some(bus) = self.bus() else { return };
            match plug(bus, viewer, g.pad, self.sink.clone()) {
                Ok(pad) => {
                    logi!("controllers: viewer {viewer}'s pad {} ({}) is an Xbox 360 controller now", g.pad, g.name.as_deref().unwrap_or("a controller"));
                    self.pads.insert(key, pad);
                    self.count_changed();
                }
                Err(e) => {
                    logi!("controllers: couldn't plug in a virtual controller for viewer {viewer}'s pad {}: {e:?}", g.pad);
                    return;
                }
            }
        }
        let Some(pad) = self.pads.get_mut(&key) else { return };
        if pad.last == Some(report) {
            return;
        }
        // ViGEmBus sometimes refuses a report (259) while it's settling a newly plugged controller. A lost
        // report could be a button's release (a button stuck down), so try a few more times, briefly.
        let mut result = pad.target.update(&xgamepad(report));
        for _ in 0..5 {
            if result.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
            result = pad.target.update(&xgamepad(report));
        }
        match result {
            Ok(()) => pad.last = Some(report),
            Err(vigem_client::Error::TargetNotReady) => {}
            Err(e) => logi!("controllers: viewer {viewer}'s pad {}: update failed: {e:?}", g.pad),
        }
    }

    /// Nothing pressed on any of this viewer's controllers (they stay plugged in).
    pub fn release(&mut self, viewer: u32) {
        let neutral = gamepad::XReport::default();
        for (_, pad) in self.pads.iter_mut().filter(|((v, _), _)| *v == viewer) {
            if pad.target.update(&xgamepad(neutral)).is_ok() {
                pad.last = Some(neutral);
            }
        }
    }

    /// Unplug this viewer's controllers.
    pub fn viewer_left(&mut self, viewer: u32) {
        let before = self.pads.len();
        self.pads.retain(|(v, _), _| *v != viewer);
        self.count_changed();
        if self.pads.len() != before {
            logi!("controllers: unplugged viewer {viewer}'s {} controller(s)", before - self.pads.len());
        }
    }

    fn bus(&mut self) -> Option<Arc<vigem_client::Client>> {
        if self.bus.is_none() && self.bus_tried.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
            self.bus_tried = Some(Instant::now());
            match vigem_client::Client::connect() {
                Ok(c) => {
                    logi!("controllers: connected to ViGEmBus");
                    self.bus = Some(Arc::new(c));
                }
                Err(e) => {
                    if !self.missing_logged {
                        self.missing_logged = true;
                        logi!("controllers: ViGEmBus isn't installed ({e:?}); friends' controllers are ignored until it is (Settings → Game controllers → Install driver)");
                    }
                }
            }
        }
        self.bus.clone()
    }
}

fn xgamepad(r: gamepad::XReport) -> vigem_client::XGamepad {
    vigem_client::XGamepad {
        buttons: vigem_client::XButtons(r.buttons),
        left_trigger: r.left_trigger,
        right_trigger: r.right_trigger,
        thumb_lx: r.thumb_lx,
        thumb_ly: r.thumb_ly,
        thumb_rx: r.thumb_rx,
        thumb_ry: r.thumb_ry,
    }
}

/// Plugs in a virtual Xbox 360 controller and forwards what games ask its motors to do.
fn plug(bus: Arc<vigem_client::Client>, viewer: u32, pad: i64, sink: Option<RumbleSink>) -> Result<Pad, vigem_client::Error> {
    let mut target = vigem_client::Xbox360Wired::new(bus, vigem_client::TargetId::XBOX360_WIRED);
    target.plugin()?;
    // Until Windows has set it up, reports may be refused. Usually a few milliseconds.
    target.wait_ready()?;
    // Even after that, ViGEmBus refuses the very first report (ERROR_NO_MORE_ITEMS, 259; seen on the
    // owner's PC with ViGEmBus 1.22), and the ones after it go through. Send a neutral one until it's
    // taken, so the viewer's first real state isn't the one lost.
    let neutral = xgamepad(gamepad::XReport::default());
    for _ in 0..20 {
        if target.update(&neutral).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if let Some(sink) = sink {
        match target.request_notification() {
            Ok(n) => {
                // Games often set the same strengths every frame: only changes go to the viewer (it
                // keeps the motors going until told otherwise). The thread ends when the target is unplugged.
                let mut last = (0u8, 0u8);
                let _ = n.spawn_thread(move |_, n| {
                    let now = (n.large_motor, n.small_motor);
                    if now != last {
                        last = now;
                        sink(viewer, RumbleMsg { pad, large: now.0 as i64, small: now.1 as i64 });
                    }
                });
            }
            Err(e) => logi!("controllers: no rumble for viewer {viewer}'s pad {pad}: {e:?}"),
        }
    }
    Ok(Pad { target, last: None })
}

static DRIVER_CHECK: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

/// Whether the ViGEmBus driver is there (Settings asks while it paints: checked at most every few seconds).
pub fn controller_driver_installed() -> bool {
    let mut cache = DRIVER_CHECK.lock().unwrap();
    if let Some((at, ok)) = *cache {
        if at.elapsed() < Duration::from_secs(3) {
            return ok;
        }
    }
    let ok = vigem_client::Client::connect().is_ok();
    *cache = Some((Instant::now(), ok));
    ok
}

/// Check again on the next ask (its installer just finished).
pub fn recheck_controller_driver() {
    *DRIVER_CHECK.lock().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::keymap::{ModifierLayout, mac_key};

    #[test]
    fn every_key_round_trips() {
        for &(mac, scan, ext) in KEYS {
            if mac == 71 {
                // Num Lock: the client turns the hook's E0 0x45 into 0x45 before mapping.
                assert_eq!(mac_key(0x45, false, ModifierLayout::Standard), Some(71));
                continue;
            }
            assert_eq!(mac_key(scan as u32, ext, ModifierLayout::Standard), Some(mac), "scan {scan:#x} ext {ext}");
        }
        // Every Mac code a Windows client can send has a PC key here.
        for layout in [ModifierLayout::Standard, ModifierLayout::MacPositions, ModifierLayout::CtrlIsCommand] {
            for scan in 0..=0x7Fu32 {
                for ext in [false, true] {
                    let Some(mac) = mac_key(scan, ext, layout) else { continue };
                    if mac == MAC_CAPS_LOCK {
                        continue;
                    }
                    assert!(scan_for(mac, true).is_some() && scan_for(mac, false).is_some(), "mac {mac} (scan {scan:#x} ext {ext})");
                }
            }
        }
        // No Mac code is listed twice.
        let mut codes: Vec<u16> = KEYS.iter().map(|k| k.0).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), KEYS.len());
    }

    #[test]
    fn modifiers() {
        // A Windows client in its default layout, with ⌘ as the Windows key: every modifier comes back as itself.
        for (scan, ext) in [(0x1D, false), (0x1D, true), (0x38, false), (0x38, true), (0x5B, true), (0x5C, true)] {
            let mac = mac_key(scan, ext, ModifierLayout::Standard).unwrap();
            assert_eq!(scan_for(mac, true), Some((scan as u16, ext)), "scan {scan:#x}");
        }
        // From a Mac: ⌘ is Ctrl.
        assert_eq!(scan_for(CMD_L, false), Some((0x1D, false)));
        assert_eq!(scan_for(CMD_R, false), Some((0x1D, true)));
        assert_eq!(scan_for(CTRL_L, false), Some((0x1D, false)));
        assert_eq!(scan_for(OPT_L, false), Some((0x38, false)));
        assert_eq!(scan_for(OPT_R, true), Some((0x38, true)));
        assert_eq!(scan_for(MAC_CAPS_LOCK, false), None);
        assert_eq!(scan_for(63, false), None); // fn
        assert_eq!(scan_for(126, false), Some((0x48, true))); // Up arrow is extended
        assert_eq!(scan_for(91, false), Some((0x48, false))); // keypad 8 isn't
    }

    #[test]
    fn absolute_coordinates() {
        // 1920-wide primary at 0 in a 3840-wide desktop starting at −1920.
        let left_edge = normalize(pixel(0.0, 0, 1920), -1920, 3840);
        let right_edge = normalize(pixel(1.0, 0, 1920), -1920, 3840);
        assert_eq!(left_edge * 3840 / 65536 - 1920, 0);
        assert_eq!(right_edge * 3840 / 65536 - 1920, 1919);
        assert_eq!(normalize(pixel(0.0, 0, 1920), 0, 1920), 17);
        assert!(normalize(pixel(1.0, 0, 1920), 0, 1920) <= 65535);
        // Every pixel maps back to itself.
        for p in 0..1920 {
            assert_eq!(normalize(p, 0, 1920) * 1920 / 65536, p);
        }
        assert_eq!(pixel(f64::NAN, 10, 100), 10);
        assert_eq!(pixel(2.0, 10, 100), 109);
    }

    #[test]
    fn scroll_directions() {
        // Mac scroll up (dy > 0) = wheel forward (> 0); Mac scroll left (dx > 0) = tilt left (< 0).
        assert_eq!(wheel_units(0.0, 3.0, false, 3.0), (120.0, -0.0));
        assert_eq!(wheel_units(1.0, 0.0, false, 3.0).1, -40.0);
        // The Windows client sends a touchpad delta d as d/2 pixels: it comes back as d.
        assert_eq!(wheel_units(0.0, -15.0, true, 3.0).0, -30.0);
    }
}
