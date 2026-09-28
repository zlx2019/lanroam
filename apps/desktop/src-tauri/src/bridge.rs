//! Engine bridge: starts the engine and turns its events into frontend
//! events, tray updates, on-screen indicators and the join window.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use lanroam_core::engine::{Engine, EngineEvent, PlatformInput};
use lanroam_core::lanroam_clipboard::SystemClipboard;
use lanroam_core::node::NodeConfig;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

use crate::dto::{GroupDto, InputDto, JoinEndedDto, JoinPromptDto, SelfDto, Snapshot};
use crate::settings::Settings;
use crate::state::{AppState, lock};
use crate::{indicators, tray};

/// Frontend event names (**mirrored in `src/events.ts`**)
pub mod events {
    /// The whole state changed; payload: `Snapshot`
    pub const SNAPSHOT: &str = "snapshot";
    /// Someone asks to join through this device; payload: `JoinPromptDto`
    pub const JOIN_PROMPT: &str = "join-prompt";
    /// The join this device sponsored is over; payload: `JoinEndedDto`
    pub const JOIN_ENDED: &str = "join-ended";
    /// The sponsor ended the join this device asked for; payload:
    /// `JoiningEndedDto`
    pub const JOINING_ENDED: &str = "joining-ended";
    /// Another member removed this device from the group; no payload
    pub const KICKED: &str = "kicked";
    /// A key combination was recorded; payload: `Chord`, `null` when the
    /// user gave up
    pub const RECORDED: &str = "recorded";
    /// The main window is to show one of its pages (the tray asked);
    /// payload: the page, e.g. `settings`
    pub const SHOW_PAGE: &str = "show-page";
}

/// Label of the window showing a join's PIN
pub const JOIN_WINDOW: &str = "join";

/// Development override of the data directory (a second identity on one
/// machine, like the CLI's `--data-dir`)
const DATA_DIR_VAR: &str = "LANROAM_DATA_DIR";

/// Development override of the QUIC port (`0` picks a free one, like the
/// CLI's `--port`)
const PORT_VAR: &str = "LANROAM_PORT";

/// Where Lanroam keeps its files: `~/.lanroam`, the CLI's default too, so
/// the app and the CLI are the same device
pub fn data_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os(DATA_DIR_VAR) {
        return Ok(PathBuf::from(dir));
    }
    let home = app.path().home_dir().context("no home directory")?;
    Ok(home.join(".lanroam"))
}

/// Start the engine; its events are pumped once [`spawn_pump`] runs
pub async fn start(
    data_dir: PathBuf,
) -> anyhow::Result<(AppState, mpsc::UnboundedReceiver<EngineEvent>)> {
    let mut config = NodeConfig::new(data_dir.clone());
    if let Some(port) = std::env::var(PORT_VAR).ok().and_then(|p| p.parse().ok()) {
        config.port = port;
    }
    let (engine, events) =
        Engine::start(config, Arc::new(PlatformInput), Arc::new(SystemClipboard)).await?;
    let settings = Settings::load(&data_dir);
    let state = AppState {
        engine,
        data_dir,
        settings: std::sync::Mutex::new(settings),
        control: std::sync::Mutex::default(),
        joining: tokio::sync::Mutex::new(None),
        join_seq: std::sync::atomic::AtomicU64::new(0),
        prompt: std::sync::Mutex::new(None),
        overlays: std::sync::Mutex::default(),
        identify_quiet: std::sync::Mutex::new(None),
    };
    Ok((state, events))
}

/// Handle the engine's events until it stops
pub fn spawn_pump(app: AppHandle, mut events: mpsc::UnboundedReceiver<EngineEvent>) {
    tauri::async_runtime::spawn(async move {
        while let Some(event) = events.recv().await {
            on_event(&app, event).await;
        }
    });
}

/// One engine event
async fn on_event(app: &AppHandle, event: EngineEvent) {
    let state = app.state::<AppState>();
    match event {
        EngineEvent::Online(_) | EngineEvent::Offline { .. } | EngineEvent::Group(_) => {}
        EngineEvent::Kicked => emit(app, events::KICKED, ()),
        EngineEvent::Identify => {
            indicators::identify(app);
            return;
        }
        EngineEvent::Recorded(chord) => {
            emit(app, events::RECORDED, chord);
            return;
        }
        EngineEvent::JoinPin {
            joiner,
            pin,
            attempts_left,
        } => {
            let address = state
                .engine
                .nearby()
                .into_iter()
                .find(|p| p.info.fingerprint == joiner.fingerprint)
                .and_then(|p| p.addrs.first().map(ToString::to_string));
            let prompt = JoinPromptDto {
                name: joiner.name,
                platform: joiner.platform,
                address,
                pin,
                attempts_left,
            };
            *lock(&state.prompt) = Some(prompt.clone());
            emit(app, events::JOIN_PROMPT, prompt);
            show_join_window(app);
            return;
        }
        EngineEvent::JoinEnded { joiner, admitted } => {
            *lock(&state.prompt) = None;
            emit(
                app,
                events::JOIN_ENDED,
                JoinEndedDto {
                    name: joiner.name,
                    admitted,
                },
            );
        }
        EngineEvent::Control(event) => {
            let (was, changed) = {
                let mut control = lock(&state.control);
                let was = control.clone();
                let changed = control.apply(&event);
                (was, changed)
            };
            indicators::control_event(app, &event, &was);
            let unavailable = matches!(
                event,
                lanroam_core::engine::ControlEvent::Unavailable { .. }
            );
            if !changed && !unavailable {
                return;
            }
        }
    }
    refresh(app).await;
}

/// Send the whole state to the frontend and the tray
pub async fn refresh(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snapshot = snapshot(&state, &app.package_info().version.to_string()).await;
    tray::update(app, &snapshot);
    emit(app, events::SNAPSHOT, snapshot);
}

/// The state as the frontend sees it
pub async fn snapshot(state: &AppState, version: &str) -> Snapshot {
    let info = state.engine.info();
    let online: HashSet<String> = match state.engine.status().await {
        Ok(status) => status.online.into_iter().map(|p| p.fingerprint).collect(),
        Err(_) => HashSet::new(),
    };
    let group = state
        .engine
        .group()
        .map(|doc| GroupDto::new(&doc, &info.fingerprint, &online));
    let input = state
        .engine
        .input_status()
        .await
        .map(InputDto::from)
        .unwrap_or_default();
    Snapshot {
        device: SelfDto::new(&info, version),
        group,
        control: lock(&state.control).clone(),
        input,
    }
}

/// Bring up the PIN window, on top and focused
fn show_join_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(JOIN_WINDOW) {
        let _ = window.center();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Send an event to every window; a failure is only logged
pub fn emit<T: Serialize + Clone>(app: &AppHandle, event: &str, payload: T) {
    if let Err(e) = app.emit(event, payload) {
        tracing::warn!("cannot send the {event} event: {e}");
    }
}
