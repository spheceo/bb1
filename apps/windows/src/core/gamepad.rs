//! Game controllers, the parts that don't need Windows: SDL3 state → GamepadMsg (the client,
//! win/gamepad.rs), GamepadMsg → an Xbox 360 report (the host's ViGEm controllers, host/input.rs),
//! rumble strengths, and which pad number each controller gets.
//!
//! GamepadMsg is the Xbox layout: XInput wButtons bits, triggers 0…255, sticks −32768…32767 with
//! up and right positive (docs/02-protocol.md, BB1Core/Messages.swift).

use crate::core::proto::GamepadMsg;

/// XInput wButtons bits.
pub mod xbutton {
    pub const DPAD_UP: u16 = 0x0001;
    pub const DPAD_DOWN: u16 = 0x0002;
    pub const DPAD_LEFT: u16 = 0x0004;
    pub const DPAD_RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const LEFT_THUMB: u16 = 0x0040;
    pub const RIGHT_THUMB: u16 = 0x0080;
    pub const LEFT_SHOULDER: u16 = 0x0100;
    pub const RIGHT_SHOULDER: u16 = 0x0200;
    pub const GUIDE: u16 = 0x0400;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;
}

/// Controllers per viewer (XInput has four slots).
pub const MAX_PADS: usize = 4;

/// SDL3's SDL_GamepadButton values (SDL_gamepad.h, 3.2), and the XInput bit each one is.
/// South is the bottom face button whatever it's labelled (Cross on a PlayStation pad = A).
pub const SDL_BUTTONS: [(i32, u16); 15] = [
    (0, xbutton::A),               // SDL_GAMEPAD_BUTTON_SOUTH
    (1, xbutton::B),               // EAST
    (2, xbutton::X),               // WEST
    (3, xbutton::Y),               // NORTH
    (4, xbutton::BACK),            // BACK
    (5, xbutton::GUIDE),           // GUIDE
    (6, xbutton::START),           // START
    (7, xbutton::LEFT_THUMB),      // LEFT_STICK
    (8, xbutton::RIGHT_THUMB),     // RIGHT_STICK
    (9, xbutton::LEFT_SHOULDER),   // LEFT_SHOULDER
    (10, xbutton::RIGHT_SHOULDER), // RIGHT_SHOULDER
    (11, xbutton::DPAD_UP),        // DPAD_UP
    (12, xbutton::DPAD_DOWN),      // DPAD_DOWN
    (13, xbutton::DPAD_LEFT),      // DPAD_LEFT
    (14, xbutton::DPAD_RIGHT),     // DPAD_RIGHT
];

/// SDL3's SDL_GamepadAxis values: LEFTX, LEFTY, RIGHTX, RIGHTY, LEFT_TRIGGER, RIGHT_TRIGGER.
pub const SDL_AXES: usize = 6;

/// SDL's stick axes have down positive; ours have up positive. −32768 flipped doesn't fit, so it
/// becomes 32767.
pub fn flip_y(v: i16) -> i64 {
    (-(v as i64)).min(32767)
}

/// An SDL trigger (0…32767) → 0…255.
pub fn trigger(v: i16) -> i64 {
    (v.max(0) as i64 * 255 + 16383) / 32767
}

/// One controller's SDL state → its message. `pressed(b)` says whether SDL button `b` is down;
/// `axes` are SDL_GamepadAxis 0…5.
pub fn from_sdl(pad: usize, pressed: impl Fn(i32) -> bool, axes: [i16; SDL_AXES]) -> GamepadMsg {
    let buttons = SDL_BUTTONS.iter().filter(|(b, _)| pressed(*b)).fold(0u16, |acc, (_, bit)| acc | bit);
    GamepadMsg {
        pad: pad as i64,
        connected: true,
        buttons: buttons as i64,
        lt: trigger(axes[4]),
        rt: trigger(axes[5]),
        lx: axes[0] as i64,
        ly: flip_y(axes[1]),
        rx: axes[2] as i64,
        ry: flip_y(axes[3]),
        name: None,
    }
}

/// Nothing pressed, sticks centred (what a controller is left at when its access is turned off,
/// or when the stream window isn't in front).
pub fn neutral(pad: usize) -> GamepadMsg {
    GamepadMsg { pad: pad as i64, connected: true, ..Default::default() }
}

/// Same state, ignoring the name (sent once, with the first state).
pub fn same_state(a: &GamepadMsg, b: &GamepadMsg) -> bool {
    (a.pad, a.connected, a.buttons, a.lt, a.rt, a.lx, a.ly, a.rx, a.ry) == (b.pad, b.connected, b.buttons, b.lt, b.rt, b.lx, b.ly, b.rx, b.ry)
}

/// An Xbox 360 report (XINPUT_GAMEPAD's fields), what ViGEm takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XReport {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub thumb_lx: i16,
    pub thumb_ly: i16,
    pub thumb_rx: i16,
    pub thumb_ry: i16,
}

/// A viewer's message → the report, with everything out of range clamped (it came over the network).
pub fn report(g: &GamepadMsg) -> XReport {
    let stick = |v: i64| v.clamp(-32768, 32767) as i16;
    XReport {
        buttons: (g.buttons & 0xFFFF) as u16 & !0x0800, // 0x0800 isn't a button
        left_trigger: g.lt.clamp(0, 255) as u8,
        right_trigger: g.rt.clamp(0, 255) as u8,
        thumb_lx: stick(g.lx),
        thumb_ly: stick(g.ly),
        thumb_rx: stick(g.rx),
        thumb_ry: stick(g.ry),
    }
}

/// Rumble strength 0…255 → SDL's 0…65535 (large = low-frequency motor, small = high-frequency).
pub fn motor(v: i64) -> u16 {
    (v.clamp(0, 255) * 257) as u16
}

/// Which pad number (0…3) each controller has, in the order they were connected. A pad number is
/// reused once its controller is gone.
#[derive(Default, Debug)]
pub struct Slots {
    ids: [Option<u32>; MAX_PADS],
}

impl Slots {
    /// The pad for controller `id`, giving it the lowest free one. None: four are in use already.
    pub fn assign(&mut self, id: u32) -> Option<usize> {
        if let Some(i) = self.slot_of(id) {
            return Some(i);
        }
        let i = self.ids.iter().position(|s| s.is_none())?;
        self.ids[i] = Some(id);
        Some(i)
    }

    pub fn release(&mut self, id: u32) -> Option<usize> {
        let i = self.slot_of(id)?;
        self.ids[i] = None;
        Some(i)
    }

    pub fn slot_of(&self, id: u32) -> Option<usize> {
        self.ids.iter().position(|s| *s == Some(id))
    }

    /// (pad, controller) for every pad in use.
    pub fn used(&self) -> Vec<(usize, u32)> {
        self.ids.iter().enumerate().filter_map(|(i, s)| s.map(|id| (i, id))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdl_to_xbox() {
        // Nothing pressed, sticks centred.
        let g = from_sdl(1, |_| false, [0; SDL_AXES]);
        assert_eq!(g, GamepadMsg { pad: 1, connected: true, ..Default::default() });
        // Face buttons by position: South (Cross / A) is A, East is B, West is X, North is Y.
        assert_eq!(from_sdl(0, |b| b == 0, [0; 6]).buttons, xbutton::A as i64);
        assert_eq!(from_sdl(0, |b| b == 1, [0; 6]).buttons, xbutton::B as i64);
        assert_eq!(from_sdl(0, |b| b == 2, [0; 6]).buttons, xbutton::X as i64);
        assert_eq!(from_sdl(0, |b| b == 3, [0; 6]).buttons, xbutton::Y as i64);
        assert_eq!(from_sdl(0, |b| b == 5, [0; 6]).buttons, xbutton::GUIDE as i64);
        assert_eq!(from_sdl(0, |b| b == 11, [0; 6]).buttons, xbutton::DPAD_UP as i64);
        assert_eq!(from_sdl(0, |b| b == 14, [0; 6]).buttons, xbutton::DPAD_RIGHT as i64);
        // Everything SDL has an Xbox button for, and nothing else (touchpad, paddles, share).
        assert_eq!(from_sdl(0, |_| true, [0; 6]).buttons, 0xF7FF);
        assert_eq!(from_sdl(0, |b| b >= 15, [0; 6]).buttons, 0);
        // SDL's sticks are down-positive: pushing up (SDL −32768) is +32767 here; right stays right.
        let g = from_sdl(0, |_| false, [32767, -32768, -32768, 32767, 0, 32767]);
        assert_eq!((g.lx, g.ly, g.rx, g.ry), (32767, 32767, -32768, -32767));
        assert_eq!((g.lt, g.rt), (0, 255));
        assert_eq!(trigger(16384), 128);
        assert_eq!(trigger(-5), 0);
        assert_eq!(trigger(1), 0);
    }

    #[test]
    fn xbox_report() {
        let g = GamepadMsg { pad: 0, connected: true, buttons: 0x1000 | 0x0800 | 0x1_0000, lt: 300, rt: -1, lx: 40000, ly: -40000, rx: 5, ry: -5, name: None };
        let r = report(&g);
        assert_eq!(r, XReport { buttons: 0x1000, left_trigger: 255, right_trigger: 0, thumb_lx: 32767, thumb_ly: -32768, thumb_rx: 5, thumb_ry: -5 });
        assert_eq!(report(&neutral(2)), XReport::default());
    }

    #[test]
    fn rumble_strengths() {
        assert_eq!(motor(0), 0);
        assert_eq!(motor(255), 65535);
        assert_eq!(motor(128), 32896);
        assert_eq!(motor(999), 65535);
        assert_eq!(motor(-3), 0);
    }

    #[test]
    fn pads_in_connection_order() {
        let mut s = Slots::default();
        assert_eq!(s.assign(7), Some(0));
        assert_eq!(s.assign(3), Some(1));
        assert_eq!(s.assign(7), Some(0)); // already has one
        assert_eq!(s.assign(9), Some(2));
        assert_eq!(s.assign(11), Some(3));
        assert_eq!(s.assign(12), None); // four is the most
        assert_eq!(s.release(3), Some(1));
        assert_eq!(s.release(3), None);
        assert_eq!(s.assign(12), Some(1)); // the free one
        assert_eq!(s.used(), vec![(0, 7), (1, 12), (2, 9), (3, 11)]);
    }

    #[test]
    fn same_state_ignores_the_name() {
        let a = GamepadMsg { name: Some("DualSense".into()), ..neutral(0) };
        assert!(same_state(&a, &neutral(0)));
        assert!(!same_state(&a, &GamepadMsg { lx: 1, ..neutral(0) }));
    }
}
