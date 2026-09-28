//! What the user sets about switching: the hotkeys, key combinations kept on
//! this machine, how the pointer crosses an edge, settings of single edges,
//! where media keys go, and how fast a controlled device's pointer and
//! wheel go.
//!
//! Keys are physical (USB HID usages) and modifiers count on either side,
//! except that a Ctrl+Alt hotkey needs the left Alt with a key AltGr types
//! with (see [`Chord::needs_left_alt`]).

use serde::{Deserialize, Serialize};

use crate::keymap::usage;

/// Modifiers of a key combination, left or right
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Mods {
    /// Control
    pub ctrl: bool,
    /// Alt (Option on macOS)
    pub alt: bool,
    /// Shift
    pub shift: bool,
    /// Meta (Command on macOS, the Windows key on Windows)
    pub meta: bool,
}

impl Mods {
    /// Control and Alt, the default of every hotkey
    pub const CTRL_ALT: Self = Self {
        ctrl: true,
        alt: true,
        shift: false,
        meta: false,
    };

    /// The modifiers among the keys held
    pub fn held(keys: impl IntoIterator<Item = u16>) -> Self {
        let mut mods = Self::default();
        for key in keys {
            match key {
                usage::LEFT_CTRL | usage::RIGHT_CTRL => mods.ctrl = true,
                usage::LEFT_ALT | usage::RIGHT_ALT => mods.alt = true,
                usage::LEFT_SHIFT | usage::RIGHT_SHIFT => mods.shift = true,
                usage::LEFT_META | usage::RIGHT_META => mods.meta = true,
                _ => {}
            }
        }
        mods
    }

    /// Whether there is Control, Alt or Meta: a combination with Shift at
    /// most would take over keys used for typing
    pub fn commanding(&self) -> bool {
        self.ctrl || self.alt || self.meta
    }
}

/// Whether `key` is a modifier
pub fn is_modifier(key: u16) -> bool {
    (usage::LEFT_CTRL..=usage::RIGHT_META).contains(&key)
}

/// Whether `key` is a function key (F1 to F24)
pub fn is_function_key(key: u16) -> bool {
    (usage::F1..=usage::F12).contains(&key) || (usage::F13..=usage::F24).contains(&key)
}

/// A key pressed with modifiers held
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Chord {
    /// The modifiers, exactly
    #[serde(flatten)]
    pub mods: Mods,
    /// The key (HID usage), not a modifier
    pub key: u16,
}

impl Chord {
    /// `key` with `mods`
    pub const fn new(mods: Mods, key: u16) -> Self {
        Self { mods, key }
    }

    /// Whether the chord may be a hotkey: Control, Alt or Meta in it, or a
    /// function key, so that it never takes over typing
    pub fn valid_hotkey(&self) -> bool {
        !is_modifier(self.key) && (self.mods.commanding() || is_function_key(self.key))
    }

    /// Whether only the left Alt counts for `key` with Control and Alt:
    /// Windows reports AltGr as Control + right Alt, and AltGr with keys
    /// that type (digits, letters, arrows too, as before) types characters
    /// on many layouts; Esc and the function keys type nothing
    pub fn needs_left_alt(mods: Mods, key: u16) -> bool {
        mods.ctrl && mods.alt && key != usage::ESCAPE && !is_function_key(key)
    }
}

/// The hotkeys; Scroll Lock always locks too
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Hotkeys {
    /// Come home and pause crossing, or resume
    pub pause: Chord,
    /// Lock the pointer to its device, or unlock
    pub lock: Chord,
    /// With a digit: jump to device n
    pub jump: Mods,
    /// With an arrow: jump to the neighbour in that direction
    pub step: Mods,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            pause: Chord::new(Mods::CTRL_ALT, usage::ESCAPE),
            lock: Chord::new(Mods::CTRL_ALT, usage::KEY_L),
            jump: Mods::CTRL_ALT,
            step: Mods::CTRL_ALT,
        }
    }
}

impl Hotkeys {
    /// Whether every hotkey is valid (see [`Chord::valid_hotkey`]); the
    /// digits and arrows need Control, Alt or Meta with them
    pub fn valid(&self) -> bool {
        self.pause.valid_hotkey()
            && self.lock.valid_hotkey()
            && self.jump.commanding()
            && self.step.commanding()
    }
}

/// How the pointer crosses an edge
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SwitchMode {
    /// As soon as it is pushed past the edge
    #[default]
    Direct,
    /// Only while the switching modifier is held
    Modifier,
    /// Only after being pushed against the edge for the dwell time
    Dwell,
}

/// The modifier to hold in [`SwitchMode::Modifier`]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HoldKey {
    /// Shift
    #[default]
    Shift,
    /// Control
    Ctrl,
    /// Alt (Option on macOS)
    Alt,
}

impl HoldKey {
    /// Whether it is among `mods`
    pub fn held_in(self, mods: Mods) -> bool {
        match self {
            Self::Shift => mods.shift,
            Self::Ctrl => mods.ctrl,
            Self::Alt => mods.alt,
        }
    }
}

/// How the pointer crosses edges unless an edge says otherwise
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Switching {
    /// The mode
    pub mode: SwitchMode,
    /// The modifier of [`SwitchMode::Modifier`]
    pub hold: HoldKey,
    /// The dwell of [`SwitchMode::Dwell`], in milliseconds
    pub dwell_ms: u32,
    /// Width of the zone at each end of a display edge that never crosses,
    /// in logical pixels: aiming for a screen corner must not throw the
    /// pointer to another device
    pub corner_px: u32,
}

/// Default dwell before crossing
pub const DEFAULT_DWELL_MS: u32 = 300;

/// Default corner guard
pub const DEFAULT_CORNER_PX: u32 = 8;

/// Longest dwell accepted
pub const MAX_DWELL_MS: u32 = 2000;

/// Widest corner guard accepted
pub const MAX_CORNER_PX: u32 = 200;

impl Default for Switching {
    fn default() -> Self {
        Self {
            mode: SwitchMode::Direct,
            hold: HoldKey::Shift,
            dwell_ms: DEFAULT_DWELL_MS,
            corner_px: DEFAULT_CORNER_PX,
        }
    }
}

impl Switching {
    /// Whether the numbers are in range
    pub fn valid(&self) -> bool {
        self.dwell_ms <= MAX_DWELL_MS && self.corner_px <= MAX_CORNER_PX
    }
}

/// Settings of the edge between two devices, both ways; `None` follows
/// [`Switching`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EdgeSettings {
    /// The pointer may cross it at all
    pub crossable: bool,
    /// Corner guard, in logical pixels
    pub corner_px: Option<u32>,
    /// How the pointer crosses it
    pub mode: Option<SwitchMode>,
}

impl Default for EdgeSettings {
    fn default() -> Self {
        Self {
            crossable: true,
            corner_px: None,
            mode: None,
        }
    }
}

impl EdgeSettings {
    /// Whether the numbers are in range
    pub fn valid(&self) -> bool {
        self.corner_px.is_none_or(|px| px <= MAX_CORNER_PX)
    }
}

/// Where media and volume keys go while another device is controlled
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MediaKeys {
    /// To the device being controlled
    #[default]
    Remote,
    /// To this machine, as if nothing were controlled
    Local,
}

/// Slowest pointer and scrolling speed, in percent
pub const MIN_SPEED: u32 = 50;

/// Fastest pointer speed, in percent
pub const MAX_POINTER_SPEED: u32 = 200;

/// Fastest scrolling speed, in percent
pub const MAX_SCROLL_SPEED: u32 = 300;

/// Normal speed, in percent
pub const NORMAL_SPEED: u32 = 100;

/// Whether `speed` (percent) is a pointer speed in range
pub fn valid_pointer_speed(speed: u32) -> bool {
    (MIN_SPEED..=MAX_POINTER_SPEED).contains(&speed)
}

/// How scrolling from a controlling device is replayed on this one
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Scrolling {
    /// Speed, in percent
    pub speed: u32,
    /// Both directions turned around (e.g. for a Mac with natural
    /// scrolling driving a PC)
    pub reverse: bool,
}

impl Default for Scrolling {
    fn default() -> Self {
        Self {
            speed: NORMAL_SPEED,
            reverse: false,
        }
    }
}

impl Scrolling {
    /// Whether the speed is in range
    pub fn valid(&self) -> bool {
        (MIN_SPEED..=MAX_SCROLL_SPEED).contains(&self.speed)
    }
}

/// The key of the edge between devices `a` and `b`, the same either way
pub fn edge_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hotkeys need a commanding modifier or a function key
    #[test]
    fn hotkey_validity() {
        assert!(Hotkeys::default().valid());
        let shift = Mods {
            shift: true,
            ..Mods::default()
        };
        assert!(!Chord::new(shift, usage::KEY_L).valid_hotkey());
        assert!(Chord::new(Mods::default(), usage::F1 + 7).valid_hotkey());
        assert!(!Chord::new(Mods::CTRL_ALT, usage::LEFT_SHIFT).valid_hotkey());
        let hotkeys = Hotkeys {
            jump: shift,
            ..Hotkeys::default()
        };
        assert!(!hotkeys.valid());
    }

    /// Chords read and write as flat JSON, and missing fields default
    #[test]
    fn chords_on_disk() {
        let chord = Chord::new(Mods::CTRL_ALT, usage::ESCAPE);
        let json = serde_json::to_value(chord).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "ctrl": true, "alt": true, "shift": false, "meta": false, "key": 0x29 })
        );
        let read: Chord =
            serde_json::from_value(serde_json::json!({ "meta": true, "key": 44 })).unwrap();
        assert_eq!(
            read,
            Chord::new(
                Mods {
                    meta: true,
                    ..Mods::default()
                },
                44
            )
        );
        let partial: Hotkeys = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(partial, Hotkeys::default());
    }

    /// An edge's key does not depend on the direction
    #[test]
    fn edge_keys() {
        assert_eq!(edge_key("b", "a"), edge_key("a", "b"));
    }
}
