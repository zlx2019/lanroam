//! The app's own preferences (language, theme, opacity, on-screen indicators), in
//! `app.json` next to the engine's files. Starting at login is not stored here: the OS login item
//! is the source of truth.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name in the data directory
const SETTINGS_FILE: &str = "app.json";

/// Closing the main window hides it in the tray (menu bar)
pub const CLOSE_TO_TRAY: &str = "tray";

/// Closing the main window quits Lanroam
pub const CLOSE_TO_QUIT: &str = "quit";

/// Lowest opacity of the windows' tint over the blurred desktop (percent)
pub const MIN_OPACITY: u8 = 40;

/// Default opacity of the tint (percent)
const DEFAULT_OPACITY: u8 = 80;

/// Highest opacity of the tint: opaque (percent)
pub const MAX_OPACITY: u8 = 100;

/// The app's preferences
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// `system`, `zh` or `en`
    pub language: String,
    /// `system`, `dark` or `light`
    pub theme: String,
    /// Light up the edge the pointer comes in by
    pub edge_glow: bool,
    /// Say pauses, locks, jumps and lost devices mid-screen
    pub hints: bool,
    /// Dim this device's screens while it controls another
    pub dim: bool,
    /// The first close of the window already said that Lanroam keeps
    /// running
    pub close_hinted: bool,
    /// Closing the main window: `tray` hides it, `quit` quits
    pub close_window: String,
    /// How opaque the windows' tint is over the blurred desktop, in percent
    pub opacity: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "system".into(),
            theme: "system".into(),
            edge_glow: true,
            hints: true,
            dim: false,
            close_hinted: false,
            close_window: CLOSE_TO_TRAY.into(),
            opacity: DEFAULT_OPACITY,
        }
    }
}

impl Settings {
    /// Whether closing the main window quits
    pub fn close_quits(&self) -> bool {
        self.close_window == CLOSE_TO_QUIT
    }

    /// Read the preferences; defaults when missing or unreadable
    pub fn load(dir: &Path) -> Self {
        fs::read(path(dir))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Write the preferences (temp file + rename, so a crash never leaves a
    /// torn file)
    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(dir)?;
        let target = path(dir);
        let tmp = target.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&tmp, &target)?;
        Ok(())
    }
}

/// Where the preferences live
fn path(dir: &Path) -> PathBuf {
    dir.join(SETTINGS_FILE)
}
