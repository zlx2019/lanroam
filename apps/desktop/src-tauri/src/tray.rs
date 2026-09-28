//! The tray icon and its menu: the state, pause and lock, a jump to any
//! online member, the arrangement panel, the window, quitting.
//!
//! The menu is rebuilt from each snapshot: members come and go, and the
//! texts follow the language setting. The icon's shape follows where input
//! is (two screens, the filled one holding the pointer), so it reads
//! without color in a monochrome menu bar.

use std::sync::Mutex;

use lanroam_core::engine::Request;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::dto::{ControlMode, Snapshot};
use crate::locale::{self, Lang, Texts};
use crate::panel;
use crate::state::{AppState, lock};

/// ID of the tray icon
const TRAY_ID: &str = "lanroam";

/// Menu item IDs
mod ids {
    /// Pause or resume
    pub const PAUSE: &str = "pause";
    /// Lock or unlock
    pub const LOCK: &str = "lock";
    /// Prefix of a jump to a device, followed by its fingerprint
    pub const JUMP: &str = "jump:";
    /// Open the arrangement panel
    pub const ARRANGE: &str = "arrange";
    /// Show the window
    pub const OPEN: &str = "open";
    /// Quit
    pub const QUIT: &str = "quit";
}

/// Where input is, as the icon shows it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Here
    Idle,
    /// On another device
    Controlling,
    /// Another device drives this one
    Controlled,
    /// Crossing is paused
    Paused,
    /// Locked on another device
    Locked,
}

/// The status the icon shows now, to change it only when that changes
static SHOWN: Mutex<Option<Status>> = Mutex::new(None);

/// Picks an icon file by status: a monochrome template on macOS (the menu
/// bar tints it), the accent color elsewhere so it reads on dark and light
/// taskbars alike
macro_rules! icon_bytes {
    ($name:literal) => {{
        #[cfg(target_os = "macos")]
        let bytes: &[u8] = include_bytes!(concat!("../icons/tray/", $name, "-template.png"));
        #[cfg(not(target_os = "macos"))]
        let bytes: &[u8] = include_bytes!(concat!("../icons/tray/", $name, ".png"));
        bytes
    }};
}

/// The icon for `status`
fn icon(status: Status) -> tauri::Result<Image<'static>> {
    let bytes = match status {
        Status::Idle => icon_bytes!("idle"),
        Status::Controlling => icon_bytes!("controlling"),
        Status::Controlled => icon_bytes!("controlled"),
        Status::Paused => icon_bytes!("paused"),
        Status::Locked => icon_bytes!("locked"),
    };
    Image::from_bytes(bytes)
}

/// Where input is in `snapshot`
fn status(snapshot: &Snapshot) -> Status {
    let control = &snapshot.control;
    match control.mode {
        ControlMode::Controlling if control.locked => Status::Locked,
        ControlMode::Controlling => Status::Controlling,
        ControlMode::Controlled => Status::Controlled,
        ControlMode::Idle if control.paused => Status::Paused,
        ControlMode::Idle => Status::Idle,
    }
}

/// Create the tray icon with its first menu
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app, None)?;
    *lock(&SHOWN) = Some(Status::Idle);
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon(Status::Idle)?)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("Lanroam")
        .menu(&menu)
        // macOS opens menus on a left click; on Windows the left click
        // opens the window and the right one the menu
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
                && !cfg!(target_os = "macos")
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Follow a new snapshot: icon, menu and tooltip
pub fn update(app: &AppHandle, snapshot: &Snapshot) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let now = status(snapshot);
    if lock(&SHOWN).replace(now) != Some(now) {
        let changed = icon(now).and_then(|icon| {
            tray.set_icon(Some(icon))?;
            tray.set_icon_as_template(cfg!(target_os = "macos"))
        });
        if let Err(e) = changed {
            tracing::warn!("cannot update the tray icon: {e}");
        }
    }
    let line = status_line(app, snapshot);
    let rebuilt = build_menu(app, Some(snapshot)).and_then(|menu| tray.set_menu(Some(menu)));
    if let Err(e) = rebuilt {
        tracing::warn!("cannot update the tray menu: {e}");
    }
    let _ = tray.set_tooltip(Some(format!("Lanroam · {line}")));
}

/// The words in the language of the settings
fn texts(app: &AppHandle) -> &'static Texts {
    let language = app
        .try_state::<AppState>()
        .map(|state| lock(&state.settings).language.clone())
        .unwrap_or_default();
    locale::texts(Lang::from_setting(&language))
}

/// The state word heading the menu, as the main window shows it
fn status_line(app: &AppHandle, snapshot: &Snapshot) -> &'static str {
    let t = texts(app);
    let others_online = snapshot
        .group
        .as_ref()
        .is_some_and(|group| group.devices.iter().any(|d| !d.local && d.online));
    if !others_online {
        t.inactive
    } else if snapshot.control.paused {
        t.paused
    } else {
        t.active
    }
}

/// The menu for `snapshot` (a bare one before the first)
fn build_menu(app: &AppHandle, snapshot: Option<&Snapshot>) -> tauri::Result<Menu<Wry>> {
    let t = texts(app);
    let menu = Menu::new(app)?;
    if let Some(snapshot) = snapshot {
        let status = MenuItem::new(app, status_line(app, snapshot), false, None::<&str>)?;
        menu.append(&status)?;
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        if let Some(group) = &snapshot.group {
            let paused = snapshot.control.paused;
            let pause_text = if paused { t.resume } else { t.pause };
            menu.append(&MenuItem::with_id(
                app,
                ids::PAUSE,
                pause_text,
                true,
                None::<&str>,
            )?)?;
            // Nothing to lock while paused (the pointer stays here anyway),
            // but a lock from before can still be undone
            let locked = snapshot.control.locked || snapshot.control.peer_locked;
            menu.append(&CheckMenuItem::with_id(
                app,
                ids::LOCK,
                t.lock,
                !paused || locked,
                locked,
                None::<&str>,
            )?)?;
            menu.append(&PredefinedMenuItem::separator(app)?)?;
            menu.append(&MenuItem::new(app, t.switch_to, false, None::<&str>)?)?;
            let here = match snapshot.control.mode {
                ControlMode::Controlling => snapshot.control.peer_fingerprint.as_deref(),
                _ => None,
            };
            for device in group.devices.iter().filter(|d| d.number.is_some()) {
                let mut label = format!("{}  {}", device.number.unwrap_or_default(), device.name);
                if !device.online {
                    label.push_str(&format!(" · {}", t.offline));
                }
                let current = here.map_or(device.local, |fp| fp == device.fingerprint);
                let item = CheckMenuItem::with_id(
                    app,
                    format!("{}{}", ids::JUMP, device.fingerprint),
                    label,
                    device.online,
                    current,
                    None::<&str>,
                )?;
                menu.append(&item)?;
            }
            menu.append(&PredefinedMenuItem::separator(app)?)?;
            menu.append(&MenuItem::with_id(
                app,
                ids::ARRANGE,
                t.arrange,
                true,
                None::<&str>,
            )?)?;
        }
    }
    menu.append(&MenuItem::with_id(
        app,
        ids::OPEN,
        t.open,
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        ids::QUIT,
        t.quit,
        true,
        None::<&str>,
    )?)?;
    Ok(menu)
}

/// A menu item was chosen
fn on_menu(app: &AppHandle, id: &str) {
    let request = match id {
        ids::OPEN => return show_main_window(app),
        ids::ARRANGE => {
            // Off the event loop: the panel may create its window
            let app = app.clone();
            tauri::async_runtime::spawn(async move { panel::show(&app) });
            return;
        }
        ids::QUIT => return app.exit(0),
        ids::PAUSE => Request::Pause,
        ids::LOCK => Request::Lock,
        _ => match id.strip_prefix(ids::JUMP) {
            Some(fingerprint) => Request::Jump(fingerprint.to_string()),
            None => return,
        },
    };
    if let Some(state) = app.try_state::<AppState>()
        && let Err(e) = state.engine.request(request)
    {
        tracing::warn!("cannot pass on the tray's request: {e}");
    }
}

/// Raise the main window (tray, second launch, first launch)
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
