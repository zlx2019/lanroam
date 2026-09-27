//! Platform backends: capture on the source, injection on the target.
//!
//! | | capture | injection |
//! |---|---|---|
//! | macOS | Quartz event tap | `CGEventPost` |
//! | Windows | low-level hooks | `SendInput` |
//!
//! Other platforms report [`InputError::Unsupported`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use crate::InputError;
use crate::event::InputEvent;
use crate::geometry::Rect;
use crate::inject::Injector;
use crate::switch::{self, Decision, Emit, Switch, Verdict};

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

/// Device units per logical pixel on this machine, in percent: 100 for
/// macOS points, the primary monitor's DPI scale for Windows' physical
/// pixels (150 at 144 DPI)
pub fn scale() -> Result<u32, InputError> {
    imp::scale()
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

/// Readiness report of a capture thread
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
type Ready = mpsc::Sender<Result<(), InputError>>;

/// Run `body` on a new capture thread and wait until it reports whether the
/// capture is running
///
/// `body` polls the stop flag and sends one readiness report; the thread
/// ends when `body` returns.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn spawn_capture(
    body: impl FnOnce(&AtomicBool, &Ready) + Send + 'static,
) -> Result<Capture, InputError> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("lanroam-capture".into())
        .spawn({
            let stop = Arc::clone(&stop);
            move || body(&stop, &ready_tx)
        })
        .map_err(|e| InputError::Os(format!("cannot start the capture thread: {e}")))?;
    let started = ready_rx.recv();
    let capture = Capture {
        stop,
        thread: Some(thread),
    };
    match started {
        Ok(Ok(())) => Ok(capture),
        // Dropping the capture joins the thread
        Ok(Err(e)) => Err(e),
        Err(_) => Err(InputError::Os(
            "the capture thread ended during startup".into(),
        )),
    }
}

/// Decide one captured event through the switch and hand what it emits to
/// the sink
///
/// `None` is an event Lanroam does not forward: it stays local, or is
/// dropped while the target is controlled so it cannot leak to local apps.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn decide(
    switch: &Mutex<Switch>,
    event: Option<InputEvent>,
    out: &mut Vec<Emit>,
    sink: &mut EmitSink,
) -> Decision {
    let decision = {
        let mut switch = switch::lock(switch);
        match event {
            Some(event) => switch.handle(event, out),
            None => Decision {
                verdict: if switch.is_remote() {
                    Verdict::Swallow
                } else {
                    Verdict::Pass
                },
                cursor: None,
            },
        }
    };
    for emit in out.drain(..) {
        sink(emit);
    }
    decision
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
