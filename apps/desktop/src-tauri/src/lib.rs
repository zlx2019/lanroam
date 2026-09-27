//! Lanroam desktop shell: the engine behind a window and a tray icon.
//!
//! - Closing a window hides it; the tray keeps Lanroam running, and quitting
//!   goes through the tray menu
//! - On macOS it lives in the menu bar only (no Dock icon)
//! - Started at login with `--hidden`, it stays in the tray; any other start
//!   shows the window, and a second start raises the first one's
//! - Ctrl-C and SIGTERM go through a normal exit, so the engine still hands
//!   input back and says goodbye on the LAN

mod bridge;
mod commands;
mod dto;
mod locale;
mod settings;
mod state;
mod tray;

use std::path::Path;

use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

use crate::state::AppState;

/// Launch argument of the login item
const HIDDEN_ARG: &str = "--hidden";

/// Run the app until it quits
pub fn run() {
    init_logging();
    let built = tauri::Builder::default()
        // First, so that a second start exits at once and raises this one
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_ARG]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            setup(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing hides; prevent_close must come first or the app quits
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                // Closing the PIN window turns the join down
                if window.label() == bridge::JOIN_WINDOW
                    && let Some(state) = window.try_state::<AppState>()
                {
                    state.engine.reject_join();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::list_nearby,
            commands::start_join,
            commands::answer_join,
            commands::cancel_join,
            commands::get_join_prompt,
            commands::reject_join,
            commands::leave_group,
            commands::kick,
            commands::rename,
            commands::set_swap,
            commands::request_action,
            commands::get_permissions,
            commands::open_permission,
            commands::restart_input,
            commands::relaunch,
            commands::get_settings,
            commands::save_settings,
            commands::show_main_window,
            commands::quit_app,
        ])
        .build(tauri::generate_context!());
    let app = match built {
        Ok(app) => app,
        Err(e) => {
            tracing::error!("cannot build the app: {e}");
            return;
        }
    };

    // A menu bar app: the policy has to be set before the event loop starts
    #[cfg(target_os = "macos")]
    let mut app = app;
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    app.run(|app, event| {
        if let RunEvent::Exit = event
            // Absent when the engine never started
            && let Some(state) = app.try_state::<AppState>()
        {
            tauri::async_runtime::block_on(state.engine.shutdown());
        }
    });
}

/// Start the engine, the tray and the window; a failure to start is shown
/// in a dialog before quitting, not as a crash
fn setup(app: &AppHandle) {
    let started = bridge::data_dir(app).and_then(|dir| {
        tauri::async_runtime::block_on(bridge::start(dir.clone())).map(|state| (dir, state))
    });
    let (dir, (state, events)) = match started {
        Ok(started) => started,
        Err(e) => return startup_failed(app, &e),
    };
    app.manage(state);
    if let Err(e) = tray::setup(app) {
        tracing::error!("cannot create the tray icon: {e}");
    }
    bridge::spawn_pump(app.clone(), events);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        wait_for_termination().await;
        handle.exit(0);
    });
    tauri::async_runtime::spawn({
        let app = app.clone();
        async move { bridge::refresh(&app).await }
    });
    if !std::env::args().any(|arg| arg == HIDDEN_ARG) {
        tray::show_main_window(app);
    }
    tracing::info!("Lanroam is running, data in {}", dir.display());
}

/// Tell the user why Lanroam cannot start (most likely another Lanroam, or
/// the CLI, holds the port), then quit
///
/// The callback form of the dialog: setup runs on the main thread, which
/// also serves a blocking dialog's reply, so waiting here would deadlock.
fn startup_failed(app: &AppHandle, error: &anyhow::Error) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

    tracing::error!("cannot start the engine: {error:#}");
    let language = bridge::data_dir(app)
        .map(|dir| settings::Settings::load(&dir).language)
        .unwrap_or_default();
    let texts = locale::texts(locale::Lang::from_setting(&language));
    let handle = app.clone();
    app.dialog()
        .message(format!("{error:#}\n\n{}", texts.start_failed_hint))
        .title(texts.start_failed_title)
        .kind(MessageDialogKind::Error)
        .show(move |_| handle.exit(1));
}

/// Log to stderr, and to `lanroam.log` in the data directory when it can
/// be opened (the only place to look for a build without a console)
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt::writer::MakeWriterExt as _;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let dir = std::env::var_os("LANROAM_DATA_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| Path::new(&home).join(".lanroam"))
        });
    let file = dir.and_then(|dir| open_log(&dir));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    let _ = match file {
        Some(file) => builder
            .with_ansi(false)
            .with_writer(std::io::stderr.and(std::sync::Mutex::new(file)))
            .try_init(),
        None => builder.try_init(),
    };
}

/// The log file, emptied at each start
fn open_log(dir: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(dir).ok()?;
    std::fs::File::create(dir.join("lanroam.log")).ok()
}

/// Wait for Ctrl-C, or SIGTERM on unix
async fn wait_for_termination() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
