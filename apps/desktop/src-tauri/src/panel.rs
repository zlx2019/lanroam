//! The arrangement panel: a frameless window floating mid-screen over a
//! material, where the group's screens are dragged into place.
//!
//! It opens from the main window or the tray, on the display the pointer is
//! on, and goes away with Esc, "Done" or a click anywhere else (it hides as
//! soon as it loses focus). Opening it has every other member show its
//! number on its screens, to match the tiles with the real screens; this
//! device's own numbers stay off, they would cover the panel.

use tauri::{AppHandle, Manager, Monitor, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::material;
use crate::overlay;
use crate::state::AppState;

/// Label of the panel's window
pub const PANEL_WINDOW: &str = "arrange";

/// The panel's size, in logical pixels
const WIDTH: f64 = 820.0;
const HEIGHT: f64 = 520.0;

/// Show the panel mid-screen where the pointer is, focused, and have the
/// other members identify their screens
///
/// Creating a window from the main thread's event handlers or a blocking
/// command can deadlock on Windows: call this from an async context.
pub fn show(app: &AppHandle) {
    let window = match window(app) {
        Ok(window) => window,
        Err(e) => {
            tracing::warn!("cannot create the arrangement panel: {e}");
            return;
        }
    };
    if let Some(monitor) = overlay::pointer_monitor(app) {
        center(&window, &monitor);
    }
    let _ = window.show();
    let _ = window.set_focus();
    if let Some(state) = app.try_state::<AppState>()
        && let Err(e) = state.engine.identify()
    {
        tracing::debug!("cannot identify the screens: {e}");
    }
}

/// Whether the panel is on screen
pub fn is_open(app: &AppHandle) -> bool {
    app.get_webview_window(PANEL_WINDOW)
        .is_some_and(|window| window.is_visible().unwrap_or(false))
}

/// The panel's window, created on first use
fn window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(window) = app.get_webview_window(PANEL_WINDOW) {
        return Ok(window);
    }
    WebviewWindowBuilder::new(app, PANEL_WINDOW, WebviewUrl::App("index.html".into()))
        .title("Lanroam")
        .inner_size(WIDTH, HEIGHT)
        .decorations(false)
        .transparent(true)
        .effects(material::panel())
        .initialization_script(material::MARK)
        .always_on_top(true)
        // Shown on the current space, not back where it was first shown
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .visible(false)
        .build()
}

/// Move `window` to the middle of `monitor`
///
/// As the overlays: macOS places windows in points across displays, so
/// logical units there; Windows in physical pixels of the virtual desktop,
/// sized by the target display's scale.
fn center(window: &WebviewWindow, monitor: &Monitor) {
    let scale = monitor.scale_factor();
    #[cfg(target_os = "macos")]
    let placed = {
        let at = monitor.position().to_logical::<f64>(scale);
        let size = monitor.size().to_logical::<f64>(scale);
        window
            .set_size(tauri::LogicalSize::new(WIDTH, HEIGHT))
            .and_then(|()| {
                window.set_position(tauri::LogicalPosition::new(
                    at.x + (size.width - WIDTH) / 2.0,
                    at.y + (size.height - HEIGHT) / 2.0,
                ))
            })
    };
    #[cfg(not(target_os = "macos"))]
    let placed = {
        // Whole pixels are all a window can take
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (width, height) = ((WIDTH * scale) as u32, (HEIGHT * scale) as u32);
        let at = monitor.position();
        let size = monitor.size();
        #[allow(clippy::cast_possible_wrap)]
        let (x, y) = (
            at.x + (size.width.saturating_sub(width) / 2) as i32,
            at.y + (size.height.saturating_sub(height) / 2) as i32,
        );
        window
            .set_position(tauri::PhysicalPosition::new(x, y))
            .and_then(|()| window.set_size(tauri::PhysicalSize::new(width, height)))
    };
    if let Err(e) = placed {
        tracing::warn!("cannot place the arrangement panel: {e}");
    }
}
