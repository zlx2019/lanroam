//! Platform backends: capture on the source, injection on the target.
//!
//! | | capture | injection |
//! |---|---|---|
//! | macOS | Quartz event tap | (M1 step 2) |
//! | Windows | (M1 step 2) | `SendInput` |
//!
//! Anything missing reports [`InputError::Unsupported`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::InputError;
use crate::geometry::Rect;
use crate::inject::Injector;
use crate::switch::{Emit, Switch};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(not(any(target_os = "macos", windows)))]
mod unsupported;
#[cfg(not(any(target_os = "macos", windows)))]
use unsupported as imp;

/// Receives the switch's messages for the target
///
/// Called on the capture thread while the OS waits for a verdict, so it
/// must hand the message off and return (an unbounded channel send).
pub type EmitSink = Box<dyn FnMut(Emit) + Send>;

/// Bounds of this machine's displays, in desktop coordinates
pub fn displays() -> Result<Vec<Rect>, InputError> {
    imp::displays()
}

/// Start capturing local input, deciding each event with `switch`
///
/// Fails with [`InputError::PermissionDenied`] when the OS withholds the
/// right to observe input, with a message saying what to grant.
pub fn start_capture(switch: Arc<Mutex<Switch>>, sink: EmitSink) -> Result<Capture, InputError> {
    imp::start_capture(switch, sink)
}

/// An injector for this machine's session
pub fn injector() -> Result<Box<dyn Injector>, InputError> {
    imp::injector()
}

/// A running capture thread
///
/// Dropping it stops the capture and gives the local cursor back.
pub struct Capture {
    /// Asks the thread to stop
    stop: Arc<AtomicBool>,
    /// The capture thread
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Wrap a capture thread that polls `stop`
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn new(stop: Arc<AtomicBool>, thread: JoinHandle<()>) -> Self {
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Capture {
    /// Stop the thread and wait until it has restored the cursor
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
