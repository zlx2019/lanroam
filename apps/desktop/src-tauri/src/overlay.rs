//! On-screen overlays: one transparent window per display, above everything
//! (full-screen apps included), letting clicks through and never taking
//! focus.
//!
//! What they show is a scene per display, made of parts that come and go
//! on their own: this device's number (every display), a hint and a lit
//! edge (one display each, for a moment) and a dimmed screen (every
//! display, until turned off). A window is shown while its scene has
//! anything in it.
//!
//! The windows are created on first use and then only shown and hidden. A
//! freshly created page may miss the event that asked for it, so it also
//! asks for its scene once loaded ([`scene_of`]).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lanroam_core::lanroam_input::Point;
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, Monitor, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

use crate::state::{AppState, lock};

/// Labels of the overlay windows: this prefix and the display's index
pub const OVERLAY_PREFIX: &str = "overlay-";

/// Frontend event carrying one display's scene; payload: `SceneDto`
pub const SCENE_EVENT: &str = "overlay-scene";

/// How long a device's number stays on its screens
const IDENTIFY_TIME: Duration = Duration::from_millis(2600);

/// How long a hint stays (its fade-out included)
const HINT_TIME: Duration = Duration::from_millis(2400);

/// How long an edge stays lit (its fade-out included)
const GLOW_TIME: Duration = Duration::from_millis(1000);

/// How far from a display's edge a point still lies on it: a crossing
/// lands a pixel or two inside
const EDGE_REACH: f64 = 3.0;

/// Numbers the parts shown, so that the timer of an older part does not
/// remove a newer one, and the page restarts an animation for a new one
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One change of the windows at a time: two could create the same window,
/// or leave an older scene on screen
static APPLYING: Mutex<()> = Mutex::new(());

/// This device's number in the layout, and its name
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentifyDto {
    /// Changes each time it is shown
    pub id: u64,
    /// Number of the Ctrl+Alt+n hotkey; `None` until placed
    pub number: Option<usize>,
    /// Device name
    pub name: String,
}

/// What a hint says (the page words it)
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Hint {
    /// Crossing edges was paused or resumed; `platform` names the keys
    Paused {
        /// Paused
        on: bool,
        /// Whose hotkeys (`macos`, `windows`)
        platform: String,
    },
    /// The pointer was locked to `name`, or unlocked
    Locked {
        /// Locked
        on: bool,
        /// The device it stays on
        name: String,
        /// Whose hotkeys unlock it
        platform: String,
    },
    /// The pointer jumped here
    Jump {
        /// This device's number
        number: Option<usize>,
        /// This device's name
        name: String,
    },
    /// The device being controlled stopped answering
    Unresponsive {
        /// The device
        name: String,
    },
    /// The link to the device being controlled dropped
    Lost {
        /// The device
        name: String,
    },
    /// The device being controlled let go (a `released` reason)
    LetGo {
        /// The device
        name: String,
        /// Why
        reason: String,
    },
    /// Closing the window does not quit
    StillRunning {
        /// Menu bar (`macos`) or tray
        platform: String,
    },
}

/// A hint on one display
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HintDto {
    /// Changes each time it is shown
    pub id: u64,
    /// What it says
    pub hint: Hint,
}

/// A side of a display
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Edge {
    /// Left
    Left,
    /// Right
    Right,
    /// Top
    Top,
    /// Bottom
    Bottom,
}

/// A lit edge
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlowDto {
    /// Changes each time it is shown
    pub id: u64,
    /// Which one
    pub edge: Edge,
}

/// What one display's overlay shows
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneDto {
    /// This device's number
    pub identify: Option<IdentifyDto>,
    /// A hint near the bottom
    pub hint: Option<HintDto>,
    /// A lit edge
    pub glow: Option<GlowDto>,
    /// Darkened
    pub dim: bool,
}

impl SceneDto {
    /// Nothing to show
    fn is_empty(&self) -> bool {
        self.identify.is_none() && self.hint.is_none() && self.glow.is_none() && !self.dim
    }
}

/// Every part shown right now, and where
#[derive(Debug, Clone, Default)]
pub struct Overlays {
    /// On every display
    identify: Option<IdentifyDto>,
    /// On one display, by index
    hint: Option<(usize, HintDto)>,
    /// On one display, by index
    glow: Option<(usize, GlowDto)>,
    /// Every display
    dim: bool,
}

impl Overlays {
    /// The scene of display `index`
    fn scene(&self, index: usize) -> SceneDto {
        SceneDto {
            identify: self.identify.clone(),
            hint: on_display(&self.hint, index),
            glow: on_display(&self.glow, index),
            dim: self.dim,
        }
    }
}

/// A part shown on one display, if that is display `index`
fn on_display<T: Clone>(part: &Option<(usize, T)>, index: usize) -> Option<T> {
    part.as_ref()
        .filter(|(i, _)| *i == index)
        .map(|(_, p)| p.clone())
}

/// Show this device's number on every display for a moment
pub fn identify(app: &AppHandle, number: Option<usize>, name: String) {
    let id = next_id();
    change(app, |o| {
        o.identify = Some(IdentifyDto { id, number, name });
        true
    });
    later(app, IDENTIFY_TIME, move |o| {
        o.identify.take_if(|p| p.id == id).is_some()
    });
}

/// Show `hint` for a moment on display `index`, or where the pointer is
pub fn hint(app: &AppHandle, hint: Hint, index: Option<usize>) {
    let index = index.unwrap_or_else(|| pointer_display(app));
    let id = next_id();
    change(app, |o| {
        o.hint = Some((index, HintDto { id, hint }));
        true
    });
    later(app, HINT_TIME, move |o| {
        o.hint.take_if(|(_, p)| p.id == id).is_some()
    });
}

/// Light up `edge` of display `index` for a moment
pub fn glow(app: &AppHandle, index: usize, edge: Edge) {
    let id = next_id();
    change(app, |o| {
        o.glow = Some((index, GlowDto { id, edge }));
        true
    });
    later(app, GLOW_TIME, move |o| {
        o.glow.take_if(|(_, p)| p.id == id).is_some()
    });
}

/// Dim every display, or stop
pub fn dim(app: &AppHandle, on: bool) {
    change(app, |o| std::mem::replace(&mut o.dim, on) != on);
}

/// The scene of the overlay window `label`, for its page that just loaded
pub fn scene_of(app: &AppHandle, label: &str) -> SceneDto {
    let index = label
        .strip_prefix(OVERLAY_PREFIX)
        .and_then(|i| i.parse().ok());
    match (app.try_state::<AppState>(), index) {
        (Some(state), Some(index)) => lock(&state.overlays).scene(index),
        _ => SceneDto::default(),
    }
}

/// The display holding `at` (in the input layer's device coordinates),
/// and the edge `at` lies on, if any
pub fn locate(app: &AppHandle, at: Point) -> Option<(usize, Option<Edge>)> {
    let (x, y) = (f64::from(at.x), f64::from(at.y));
    monitors(app)
        .iter()
        .enumerate()
        .find_map(|(index, monitor)| {
            let (left, top, width, height) = device_rect(monitor);
            let (right, bottom) = (left + width, top + height);
            if x < left || x >= right || y < top || y >= bottom {
                return None;
            }
            let edge = [
                (x - left, Edge::Left),
                (right - x, Edge::Right),
                (y - top, Edge::Top),
                (bottom - y, Edge::Bottom),
            ]
            .into_iter()
            .filter(|(distance, _)| *distance <= EDGE_REACH)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, edge)| edge);
            Some((index, edge))
        })
}

/// A new part number
fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Change the parts shown (`update` says whether anything changed) and
/// bring the windows in line
///
/// Both under one lock, so that the windows end up showing the latest
/// change whatever order two changes race in.
fn change(app: &AppHandle, update: impl FnOnce(&mut Overlays) -> bool) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let _one_at_a_time = lock(&APPLYING);
    let overlays = {
        let mut overlays = lock(&state.overlays);
        if !update(&mut overlays) {
            return;
        }
        overlays.clone()
    };
    apply(app, &overlays);
}

/// After `time`, remove a part if `expire` finds it still the one shown
fn later(
    app: &AppHandle,
    time: Duration,
    expire: impl FnOnce(&mut Overlays) -> bool + Send + 'static,
) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(time).await;
        change(&app, expire);
    });
}

/// Bring every window in line with `overlays`: send each display its scene,
/// show the windows with something to show, hide the others
fn apply(app: &AppHandle, overlays: &Overlays) {
    let monitors = monitors(app);
    for (index, monitor) in monitors.iter().enumerate() {
        let scene = overlays.scene(index);
        let label = label(index);
        if scene.is_empty() {
            if let Some(window) = app.get_webview_window(&label) {
                send(app, &label, &scene);
                let _ = window.hide();
            }
            continue;
        }
        match window(app, index) {
            Ok(window) => {
                send(app, &label, &scene);
                cover(&window, monitor);
            }
            Err(e) => tracing::warn!("cannot create the overlay for display {index}: {e}"),
        }
    }
    // Displays unplugged since
    for (label, window) in app.webview_windows() {
        let index = label
            .strip_prefix(OVERLAY_PREFIX)
            .and_then(|i| i.parse::<usize>().ok());
        if index.is_some_and(|i| i >= monitors.len()) {
            let _ = window.hide();
        }
    }
}

/// Send one window its scene
fn send(app: &AppHandle, label: &str, scene: &SceneDto) {
    if let Err(e) = app.emit_to(label, SCENE_EVENT, scene) {
        tracing::warn!("cannot update the overlay {label}: {e}");
    }
}

/// Label of the overlay window of display `index`
fn label(index: usize) -> String {
    format!("{OVERLAY_PREFIX}{index}")
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

/// A display in the input layer's device coordinates: (left, top, width,
/// height). Those are points on macOS, physical pixels on Windows
fn device_rect(monitor: &Monitor) -> (f64, f64, f64, f64) {
    #[cfg(target_os = "macos")]
    {
        let scale = monitor.scale_factor();
        let at = monitor.position().to_logical::<f64>(scale);
        let size = monitor.size().to_logical::<f64>(scale);
        (at.x, at.y, size.width, size.height)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let at = monitor.position();
        let size = monitor.size();
        (
            f64::from(at.x),
            f64::from(at.y),
            f64::from(size.width),
            f64::from(size.height),
        )
    }
}

/// The display the pointer is on (the first one when that is unknown)
pub fn pointer_monitor(app: &AppHandle) -> Option<Monitor> {
    monitors(app).into_iter().nth(pointer_display(app))
}

/// Index of the display the pointer is on (the first one when that is
/// unknown)
fn pointer_display(app: &AppHandle) -> usize {
    let Ok(position) = app.cursor_position() else {
        return 0;
    };
    // Physical pixels on Windows; on macOS, points scaled by the primary
    // display's factor
    #[cfg(target_os = "macos")]
    let position = {
        let scale = app
            .primary_monitor()
            .ok()
            .flatten()
            .map_or(1.0, |m| m.scale_factor());
        position.to_logical::<f64>(scale)
    };
    // Truncation is fine: a display is found by the pixel the point is in
    #[allow(clippy::cast_possible_truncation)]
    let at = Point::new(position.x.floor() as i32, position.y.floor() as i32);
    locate(app, at).map_or(0, |(index, _)| index)
}

/// The overlay window of display `index`, created on first use
fn window(app: &AppHandle, index: usize) -> tauri::Result<WebviewWindow> {
    let label = label(index);
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
    #[cfg(target_os = "macos")]
    above_everything(&window);
    Ok(window)
}

/// Keep `window` above full-screen apps, the menu bar and the Dock, on
/// every space: always-on-top alone floats it over ordinary windows of the
/// current space only
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // AppKit: the NSWindow behind a Tauri window
fn above_everything(window: &WebviewWindow) {
    use objc2_app_kit::{NSStatusWindowLevel, NSWindow, NSWindowCollectionBehavior};

    let target = window.clone();
    // AppKit wants the main thread
    let queued = window.run_on_main_thread(move || {
        let Ok(pointer) = target.ns_window() else {
            return;
        };
        // SAFETY: Tauri hands out the window's live NSWindow, and this runs
        // on the main thread
        let ns_window = unsafe { &*pointer.cast::<NSWindow>() };
        ns_window.setLevel(NSStatusWindowLevel);
        ns_window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
    });
    if let Err(e) = queued {
        tracing::warn!("cannot raise an overlay above full-screen apps: {e}");
    }
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
