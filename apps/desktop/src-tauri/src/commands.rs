//! Commands the frontend invokes (**mirrored in `src/api.ts`**).

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use std::collections::BTreeMap;

use lanroam_core::engine::{Request, Spot};
use lanroam_core::group::join::normalize_pin;
use lanroam_core::group::{ClipboardShare, FileShare};
use lanroam_core::lanroam_input::config::EdgeSettings;
use lanroam_core::lanroam_input::{Point, keymap, platform};
use lanroam_core::settings::InputSettings;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_opener::OpenerExt as _;

use crate::bridge::{self, events};
use crate::dto::{
    CommandError, ControlMode, InputDto, JoinAnswerDto, JoinPromptDto, JoinStartDto,
    JoiningEndedDto, NearbyDto, PermissionsDto, Snapshot,
};
use crate::overlay::{self, SceneDto};
use crate::settings::{CLOSE_TO_QUIT, CLOSE_TO_TRAY, MAX_OPACITY, MIN_OPACITY, Settings};
use crate::state::{AppState, lock};
use crate::tray;

/// Result of a command
type Reply<T> = Result<T, CommandError>;

/// How long this device keeps its own numbers off after asking the group
/// to identify its screens: long enough for its own request to come back
const IDENTIFY_QUIET: Duration = Duration::from_secs(2);

/// The whole state
#[tauri::command]
pub async fn get_snapshot(app: AppHandle, state: State<'_, AppState>) -> Reply<Snapshot> {
    Ok(bridge::snapshot(&state, &app.package_info().version.to_string()).await)
}

/// Devices on the LAN outside this device's group
#[tauri::command]
pub fn list_nearby(state: State<'_, AppState>) -> Vec<NearbyDto> {
    let own = state.engine.info().fingerprint;
    let group = state.engine.group();
    let mut nearby: Vec<NearbyDto> = state
        .engine
        .nearby()
        .iter()
        .filter(|peer| peer.info.fingerprint != own)
        .filter(|peer| {
            !group
                .as_ref()
                .is_some_and(|doc| doc.is_member(&peer.info.fingerprint))
        })
        .map(NearbyDto::from)
        .collect();
    nearby.sort_by(|a, b| a.name.cmp(&b.name));
    nearby
}

/// Ask a nearby device to let this one into its group; returns once it
/// shows its PIN
#[tauri::command]
pub async fn start_join(
    app: AppHandle,
    state: State<'_, AppState>,
    fingerprint: String,
) -> Reply<JoinStartDto> {
    let peer = state
        .engine
        .nearby()
        .into_iter()
        .find(|p| p.info.fingerprint == fingerprint)
        .ok_or_else(|| CommandError::new("gone", "the device is no longer on the LAN"))?;
    let joining = state.engine.join(&peer).await?;
    let started = JoinStartDto {
        sponsor: joining.sponsor().name.clone(),
        attempts_left: joining.attempts_left(),
    };
    let seq = state.join_seq.fetch_add(1, Ordering::SeqCst) + 1;
    let ended = joining.ended();
    *state.joining.lock().await = Some(joining);
    // The sponsor may turn it down (or time out) while the PIN is typed
    tauri::async_runtime::spawn(async move {
        let reason = ended.await;
        let state = app.state::<AppState>();
        if state.join_seq.load(Ordering::SeqCst) != seq {
            return;
        }
        if state.joining.lock().await.take().is_some() {
            bridge::emit(&app, events::JOINING_ENDED, JoiningEndedDto { reason });
        }
    });
    Ok(started)
}

/// Answer the join in progress with a PIN
#[tauri::command]
pub async fn answer_join(state: State<'_, AppState>, pin: String) -> Reply<JoinAnswerDto> {
    let pin =
        normalize_pin(&pin).ok_or_else(|| CommandError::new("pin_format", "a PIN is 6 digits"))?;
    let mut slot = state.joining.lock().await;
    let joining = slot
        .as_mut()
        .ok_or_else(|| CommandError::new("no_join", "no join in progress"))?;
    match joining.answer(&pin).await {
        Ok(Some(_)) => {
            *slot = None;
            Ok(JoinAnswerDto {
                joined: true,
                attempts_left: 0,
            })
        }
        Ok(None) => Ok(JoinAnswerDto {
            joined: false,
            attempts_left: joining.attempts_left(),
        }),
        Err(e) => {
            *slot = None;
            Err(e.into())
        }
    }
}

/// Give up the join in progress
#[tauri::command]
pub async fn cancel_join(state: State<'_, AppState>) -> Reply<()> {
    state.join_seq.fetch_add(1, Ordering::SeqCst);
    state.joining.lock().await.take();
    Ok(())
}

/// The join this device sponsors right now, for a join window that opened
/// after the event
#[tauri::command]
pub fn get_join_prompt(state: State<'_, AppState>) -> Option<JoinPromptDto> {
    lock(&state.prompt).clone()
}

/// Turn down the join this device sponsors
#[tauri::command]
pub fn reject_join(state: State<'_, AppState>) {
    state.engine.reject_join();
}

/// Leave the desk group
#[tauri::command]
pub async fn leave_group(state: State<'_, AppState>) -> Reply<()> {
    Ok(state.engine.leave().await?)
}

/// Remove a member from the group
#[tauri::command]
pub async fn kick(state: State<'_, AppState>, fingerprint: String) -> Reply<()> {
    Ok(state.engine.kick(&fingerprint).await?)
}

/// Move a member on the layout canvas: its origin to (`x`, `y`)
#[tauri::command]
pub async fn place(state: State<'_, AppState>, fingerprint: String, x: i32, y: i32) -> Reply<()> {
    let spot = Spot::At(Point::new(x, y));
    Ok(state.engine.place(&fingerprint, spot).await?)
}

/// Have every other online member show its number on its screens (the
/// arrangement page opened); this device's own would cover the window
#[tauri::command]
pub fn identify(state: State<'_, AppState>) -> Reply<()> {
    *lock(&state.identify_quiet) = Some(Instant::now() + IDENTIFY_QUIET);
    Ok(state.engine.identify()?)
}

/// What the calling overlay window shows right now (for its page that just
/// loaded)
#[tauri::command]
pub fn get_overlay(app: AppHandle, window: tauri::WebviewWindow) -> SceneDto {
    overlay::scene_of(&app, window.label())
}

/// Where the calling overlay window takes clicks, if anywhere (the card of a
/// drop)
#[tauri::command]
pub fn set_overlay_area(
    app: AppHandle,
    window: tauri::WebviewWindow,
    area: Option<overlay::AreaDto>,
) {
    overlay::hot_area(&app, window.label(), area);
}

/// Cancel a drop whose files are still coming (the button on its card)
#[tauri::command]
pub fn cancel_drop(state: State<'_, AppState>, id: u64) -> Reply<()> {
    Ok(state.engine.cancel_drop(id)?)
}

/// Rename this device
#[tauri::command]
pub async fn rename(app: AppHandle, state: State<'_, AppState>, name: String) -> Reply<()> {
    state.engine.rename(&name).await?;
    // The name shows in the window even outside a group
    bridge::refresh(&app).await;
    Ok(())
}

/// Turn the Command / Control swap for input into this device on or off
#[tauri::command]
pub async fn set_swap(state: State<'_, AppState>, on: bool) -> Reply<()> {
    Ok(state.engine.set_swap(on).await?)
}

/// Set how fast the pointer goes on this device while another member
/// controls it, in percent
#[tauri::command]
pub async fn set_pointer_speed(state: State<'_, AppState>, speed: u32) -> Reply<()> {
    Ok(state.engine.set_pointer_speed(speed).await?)
}

/// Set what of this device's clipboard is shared with the group
#[tauri::command]
pub async fn set_clipboard(state: State<'_, AppState>, share: ClipboardShare) -> Reply<()> {
    Ok(state.engine.set_clipboard(share).await?)
}

/// Set what this device does with files from the group
#[tauri::command]
pub async fn set_files(state: State<'_, AppState>, share: FileShare) -> Reply<()> {
    Ok(state.engine.set_files(share).await?)
}

/// What the window asks the keyboard and mouse to do
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    /// Pause crossing, or resume
    Pause,
    /// Lock the pointer to its device, or unlock
    Lock,
    /// Move control to a device
    Jump {
        /// The device
        fingerprint: String,
    },
}

/// Carry out an action at the next local input event
#[tauri::command]
pub fn request_action(state: State<'_, AppState>, action: Action) -> Reply<()> {
    let request = match action {
        Action::Pause => Request::Pause,
        Action::Lock => Request::Lock,
        Action::Jump { fingerprint } => Request::Jump(fingerprint),
    };
    Ok(state.engine.request(request)?)
}

/// The OS input permissions
#[tauri::command]
pub fn get_permissions() -> PermissionsDto {
    let granted = platform::permissions();
    PermissionsDto {
        required: cfg!(target_os = "macos"),
        accessibility: granted.accessibility,
        input_monitoring: granted.input_monitoring,
    }
}

/// Which permission to grant
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    /// Accessibility
    Accessibility,
    /// Input Monitoring
    InputMonitoring,
}

/// Open the system settings where `permission` is granted, with Lanroam
/// already listed there
#[tauri::command]
pub fn open_permission(app: AppHandle, permission: Permission) -> Reply<()> {
    platform::request_permissions();
    let pane = match permission {
        Permission::Accessibility => "Privacy_Accessibility",
        Permission::InputMonitoring => "Privacy_ListenEvent",
    };
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| CommandError::new("internal", e.to_string()))
}

/// Start capture and injection if they do not run yet (after a permission
/// was granted)
#[tauri::command]
pub async fn restart_input(app: AppHandle, state: State<'_, AppState>) -> Reply<InputDto> {
    let status = state.engine.restart_input().await?;
    bridge::refresh(&app).await;
    Ok(status.into())
}

/// Start Lanroam over (some permissions only apply to a new process)
#[tauri::command]
pub fn relaunch(app: AppHandle) {
    app.restart();
}

/// The app's preferences as the settings page edits them
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    /// `system`, `zh` or `en`
    pub language: String,
    /// `system`, `dark` or `light`
    pub theme: String,
    /// Start at login
    pub autostart: bool,
    /// Light up the edge the pointer comes in by
    pub edge_glow: bool,
    /// Say pauses, locks, jumps and lost devices mid-screen
    pub hints: bool,
    /// Dim this device's screens while it controls another
    pub dim: bool,
    /// Closing the main window: `tray` or `quit`
    pub close_window: String,
    /// How opaque the windows' tint is over the blurred desktop, in percent
    pub opacity: u8,
}

/// The app's preferences
#[tauri::command]
pub fn get_settings(app: AppHandle, state: State<'_, AppState>) -> SettingsDto {
    let settings = lock(&state.settings).clone();
    SettingsDto {
        language: settings.language,
        theme: settings.theme,
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        edge_glow: settings.edge_glow,
        hints: settings.hints,
        dim: settings.dim,
        close_window: settings.close_window,
        opacity: settings.opacity,
    }
}

/// Save the app's preferences and apply them
#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: SettingsDto,
) -> Reply<()> {
    let autolaunch = app.autolaunch();
    if autolaunch.is_enabled().unwrap_or(false) != settings.autostart {
        let switched = if settings.autostart {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        switched.map_err(|e| CommandError::new("autostart", e.to_string()))?;
    }
    let saved = Settings {
        language: settings.language,
        theme: settings.theme,
        edge_glow: settings.edge_glow,
        hints: settings.hints,
        dim: settings.dim,
        close_window: if settings.close_window == CLOSE_TO_QUIT {
            CLOSE_TO_QUIT.into()
        } else {
            CLOSE_TO_TRAY.into()
        },
        opacity: settings.opacity.clamp(MIN_OPACITY, MAX_OPACITY),
        ..lock(&state.settings).clone()
    };
    saved.save(&state.data_dir)?;
    *lock(&state.settings) = saved;
    // Dimming follows at once when this device controls another
    let controlling = lock(&state.control).mode == ControlMode::Controlling;
    overlay::dim(&app, settings.dim && controlling);
    // The tray speaks the language just chosen
    bridge::refresh(&app).await;
    Ok(())
}

/// This device's input settings: hotkeys, keys kept local, switching
#[tauri::command]
pub fn get_input_settings(state: State<'_, AppState>) -> InputSettings {
    state.engine.input_settings()
}

/// Use and save new input settings
#[tauri::command]
pub async fn save_input_settings(state: State<'_, AppState>, settings: InputSettings) -> Reply<()> {
    Ok(state.engine.set_input_settings(settings).await?)
}

/// Start recording a key combination, or stop; it comes as the `recorded`
/// event
#[tauri::command]
pub fn record_keys(state: State<'_, AppState>, on: bool) -> Reply<()> {
    Ok(state.engine.record(on)?)
}

/// Set the edge between two members, for the whole group
#[tauri::command]
pub async fn set_edge(
    state: State<'_, AppState>,
    a: String,
    b: String,
    settings: EdgeSettings,
) -> Reply<()> {
    Ok(state.engine.set_edge(&a, &b, settings).await?)
}

/// The keys' names by HID usage (W3C `code` values), to show combinations
#[tauri::command]
pub fn key_names() -> BTreeMap<u16, &'static str> {
    keymap::names().collect()
}

/// Show the log file in the file manager (its folder, when there is no
/// file yet)
#[tauri::command]
pub fn open_logs(app: AppHandle, state: State<'_, AppState>) -> Reply<()> {
    let log = state.data_dir.join(crate::LOG_FILE);
    let opened = if log.exists() {
        app.opener().reveal_item_in_dir(&log)
    } else {
        app.opener()
            .open_path(state.data_dir.to_string_lossy(), None::<&str>)
    };
    opened.map_err(|e| CommandError::new("internal", e.to_string()))
}

/// Bring up the main window
#[tauri::command]
pub fn show_main_window(app: AppHandle) {
    tray::show_main_window(&app);
}

/// Quit Lanroam
#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}
