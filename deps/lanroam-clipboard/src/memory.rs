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
        self.put(content, false);
    }

    /// A password manager copies `content` here, marked concealed
    pub fn copy_concealed(&self, content: Content) {
        self.put(content, true);
    }

    /// What the clipboard holds, concealed or not
    pub fn content(&self) -> Option<Content> {
        self.lock().content.clone()
    }

    /// Hold `content` from now on; the new stamp
    fn put(&self, content: Content, concealed: bool) -> i64 {
        let mut state = self.lock();
        state.stamp += 1;
        state.content = Some(content);
        state.concealed = concealed;
        state.stamp
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

    fn write(&self, content: &Content) -> Result<Option<i64>, ClipboardError> {
        Ok(Some(self.put(content.clone(), false)))
    }
}
