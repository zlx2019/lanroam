//! Window materials: the desktop behind the main window and the
//! arrangement panel shows through, blurred, under a tint in the page's own
//! colors, as opaque as the user likes (Settings → Appearance). The way
//! terminals do it: the tint is ours, so the look follows the app's theme
//! whatever the system's (a system material would not).
//!
//! - macOS: a blur radius on the window, the private CoreGraphics call
//!   iTerm2, kitty, WezTerm and Alacritty use
//! - Windows 10: DWM's blur behind the window
//! - Windows 11: Acrylic, DWM's own backdrop (blur behind lags there while
//!   a window is dragged)
//!
//! A page over a material is told so (`data-material` on its root), and
//! paints its background translucent.

use tauri::WebviewWindow;
use tauri::utils::config::WindowEffectsConfig;
use tauri::window::Effect;

/// Marks the page as sitting on a material; runs before its own scripts
pub const MARK: &str = "document.documentElement.dataset.material = '1';";

/// How far the desktop behind a window is blurred (macOS), in points
#[cfg(target_os = "macos")]
const BLUR_RADIUS: i64 = 60;

/// The effect Tauri applies when the window is built: the blur on Windows.
/// On macOS it has nothing to apply; the blur is set once the window shows
/// ([`blur_behind`])
pub fn effects() -> WindowEffectsConfig {
    let effect = if windows_11() {
        Effect::Acrylic
    } else {
        Effect::Blur
    };
    WindowEffectsConfig {
        effects: vec![effect],
        ..Default::default()
    }
}

/// Blur what is behind `window` (macOS; elsewhere its effect does). Only a
/// window that has been on screen has a number to set it on: call it after
/// showing the window
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // AppKit and a private CoreGraphics call
pub fn blur_behind(window: &WebviewWindow) {
    use objc2_app_kit::NSWindow;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSMainConnectionID() -> u32;
        fn CGSSetWindowBackgroundBlurRadius(connection: u32, window: isize, radius: i64) -> i32;
    }

    let target = window.clone();
    // AppKit wants the main thread
    let queued = window.run_on_main_thread(move || {
        let Ok(pointer) = target.ns_window() else {
            return;
        };
        // SAFETY: Tauri hands out the window's live NSWindow, and this runs
        // on the main thread
        let ns_window = unsafe { &*pointer.cast::<NSWindow>() };
        let number = ns_window.windowNumber();
        if number <= 0 {
            return;
        }
        // SAFETY: plain C calls about this process's own window, which is
        // on screen (it has a number)
        unsafe {
            CGSSetWindowBackgroundBlurRadius(CGSMainConnectionID(), number, BLUR_RADIUS);
        }
    });
    if let Err(e) = queued {
        tracing::warn!("cannot blur behind a window: {e}");
    }
}

/// Blur what is behind `window`: its effect does it on this system
#[cfg(not(target_os = "macos"))]
pub fn blur_behind(_window: &WebviewWindow) {}

/// Whether this is Windows 11 (build 22000 on), whose Acrylic does not lag
#[cfg(windows)]
fn windows_11() -> bool {
    windows_version::OsVersion::current().build >= 22_000
}

/// Whether this is Windows 11: not on this system
#[cfg(not(windows))]
fn windows_11() -> bool {
    false
}
