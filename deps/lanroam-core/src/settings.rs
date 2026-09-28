//! This device's input settings, in `input.json` in the data directory:
//! the hotkeys, the key combinations kept on this machine, how the pointer
//! crosses edges, where media keys go, and how scrolling from a controlling
//! device is replayed. They stay on the device; the group only shares the
//! settings of single edges and each device's pointer speed (in its
//! document).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lanroam_input::config::{Chord, Hotkeys, MediaKeys, Scrolling, Switching, is_modifier};
use serde::{Deserialize, Serialize};

/// File name in the data directory
const SETTINGS_FILE: &str = "input.json";

/// Most key combinations kept local
pub const MAX_KEEP_LOCAL: usize = 32;

/// The input settings
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct InputSettings {
    /// The hotkeys
    pub hotkeys: Hotkeys,
    /// Key combinations that stay on this machine while another device is
    /// controlled
    pub keep_local: Vec<Chord>,
    /// How the pointer crosses edges
    pub switching: Switching,
    /// Where media and volume keys go while another device is controlled
    pub media_keys: MediaKeys,
    /// How scrolling from a controlling device is replayed here
    pub scrolling: Scrolling,
}

impl InputSettings {
    /// Whether the settings can be used: valid hotkeys, numbers in range,
    /// and combinations kept local that end with a key that is not a
    /// modifier
    pub fn valid(&self) -> bool {
        self.hotkeys.valid()
            && self.switching.valid()
            && self.scrolling.valid()
            && self.keep_local.len() <= MAX_KEEP_LOCAL
            && self.keep_local.iter().all(|chord| !is_modifier(chord.key))
    }

    /// Read the settings in `dir`; defaults when there are none, or they
    /// cannot be read or used (logged)
    pub fn load(dir: &Path) -> Self {
        let bytes = match fs::read(path(dir)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!("cannot read the input settings, using the defaults: {e}");
                return Self::default();
            }
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(settings) if settings.valid() => settings,
            Ok(_) => {
                tracing::warn!("the input settings are out of range, using the defaults");
                Self::default()
            }
            Err(e) => {
                tracing::warn!("cannot parse the input settings, using the defaults: {e}");
                Self::default()
            }
        }
    }

    /// Write the settings to `dir` (temp file + rename, so a crash never
    /// leaves a torn file)
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        let target = path(dir);
        let tmp = target.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&tmp, &target)
    }
}

/// Where the settings live
fn path(dir: &Path) -> PathBuf {
    dir.join(SETTINGS_FILE)
}

#[cfg(test)]
mod tests {
    use lanroam_input::config::{Mods, SwitchMode};
    use lanroam_input::keymap::usage;

    use super::*;
    use crate::test_util::TempDir;

    /// Saved settings read back; missing or broken files give the defaults
    #[test]
    fn round_trip() {
        let dir = TempDir::new();
        assert_eq!(InputSettings::load(&dir.0), InputSettings::default());
        let settings = InputSettings {
            keep_local: vec![Chord::new(
                Mods {
                    meta: true,
                    ..Mods::default()
                },
                0x2C,
            )],
            switching: Switching {
                mode: SwitchMode::Dwell,
                ..Switching::default()
            },
            media_keys: MediaKeys::Local,
            scrolling: Scrolling {
                speed: 150,
                reverse: true,
            },
            ..InputSettings::default()
        };
        settings.save(&dir.0).unwrap();
        assert_eq!(InputSettings::load(&dir.0), settings);
        fs::write(path(&dir.0), b"{ not json").unwrap();
        assert_eq!(InputSettings::load(&dir.0), InputSettings::default());
    }

    /// A combination kept local needs a key besides its modifiers
    #[test]
    fn validity() {
        assert!(InputSettings::default().valid());
        let settings = InputSettings {
            keep_local: vec![Chord::new(Mods::CTRL_ALT, usage::LEFT_SHIFT)],
            ..InputSettings::default()
        };
        assert!(!settings.valid());
        let settings = InputSettings {
            scrolling: Scrolling {
                speed: 10,
                reverse: false,
            },
            ..InputSettings::default()
        };
        assert!(!settings.valid());
    }
}
