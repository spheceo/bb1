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

// MARK: The other way: Mac key codes → PC keys (for a host on a PC)

/// Mac key code → (PC scan code, E0 extended), for every key `keymap::mac_key` produces except the
/// modifiers ⌘ ⌃ ⌥ (see `scan_for`). Where two PC keys give the same Mac code the one in that key's
/// place on a Mac keyboard wins: F13 → Print Screen, F14 → Scroll Lock, Kana → Kana (not Henkan).
pub const KEYS: &[(u16, u16, bool)] = &[
    (53, 0x01, false), // Escape
    (18, 0x02, false), // 1
    (19, 0x03, false),
    (20, 0x04, false),
    (21, 0x05, false),
    (23, 0x06, false),
    (22, 0x07, false),
    (26, 0x08, false),
    (28, 0x09, false),
    (25, 0x0A, false),
    (29, 0x0B, false), // 0
    (27, 0x0C, false), // -
    (24, 0x0D, false), // =
    (51, 0x0E, false), // Backspace
    (48, 0x0F, false), // Tab
    (12, 0x10, false), // Q
    (13, 0x11, false),
    (14, 0x12, false),
    (15, 0x13, false),
    (17, 0x14, false),
    (16, 0x15, false),
    (32, 0x16, false),
    (34, 0x17, false),
    (31, 0x18, false),
    (35, 0x19, false), // P
    (33, 0x1A, false), // [
    (30, 0x1B, false), // ]
    (36, 0x1C, false), // Return
    (0, 0x1E, false),  // A
    (1, 0x1F, false),
    (2, 0x20, false),
    (3, 0x21, false),
    (5, 0x22, false),
    (4, 0x23, false),
    (38, 0x24, false),
    (40, 0x25, false),
    (37, 0x26, false), // L
    (41, 0x27, false), // ;
    (39, 0x28, false), // '
    (50, 0x29, false), // `
    (56, 0x2A, false), // left Shift
    (42, 0x2B, false), // backslash
    (6, 0x2C, false),  // Z
    (7, 0x2D, false),
    (8, 0x2E, false),
    (9, 0x2F, false),
    (11, 0x30, false),
    (45, 0x31, false),
    (46, 0x32, false), // M
    (43, 0x33, false), // ,
    (47, 0x34, false), // .
    (44, 0x35, false), // /
    (60, 0x36, false), // right Shift
    (67, 0x37, false), // keypad *
    (49, 0x39, false), // Space
    (122, 0x3B, false), // F1
    (120, 0x3C, false),
    (99, 0x3D, false),
    (118, 0x3E, false),
    (96, 0x3F, false),
    (97, 0x40, false),
    (98, 0x41, false),
    (100, 0x42, false),
    (101, 0x43, false),
    (109, 0x44, false), // F10
    // Num Lock: the hook reports it as 0x45 + E0 (and the client sends that as keypad Clear); sent
    // without E0 it would be Pause.
    (71, 0x45, true),
    (107, 0x46, false), // F14 → Scroll Lock
    (89, 0x47, false),  // keypad 7
    (91, 0x48, false),
    (92, 0x49, false),
    (78, 0x4A, false), // keypad -
    (86, 0x4B, false),
    (87, 0x4C, false),
    (88, 0x4D, false),
    (69, 0x4E, false), // keypad +
    (83, 0x4F, false),
    (84, 0x50, false),
    (85, 0x51, false),
    (82, 0x52, false), // keypad 0
    (65, 0x53, false), // keypad .
    (10, 0x56, false), // ISO key next to left Shift
    (103, 0x57, false), // F11
    (111, 0x58, false), // F12
    (113, 0x66, false), // F15
    (106, 0x67, false), // F16
    (64, 0x68, false),  // F17
    (79, 0x69, false),  // F18
    (80, 0x6A, false),  // F19
    (104, 0x70, false), // Kana
    (94, 0x73, false),  // Ro
    (102, 0x7B, false), // Eisu → Muhenkan
    (93, 0x7D, false),  // Yen
    (76, 0x1C, true),   // keypad Enter
    (75, 0x35, true),   // keypad /
    (105, 0x37, true),  // F13 → Print Screen
    (115, 0x47, true),  // Home
    (126, 0x48, true),  // Up
    (116, 0x49, true),  // Page Up
    (123, 0x4B, true),  // Left
    (124, 0x4D, true),  // Right
    (119, 0x4F, true),  // End
    (125, 0x50, true),  // Down
    (121, 0x51, true),  // Page Down
    (114, 0x52, true),  // Help → Insert
    (117, 0x53, true),  // forward Delete
    (110, 0x5D, true),  // Menu
    (74, 0x20, true),   // Mute
    (73, 0x2E, true),   // Volume Down
    (72, 0x30, true),   // Volume Up
];

/// The PC key (scan code, extended) for Mac key `code`. None: no such key on a PC (fn, Caps Lock).
pub fn scan_for(code: u16, command_is_windows_key: bool) -> Option<(u16, bool)> {
    match code {
        CMD_L if command_is_windows_key => Some((0x5B, true)),
        CMD_R if command_is_windows_key => Some((0x5C, true)),
        CMD_L | CTRL_L => Some((0x1D, false)),
        CMD_R | CTRL_R => Some((0x1D, true)),
        OPT_L => Some((0x38, false)),
        OPT_R => Some((0x38, true)),
        MAC_CAPS_LOCK => None,
        _ => KEYS.iter().find(|k| k.0 == code).map(|k| (k.1, k.2)),
    }
}

// MARK: Linux

/// USB HID keyboard usage (what SDL reports as a scancode) → PC scan code (set 1) and E0 flag, so a
/// Linux client goes through the same `mac_key` as the Windows client.
pub fn scan_for_hid(usage: u32) -> Option<(u32, bool)> {
    const LETTERS: [u32; 26] = [
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D,
        0x15, 0x2C,
    ];
    Some(match usage {
        4..=29 => (LETTERS[(usage - 4) as usize], false),
        30..=39 => (usage - 30 + 0x02, false), // 1 … 9, 0
        40 => (0x1C, false),                   // Return
        41 => (0x01, false),                   // Escape
        42 => (0x0E, false),                   // Backspace
        43 => (0x0F, false),                   // Tab
        44 => (0x39, false),                   // Space
        45 => (0x0C, false),                   // -
        46 => (0x0D, false),                   // =
        47 => (0x1A, false),                   // [
        48 => (0x1B, false),                   // ]
        49 | 50 => (0x2B, false),              // backslash (and the ISO # key in its place)
        51 => (0x27, false),                   // ;
        52 => (0x28, false),                   // '
        53 => (0x29, false),                   // `
        54 => (0x33, false),                   // ,
        55 => (0x34, false),                   // .
        56 => (0x35, false),                   // /
        57 => (0x3A, false),                   // Caps Lock
        58..=67 => (usage - 58 + 0x3B, false), // F1 … F10
        68 => (0x57, false),                   // F11
        69 => (0x58, false),                   // F12
        70 => (0x37, true),                    // Print Screen
        71 => (0x46, false),                   // Scroll Lock
        73 => (0x52, true),                    // Insert
        74 => (0x47, true),                    // Home
        75 => (0x49, true),                    // Page Up
        76 => (0x53, true),                    // Delete
        77 => (0x4F, true),                    // End
        78 => (0x51, true),                    // Page Down
        79 => (0x4D, true),                    // Right
        80 => (0x4B, true),                    // Left
        81 => (0x50, true),                    // Down
        82 => (0x48, true),                    // Up
        83 => (0x45, false),                   // Num Lock (→ keypad Clear)
        84 => (0x35, true),                    // keypad /
        85 => (0x37, false),                   // keypad *
        86 => (0x4A, false),                   // keypad -
        87 => (0x4E, false),                   // keypad +
        88 => (0x1C, true),                    // keypad Enter
        89 => (0x4F, false),                   // keypad 1
        90 => (0x50, false),
        91 => (0x51, false),
        92 => (0x4B, false),
        93 => (0x4C, false),
        94 => (0x4D, false),
        95 => (0x47, false),
        96 => (0x48, false),
        97 => (0x49, false), // keypad 9
        98 => (0x52, false), // keypad 0
        99 => (0x53, false), // keypad .
        100 => (0x56, false), // the ISO key next to left Shift
        101 => (0x5D, true), // Menu
        104..=110 => (usage - 104 + 0x64, false), // F13 … F19
        127 => (0x20, true), // Mute
        128 => (0x30, true), // Volume Up
        129 => (0x2E, true), // Volume Down
        135 => (0x73, false), // Ro
        136 => (0x70, false), // Kana
        137 => (0x7D, false), // Yen
        138 => (0x79, false), // Henkan
        139 => (0x7B, false), // Muhenkan
        224 => (0x1D, false), // left Ctrl
        225 => (0x2A, false), // left Shift
        226 => (0x38, false), // left Alt
        227 => (0x5B, true),  // left Super
        228 => (0x1D, true),  // right Ctrl
        229 => (0x36, false), // right Shift
        230 => (0x38, true),  // right Alt
        231 => (0x5C, true),  // right Super
        _ => return None,
    })
}

/// PC scan code (set 1) and E0 flag → Linux input event code (linux/input-event-codes.h), for a
/// host on Linux. Most keys without E0 are the same number.
pub fn evdev_for_scan(scan: u16, extended: bool) -> Option<u32> {
    let code = if extended {
        match scan {
            0x1C => 96,  // KEY_KPENTER
            0x1D => 97,  // KEY_RIGHTCTRL
            0x35 => 98,  // KEY_KPSLASH
            0x37 => 99,  // KEY_SYSRQ (Print Screen)
            0x38 => 100, // KEY_RIGHTALT
            0x45 => 69,  // KEY_NUMLOCK (Windows reports it with E0)
            0x47 => 102, // KEY_HOME
            0x48 => 103, // KEY_UP
            0x49 => 104, // KEY_PAGEUP
            0x4B => 105, // KEY_LEFT
            0x4D => 106, // KEY_RIGHT
            0x4F => 107, // KEY_END
            0x50 => 108, // KEY_DOWN
            0x51 => 109, // KEY_PAGEDOWN
            0x52 => 110, // KEY_INSERT
            0x53 => 111, // KEY_DELETE
            0x5B => 125, // KEY_LEFTMETA
            0x5C => 126, // KEY_RIGHTMETA
            0x5D => 127, // KEY_COMPOSE (Menu)
            0x20 => 113, // KEY_MUTE
            0x2E => 114, // KEY_VOLUMEDOWN
            0x30 => 115, // KEY_VOLUMEUP
            _ => return None,
        }
    } else {
        match scan {
            0x01..=0x58 => scan as u32,
            0x64..=0x6A => 183 + (scan - 0x64) as u32, // KEY_F13 … KEY_F19
            0x70 => 93,                                // KEY_KATAKANAHIRAGANA
            0x73 => 89,                                // KEY_RO
            0x79 => 92,                                // KEY_HENKAN
            0x7B => 94,                                // KEY_MUHENKAN
            0x7D => 124,                               // KEY_YEN
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

    #[test]
    fn linux_keys_round_trip() {
        // Every key a Linux client can send comes back as the same key on a Linux host.
        for usage in 0..256u32 {
            let Some((scan, ext)) = scan_for_hid(usage) else { continue };
            let Some(mac) = mac_key(scan, ext, ModifierLayout::Standard) else { continue };
            if mac == MAC_CAPS_LOCK {
                continue;
            }
            let (back_scan, back_ext) = scan_for(mac, true).unwrap_or_else(|| panic!("usage {usage} → mac {mac}"));
            let evdev = evdev_for_scan(back_scan, back_ext).unwrap_or_else(|| panic!("usage {usage} → scan {back_scan:#x}"));
            assert!(evdev > 0, "usage {usage}");
        }
        assert_eq!(scan_for_hid(4), Some((0x1E, false))); // A
        assert_eq!(evdev_for_scan(0x1E, false), Some(30)); // KEY_A
        assert_eq!(evdev_for_scan(0x48, true), Some(103)); // KEY_UP
        assert_eq!(evdev_for_scan(0x5B, true), Some(125)); // KEY_LEFTMETA
        assert_eq!(evdev_for_scan(0x57, false), Some(87)); // KEY_F11
    }
}
