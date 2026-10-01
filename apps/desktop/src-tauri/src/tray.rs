//! The tray icon and its menu: the state, pause and lock, a jump to any
//! online member, the settings, quitting.
//!
//! The menu follows each snapshot: members come and go, and the texts
//! follow the language setting. It is set anew only when what it shows
//! changed, since that replaces a menu the user has open. The icon's shape follows where input
//! is (a mouse under signal waves: hollow here, filled on another device,
//! with a badge or a slash for the rest), so it reads without color in a
//! monochrome menu bar.

use std::sync::Mutex;

use lanroam_core::engine::Request;
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::bridge::{self, events};
use crate::dto::{ControlMode, Snapshot};
use crate::locale::{self, Lang, Texts};
use crate::material;
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
    /// Show the window on its settings page
    pub const SETTINGS: &str = "settings";
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
    /// Locked here: nothing crosses, like paused, until unlocked
    LockedHome,
}

/// The status the icon shows now, to change it only when that changes
static SHOWN: Mutex<Option<Status>> = Mutex::new(None);

/// One line of the tray menu
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    /// Words that cannot be chosen
    Heading(String),
    /// An item to choose
    Item {
        /// Its ID (see [`ids`])
        id: String,
        /// Its words
        text: String,
    },
    /// An item with a check mark
    Check {
        /// Its ID (see [`ids`])
        id: String,
        /// Its words
        text: String,
        /// It can be chosen
        enabled: bool,
        /// It is checked
        checked: bool,
    },
    /// A line between groups of items
    Separator,
}

/// The lines the menu shows now, to set it anew only when they change
static SHOWN_LINES: Mutex<Option<Vec<Line>>> = Mutex::new(None);

/// Picks an icon file by status: a monochrome template on macOS (the menu
/// bar tints it), a colored tile elsewhere so it reads on dark and light
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
        Status::LockedHome => icon_bytes!("locked-home"),
    };
    Image::from_bytes(bytes)
}

/// Where input is in `snapshot`; here, paused wins over locked, as in the
/// menu, where the lock is greyed out while paused
fn status(snapshot: &Snapshot) -> Status {
    let control = &snapshot.control;
    match control.mode {
        ControlMode::Controlling if control.locked => Status::Locked,
        ControlMode::Controlling => Status::Controlling,
        ControlMode::Controlled => Status::Controlled,
        ControlMode::Idle if control.paused => Status::Paused,
        ControlMode::Idle if control.locked => Status::LockedHome,
        ControlMode::Idle => Status::Idle,
    }
}

/// Create the tray icon with its first menu
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let lines = menu_lines(texts(app), None);
    let menu = build_menu(app, &lines)?;
    *lock(&SHOWN_LINES) = Some(lines);
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

/// Follow a new snapshot: icon, menu and tooltip. On the main thread only:
/// the tray icon's handle is not thread-safe
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
    let t = texts(app);
    let lines = menu_lines(t, Some(snapshot));
    if lock(&SHOWN_LINES).as_ref() != Some(&lines) {
        match build_menu(app, &lines).and_then(|menu| tray.set_menu(Some(menu))) {
            Ok(()) => *lock(&SHOWN_LINES) = Some(lines),
            Err(e) => tracing::warn!("cannot update the tray menu: {e}"),
        }
    }
    let line = status_line(t, snapshot);
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

/// The state word heading the menu, as the main window shows it, in the
/// words `t`
fn status_line(t: &Texts, snapshot: &Snapshot) -> &'static str {
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

/// The lines of the menu for `snapshot` (a bare one before the first), in
/// the words `t`
fn menu_lines(t: &Texts, snapshot: Option<&Snapshot>) -> Vec<Line> {
    let item = |id: &str, text: &str| Line::Item {
        id: id.to_string(),
        text: text.to_string(),
    };
    let mut lines = Vec::new();
    if let Some(snapshot) = snapshot {
        lines.push(Line::Heading(status_line(t, snapshot).to_string()));
        lines.push(Line::Separator);
        if let Some(group) = &snapshot.group {
            let paused = snapshot.control.paused;
            lines.push(item(ids::PAUSE, if paused { t.resume } else { t.pause }));
            // Nothing to lock while paused (the pointer stays here anyway),
            // but a lock from before can still be undone
            let locked = snapshot.control.locked || snapshot.control.peer_locked;
            lines.push(Line::Check {
                id: ids::LOCK.to_string(),
                text: t.lock.to_string(),
                enabled: !paused || locked,
                checked: locked,
            });
            lines.push(Line::Separator);
            lines.push(Line::Heading(t.switch_to.to_string()));
            let here = match snapshot.control.mode {
                ControlMode::Controlling => snapshot.control.peer_fingerprint.as_deref(),
                _ => None,
            };
            for device in group.devices.iter().filter(|d| d.number.is_some()) {
                let mut text = format!("{}  {}", device.number.unwrap_or_default(), device.name);
                if !device.online {
                    text.push_str(&format!(" · {}", t.offline));
                }
                lines.push(Line::Check {
                    id: format!("{}{}", ids::JUMP, device.fingerprint),
                    text,
                    enabled: device.online,
                    checked: here.map_or(device.local, |fp| fp == device.fingerprint),
                });
            }
            lines.push(Line::Separator);
        }
    }
    lines.push(item(ids::SETTINGS, t.settings));
    lines.push(Line::Separator);
    lines.push(item(ids::QUIT, t.quit));
    lines
}

/// A menu showing `lines`
fn build_menu(app: &AppHandle, lines: &[Line]) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::new(app)?;
    for line in lines {
        match line {
            Line::Heading(text) => menu.append(&MenuItem::new(app, text, false, None::<&str>)?)?,
            Line::Item { id, text } => {
                menu.append(&MenuItem::with_id(app, id, text, true, None::<&str>)?)?;
            }
            Line::Check {
                id,
                text,
                enabled,
                checked,
            } => {
                let check =
                    CheckMenuItem::with_id(app, id, text, *enabled, *checked, None::<&str>)?;
                menu.append(&check)?;
            }
            Line::Separator => menu.append(&PredefinedMenuItem::separator(app)?)?,
        }
    }
    Ok(menu)
}

/// A menu item was chosen
fn on_menu(app: &AppHandle, id: &str) {
    let request = match id {
        ids::SETTINGS => {
            show_main_window(app);
            return bridge::emit(app, events::SHOW_PAGE, "settings");
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
        material::blur_behind(&window);
    }
}

#[cfg(test)]
mod tests {
    use lanroam_core::group::{ClipboardShare, FileShare};

    use super::*;
    use crate::dto::{ControlDto, DeviceDto, GroupDto, InputDto, SelfDto};

    /// A member named `name`, numbered `number` once placed
    fn device(name: &str, number: Option<usize>, online: bool, local: bool) -> DeviceDto {
        DeviceDto {
            fingerprint: format!("fp-{name}"),
            name: name.to_string(),
            platform: "macos".to_string(),
            online,
            local,
            number,
            displays: 1,
            resolution: String::new(),
            scale: 100,
            swap: false,
            pointer_speed: 100,
            clipboard: ClipboardShare::default(),
            files: FileShare::default(),
            rect: None,
            origin: None,
            screens: Vec::new(),
        }
    }

    /// This device in a group of `devices`
    fn snapshot(devices: Vec<DeviceDto>) -> Snapshot {
        Snapshot {
            device: SelfDto {
                fingerprint: "fp-here".to_string(),
                name: "here".to_string(),
                platform: "macos".to_string(),
                version: "0".to_string(),
            },
            group: Some(GroupDto {
                id: "group".to_string(),
                devices,
                edges: Vec::new(),
            }),
            control: ControlDto::default(),
            input: InputDto::default(),
        }
    }

    /// Before the first snapshot: the settings and quitting only
    #[test]
    fn a_bare_menu() {
        let t = locale::texts(Lang::En);
        let item = |id: &str, text: &str| Line::Item {
            id: id.to_string(),
            text: text.to_string(),
        };
        assert_eq!(
            menu_lines(t, None),
            [
                item(ids::SETTINGS, t.settings),
                Line::Separator,
                item(ids::QUIT, t.quit)
            ]
        );
    }

    /// Each placed member to jump to, this device checked and offline ones
    /// not to be chosen; the same state gives the same lines, so the menu
    /// is not set anew, and a change gives others
    #[test]
    fn members_to_jump_to() {
        let t = locale::texts(Lang::En);
        let mut shown = snapshot(vec![
            device("here", Some(1), true, true),
            device("pc", Some(2), false, false),
            device("new", None, true, false),
        ]);
        let lines = menu_lines(t, Some(&shown));
        let jumps: Vec<&Line> = lines
            .iter()
            .filter(|line| matches!(line, Line::Check { id, .. } if id.starts_with(ids::JUMP)))
            .collect();
        let jump = |name: &str, text: String, enabled, checked| Line::Check {
            id: format!("{}fp-{name}", ids::JUMP),
            text,
            enabled,
            checked,
        };
        assert_eq!(
            jumps,
            [
                &jump("here", "1  here".to_string(), true, true),
                &jump("pc", format!("2  pc · {}", t.offline), false, false),
            ]
        );
        assert_eq!(menu_lines(t, Some(&shown.clone())), lines);
        shown.control.paused = true;
        assert_ne!(menu_lines(t, Some(&shown)), lines);
    }
}
