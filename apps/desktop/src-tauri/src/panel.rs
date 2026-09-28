//! The arrangement panel: not a window to look at but a sheet over the
//! whole display the pointer is on, the desktop blurred under it (the
//! window material), with the group's screens floating in the middle to be
//! dragged into place.
//!
//! It opens from the main window or the tray, and goes away with Esc,
//! "Done", a click on the empty sheet, or anywhere else (it hides as soon
//! as it loses focus). Opening it has every other member show its number on
//! its screens, to match the tiles with the real screens; this device's own
//! numbers stay off, the sheet covers them.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::material;
use crate::overlay;
use crate::state::AppState;

/// Label of the panel's window
pub const PANEL_WINDOW: &str = "arrange";

/// Show the panel over the display the pointer is on, focused, and have
/// the other members identify their screens
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
    let Some(monitor) = overlay::pointer_monitor(app) else {
        tracing::warn!("no display to show the arrangement panel on");
        return;
    };
    overlay::cover(&window, &monitor);
    let _ = window.set_focus();
    material::blur_behind(&window);
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

/// The panel's window, created on first use: borderless and see-through,
/// above the menu bar and the Dock like the overlays
fn window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(window) = app.get_webview_window(PANEL_WINDOW) {
        return Ok(window);
    }
    let window = WebviewWindowBuilder::new(app, PANEL_WINDOW, WebviewUrl::App("index.html".into()))
        .title("Lanroam")
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .effects(material::effects())
        .initialization_script(material::MARK)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .visible(false)
        .build()?;
    #[cfg(target_os = "macos")]
    overlay::above_everything(&window);
    Ok(window)
}
