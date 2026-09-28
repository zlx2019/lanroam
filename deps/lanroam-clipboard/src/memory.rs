//! A clipboard in memory, standing in for the system's in tests.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::{Clipboard, ClipboardError, Content};

/// A clipboard in memory: its stamp goes up on every write, as the
/// system's does
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    state: Mutex<State>,
}

/// What the clipboard holds
#[derive(Debug, Default)]
struct State {
    stamp: i64,
    content: Option<Content>,
    concealed: bool,
}

impl MemoryClipboard {
    /// An empty clipboard
    pub fn new() -> Self {
        Self::default()
    }

    /// The user copies `content` here
    pub fn copy(&self, content: Content) {
        let mut state = self.lock();
        state.stamp += 1;
        state.content = Some(content);
        state.concealed = false;
    }

    /// A password manager copies `content` here, marked concealed
    pub fn copy_concealed(&self, content: Content) {
        self.copy(content);
        self.lock().concealed = true;
    }

    /// What the clipboard holds, concealed or not
    pub fn content(&self) -> Option<Content> {
        self.lock().content.clone()
    }

    /// The state, even after a panic elsewhere
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clipboard for MemoryClipboard {
    fn stamp(&self) -> Option<i64> {
        Some(self.lock().stamp)
    }

    fn read(&self) -> Result<Option<Content>, ClipboardError> {
        let state = self.lock();
        Ok(if state.concealed {
            None
        } else {
            state.content.clone()
        })
    }

    fn write(&self, content: &Content) -> Result<(), ClipboardError> {
        self.copy(content.clone());
        Ok(())
    }
}
