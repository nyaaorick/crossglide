//! Keys: the Mac's virtual key codes (`kVK_*` in Carbon's `Events.h`) to Windows scan codes
//! (set 1, extended keys with `0xE0` in the high byte), and the hotkey that switches control.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Marks an extended scan code, e.g. `0xE04B` for Left Arrow.
pub const EXTENDED: u16 = 0xE000;

/// What the Mac's Command keys become on the PC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandKey {
    /// The Windows key, as Deskflow maps it.
    #[default]
    Win,
    /// Control, so Cmd-C copies; the Mac's Control keys become Windows keys instead.
    Ctrl,
}

/// The Windows scan code for Mac key `code`, or `None` for keys the PC has no use for (Fn).
pub fn scancode(code: u16, command: CommandKey) -> Option<u16> {
    let swap = command == CommandKey::Ctrl;
    Some(match code {
        0x00 => 0x1E,                    // A
        0x01 => 0x1F,                    // S
        0x02 => 0x20,                    // D
        0x03 => 0x21,                    // F
        0x04 => 0x23,                    // H
        0x05 => 0x22,                    // G
        0x06 => 0x2C,                    // Z
        0x07 => 0x2D,                    // X
        0x08 => 0x2E,                    // C
        0x09 => 0x2F,                    // V
        0x0A => 0x56,                    // ISO section: beside the left Shift on ISO boards
        0x0B => 0x30,                    // B
        0x0C => 0x10,                    // Q
        0x0D => 0x11,                    // W
        0x0E => 0x12,                    // E
        0x0F => 0x13,                    // R
        0x10 => 0x15,                    // Y
        0x11 => 0x14,                    // T
        0x12 => 0x02,                    // 1
        0x13 => 0x03,                    // 2
        0x14 => 0x04,                    // 3
        0x15 => 0x05,                    // 4
        0x16 => 0x07,                    // 6
        0x17 => 0x06,                    // 5
        0x18 => 0x0D,                    // =
        0x19 => 0x0A,                    // 9
        0x1A => 0x08,                    // 7
        0x1B => 0x0C,                    // -
        0x1C => 0x09,                    // 8
        0x1D => 0x0B,                    // 0
        0x1E => 0x1B,                    // ]
        0x1F => 0x18,                    // O
        0x20 => 0x16,                    // U
        0x21 => 0x1A,                    // [
        0x22 => 0x17,                    // I
        0x23 => 0x19,                    // P
        0x24 => 0x1C,                    // Return
        0x25 => 0x26,                    // L
        0x26 => 0x24,                    // J
        0x27 => 0x28,                    // '
        0x28 => 0x25,                    // K
        0x29 => 0x27,                    // ;
        0x2A => 0x2B,                    // backslash
        0x2B => 0x33,                    // ,
        0x2C => 0x35,                    // /
        0x2D => 0x31,                    // N
        0x2E => 0x32,                    // M
        0x2F => 0x34,                    // .
        0x30 => 0x0F,                    // Tab
        0x31 => 0x39,                    // Space
        0x32 => 0x29,                    // `
        0x33 => 0x0E,                    // Delete (Backspace)
        0x35 => 0x01,                    // Escape
        0x36 if swap => EXTENDED | 0x1D, // Right Command → Right Ctrl
        0x36 => EXTENDED | 0x5C,         // Right Command → Right Windows
        0x37 if swap => 0x1D,            // Command → Left Ctrl
        0x37 => EXTENDED | 0x5B,         // Command → Left Windows
        0x38 => 0x2A,                    // Shift
        0x39 => 0x3A,                    // Caps Lock
        0x3A => 0x38,                    // Option → Left Alt
        0x3B if swap => EXTENDED | 0x5B, // Control → Left Windows
        0x3B => 0x1D,                    // Control → Left Ctrl
        0x3C => 0x36,                    // Right Shift
        0x3D => EXTENDED | 0x38,         // Right Option → Right Alt
        0x3E if swap => EXTENDED | 0x5C, // Right Control → Right Windows
        0x3E => EXTENDED | 0x1D,         // Right Control
        0x40 => 0x68,                    // F17
        0x41 => 0x53,                    // Keypad .
        0x43 => 0x37,                    // Keypad *
        0x45 => 0x4E,                    // Keypad +
        0x47 => 0x45,                    // Keypad Clear → Num Lock
        0x48 => EXTENDED | 0x30,         // Volume Up
        0x49 => EXTENDED | 0x2E,         // Volume Down
        0x4A => EXTENDED | 0x20,         // Mute
        0x4B => EXTENDED | 0x35,         // Keypad /
        0x4C => EXTENDED | 0x1C,         // Keypad Enter
        0x4E => 0x4A,                    // Keypad -
        0x4F => 0x69,                    // F18
        0x50 => 0x6A,                    // F19
        0x51 => 0x59,                    // Keypad =
        0x52 => 0x52,                    // Keypad 0
        0x53 => 0x4F,                    // Keypad 1
        0x54 => 0x50,                    // Keypad 2
        0x55 => 0x51,                    // Keypad 3
        0x56 => 0x4B,                    // Keypad 4
        0x57 => 0x4C,                    // Keypad 5
        0x58 => 0x4D,                    // Keypad 6
        0x59 => 0x47,                    // Keypad 7
        0x5A => 0x6B,                    // F20
        0x5B => 0x48,                    // Keypad 8
        0x5C => 0x49,                    // Keypad 9
        0x5D => 0x7D,                    // JIS Yen
        0x5E => 0x73,                    // JIS underscore
        0x5F => 0x7E,                    // JIS keypad comma
        0x60 => 0x3F,                    // F5
        0x61 => 0x40,                    // F6
        0x62 => 0x41,                    // F7
        0x63 => 0x3D,                    // F3
        0x64 => 0x42,                    // F8
        0x65 => 0x43,                    // F9
        0x66 => 0x7B,                    // JIS Eisu → Muhenkan
        0x67 => 0x57,                    // F11
        0x68 => 0x70,                    // JIS Kana → Katakana/Hiragana
        0x69 => 0x64,                    // F13
        0x6A => 0x67,                    // F16
        0x6B => 0x65,                    // F14
        0x6D => 0x44,                    // F10
        0x6E => EXTENDED | 0x5D,         // Context menu
        0x6F => 0x58,                    // F12
        0x71 => 0x66,                    // F15
        0x72 => EXTENDED | 0x52,         // Help → Insert
        0x73 => EXTENDED | 0x47,         // Home
        0x74 => EXTENDED | 0x49,         // Page Up
        0x75 => EXTENDED | 0x53,         // Forward Delete
        0x76 => 0x3E,                    // F4
        0x77 => EXTENDED | 0x4F,         // End
        0x78 => 0x3C,                    // F2
        0x79 => EXTENDED | 0x51,         // Page Down
        0x7A => 0x3B,                    // F1
        0x7B => EXTENDED | 0x4B,         // Left Arrow
        0x7C => EXTENDED | 0x4D,         // Right Arrow
        0x7D => EXTENDED | 0x50,         // Down Arrow
        0x7E => EXTENDED | 0x48,         // Up Arrow
        _ => return None,
    })
}

/// Modifier keys, as bits of [`Hotkey::modifiers`].
pub mod modifier {
    pub const SHIFT: u8 = 1;
    pub const CONTROL: u8 = 2;
    pub const OPTION: u8 = 4;
    pub const COMMAND: u8 = 8;
}

/// The [`modifier`] bit of Mac key `code`, for keys that are Shift, Control, Option or Command.
pub fn modifier_of(code: u16) -> Option<u8> {
    Some(match code {
        0x38 | 0x3C => modifier::SHIFT,
        0x3B | 0x3E => modifier::CONTROL,
        0x3A | 0x3D => modifier::OPTION,
        0x37 | 0x36 => modifier::COMMAND,
        _ => return None,
    })
}

/// A key with exactly these modifiers held, e.g. `ctrl+option+cmd+space`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u8,
    /// Mac virtual key code.
    pub key: u16,
}

impl Hotkey {
    pub const DEFAULT: &str = "ctrl+option+cmd+space";
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyError(String);

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HotkeyError {}

impl FromStr for Hotkey {
    type Err = HotkeyError;

    fn from_str(s: &str) -> Result<Self, HotkeyError> {
        let mut modifiers = 0;
        let mut key = None;
        for part in s.split('+').map(|p| p.trim().to_ascii_lowercase()) {
            modifiers |= match part.as_str() {
                "shift" => modifier::SHIFT,
                "ctrl" | "control" => modifier::CONTROL,
                "alt" | "opt" | "option" => modifier::OPTION,
                "cmd" | "command" => modifier::COMMAND,
                name => {
                    let code = key_code(name).ok_or_else(|| {
                        HotkeyError(format!("hotkey \"{s}\": unknown key \"{name}\""))
                    })?;
                    if key.replace(code).is_some() {
                        return Err(HotkeyError(format!(
                            "hotkey \"{s}\": more than one key besides modifiers"
                        )));
                    }
                    0
                }
            };
        }
        match key {
            Some(key) if modifiers != 0 => Ok(Self { modifiers, key }),
            Some(_) => Err(HotkeyError(format!(
                "hotkey \"{s}\" needs at least one modifier, or it would take over a normal key"
            ))),
            None => Err(HotkeyError(format!("hotkey \"{s}\" has no key"))),
        }
    }
}

/// The Mac key code for a key name in a hotkey: a letter, digit, `f1`–`f12`, or a named key.
fn key_code(name: &str) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E, 0x2D, 0x1F,
        0x23, 0x0C, 0x0F, 0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06,
    ];
    const DIGITS: [u16; 10] = [0x1D, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19];
    const FUNCTION: [u16; 12] = [
        0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F,
    ];
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_lowercase() {
            return Some(LETTERS[(c as u8 - b'a') as usize]);
        }
        if let Some(d) = c.to_digit(10) {
            return Some(DIGITS[d as usize]);
        }
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<usize>().ok())
        && (1..=12).contains(&n)
    {
        return Some(FUNCTION[n - 1]);
    }
    Some(match name {
        "space" => 0x31,
        "return" | "enter" => 0x24,
        "tab" => 0x30,
        "escape" | "esc" => 0x35,
        "grave" | "`" => 0x32,
        "left" => 0x7B,
        "right" => 0x7C,
        "down" => 0x7D,
        "up" => 0x7E,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_extended_keys_map() {
        assert_eq!(scancode(0x00, CommandKey::Win), Some(0x1E));
        assert_eq!(scancode(0x1D, CommandKey::Win), Some(0x0B));
        assert_eq!(scancode(0x7B, CommandKey::Win), Some(0xE04B));
        assert_eq!(scancode(0x3F, CommandKey::Win), None); // Fn
    }

    #[test]
    fn command_is_win_or_swaps_with_control() {
        assert_eq!(scancode(0x37, CommandKey::Win), Some(0xE05B));
        assert_eq!(scancode(0x3B, CommandKey::Win), Some(0x1D));
        assert_eq!(scancode(0x37, CommandKey::Ctrl), Some(0x1D));
        assert_eq!(scancode(0x3B, CommandKey::Ctrl), Some(0xE05B));
        assert_eq!(scancode(0x36, CommandKey::Ctrl), Some(0xE01D));
    }

    #[test]
    fn every_mapped_key_has_its_own_scan_code() {
        for command in [CommandKey::Win, CommandKey::Ctrl] {
            let mut seen = std::collections::HashMap::new();
            for code in 0..128 {
                if let Some(sc) = scancode(code, command)
                    && let Some(other) = seen.insert(sc, code)
                {
                    panic!("{code:#x} and {other:#x} both map to {sc:#x}");
                }
            }
        }
    }

    #[test]
    fn hotkeys_parse() {
        assert_eq!(
            Hotkey::DEFAULT.parse(),
            Ok(Hotkey {
                modifiers: modifier::CONTROL | modifier::OPTION | modifier::COMMAND,
                key: 0x31
            })
        );
        assert_eq!(
            "Shift + Cmd + K".parse(),
            Ok(Hotkey {
                modifiers: modifier::SHIFT | modifier::COMMAND,
                key: 0x28
            })
        );
        assert_eq!("alt+f12".parse::<Hotkey>().map(|h| h.key), Ok(0x6F));
        assert_eq!("ctrl+2".parse::<Hotkey>().map(|h| h.key), Ok(0x13));
        assert!("space".parse::<Hotkey>().is_err());
        assert!("ctrl+cmd".parse::<Hotkey>().is_err());
        assert!("ctrl+a+b".parse::<Hotkey>().is_err());
        assert!("ctrl+banana".parse::<Hotkey>().is_err());
    }
}
