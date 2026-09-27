//! lanroam-input: the keyboard and mouse layer of Lanroam.
//!
//! ```text
//! ┌─ keymap   ─ physical keys as USB HID usages, with the macOS and
//! │             Windows codes of each
//! ├─ config   ─ what the user sets: hotkeys, keys kept local, switching
//! │             modes, settings of single edges
//! ├─ geometry ─ displays, desktop edges, where the pointer lands when it
//! │             crosses from one desktop to another
//! ├─ world    ─ every device's desktop on one shared canvas: crossing
//! │             between devices, neighbours, reading order
//! ├─ switch   ─ source side: event by event, keep input local or send it
//! │             to the target
//! ├─ inject   ─ target side: replay a source's input and release whatever
//! │             it still holds when it goes away
//! └─ platform ─ capture and injection per OS (macOS event taps and
//!               CGEventPost, Windows low-level hooks and SendInput)
//! ```
//!
//! Everything outside [`platform`] is pure logic, testable on any OS. No
//! networking lives here: the engine (`lanroam-core`) carries what
//! [`switch::Switch`] emits to the target.

use thiserror::Error;

pub mod config;
pub mod event;
pub mod geometry;
pub mod inject;
pub mod keymap;
pub mod platform;
pub mod switch;
pub mod world;

pub use event::{InputEvent, MouseButton};
pub use geometry::{Desktop, Edge, Point, Rect};

/// Tag carried by every event Lanroam injects, so that a capture running on
/// the same machine can tell them from real input and leave them alone
pub const INJECTED_MARKER: u32 = 0x4C52_4F4D;

/// Input layer errors
#[derive(Debug, Error)]
pub enum InputError {
    /// Not available on this platform (yet)
    #[error("{0} is not supported on this platform yet")]
    Unsupported(&'static str),
    /// The OS refused access; the message says what to grant
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// An OS call failed
    #[error("{0}")]
    Os(String),
}
