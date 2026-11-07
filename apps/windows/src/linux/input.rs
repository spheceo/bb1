//! Keyboard and mouse from viewers into this computer, through the desktop portal's remote-input
//! session (portal.rs), so it works on Wayland without root.
//!
//! Keys arrive as macOS key codes; `keymap::scan_for` turns them into PC keys and
//! `keymap::evdev_for_scan` into Linux key codes. ⌘ is Ctrl for Mac viewers (⌘C copies) and the
//! Super key for Windows and Linux viewers, whose Windows/Super key arrives as ⌘ (like the Windows
//! host). Two Mac keys can land on one key (⌘ and ⌃ both on Ctrl): it goes down with the first and
//! up with the last. Key repeats aren't sent on: Wayland apps repeat held keys themselves.
//!
//! Mouse positions (0…1 of the shared screen) become the portal stream's logical pixels. A wheel's
//! lines become wheel clicks (3 lines a click, as Windows counts them); touchpad pixels scroll smoothly.

use crate::core::keymap::{MAC_CAPS_LOCK, evdev_for_scan, scan_for};
use crate::core::proto::{KeyMsg, MouseButtonMsg, MouseMoveMsg, ScrollMsg};
use crate::linux::portal::Portal;
use crate::logi;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BTN_LEFT: i32 = 0x110;
const KEY_CAPSLOCK: u32 = 58;
/// NSEvent.Phase: began, ended (and momentum's end).
const PHASE_BEGAN: u64 = 1;
const PHASE_ENDED: u64 = 8;
const LINES_PER_CLICK: f64 = 3.0;

pub struct PortalInput {
    portal: Arc<Portal>,
    command_is_super: bool,
    /// Mac key code and the Linux key it pressed.
    held_keys: Vec<(u16, u32)>,
    held_buttons: Vec<i32>,
    /// Wheel clicks not sent yet (vertical, horizontal).
    clicks_left: (f64, f64),
    caps_sent: Option<(bool, Instant)>,
    told_no_input: bool,
    told_no_pads: bool,
}

impl PortalInput {
    pub fn new(portal: Arc<Portal>) -> PortalInput {
        PortalInput {
            portal,
            command_is_super: false,
            held_keys: Vec::new(),
            held_buttons: Vec::new(),
            clicks_left: (0.0, 0.0),
            caps_sent: None,
            told_no_input: false,
            told_no_pads: false,
        }
    }

    fn allowed(&mut self) -> bool {
        if !self.portal.input && !self.told_no_input {
            self.told_no_input = true;
            logi!("input: the desktop didn't allow remote control; viewers can only watch");
        }
        self.portal.input
    }

    fn evdev(&self, code: u16) -> Option<u32> {
        let (scan, ext) = scan_for(code, self.command_is_super)?;
        evdev_for_scan(scan, ext)
    }

    fn position(&self, x: f64, y: f64) -> (f64, f64) {
        let (w, h) = self.portal.logical_size;
        (x.clamp(0.0, 1.0) * (w - 1.0).max(0.0), y.clamp(0.0, 1.0) * (h - 1.0).max(0.0))
    }
}

/// Caps Lock's light on any keyboard (what the desktop has it set to), where Linux shows it.
fn caps_lock_led() -> Option<bool> {
    let mut any = None;
    for e in std::fs::read_dir("/sys/class/leds").ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with("::capslock")
            && let Ok(v) = std::fs::read_to_string(e.path().join("brightness"))
        {
            let on = v.trim() != "0";
            any = Some(any.unwrap_or(false) || on);
        }
    }
    any
}

impl crate::core::host::InputBackend for PortalInput {
    fn key(&mut self, k: &KeyMsg, command_is_windows_key: bool) {
        if !self.allowed() || k.code == MAC_CAPS_LOCK {
            return;
        }
        self.command_is_super = command_is_windows_key;
        let held = self.held_keys.iter().position(|h| h.0 == k.code);
        if k.down {
            if held.is_some() {
                return; // a repeat
            }
            let Some(key) = self.evdev(k.code) else { return };
            let shared = self.held_keys.iter().any(|h| h.1 == key);
            self.held_keys.push((k.code, key));
            if !shared {
                self.portal.key(key, true);
            }
        } else {
            let key = match held {
                Some(i) => self.held_keys.remove(i).1,
                None => match self.evdev(k.code) {
                    Some(key) => key,
                    None => return,
                },
            };
            if !self.held_keys.iter().any(|h| h.1 == key) {
                self.portal.key(key, false);
            }
        }
    }

    fn mouse_move(&mut self, m: &MouseMoveMsg) {
        if !self.allowed() {
            return;
        }
        let (x, y) = self.position(m.x, m.y);
        self.portal.pointer_to(x, y);
    }

    fn mouse_button(&mut self, b: &MouseButtonMsg) {
        if !self.allowed() {
            return;
        }
        // 0 left, 1 right, 2 middle, then side and extra.
        let button = match b.button {
            0 => BTN_LEFT,
            1 => BTN_LEFT + 1,
            2 => BTN_LEFT + 2,
            n @ 3..=7 => BTN_LEFT + n as i32,
            _ => return,
        };
        let (x, y) = self.position(b.x, b.y);
        self.portal.pointer_to(x, y);
        if b.down {
            if !self.held_buttons.contains(&button) {
                self.held_buttons.push(button);
            }
        } else {
            self.held_buttons.retain(|&h| h != button);
        }
        self.portal.button(button, b.down);
    }

    fn scroll(&mut self, s: &ScrollMsg) {
        if !self.allowed() {
            return;
        }
        // Mac: dy > 0 scrolls up, dx > 0 scrolls left. Linux: positive is down / right.
        if s.precise {
            let finish = s.phase & PHASE_ENDED != 0 || s.momentum & PHASE_ENDED != 0;
            if s.dx != 0.0 || s.dy != 0.0 || finish {
                self.portal.scroll(-s.dx, -s.dy, finish);
            }
            return;
        }
        if s.phase & PHASE_BEGAN != 0 {
            self.clicks_left = (0.0, 0.0);
        }
        self.clicks_left.0 += -s.dy / LINES_PER_CLICK;
        self.clicks_left.1 += -s.dx / LINES_PER_CLICK;
        let v = self.clicks_left.0.trunc();
        let h = self.clicks_left.1.trunc();
        self.clicks_left.0 -= v;
        self.clicks_left.1 -= h;
        if v != 0.0 {
            self.portal.scroll_steps(0, v as i32);
        }
        if h != 0.0 {
            self.portal.scroll_steps(1, h as i32);
        }
    }

    fn set_caps_lock(&mut self, on: bool) {
        if !self.allowed() {
            return;
        }
        let current = match self.caps_sent {
            Some((state, at)) if at.elapsed() < Duration::from_millis(500) => Some(state),
            _ => caps_lock_led(),
        };
        // Without a light to read, trust what was last set (or assume off).
        let current = current.or(self.caps_sent.map(|c| c.0)).unwrap_or(false);
        if current != on {
            self.portal.key(KEY_CAPSLOCK, true);
            self.portal.key(KEY_CAPSLOCK, false);
            self.caps_sent = Some((on, Instant::now()));
        }
    }

    fn release_all(&mut self) {
        let mut released = Vec::new();
        for (_, key) in std::mem::take(&mut self.held_keys) {
            if !released.contains(&key) {
                released.push(key);
                self.portal.key(key, false);
            }
        }
        for b in std::mem::take(&mut self.held_buttons) {
            self.portal.button(b, false);
        }
    }

    fn gamepad(&mut self, _viewer: u32, _g: &crate::core::proto::GamepadMsg) {
        if !self.told_no_pads {
            self.told_no_pads = true;
            logi!("input: a viewer's game controller arrived; controllers aren't shared on Linux yet");
        }
    }
}
