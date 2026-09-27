//! On-screen overlays: one transparent window per display, above everything,
//! letting clicks through and never taking focus. They show this device's
//! number when the group identifies its screens (and, later, the on-screen
//! hints).
//!
//! The windows are created on first use and then only shown and hidden. A
//! freshly created page may miss the event that asked for it, so it also
//! asks for the current overlay once loaded ([`current`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager, Monitor, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::bridge::emit;
use crate::state::{AppState, lock};

/// Labels of the overlay windows: this prefix and the display's index
pub const OVERLAY_PREFIX: &str = "overlay-";

/// Frontend event carrying what the overlays show; payload: `OverlayDto`
pub const OVERLAY_EVENT: &str = "overlay";

/// How long a device's number stays on its screens
pub const IDENTIFY_TIME: Duration = Duration::from_millis(2600);

/// Counts overlays shown, so that the timer of an older one does not hide a
/// newer one
static SHOWN: AtomicU64 = AtomicU64::new(0);

/// What the overlays show
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum OverlayDto {
    /// This device's number in the layout, and its name
    #[serde(rename_all = "camelCase")]
    Identify {
        /// Number of the Ctrl+Alt+n hotkey; `None` until placed
        number: Option<usize>,
        /// Device name
        name: String,
    },
}

/// Show `what` on every display for `time`
pub fn show(app: &AppHandle, what: OverlayDto, time: Duration) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    *lock(&state.overlay) = Some(what.clone());
    let shown = SHOWN.fetch_add(1, Ordering::SeqCst) + 1;
    let monitors = monitors(app);
    for (index, monitor) in monitors.iter().enumerate() {
        match window(app, index) {
            Ok(window) => cover(&window, monitor),
            Err(e) => tracing::warn!("cannot create the overlay for display {index}: {e}"),
        }
    }
    emit(app, OVERLAY_EVENT, what);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(time).await;
        if SHOWN.load(Ordering::SeqCst) == shown {
            hide(&app);
        }
    });
}

/// What the overlays show right now, for a page that just loaded
pub fn current(app: &AppHandle) -> Option<OverlayDto> {
    let state = app.try_state::<AppState>()?;
    lock(&state.overlay).clone()
}

/// Hide every overlay
fn hide(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        *lock(&state.overlay) = None;
    }
    for (label, window) in app.webview_windows() {
        if label.starts_with(OVERLAY_PREFIX) {
            let _ = window.hide();
        }
    }
}

/// This device's displays
fn monitors(app: &AppHandle) -> Vec<Monitor> {
    match app.available_monitors() {
        Ok(monitors) => monitors,
        Err(e) => {
            tracing::warn!("cannot list the displays: {e}");
            Vec::new()
        }
    }
}

/// The overlay window of display `index`, created on first use
fn window(app: &AppHandle, index: usize) -> tauri::Result<WebviewWindow> {
    let label = format!("{OVERLAY_PREFIX}{index}");
    if let Some(window) = app.get_webview_window(&label) {
        return Ok(window);
    }
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
        .title("Lanroam")
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .resizable(false)
        // Never takes focus, not even when shown (Windows activates a shown
        // window unless it starts unfocused)
        .focusable(false)
        .focused(false)
        .visible(false)
        .build()?;
    window.set_ignore_cursor_events(true)?;
    Ok(window)
}

/// Move `window` over the whole of `monitor` and show it
///
/// macOS places windows in points across displays, so logical units there;
/// Windows in physical pixels of the virtual desktop, which a logical size
/// would get wrong between displays of different scales.
fn cover(window: &WebviewWindow, monitor: &Monitor) {
    #[cfg(target_os = "macos")]
    let placed = {
        let scale = monitor.scale_factor();
        window
            .set_position(monitor.position().to_logical::<f64>(scale))
            .and_then(|()| window.set_size(monitor.size().to_logical::<f64>(scale)))
    };
    #[cfg(not(target_os = "macos"))]
    let placed = window
        .set_position(*monitor.position())
        .and_then(|()| window.set_size(*monitor.size()));
    if let Err(e) = placed.and_then(|()| window.show()) {
        tracing::warn!("cannot show an overlay: {e}");
    }
}
