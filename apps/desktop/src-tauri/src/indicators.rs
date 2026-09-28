//! What the on-screen overlays say about who controls what, where the user
//! looks: the device the pointer is on.
//!
//! - The device the pointer comes into (back home included) lights the
//!   edge it came in by, or shows its number and name after a jump
//! - Pauses, locks and lost devices are said on the device the pointer is on
//! - A device controlling another dims its own screens (off by default)
//!
//! The appearance settings turn each of them off.

use lanroam_core::engine::ControlEvent;
use lanroam_core::lanroam_input::Point;
use tauri::{AppHandle, Manager};

use crate::dto::{ControlDto, ControlMode};
use crate::overlay::{self, Hint};
use crate::panel;
use crate::state::{AppState, lock};

/// Show what `event` means; `was` is the control state before it
pub fn control_event(app: &AppHandle, event: &ControlEvent, was: &ControlDto) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let settings = lock(&state.settings).clone();
    let hint = |hint: Hint| {
        if settings.hints {
            overlay::hint(app, hint, None);
        }
    };
    match event {
        ControlEvent::Controlling { .. } if settings.dim => overlay::dim(app, true),
        ControlEvent::Home { at, jumped } => {
            overlay::dim(app, false);
            // Mid-display after a pause or a release too, which have their
            // own hints
            came_in(app, *at, *jumped, &settings);
        }
        // Mid-display here only after a jump
        ControlEvent::ControlledBy { at, .. } => came_in(app, *at, true, &settings),
        ControlEvent::Paused { on } => hint(Hint::Paused {
            on: *on,
            platform: state.engine.info().platform,
        }),
        // While another device is controlled, that is where the pointer is
        // locked: it says so itself (LockedHere)
        ControlEvent::Locked { on } if was.mode != ControlMode::Controlling => {
            let info = state.engine.info();
            hint(Hint::Locked {
                on: *on,
                name: info.name,
                platform: info.platform,
            });
        }
        ControlEvent::LockedHere {
            fingerprint, on, ..
        } => hint(Hint::Locked {
            on: *on,
            name: state.engine.info().name,
            platform: platform_of(&state, fingerprint),
        }),
        ControlEvent::Unresponsive { name, .. } => hint(Hint::Unresponsive { name: name.clone() }),
        ControlEvent::Lost { name, .. } => hint(Hint::Lost { name: name.clone() }),
        ControlEvent::LetGo { name, reason, .. } => hint(Hint::LetGo {
            name: name.clone(),
            reason: reason.clone(),
        }),
        _ => {}
    }
}

/// The pointer came into this device at `at`: light the edge it crossed,
/// or, when `jumped`, say where the jump landed
fn came_in(app: &AppHandle, at: Point, jumped: bool, settings: &crate::settings::Settings) {
    let Some((index, edge)) = overlay::locate(app, at) else {
        return;
    };
    match edge {
        Some(edge) if settings.edge_glow => overlay::glow(app, index, edge),
        None if jumped && settings.hints => {
            let Some(state) = app.try_state::<AppState>() else {
                return;
            };
            let hint = Hint::Jump {
                number: own_number(&state),
                name: state.engine.info().name,
            };
            overlay::hint(app, hint, Some(index));
        }
        _ => {}
    }
}

/// Show this device's number on its screens (the group identifies them),
/// unless the arrangement panel is open here: the number would cover it
pub fn identify(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if panel::is_open(app) {
        return;
    }
    overlay::identify(app, own_number(&state), state.engine.info().name);
}

/// The main window was closed: the first time, say that Lanroam keeps
/// running (whatever the hint setting, it is said once)
pub fn window_closed(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let settings = {
        let mut settings = lock(&state.settings);
        if settings.close_hinted {
            return;
        }
        settings.close_hinted = true;
        settings.clone()
    };
    if let Err(e) = settings.save(&state.data_dir) {
        tracing::warn!("cannot save the settings: {e:#}");
    }
    let hint = Hint::StillRunning {
        platform: state.engine.info().platform,
    };
    overlay::hint(app, hint, None);
}

/// This device's number in the layout; `None` until placed
fn own_number(state: &AppState) -> Option<usize> {
    let own = state.engine.info().fingerprint;
    let doc = state.engine.group()?;
    lanroam_core::layout::numbered(&doc)
        .iter()
        .position(|fp| *fp == own)
        .map(|i| i + 1)
}

/// A member's platform (`macos`, `windows`), empty when unknown
fn platform_of(state: &AppState, fingerprint: &str) -> String {
    state
        .engine
        .group()
        .and_then(|doc| {
            doc.members()
                .find(|(fp, _)| *fp == fingerprint)
                .map(|(_, record)| record.profile.platform.clone())
        })
        .unwrap_or_default()
}
