//! Platforms without drags of files between devices

use std::path::PathBuf;

use lanroam_input::Point;

use crate::{DndError, Sink};

/// Nothing to drop early
pub(crate) const DROPS_EARLY: bool = false;

/// Never starts
pub(crate) struct Dnd;

impl Dnd {
    /// Not supported here
    pub(crate) fn start(_sink: Sink) -> Result<Self, DndError> {
        Err(DndError::Unsupported)
    }

    /// Nothing to do
    pub(crate) fn pressed(&self) {}

    /// Nothing to do
    pub(crate) fn probe(&self, _id: u64) {}

    /// Nothing to do
    pub(crate) fn unprobe(&self) {}

    /// Nothing to do
    pub(crate) fn arm(&self, _id: u64, _at: Point, _paths: Vec<PathBuf>) {}

    /// Nothing to do
    pub(crate) fn cancel(&self, _id: u64) {}

    /// Nothing to do
    pub(crate) fn deliver(&self, _id: u64, _ok: bool) {}
}
