//! Physical keys: USB HID usages (keyboard page 0x07) and the platform codes
//! of each.
//!
//! Keys travel between machines as HID usages, i.e. by position on the
//! keyboard, never as characters: the target interprets them with its own
//! layout and input method. Names follow the W3C UI Events `code` values.
//!
//! - macOS: virtual key codes (`kVK_*`, Carbon `Events.h`)
//! - Windows: set-1 scan codes; extended keys carry the `0xE0` prefix in the
//!   high byte (`0xE01D` is Right Control)
//!
//! Media keys that the keyboard page lacks (play / pause, next and previous
//! track) come from the consumer page (0x0C) and travel as `0x0C00 | usage`,
//! out of the keyboard page's range. On macOS every media key, the volume
//! keys included, arrives and is posted as a system-defined event rather
//! than a key code (see [`is_media`]).

/// HID usages the engine refers to by name
pub mod usage {
    /// L
    pub const KEY_L: u16 = 0x0F;
    /// 1 on the main block (2 to 9 follow it)
    pub const DIGIT_1: u16 = 0x1E;
    /// 9 on the main block
    pub const DIGIT_9: u16 = 0x26;
    /// Escape
    pub const ESCAPE: u16 = 0x29;
    /// F1 (F2 to F12 follow it)
    pub const F1: u16 = 0x3A;
    /// F12
    pub const F12: u16 = 0x45;
    /// F13 (F14 to F24 follow it)
    pub const F13: u16 = 0x68;
    /// F24
    pub const F24: u16 = 0x73;
    /// Scroll Lock
    pub const SCROLL_LOCK: u16 = 0x47;
    /// Right arrow
    pub const ARROW_RIGHT: u16 = 0x4F;
    /// Left arrow
    pub const ARROW_LEFT: u16 = 0x50;
    /// Down arrow
    pub const ARROW_DOWN: u16 = 0x51;
    /// Up arrow
    pub const ARROW_UP: u16 = 0x52;
    /// Caps Lock
    pub const CAPS_LOCK: u16 = 0x39;
    /// Pause / Break
    pub const PAUSE: u16 = 0x48;
    /// Mute
    pub const VOLUME_MUTE: u16 = 0x7F;
    /// Volume up
    pub const VOLUME_UP: u16 = 0x80;
    /// Volume down
    pub const VOLUME_DOWN: u16 = 0x81;
    /// Next track (consumer page 0xB5)
    pub const MEDIA_NEXT: u16 = 0x0CB5;
    /// Previous track (consumer page 0xB6)
    pub const MEDIA_PREVIOUS: u16 = 0x0CB6;
    /// Play / pause (consumer page 0xCD)
    pub const MEDIA_PLAY_PAUSE: u16 = 0x0CCD;
    /// Left Control
    pub const LEFT_CTRL: u16 = 0xE0;
    /// Left Shift
    pub const LEFT_SHIFT: u16 = 0xE1;
    /// Left Alt (Option on macOS)
    pub const LEFT_ALT: u16 = 0xE2;
    /// Left Meta (Command on macOS, Windows key on Windows)
    pub const LEFT_META: u16 = 0xE3;
    /// Right Control
    pub const RIGHT_CTRL: u16 = 0xE4;
    /// Right Shift
    pub const RIGHT_SHIFT: u16 = 0xE5;
    /// Right Alt (Option on macOS, AltGr on some layouts)
    pub const RIGHT_ALT: u16 = 0xE6;
    /// Right Meta
    pub const RIGHT_META: u16 = 0xE7;
}

/// No code on this platform
const NO: u16 = u16::MAX;

/// One physical key
struct Key {
    /// USB HID usage
    usage: u16,
    /// W3C `code` name
    name: &'static str,
    /// macOS virtual key code, or [`NO`]
    mac: u16,
    /// Windows scan code (`0xE0xx` when extended), or [`NO`]
    win: u16,
}

/// Build a table row
const fn k(usage: u16, name: &'static str, mac: u16, win: u16) -> Key {
    Key {
        usage,
        name,
        mac,
        win,
    }
}

/// Every key Lanroam forwards
#[rustfmt::skip]
static KEYS: &[Key] = &[
    k(0x04, "KeyA", 0x00, 0x1E),
    k(0x05, "KeyB", 0x0B, 0x30),
    k(0x06, "KeyC", 0x08, 0x2E),
    k(0x07, "KeyD", 0x02, 0x20),
    k(0x08, "KeyE", 0x0E, 0x12),
    k(0x09, "KeyF", 0x03, 0x21),
    k(0x0A, "KeyG", 0x05, 0x22),
    k(0x0B, "KeyH", 0x04, 0x23),
    k(0x0C, "KeyI", 0x22, 0x17),
    k(0x0D, "KeyJ", 0x26, 0x24),
    k(0x0E, "KeyK", 0x28, 0x25),
    k(0x0F, "KeyL", 0x25, 0x26),
    k(0x10, "KeyM", 0x2E, 0x32),
    k(0x11, "KeyN", 0x2D, 0x31),
    k(0x12, "KeyO", 0x1F, 0x18),
    k(0x13, "KeyP", 0x23, 0x19),
    k(0x14, "KeyQ", 0x0C, 0x10),
    k(0x15, "KeyR", 0x0F, 0x13),
    k(0x16, "KeyS", 0x01, 0x1F),
    k(0x17, "KeyT", 0x11, 0x14),
    k(0x18, "KeyU", 0x20, 0x16),
    k(0x19, "KeyV", 0x09, 0x2F),
    k(0x1A, "KeyW", 0x0D, 0x11),
    k(0x1B, "KeyX", 0x07, 0x2D),
    k(0x1C, "KeyY", 0x10, 0x15),
    k(0x1D, "KeyZ", 0x06, 0x2C),
    k(0x1E, "Digit1", 0x12, 0x02),
    k(0x1F, "Digit2", 0x13, 0x03),
    k(0x20, "Digit3", 0x14, 0x04),
    k(0x21, "Digit4", 0x15, 0x05),
    k(0x22, "Digit5", 0x17, 0x06),
    k(0x23, "Digit6", 0x16, 0x07),
    k(0x24, "Digit7", 0x1A, 0x08),
    k(0x25, "Digit8", 0x1C, 0x09),
    k(0x26, "Digit9", 0x19, 0x0A),
    k(0x27, "Digit0", 0x1D, 0x0B),
    k(0x28, "Enter", 0x24, 0x1C),
    k(0x29, "Escape", 0x35, 0x01),
    k(0x2A, "Backspace", 0x33, 0x0E),
    k(0x2B, "Tab", 0x30, 0x0F),
    k(0x2C, "Space", 0x31, 0x39),
    k(0x2D, "Minus", 0x1B, 0x0C),
    k(0x2E, "Equal", 0x18, 0x0D),
    k(0x2F, "BracketLeft", 0x21, 0x1A),
    k(0x30, "BracketRight", 0x1E, 0x1B),
    k(0x31, "Backslash", 0x2A, 0x2B),
    k(0x33, "Semicolon", 0x29, 0x27),
    k(0x34, "Quote", 0x27, 0x28),
    k(0x35, "Backquote", 0x32, 0x29),
    k(0x36, "Comma", 0x2B, 0x33),
    k(0x37, "Period", 0x2F, 0x34),
    k(0x38, "Slash", 0x2C, 0x35),
    k(0x39, "CapsLock", 0x39, 0x3A),
    k(0x3A, "F1", 0x7A, 0x3B),
    k(0x3B, "F2", 0x78, 0x3C),
    k(0x3C, "F3", 0x63, 0x3D),
    k(0x3D, "F4", 0x76, 0x3E),
    k(0x3E, "F5", 0x60, 0x3F),
    k(0x3F, "F6", 0x61, 0x40),
    k(0x40, "F7", 0x62, 0x41),
    k(0x41, "F8", 0x64, 0x42),
    k(0x42, "F9", 0x65, 0x43),
    k(0x43, "F10", 0x6D, 0x44),
    k(0x44, "F11", 0x67, 0x57),
    k(0x45, "F12", 0x6F, 0x58),
    // macOS has no Print Screen / Scroll Lock / Pause; a PC keyboard on a
    // Mac reports them as F13 / F14 / F15
    k(0x46, "PrintScreen", NO, 0xE037),
    k(0x47, "ScrollLock", NO, 0x46),
    // Windows reports Pause as scan code 0x45 without the extended flag
    // (Num Lock is 0xE045); injection sends it as a virtual key instead
    k(0x48, "Pause", NO, 0x45),
    // kVK_Help sits where Insert is on a PC keyboard
    k(0x49, "Insert", 0x72, 0xE052),
    k(0x4A, "Home", 0x73, 0xE047),
    k(0x4B, "PageUp", 0x74, 0xE049),
    k(0x4C, "Delete", 0x75, 0xE053),
    k(0x4D, "End", 0x77, 0xE04F),
    k(0x4E, "PageDown", 0x79, 0xE051),
    k(0x4F, "ArrowRight", 0x7C, 0xE04D),
    k(0x50, "ArrowLeft", 0x7B, 0xE04B),
    k(0x51, "ArrowDown", 0x7D, 0xE050),
    k(0x52, "ArrowUp", 0x7E, 0xE048),
    // Keypad Clear sits where Num Lock is on a PC keypad
    k(0x53, "NumLock", 0x47, 0xE045),
    k(0x54, "NumpadDivide", 0x4B, 0xE035),
    k(0x55, "NumpadMultiply", 0x43, 0x37),
    k(0x56, "NumpadSubtract", 0x4E, 0x4A),
    k(0x57, "NumpadAdd", 0x45, 0x4E),
    k(0x58, "NumpadEnter", 0x4C, 0xE01C),
    k(0x59, "Numpad1", 0x53, 0x4F),
    k(0x5A, "Numpad2", 0x54, 0x50),
    k(0x5B, "Numpad3", 0x55, 0x51),
    k(0x5C, "Numpad4", 0x56, 0x4B),
    k(0x5D, "Numpad5", 0x57, 0x4C),
    k(0x5E, "Numpad6", 0x58, 0x4D),
    k(0x5F, "Numpad7", 0x59, 0x47),
    k(0x60, "Numpad8", 0x5B, 0x48),
    k(0x61, "Numpad9", 0x5C, 0x49),
    k(0x62, "Numpad0", 0x52, 0x52),
    k(0x63, "NumpadDecimal", 0x41, 0x53),
    // The extra key next to Left Shift on ISO keyboards (§ on a Mac)
    k(0x64, "IntlBackslash", 0x0A, 0x56),
    k(0x65, "ContextMenu", 0x6E, 0xE05D),
    k(0x67, "NumpadEqual", 0x51, 0x59),
    k(0x68, "F13", 0x69, 0x64),
    k(0x69, "F14", 0x6B, 0x65),
    k(0x6A, "F15", 0x71, 0x66),
    k(0x6B, "F16", 0x6A, 0x67),
    k(0x6C, "F17", 0x40, 0x68),
    k(0x6D, "F18", 0x4F, 0x69),
    k(0x6E, "F19", 0x50, 0x6A),
    k(0x6F, "F20", 0x5A, 0x6B),
    k(0x70, "F21", NO, 0x6C),
    k(0x71, "F22", NO, 0x6D),
    k(0x72, "F23", NO, 0x6E),
    k(0x73, "F24", NO, 0x76),
    k(0x7F, "AudioVolumeMute", 0x4A, 0xE020),
    k(0x80, "AudioVolumeUp", 0x48, 0xE030),
    k(0x81, "AudioVolumeDown", 0x49, 0xE02E),
    k(0x85, "NumpadComma", 0x5F, 0x7E),
    k(0x87, "IntlRo", 0x5E, 0x73),
    k(0x88, "KanaMode", NO, 0x70),
    k(0x89, "IntlYen", 0x5D, 0x7D),
    k(0x8A, "Convert", NO, 0x79),
    k(0x8B, "NonConvert", NO, 0x7B),
    k(0x90, "Lang1", 0x68, 0x72),
    k(0x91, "Lang2", 0x66, 0x71),
    k(0xE0, "ControlLeft", 0x3B, 0x1D),
    k(0xE1, "ShiftLeft", 0x38, 0x2A),
    k(0xE2, "AltLeft", 0x3A, 0x38),
    k(0xE3, "MetaLeft", 0x37, 0xE05B),
    k(0xE4, "ControlRight", 0x3E, 0xE01D),
    k(0xE5, "ShiftRight", 0x3C, 0x36),
    k(0xE6, "AltRight", 0x3D, 0xE038),
    k(0xE7, "MetaRight", 0x36, 0xE05C),
    // Consumer page; macOS has no key codes for them (system-defined
    // events instead)
    k(0x0CB5, "MediaTrackNext", NO, 0xE019),
    k(0x0CB6, "MediaTrackPrevious", NO, 0xE010),
    k(0x0CCD, "MediaPlayPause", NO, 0xE022),
];

/// The row of a HID usage
fn by_usage(usage: u16) -> Option<&'static Key> {
    KEYS.iter().find(|key| key.usage == usage)
}

/// HID usage of a macOS virtual key code
pub fn usage_from_mac(code: u16) -> Option<u16> {
    KEYS.iter()
        .find(|key| key.mac == code && code != NO)
        .map(|key| key.usage)
}

/// macOS virtual key code of a HID usage
pub fn mac_from_usage(usage: u16) -> Option<u16> {
    by_usage(usage)
        .map(|key| key.mac)
        .filter(|&code| code != NO)
}

/// HID usage of a Windows scan code (`0xE0xx` for extended keys)
pub fn usage_from_win(scan: u16) -> Option<u16> {
    KEYS.iter()
        .find(|key| key.win == scan && scan != NO)
        .map(|key| key.usage)
}

/// Windows scan code of a HID usage (`0xE0xx` for extended keys)
pub fn win_from_usage(usage: u16) -> Option<u16> {
    by_usage(usage)
        .map(|key| key.win)
        .filter(|&scan| scan != NO)
}

/// W3C `code` name of a HID usage, for logs and diagnostics
pub fn name(usage: u16) -> Option<&'static str> {
    by_usage(usage).map(|key| key.name)
}

/// Every key with its W3C `code` name
pub fn names() -> impl Iterator<Item = (u16, &'static str)> {
    KEYS.iter().map(|key| (key.usage, key.name))
}

/// Whether `usage` is a media or volume key
pub fn is_media(usage: u16) -> bool {
    matches!(
        usage,
        usage::VOLUME_MUTE
            | usage::VOLUME_UP
            | usage::VOLUME_DOWN
            | usage::MEDIA_NEXT
            | usage::MEDIA_PREVIOUS
            | usage::MEDIA_PLAY_PAUSE
    )
}

/// The key in the Command / Control position on the other platform: Control
/// and Meta swap places, left and right kept, every other key unchanged
///
/// Applied when one platform's keyboard drives the other, so the shortcut
/// keys stay under the same fingers (Cmd+C on a Mac keyboard copies on
/// Windows, Ctrl+C on a PC keyboard copies on a Mac).
pub fn swap_cmd_ctrl(key: u16) -> u16 {
    match key {
        usage::LEFT_CTRL => usage::LEFT_META,
        usage::LEFT_META => usage::LEFT_CTRL,
        usage::RIGHT_CTRL => usage::RIGHT_META,
        usage::RIGHT_META => usage::RIGHT_CTRL,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// No usage, name or platform code appears twice, or lookups would be
    /// ambiguous
    #[test]
    fn columns_are_unique() {
        let mut usages = HashSet::new();
        let mut names = HashSet::new();
        let mut macs = HashSet::new();
        let mut wins = HashSet::new();
        for key in KEYS {
            assert!(usages.insert(key.usage), "usage {:#x}", key.usage);
            assert!(names.insert(key.name), "name {}", key.name);
            assert!(key.mac == NO || macs.insert(key.mac), "mac {:#x}", key.mac);
            assert!(key.win == NO || wins.insert(key.win), "win {:#x}", key.win);
        }
    }

    /// Every code maps back to the usage it came from
    #[test]
    fn roundtrips() {
        for key in KEYS {
            if let Some(mac) = mac_from_usage(key.usage) {
                assert_eq!(usage_from_mac(mac), Some(key.usage), "{}", key.name);
            }
            if let Some(win) = win_from_usage(key.usage) {
                assert_eq!(usage_from_win(win), Some(key.usage), "{}", key.name);
            }
        }
    }

    /// Spot checks against the platform headers
    #[test]
    fn known_codes() {
        assert_eq!(usage_from_mac(0x00), Some(0x04)); // kVK_ANSI_A
        assert_eq!(usage_from_mac(0x37), Some(usage::LEFT_META)); // kVK_Command
        assert_eq!(usage_from_mac(0x35), Some(usage::ESCAPE));
        assert_eq!(win_from_usage(usage::RIGHT_CTRL), Some(0xE01D));
        assert_eq!(win_from_usage(usage::LEFT_META), Some(0xE05B));
        assert_eq!(usage_from_win(0x1E), Some(0x04));
        assert_eq!(name(usage::CAPS_LOCK), Some("CapsLock"));
        assert_eq!(mac_from_usage(usage::PAUSE), None);
        assert_eq!(usage_from_mac(NO), None);
        assert_eq!(usage_from_win(NO), None);
        assert_eq!(usage_from_win(0xE022), Some(usage::MEDIA_PLAY_PAUSE));
        assert_eq!(usage_from_win(0xE030), Some(usage::VOLUME_UP));
    }

    /// Media keys are told from the others, and all of them have names
    #[test]
    fn media_keys() {
        assert!(is_media(usage::VOLUME_UP) && is_media(usage::MEDIA_PLAY_PAUSE));
        assert!(!is_media(usage::F12) && !is_media(0x0C00));
        let named = names().filter(|(usage, _)| is_media(*usage)).count();
        assert_eq!(named, 6);
    }

    /// Control and Meta trade places, nothing else moves
    #[test]
    fn cmd_ctrl_swap() {
        assert_eq!(swap_cmd_ctrl(usage::LEFT_CTRL), usage::LEFT_META);
        assert_eq!(swap_cmd_ctrl(usage::RIGHT_META), usage::RIGHT_CTRL);
        assert_eq!(swap_cmd_ctrl(usage::LEFT_ALT), usage::LEFT_ALT);
        assert_eq!(swap_cmd_ctrl(0x06), 0x06);
        for key in 0..=u16::from(u8::MAX) {
            assert_eq!(swap_cmd_ctrl(swap_cmd_ctrl(key)), key);
        }
    }
}
