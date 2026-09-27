//! State shared by the commands, the event pump and the tray.

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Mutex, MutexGuard, PoisonError};

use lanroam_core::engine::{Engine, Joining};

use crate::dto::{ControlDto, JoinPromptDto};
use crate::settings::Settings;

/// Everything the app keeps while it runs
pub struct AppState {
    /// The engine
    pub engine: Engine,
    /// Where Lanroam keeps its files (`~/.lanroam`, shared with the CLI)
    pub data_dir: PathBuf,
    /// The app's preferences
    pub settings: Mutex<Settings>,
    /// Who controls what, as last reported
    pub control: Mutex<ControlDto>,
    /// The join this device asked for, while its PIN is being typed
    pub joining: tokio::sync::Mutex<Option<Joining>>,
    /// Counts joins asked for, so that the watcher of an old one stays quiet
    pub join_seq: AtomicU64,
    /// The join this device sponsors, while its PIN is shown
    pub prompt: Mutex<Option<JoinPromptDto>>,
}

/// Lock a mutex; a panic while it was held leaves plain data behind, so a
/// poisoned lock is simply taken over
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
