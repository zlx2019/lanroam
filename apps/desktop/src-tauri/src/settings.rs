//! The app's own preferences (language, theme), in `app.json` next to the
//! engine's files. Starting at login is not stored here: the OS login item
//! is the source of truth.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name in the data directory
const SETTINGS_FILE: &str = "app.json";

/// The app's preferences
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// `system`, `zh` or `en`
    pub language: String,
    /// `system`, `dark` or `light`
    pub theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "system".into(),
            theme: "system".into(),
        }
    }
}

impl Settings {
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
