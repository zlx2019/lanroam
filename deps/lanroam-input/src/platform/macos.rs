//! macOS backend.
//!
//! - **Capture**: a Quartz event tap at the HID level, served by a CFRunLoop
//!   on a thread of its own. An active tap (one that may drop events) needs
//!   the Accessibility permission, and keyboard events need Input
//!   Monitoring; macOS grants both to the app that started the process,
//!   i.e. the terminal when running the CLI.
//! - **Displays**: CoreGraphics bounds, in points.
//! - **Injection**: M1 step 2.
//!
//! While the target is being controlled the local cursor is frozen by
//! disassociating it from the mouse. Motion is then read from the events'
//! delta fields, which keep reporting movement even while the cursor
//! cannot move.

#![allow(unsafe_code)] // CoreGraphics FFI: the tap callback and its context pointer

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use objc2_core_foundation::{CFMachPort, CFRunLoop, CGPoint, kCFRunLoopDefaultMode};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGDirectDisplayID, CGDisplayBounds, CGError, CGEvent,
    CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CGGetActiveDisplayList, CGRequestListenEventAccess,
    CGRequestPostEventAccess, CGWarpMouseCursorPosition,
};

use super::{Capture, EmitSink};
use crate::event::{InputEvent, MouseButton};
use crate::geometry::{Point, Rect};
use crate::inject::Injector;
use crate::keymap::{self, usage};
use crate::switch::{self, CursorAction, Decision, Emit, Switch, Verdict};
use crate::{INJECTED_MARKER, InputError};

/// How often the capture thread checks whether it should stop, in seconds
const STOP_POLL_SECS: f64 = 0.2;

/// Most displays one Mac drives
const MAX_DISPLAYS: u32 = 16;

/// What to grant when the event tap cannot be created
const PERMISSION_HELP: &str = "allow the app running lanroam (your terminal, for the CLI) under \
    System Settings > Privacy & Security > Accessibility and > Input Monitoring, then restart that app";

/// Event types the tap receives
const CAPTURED: [CGEventType; 14] = [
    CGEventType::MouseMoved,
    CGEventType::LeftMouseDown,
    CGEventType::LeftMouseUp,
    CGEventType::RightMouseDown,
    CGEventType::RightMouseUp,
    CGEventType::OtherMouseDown,
    CGEventType::OtherMouseUp,
    CGEventType::LeftMouseDragged,
    CGEventType::RightMouseDragged,
    CGEventType::OtherMouseDragged,
    CGEventType::ScrollWheel,
    CGEventType::KeyDown,
    CGEventType::KeyUp,
    CGEventType::FlagsChanged,
];

/// Generic modifier flags (`CGEventFlags`)
mod flag {
    /// Either Shift
    pub(super) const SHIFT: u64 = 0x0002_0000;
    /// Either Control
    pub(super) const CONTROL: u64 = 0x0004_0000;
    /// Either Option
    pub(super) const ALTERNATE: u64 = 0x0008_0000;
    /// Either Command
    pub(super) const COMMAND: u64 = 0x0010_0000;
}

/// Modifier keys: key code, the device-dependent flag set while that very
/// key is down (`NX_DEVICE*KEYMASK` in IOLLEvent.h), and its generic flag
const MODIFIERS: [(u16, u64, u64); 8] = [
    (0x3B, 0x0000_0001, flag::CONTROL),   // left control
    (0x3E, 0x0000_2000, flag::CONTROL),   // right control
    (0x38, 0x0000_0002, flag::SHIFT),     // left shift
    (0x3C, 0x0000_0004, flag::SHIFT),     // right shift
    (0x37, 0x0000_0008, flag::COMMAND),   // left command
    (0x36, 0x0000_0010, flag::COMMAND),   // right command
    (0x3A, 0x0000_0020, flag::ALTERNATE), // left option
    (0x3D, 0x0000_0040, flag::ALTERNATE), // right option
];

/// Key code of Caps Lock, which only reports toggles
const CAPS_LOCK_CODE: u16 = 0x39;

/// Bounds of the active displays, in points
pub(super) fn displays() -> Result<Vec<Rect>, InputError> {
    let mut ids = [CGDirectDisplayID::default(); MAX_DISPLAYS as usize];
    let mut count = 0u32;
    // SAFETY: `ids` has room for MAX_DISPLAYS entries; both pointers are valid
    let err = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count) };
    if err != CGError::Success {
        return Err(InputError::Os(format!(
            "CGGetActiveDisplayList failed ({})",
            err.0
        )));
    }
    Ok(ids
        .iter()
        .take(count as usize)
        .map(|&id| {
            let b = CGDisplayBounds(id);
            Rect::new(
                b.origin.x.round() as i32,
                b.origin.y.round() as i32,
                b.size.width.round() as i32,
                b.size.height.round() as i32,
            )
        })
        .collect())
}

/// Injection arrives in M1 step 2
pub(super) fn injector() -> Result<Box<dyn Injector>, InputError> {
    Err(InputError::Unsupported("input injection on macOS"))
}

/// Start the event tap on its own thread; returns once it runs
pub(super) fn start_capture(
    switch: Arc<Mutex<Switch>>,
    sink: EmitSink,
) -> Result<Capture, InputError> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("lanroam-capture".into())
        .spawn({
            let stop = Arc::clone(&stop);
            move || capture_thread(switch, sink, &stop, &ready_tx)
        })
        .map_err(|e| InputError::Os(format!("cannot start the capture thread: {e}")))?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(Capture::new(stop, thread)),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(InputError::Os(
                "the capture thread ended during startup".into(),
            ))
        }
    }
}

/// Body of the capture thread: own the tap context, run the tap, and give
/// the cursor back at the end
fn capture_thread(
    switch: Arc<Mutex<Switch>>,
    sink: EmitSink,
    stop: &AtomicBool,
    ready: &mpsc::Sender<Result<(), InputError>>,
) {
    let context = Box::into_raw(Box::new(Tap {
        switch,
        sink,
        port: None,
        out: Vec::with_capacity(8),
        parked_at: None,
        drift_reported: false,
    }));
    // SAFETY: `context` comes from Box::into_raw and is reclaimed only below,
    // after `run_tap` has returned
    let result = unsafe { run_tap(context, stop, ready) };
    // SAFETY: `run_tap` never created the tap or has invalidated it, so the
    // callback can no longer reach `context`
    let tap = unsafe { Box::from_raw(context) };
    if tap.parked_at.is_some() {
        // Stopped while controlling the target: free the cursor where it is
        CGAssociateMouseAndMouseCursorPosition(true);
    }
    if let Err(e) = result {
        let _ = ready.send(Err(e));
    }
}

/// Create the tap, report readiness, and serve events until `stop`
///
/// # Safety
///
/// `context` must point to a live `Tap` that nothing else accesses until
/// this function returns. The tap is invalidated before returning, so the
/// callback never outlives the call.
unsafe fn run_tap(
    context: *mut Tap,
    stop: &AtomicBool,
    ready: &mpsc::Sender<Result<(), InputError>>,
) -> Result<(), InputError> {
    let mask: CGEventMask = CAPTURED.iter().fold(0, |mask, ty| mask | (1 << ty.0));
    // SAFETY: `tap_callback` follows the CGEventTapCallBack contract and
    // `context` stays valid while the tap exists (see above)
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            mask,
            Some(tap_callback),
            context.cast(),
        )
    };
    let Some(port) = port else {
        // Makes the app appear in both privacy lists, ready to be switched on
        CGRequestListenEventAccess();
        CGRequestPostEventAccess();
        return Err(InputError::PermissionDenied(PERMISSION_HELP.into()));
    };
    let source = CFMachPort::new_run_loop_source(None, Some(&port), 0);
    let (Some(source), Some(run_loop)) = (source, CFRunLoop::current()) else {
        port.invalidate();
        return Err(InputError::Os(
            "cannot attach the event tap to a run loop".into(),
        ));
    };
    // SAFETY: reading an immutable CoreFoundation constant
    let mode = unsafe { kCFRunLoopDefaultMode };
    // SAFETY: no callback runs outside `run_in_mode`, so this write does not
    // race with one
    unsafe { (*context).port = Some(port.clone()) };
    run_loop.add_source(Some(&source), mode);
    CGEvent::tap_enable(&port, true);
    let _ = ready.send(Ok(()));

    while !stop.load(Ordering::Relaxed) {
        CFRunLoop::run_in_mode(mode, STOP_POLL_SECS, false);
    }

    CGEvent::tap_enable(&port, false);
    run_loop.remove_source(Some(&source), mode);
    port.invalidate();
    Ok(())
}

/// The event tap callback
///
/// # Safety
///
/// Called by CoreGraphics on the capture thread with the `Tap` passed to
/// `tap_create` as `context`, while the tap exists.
unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    ty: CGEventType,
    event: NonNull<CGEvent>,
    context: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: see above; the run loop serves one callback at a time and
    // nothing else touches the Tap meanwhile
    let tap = unsafe { &mut *context.cast::<Tap>() };
    // SAFETY: the event is valid for the duration of the callback
    let cg_event = unsafe { event.as_ref() };
    // A panic must not unwind into CoreGraphics. The event then passes, so
    // a bug fails open instead of eating the user's input
    let verdict =
        catch_unwind(AssertUnwindSafe(|| tap.on_event(ty, cg_event))).unwrap_or(Verdict::Pass);
    match verdict {
        Verdict::Pass => event.as_ptr(),
        Verdict::Swallow => null_mut(),
    }
}

/// State of the event tap, owned by the capture thread
struct Tap {
    /// Decides each event
    switch: Arc<Mutex<Switch>>,
    /// Receives the messages for the target
    sink: EmitSink,
    /// The tap itself, to re-enable it after macOS disabled it
    port: Option<objc2_core_foundation::CFRetained<CFMachPort>>,
    /// Reused buffer for the switch's messages
    out: Vec<Emit>,
    /// Where the local cursor is frozen while controlling the target
    parked_at: Option<Point>,
    /// Whether a moving parked cursor was already reported
    drift_reported: bool,
}

impl Tap {
    /// Decide one event
    fn on_event(&mut self, ty: CGEventType, event: &CGEvent) -> Verdict {
        if ty == CGEventType::TapDisabledByTimeout || ty == CGEventType::TapDisabledByUserInput {
            tracing::warn!(kind = ty.0, "macOS disabled the event tap, re-enabling it");
            if let Some(port) = &self.port {
                CGEvent::tap_enable(port, true);
            }
            return Verdict::Pass;
        }
        let ev = Some(event);
        if CGEvent::integer_value_field(ev, CGEventField::EventSourceUserData)
            == i64::from(INJECTED_MARKER)
        {
            return Verdict::Pass;
        }
        let translated = translate(ty, event);
        if let Translated::Event(InputEvent::Motion { at, .. }) = translated {
            self.check_parked(at);
        }
        let decision = {
            let mut switch = switch::lock(&self.switch);
            match translated {
                Translated::Event(input) => switch.handle(input, &mut self.out),
                Translated::Toggle(usage) => {
                    let press = switch.handle(InputEvent::Key { usage, down: true }, &mut self.out);
                    switch.handle(InputEvent::Key { usage, down: false }, &mut self.out);
                    press
                }
                // Something Lanroam does not forward: it must not leak to the
                // local apps while the target is being controlled either
                Translated::Ignored => Decision {
                    verdict: if switch.is_remote() {
                        Verdict::Swallow
                    } else {
                        Verdict::Pass
                    },
                    cursor: None,
                },
            }
        };
        for emit in self.out.drain(..) {
            (self.sink)(emit);
        }
        match decision.cursor {
            Some(CursorAction::Park) => {
                let p = CGEvent::location(ev);
                self.park(Point::floor(p.x, p.y));
            }
            Some(CursorAction::Release(at)) => self.release(at),
            None => {}
        }
        decision.verdict
    }

    /// Freeze the local cursor: moving the mouse no longer moves it, while
    /// the events keep their deltas
    fn park(&mut self, at: Point) {
        let err = CGAssociateMouseAndMouseCursorPosition(false);
        if err != CGError::Success {
            tracing::warn!(code = err.0, "could not freeze the cursor");
        }
        self.parked_at = Some(at);
    }

    /// Unfreeze the local cursor and put it at `at`
    fn release(&mut self, at: Point) {
        self.parked_at = None;
        CGAssociateMouseAndMouseCursorPosition(true);
        CGWarpMouseCursorPosition(CGPoint {
            x: f64::from(at.x),
            y: f64::from(at.y),
        });
        // Re-associating right after a warp cancels the quarter second during
        // which macOS would otherwise ignore mouse movement
        CGAssociateMouseAndMouseCursorPosition(true);
    }

    /// Report once if the frozen cursor moves anyway (diagnostics for the
    /// M1 prototype: the freeze is not documented for background processes)
    fn check_parked(&mut self, at: Point) {
        if let Some(parked) = self.parked_at
            && at != parked
            && !self.drift_reported
        {
            self.drift_reported = true;
            tracing::warn!(?parked, now = ?at, "the local cursor moves while controlling the target");
        }
    }
}

/// What a CoreGraphics event means for the switch
enum Translated {
    /// One input event
    Event(InputEvent),
    /// A key that only reports toggles (Caps Lock): a press and a release
    Toggle(u16),
    /// Nothing Lanroam forwards (Fn, unknown keys and buttons, ...)
    Ignored,
}

/// Translate a CoreGraphics event
fn translate(ty: CGEventType, event: &CGEvent) -> Translated {
    let ev = Some(event);
    let button = |button, down| Translated::Event(InputEvent::Button { button, down });
    match ty {
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            let p = CGEvent::location(ev);
            Translated::Event(InputEvent::Motion {
                at: Point::floor(p.x, p.y),
                dx: CGEvent::double_value_field(ev, CGEventField::MouseEventDeltaX),
                dy: CGEvent::double_value_field(ev, CGEventField::MouseEventDeltaY),
            })
        }
        CGEventType::LeftMouseDown => button(MouseButton::Left, true),
        CGEventType::LeftMouseUp => button(MouseButton::Left, false),
        CGEventType::RightMouseDown => button(MouseButton::Right, true),
        CGEventType::RightMouseUp => button(MouseButton::Right, false),
        CGEventType::OtherMouseDown | CGEventType::OtherMouseUp => {
            let down = ty == CGEventType::OtherMouseDown;
            match CGEvent::integer_value_field(ev, CGEventField::MouseEventButtonNumber) {
                2 => button(MouseButton::Middle, down),
                3 => button(MouseButton::Back, down),
                4 => button(MouseButton::Forward, down),
                _ => Translated::Ignored,
            }
        }
        CGEventType::ScrollWheel => {
            // Lines as 16.16 fixed point (fractional for trackpads); macOS
            // counts horizontal scrolling positive to the left
            let lines_y =
                CGEvent::double_value_field(ev, CGEventField::ScrollWheelEventFixedPtDeltaAxis1);
            let lines_x =
                CGEvent::double_value_field(ev, CGEventField::ScrollWheelEventFixedPtDeltaAxis2);
            let (dx, dy) = (
                (-lines_x * 120.0).round() as i32,
                (lines_y * 120.0).round() as i32,
            );
            // Trackpad phase changes (begin, end of momentum) scroll nothing
            if dx == 0 && dy == 0 {
                return Translated::Ignored;
            }
            Translated::Event(InputEvent::Wheel { dx, dy })
        }
        CGEventType::KeyDown | CGEventType::KeyUp => {
            match keymap::usage_from_mac(key_code(event)) {
                Some(usage) => Translated::Event(InputEvent::Key {
                    usage,
                    down: ty == CGEventType::KeyDown,
                }),
                None => Translated::Ignored,
            }
        }
        CGEventType::FlagsChanged => {
            let code = key_code(event);
            if code == CAPS_LOCK_CODE {
                return Translated::Toggle(usage::CAPS_LOCK);
            }
            match (
                keymap::usage_from_mac(code),
                modifier_down(code, CGEvent::flags(ev).0),
            ) {
                (Some(usage), Some(down)) => Translated::Event(InputEvent::Key { usage, down }),
                _ => Translated::Ignored,
            }
        }
        _ => Translated::Ignored,
    }
}

/// Virtual key code of a keyboard event
fn key_code(event: &CGEvent) -> u16 {
    let code = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode);
    u16::try_from(code).unwrap_or(u16::MAX)
}

/// Whether the modifier with key code `code` is down after a FlagsChanged
/// event with `flags`; `None` if `code` is not a modifier
///
/// The device-dependent bit tells left from right. Events synthesized
/// without those bits fall back to the generic flag.
fn modifier_down(code: u16, flags: u64) -> Option<bool> {
    let &(_, bit, generic) = MODIFIERS.iter().find(|m| m.0 == code)?;
    let family = MODIFIERS
        .iter()
        .filter(|m| m.2 == generic)
        .fold(0, |bits, m| bits | m.1);
    Some(if flags & family != 0 {
        flags & bit != 0
    } else {
        flags & generic != 0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Left and right modifiers are told apart by their device bits, with
    /// the generic flag as fallback
    #[test]
    fn modifier_state() {
        // Left shift down, right shift not
        let flags = flag::SHIFT | 0x02;
        assert_eq!(modifier_down(0x38, flags), Some(true));
        assert_eq!(modifier_down(0x3C, flags), Some(false));
        // Released: nothing set
        assert_eq!(modifier_down(0x38, 0), Some(false));
        // No device bits at all: the generic flag decides
        assert_eq!(modifier_down(0x3E, flag::CONTROL), Some(true));
        // Not a modifier
        assert_eq!(modifier_down(0x00, flag::SHIFT), None);
    }

    /// Every modifier key code is in the key map
    #[test]
    fn modifiers_are_mapped() {
        for (code, ..) in MODIFIERS {
            assert!(keymap::usage_from_mac(code).is_some(), "{code:#x}");
        }
    }

    /// The display list is readable (every Mac, even headless CI runners,
    /// has at least one display)
    #[test]
    fn lists_displays() {
        let displays = displays().unwrap();
        assert!(!displays.is_empty());
        assert!(displays.iter().all(|d| !d.is_empty()));
    }
}
