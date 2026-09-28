//! Window materials: the desktop showing through, blurred, behind the main
//! window and the arrangement panel.
//!
//! - macOS: the system's vibrancy behind both
//! - Windows 11: Mica behind the main window, Acrylic behind the panel
//! - Windows 10: Acrylic behind the panel only. The main window stays
//!   opaque: there is no Mica, and Acrylic lags while a window is dragged
//!
//! A page over a material is told so (`data-material` on its root), and
//! paints a tint instead of an opaque background.

use tauri::utils::config::WindowEffectsConfig;
use tauri::window::{Effect, EffectState};

/// Marks the page as sitting on a material; runs before its own scripts
pub const MARK: &str = "document.documentElement.dataset.material = '1';";

/// Corner radius of the panel's material (macOS; Windows keeps it square)
const PANEL_RADIUS: f64 = 20.0;

/// The main window's material, where the system has one for it
pub fn main_window() -> Option<WindowEffectsConfig> {
    let effect = if cfg!(target_os = "macos") {
        Effect::Sidebar
    } else if windows_11() {
        Effect::Mica
    } else {
        return None;
    };
    Some(WindowEffectsConfig {
        effects: vec![effect],
        ..Default::default()
    })
}

/// The panel's material: each system takes its own from the list. Active
/// even unfocused, or macOS would grey it out while it shows up
pub fn panel() -> WindowEffectsConfig {
    WindowEffectsConfig {
        effects: vec![Effect::HudWindow, Effect::Acrylic],
        state: Some(EffectState::Active),
        radius: Some(PANEL_RADIUS),
        ..Default::default()
    }
}

/// Whether this is Windows 11 (build 22000 on), the first with Mica
#[cfg(windows)]
fn windows_11() -> bool {
    windows_version::OsVersion::current().build >= 22_000
}

/// Whether this is Windows 11: not on this system
#[cfg(not(windows))]
fn windows_11() -> bool {
    false
}
