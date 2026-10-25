//! PC keyboard scan codes (set 1, as Windows reports them) → macOS virtual key codes.
//! Both are positions on the keyboard, not characters, so the Mac's own keyboard layout decides
//! what each key types (the same as Mac → Mac, which sends key codes as they are).

/// Which PC key acts as which Mac modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierLayout {
    /// Windows key = ⌘, Alt = ⌥, Ctrl = ⌃ (by name).
    Standard,
    /// Alt = ⌘, Windows key = ⌥, Ctrl = ⌃: the key next to the space bar is ⌘, as on a Mac keyboard.
    MacPositions,
    /// Ctrl = ⌘, Windows key = ⌃, Alt = ⌥: Ctrl+C copies, the way it does on Windows.
    CtrlIsCommand,
}

impl ModifierLayout {
    pub fn id(self) -> &'static str {
        match self {
            ModifierLayout::Standard => "standard",
            ModifierLayout::MacPositions => "mac",
            ModifierLayout::CtrlIsCommand => "ctrl",
        }
    }

    pub fn from_id(id: &str) -> ModifierLayout {
        match id {
            "mac" => ModifierLayout::MacPositions,
            "ctrl" => ModifierLayout::CtrlIsCommand,
            _ => ModifierLayout::Standard,
        }
    }
}

pub const MAC_CAPS_LOCK: u16 = 57;
const CMD_L: u16 = 55;
const CMD_R: u16 = 54;
const OPT_L: u16 = 58;
const OPT_R: u16 = 61;
const CTRL_L: u16 = 59;
const CTRL_R: u16 = 62;

/// `scan` is the scan code, `extended` the E0 prefix flag. None for keys with no Mac equivalent.
pub fn mac_key(scan: u32, extended: bool, layout: ModifierLayout) -> Option<u16> {
    // Modifiers first: they depend on the layout.
    let modifier = match (scan, extended) {
        (0x1D, false) => Some(("ctrl", false)),
        (0x1D, true) => Some(("ctrl", true)),
        (0x38, false) => Some(("alt", false)),
        (0x38, true) => Some(("alt", true)),
        (0x5B, true) => Some(("win", false)),
        (0x5C, true) => Some(("win", true)),
        _ => None,
    };
    if let Some((key, right)) = modifier {
        let role = match (layout, key) {
            (ModifierLayout::Standard, "win") | (ModifierLayout::MacPositions, "alt") | (ModifierLayout::CtrlIsCommand, "ctrl") => "cmd",
            (ModifierLayout::Standard, "alt") | (ModifierLayout::MacPositions, "win") | (ModifierLayout::CtrlIsCommand, "alt") => "opt",
            _ => "ctrl",
        };
        return Some(match (role, right) {
            ("cmd", false) => CMD_L,
            ("cmd", true) => CMD_R,
            ("opt", false) => OPT_L,
            ("opt", true) => OPT_R,
            (_, false) => CTRL_L,
            (_, true) => CTRL_R,
        });
    }
    let code = if extended {
        match scan {
            0x1C => 76,  // keypad Enter
            0x35 => 75,  // keypad /
            0x37 => 105, // Print Screen → F13
            0x47 => 115, // Home
            0x48 => 126, // Up
            0x49 => 116, // Page Up
            0x4B => 123, // Left
            0x4D => 124, // Right
            0x4F => 119, // End
            0x50 => 125, // Down
            0x51 => 121, // Page Down
            0x52 => 114, // Insert → Help
            0x53 => 117, // Delete (forward)
            0x5D => 110, // Menu (application key)
            0x20 => 74,  // Mute
            0x2E => 73,  // Volume Down
            0x30 => 72,  // Volume Up
            _ => return None,
        }
    } else {
        match scan {
            0x01 => 53, // Escape
            0x02 => 18,
            0x03 => 19,
            0x04 => 20,
            0x05 => 21,
            0x06 => 23,
            0x07 => 22,
            0x08 => 26,
            0x09 => 28,
            0x0A => 25,
            0x0B => 29,
            0x0C => 27, // -
            0x0D => 24, // =
            0x0E => 51, // Backspace
            0x0F => 48, // Tab
            0x10 => 12, // Q
            0x11 => 13,
            0x12 => 14,
            0x13 => 15,
            0x14 => 17,
            0x15 => 16,
            0x16 => 32,
            0x17 => 34,
            0x18 => 31,
            0x19 => 35, // P
            0x1A => 33, // [
            0x1B => 30, // ]
            0x1C => 36, // Return
            0x1E => 0,  // A
            0x1F => 1,
            0x20 => 2,
            0x21 => 3,
            0x22 => 5,
            0x23 => 4,
            0x24 => 38,
            0x25 => 40,
            0x26 => 37, // L
            0x27 => 41, // ;
            0x28 => 39, // '
            0x29 => 50, // `
            0x2A => 56, // left Shift
            0x2B => 42, // backslash
            0x2C => 6,  // Z
            0x2D => 7,
            0x2E => 8,
            0x2F => 9,
            0x30 => 11,
            0x31 => 45,
            0x32 => 46, // M
            0x33 => 43, // ,
            0x34 => 47, // .
            0x35 => 44, // /
            0x36 => 60, // right Shift
            0x37 => 67, // keypad *
            0x39 => 49, // Space
            0x3A => MAC_CAPS_LOCK,
            0x3B => 122, // F1
            0x3C => 120,
            0x3D => 99,
            0x3E => 118,
            0x3F => 96,
            0x40 => 97,
            0x41 => 98,
            0x42 => 100,
            0x43 => 101,
            0x44 => 109, // F10
            0x45 => 71,  // Num Lock → keypad Clear
            0x46 => 107, // Scroll Lock → F14
            0x47 => 89,  // keypad 7
            0x48 => 91,
            0x49 => 92,
            0x4A => 78, // keypad -
            0x4B => 86,
            0x4C => 87,
            0x4D => 88,
            0x4E => 69, // keypad +
            0x4F => 83,
            0x50 => 84,
            0x51 => 85,
            0x52 => 82, // keypad 0
            0x53 => 65, // keypad .
            0x56 => 10, // the extra key next to left Shift on ISO keyboards
            0x57 => 103, // F11
            0x58 => 111, // F12
            0x64 => 105, // F13
            0x65 => 107, // F14
            0x66 => 113, // F15
            0x67 => 106, // F16
            0x68 => 64,  // F17
            0x69 => 79,  // F18
            0x6A => 80,  // F19
            0x70 => 104, // Kana
            0x73 => 94,  // Ro (JIS _)
            0x7B => 102, // Muhenkan → Eisu
            0x79 => 104, // Henkan → Kana
            0x7D => 93,  // Yen
            _ => return None,
        }
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_and_modifiers() {
        assert_eq!(mac_key(0x1E, false, ModifierLayout::Standard), Some(0)); // A
        assert_eq!(mac_key(0x2E, false, ModifierLayout::Standard), Some(8)); // C
        assert_eq!(mac_key(0x5B, true, ModifierLayout::Standard), Some(CMD_L));
        assert_eq!(mac_key(0x38, false, ModifierLayout::Standard), Some(OPT_L));
        assert_eq!(mac_key(0x38, false, ModifierLayout::MacPositions), Some(CMD_L));
        assert_eq!(mac_key(0x5B, true, ModifierLayout::MacPositions), Some(OPT_L));
        assert_eq!(mac_key(0x1D, false, ModifierLayout::CtrlIsCommand), Some(CMD_L));
        assert_eq!(mac_key(0x1D, true, ModifierLayout::CtrlIsCommand), Some(CMD_R));
        assert_eq!(mac_key(0x5B, true, ModifierLayout::CtrlIsCommand), Some(CTRL_L));
        assert_eq!(mac_key(0x48, true, ModifierLayout::Standard), Some(126)); // Up arrow
        assert_eq!(mac_key(0x48, false, ModifierLayout::Standard), Some(91)); // keypad 8
    }
}
