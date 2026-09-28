//! macOS backend.
//!
//! - **Capture**: a Quartz event tap at the HID level, served by a CFRunLoop
//!   on a thread of its own. An active tap (one that may drop
//!   events) needs the Accessibility permission, and keyboard events need
//!   Input Monitoring; macOS grants both to the app that started the
//!   process, i.e. the terminal when running the CLI.
//! - **Displays**: CoreGraphics bounds, in points.
//! - **Injection**: `CGEventPost` at the HID level, where synthetic events
//!   look like hardware ones; needs the Accessibility permission as well.
//! - **Media keys** (volume, play / pause, tracks) are not key events on
//!   macOS but system-defined ones (`NX_SYSDEFINED`), read and made through
//!   `NSEvent`, which alone exposes their fields.
//!
//! While the target is being controlled, the local cursor is hidden and
//! frozen where it crossed, and motion is read from the events' delta
//! fields, which keep reporting movement while the cursor cannot move. This
//! follows Deskflow step by step: a background process only gets reliable
//! control over the cursor once its window server connection has the private
//! "SetsCursorInBackground" property. Warping the cursor back after every
//! motion instead does not work: each warp makes macOS drop mouse events
//! for a moment, and the remote cursor barely moves.

#![allow(unsafe_code)] // CoreGraphics FFI: the tap callback and its context pointer

use std::collections::HashSet;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{NonNull, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_core_foundation::{
    CFBoolean, CFMachPort, CFRetained, CFRunLoop, CFString, CGPoint, kCFBooleanTrue,
    kCFRunLoopDefaultMode,
};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGDirectDisplayID, CGDisplayBounds,
    CGDisplayHideCursor, CGDisplayShowCursor, CGError, CGEvent, CGEventField, CGEventFlags,
    CGEventMask, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CGGetActiveDisplayList, CGMainDisplayID,
    CGMouseButton, CGPreflightListenEventAccess, CGPreflightPostEventAccess,
    CGRequestListenEventAccess, CGRequestPostEventAccess, CGScrollEventUnit,
    CGWarpMouseCursorPosition,
};

use super::{Capture, EmitSink, Permissions, Ready};
use crate::event::{InputEvent, MouseButton};
use crate::geometry::{Point, Rect};
use crate::inject::Injector;
use crate::keymap::{self, usage};
use crate::switch::{CursorAction, Emit, Switch, Verdict};
use crate::{INJECTED_MARKER, InputError};

/// How often the capture thread checks whether it should stop, in seconds
const STOP_POLL_SECS: f64 = 0.2;

/// Most displays one Mac drives
const MAX_DISPLAYS: u32 = 16;

/// How long macOS may hold local mouse events back after the cursor changes
/// hands while it is parked, in seconds (Deskflow's value; the default
/// quarter second makes the first remote motions hesitate)
const PARKED_SUPPRESSION_SECS: f64 = 0.0001;

// Private SkyLight calls, used by Barrier, Deskflow and lan-mouse alike: a
// background process controls the cursor (hiding it, freezing it) reliably
// only once its window server connection has the "SetsCursorInBackground"
// property
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    /// This process's connection to the window server
    fn _CGSDefaultConnection() -> u32;
    /// Set a property of a window server connection
    fn CGSSetConnectionProperty(
        connection: u32,
        target: u32,
        key: &CFString,
        value: &CFBoolean,
    ) -> CGError;
}

/// What to grant when the event tap cannot be created
const PERMISSION_HELP: &str = "allow the app running lanroam (your terminal, for the CLI) under \
    System Settings > Privacy & Security > Accessibility and > Input Monitoring, then restart that app";

/// What to grant when events cannot be posted
const INJECT_PERMISSION_HELP: &str = "allow the app running lanroam (your terminal, for the CLI) \
    under System Settings > Privacy & Security > Accessibility, then restart that app";

/// Presses of the same button closer together than this count as a double
/// (triple, ...) click; macOS's default double-click interval
const MULTI_CLICK_TIME: Duration = Duration::from_millis(500);

/// Presses of the same button further apart than this, in points, start a
/// new click (Deskflow's value; the pointer wobbles slightly while clicking)
const MULTI_CLICK_DISTANCE: f64 = 1.5;

/// Scroll units (1/120 of a notch) per line: one wheel notch scrolls three
/// lines, as it does on Windows (Deskflow's scale)
const UNITS_PER_LINE: i32 = 40;

/// Type of system-defined events (`NX_SYSDEFINED`), which carry the media
/// keys; CoreGraphics does not name it
const SYSTEM_DEFINED: CGEventType = CGEventType(14);

/// Subtype of the system-defined events of media keys
/// (`NX_SUBTYPE_AUX_CONTROL_BUTTONS`)
const AUX_CONTROL_BUTTONS: i16 = 8;

/// Key state of a media key event (`data1` bits 8..16): pressed
const MEDIA_DOWN: isize = 0x0A;

/// Key state of a media key event: released
const MEDIA_UP: isize = 0x0B;

/// Media keys (`NX_KEYTYPE_*`, `ev_keymap.h`) and their HID usages; the
/// first of each usage is the one posted
const MEDIA_KEYS: [(isize, u16); 8] = [
    (0, usage::VOLUME_UP),         // NX_KEYTYPE_SOUND_UP
    (1, usage::VOLUME_DOWN),       // NX_KEYTYPE_SOUND_DOWN
    (7, usage::VOLUME_MUTE),       // NX_KEYTYPE_MUTE
    (16, usage::MEDIA_PLAY_PAUSE), // NX_KEYTYPE_PLAY
    (17, usage::MEDIA_NEXT),       // NX_KEYTYPE_NEXT
    (18, usage::MEDIA_PREVIOUS),   // NX_KEYTYPE_PREVIOUS
    // Apple keyboards send these for their track keys
    (19, usage::MEDIA_NEXT),     // NX_KEYTYPE_FAST
    (20, usage::MEDIA_PREVIOUS), // NX_KEYTYPE_REWIND
];

/// Event types the tap receives
const CAPTURED: [CGEventType; 15] = [
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
    SYSTEM_DEFINED,
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

/// Every modifier bit of the event flags: the generic ones, Caps Lock, and
/// the device-dependent low bits
const MODIFIER_FLAGS: u64 =
    flag::SHIFT | flag::CONTROL | flag::ALTERNATE | flag::COMMAND | 0x0001_0000 | 0xFFFF;

/// Device units per logical pixel, in percent
pub(super) fn scale() -> Result<u32, InputError> {
    // Display bounds are in points, which are logical pixels already
    Ok(100)
}

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

/// Accessibility and Input Monitoring, as granted right now
pub(super) fn permissions() -> Permissions {
    Permissions {
        accessibility: CGPreflightPostEventAccess(),
        input_monitoring: CGPreflightListenEventAccess(),
    }
}

/// List the app under both privacy settings (prompting the first time)
pub(super) fn request_permissions() {
    CGRequestListenEventAccess();
    CGRequestPostEventAccess();
}

/// Start the event tap on its own thread; returns once it runs
pub(super) fn start_capture(
    switch: Arc<Mutex<Switch>>,
    sink: EmitSink,
) -> Result<Capture, InputError> {
    super::spawn_capture(move |stop, ready| capture_thread(switch, sink, stop, ready))
}

/// Body of the capture thread: own the tap context, run the tap, and give
/// the cursor back at the end
fn capture_thread(switch: Arc<Mutex<Switch>>, sink: EmitSink, stop: &AtomicBool, ready: &Ready) {
    let context = Box::into_raw(Box::new(Tap {
        switch,
        sink,
        port: None,
        out: Vec::with_capacity(8),
        parked: false,
        frozen: None,
    }));
    // SAFETY: `context` comes from Box::into_raw and is reclaimed only below,
    // after `run_tap` has returned
    let result = unsafe { run_tap(context, stop, ready) };
    // SAFETY: `run_tap` never created the tap or has invalidated it, so the
    // callback can no longer reach `context`
    let mut tap = unsafe { Box::from_raw(context) };
    // Stopped while controlling the target, or during a drop: give the
    // cursor back
    tap.unpark();
    tap.thaw();
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
unsafe fn run_tap(context: *mut Tap, stop: &AtomicBool, ready: &Ready) -> Result<(), InputError> {
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
        Verdict::Swallow => {
            // Belt and braces: should the event get through anyway, it is
            // now a null event nobody acts on
            CGEvent::set_type(Some(cg_event), CGEventType::Null);
            null_mut()
        }
    }
}

/// Let this background process control the cursor (best effort)
fn allow_background_cursor() {
    let key = CFString::from_static_str("SetsCursorInBackground");
    // SAFETY: reading an immutable CoreFoundation constant
    let Some(yes) = (unsafe { kCFBooleanTrue }) else {
        return;
    };
    // SAFETY: private but long-stable call, with this process's connection
    // and a valid key and value
    let err = unsafe {
        let connection = _CGSDefaultConnection();
        CGSSetConnectionProperty(connection, connection, &key, yes)
    };
    warn_on_error(err, "let the background process control the cursor");
}

/// Set how long local mouse events may be held back after the cursor
/// changes hands
///
/// Deprecated without a replacement for this purpose, yet still honoured;
/// Deskflow relies on it the same way.
#[allow(deprecated)]
fn set_suppression_interval(seconds: f64) {
    let err = objc2_core_graphics::CGSetLocalEventsSuppressionInterval(seconds);
    warn_on_error(err, "set the event suppression interval");
}

/// Put the local cursor at `at`
fn warp(at: Point) {
    let to = CGPoint {
        x: f64::from(at.x),
        y: f64::from(at.y),
    };
    warn_on_error(CGWarpMouseCursorPosition(to), "move the cursor");
}

/// Log a failed CoreGraphics call; cursor handling is best effort
fn warn_on_error(err: CGError, what: &str) {
    if err != CGError::Success {
        tracing::warn!(code = err.0, "cannot {what}");
    }
}

/// State of the event tap, owned by the capture thread
struct Tap {
    /// Decides each event
    switch: Arc<Mutex<Switch>>,
    /// Receives the messages for the target
    sink: EmitSink,
    /// The tap itself, to re-enable it after macOS disabled it
    port: Option<CFRetained<CFMachPort>>,
    /// Reused buffer for the switch's messages
    out: Vec<Emit>,
    /// Whether the local cursor is hidden and frozen (target controlled)
    parked: bool,
    /// Where the local cursor is held, still shown (a drop waits here)
    frozen: Option<Frozen>,
}

/// A local cursor held while a drop waits here
struct Frozen {
    /// Where it is held
    at: Point,
    /// Whether it moved anyway (logged once)
    slipped: bool,
}

impl Tap {
    /// Decide one event
    fn on_event(&mut self, ty: CGEventType, event: &CGEvent) -> Verdict {
        if ty == CGEventType::TapDisabledByTimeout || ty == CGEventType::TapDisabledByUserInput {
            tracing::warn!(kind = ty.0, "macOS disabled the event tap, re-enabling it");
            if let Some(port) = &self.port {
                CGEvent::tap_enable(port, true);
            }
            // Input went straight to the local apps meanwhile: never leave
            // the user with an invisible cursor, and take control back
            if self.parked {
                self.unpark();
                crate::switch::lock(&self.switch).request_release(None);
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
        if let Translated::Event(InputEvent::Motion { at, dx, dy }) = translated {
            if self.parked {
                tracing::trace!(?at, dx, dy, "motion while parked");
            }
            self.hold(at);
        }
        let mut decide = |event| super::decide(&self.switch, event, &mut self.out, &mut self.sink);
        let decision = match translated {
            Translated::Event(input) => decide(Some(input)),
            Translated::Toggle(usage) => {
                let press = decide(Some(InputEvent::Key { usage, down: true }));
                decide(Some(InputEvent::Key { usage, down: false }));
                press
            }
            Translated::Ignored => decide(None),
            Translated::Foreign => return Verdict::Pass,
        };
        match decision.cursor {
            Some(CursorAction::Park) => self.park(),
            Some(CursorAction::Release(at)) => self.release(at),
            Some(CursorAction::Freeze(at)) => self.freeze(at),
            None => {}
        }
        decision.verdict
    }

    /// Hide the local cursor and freeze it where it is (Deskflow's leave)
    fn park(&mut self) {
        if std::mem::replace(&mut self.parked, true) {
            return;
        }
        tracing::debug!("parking the local cursor");
        allow_background_cursor();
        warn_on_error(CGDisplayHideCursor(CGMainDisplayID()), "hide the cursor");
        // Re-associating right after hiding avoids the cursor randomly not
        // hiding (Deskflow)
        CGAssociateMouseAndMouseCursorPosition(true);
        set_suppression_interval(PARKED_SUPPRESSION_SECS);
        warn_on_error(
            CGAssociateMouseAndMouseCursorPosition(false),
            "freeze the cursor",
        );
    }

    /// Keep the local cursor at `at`, still shown: the mouse no longer
    /// moves it (a tap cannot hold it back by dropping its events)
    fn freeze(&mut self, at: Point) {
        tracing::info!(?at, "freezing the local cursor for a drop");
        self.frozen = Some(Frozen { at, slipped: false });
        // As for parking: a background process needs it to hold the cursor
        allow_background_cursor();
        warp(at);
        warn_on_error(
            CGAssociateMouseAndMouseCursorPosition(false),
            "freeze the cursor",
        );
    }

    /// Put a frozen cursor that moved to `at` anyway back where it is held
    /// (macOS may not keep it apart from the mouse during a drag)
    fn hold(&mut self, at: Point) {
        let Some(frozen) = &mut self.frozen else {
            return;
        };
        if at == frozen.at {
            return;
        }
        if !std::mem::replace(&mut frozen.slipped, true) {
            tracing::info!(held = ?frozen.at, ?at, "the frozen cursor moved: warping it back");
        }
        warp(frozen.at);
    }

    /// Give a frozen cursor back to the mouse
    fn thaw(&mut self) {
        if self.frozen.take().is_some() {
            CGAssociateMouseAndMouseCursorPosition(true);
        }
    }

    /// Put the local cursor at `at` and give it back to the mouse
    fn release(&mut self, at: Point) {
        tracing::debug!(?at, "releasing the local cursor");
        self.frozen = None;
        CGAssociateMouseAndMouseCursorPosition(true);
        warp(at);
        self.unpark();
    }

    /// Unfreeze and show the cursor if it is parked (Deskflow's enter);
    /// hiding is counted by macOS, so every hide gets exactly one show
    fn unpark(&mut self) {
        if !std::mem::take(&mut self.parked) {
            return;
        }
        CGAssociateMouseAndMouseCursorPosition(true);
        allow_background_cursor();
        warn_on_error(CGDisplayShowCursor(CGMainDisplayID()), "show the cursor");
        CGAssociateMouseAndMouseCursorPosition(true);
        set_suppression_interval(0.0);
    }
}

/// An injector for this session
///
/// Posting events needs the Accessibility permission; without it macOS
/// drops them silently, so it is checked up front.
pub(super) fn injector() -> Result<Box<dyn Injector>, InputError> {
    if !CGPreflightPostEventAccess() {
        // Makes the app appear in the list, ready to be switched on
        CGRequestPostEventAccess();
        return Err(InputError::PermissionDenied(INJECT_PERMISSION_HELP.into()));
    }
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .ok_or_else(|| InputError::Os("cannot create an event source".into()))?;
    // Every event carries the marker, so a capture on this Mac ignores them
    CGEventSource::set_user_data(Some(&source), i64::from(INJECTED_MARKER));
    // The local mouse and keyboard stay usable while being controlled
    CGEventSource::set_local_events_suppression_interval(Some(&source), 0.0);
    let at = CGEvent::new(None).map_or(CGPoint { x: 0.0, y: 0.0 }, |now| {
        CGEvent::location(Some(&now))
    });
    Ok(Box::new(PostInjector {
        source,
        at,
        buttons: [false; MouseButton::COUNT],
        keys: HashSet::new(),
        media: HashSet::new(),
        clicks: Clicks::default(),
        scroll: (0, 0),
    }))
}

/// Injects with `CGEventPost`
///
/// Like Deskflow, it sets the modifier flags on every event itself (so that
/// Cmd+click and Shift+arrow work and no modifier sticks), sends drag events
/// while a button is held, and counts multi-clicks, since apps read the click
/// count from the event rather than timing clicks themselves.
struct PostInjector {
    /// Source of every event: tagged, without local input suppression
    source: CFRetained<CGEventSource>,
    /// Where the cursor is
    at: CGPoint,
    /// Buttons held
    buttons: [bool; MouseButton::COUNT],
    /// Key codes held
    keys: HashSet<u16>,
    /// Media keys held (HID usages)
    media: HashSet<u16>,
    /// Multi-click counting
    clicks: Clicks,
    /// Scrolling not yet worth a whole line, in 1/120 notch (x, y)
    scroll: (i32, i32),
}

// SAFETY: the event source is only ever used by the thread that currently
// owns the injector, and CoreFoundation objects may move between threads
unsafe impl Send for PostInjector {}

impl PostInjector {
    /// Set the modifier flags of the keys held and post the event
    fn post(&self, event: &CGEvent) {
        let ev = Some(event);
        // Keep what the event says beyond the modifiers (keypad, Fn)
        let other = CGEvent::flags(ev).0 & !MODIFIER_FLAGS;
        let held = MODIFIERS
            .iter()
            .filter(|(code, ..)| self.keys.contains(code))
            .fold(0, |bits, (_, device, generic)| bits | device | generic);
        CGEvent::set_flags(ev, CGEventFlags(other | held));
        CGEvent::post(CGEventTapLocation::HIDEventTap, ev);
    }

    /// The first button held, which decides the drag event type
    fn held_button(&self) -> Option<MouseButton> {
        MouseButton::ALL
            .into_iter()
            .find(|button| self.buttons[button.index()])
    }

    /// Press or release the media key `key` (`NX_KEYTYPE_*`) as the
    /// keyboard's own media keys do, with a system-defined event
    fn post_media(&self, key: isize, down: bool, repeat: bool) -> Result<(), InputError> {
        let state = if down { MEDIA_DOWN } else { MEDIA_UP };
        let data1 = (key << 16) | (state << 8) | isize::from(repeat);
        // NSEvent hands out autoreleased objects; the capture and injection
        // threads have no pool of their own
        autoreleasepool(|_| {
            let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                NSEventType::SystemDefined,
                CGPoint { x: 0.0, y: 0.0 },
                NSEventModifierFlags((state as usize) << 8),
                0.0,
                0,
                None,
                AUX_CONTROL_BUTTONS,
                data1,
                -1,
            )
            .and_then(|event| event.CGEvent())
            .ok_or_else(|| InputError::Os("cannot create a media key event".into()))?;
            let ev = Some(&*event);
            // Not the injector's source: mark it by hand, for the capture
            CGEvent::set_integer_value_field(
                ev,
                CGEventField::EventSourceUserData,
                i64::from(INJECTED_MARKER),
            );
            CGEvent::post(CGEventTapLocation::HIDEventTap, ev);
            Ok(())
        })
    }

    /// A mouse event of `ty` for `button` at the cursor
    fn mouse_event(
        &self,
        ty: CGEventType,
        button: MouseButton,
    ) -> Result<CFRetained<CGEvent>, InputError> {
        CGEvent::new_mouse_event(Some(&self.source), ty, self.at, cg_button(button))
            .ok_or_else(|| InputError::Os("cannot create a mouse event".into()))
    }
}

impl Injector for PostInjector {
    /// Move, or drag while a button is held; the deltas are filled in for
    /// apps that read them (games, 3D tools)
    fn move_to(&mut self, at: Point) -> Result<(), InputError> {
        let to = CGPoint {
            x: f64::from(at.x),
            y: f64::from(at.y),
        };
        let (dx, dy) = (to.x - self.at.x, to.y - self.at.y);
        self.at = to;
        let held = self.held_button();
        let ty = match held {
            None => CGEventType::MouseMoved,
            Some(MouseButton::Left) => CGEventType::LeftMouseDragged,
            Some(MouseButton::Right) => CGEventType::RightMouseDragged,
            Some(_) => CGEventType::OtherMouseDragged,
        };
        let event = self.mouse_event(ty, held.unwrap_or(MouseButton::Left))?;
        let ev = Some(&*event);
        CGEvent::set_double_value_field(ev, CGEventField::MouseEventDeltaX, dx);
        CGEvent::set_double_value_field(ev, CGEventField::MouseEventDeltaY, dy);
        if held.is_some() {
            CGEvent::set_integer_value_field(
                ev,
                CGEventField::MouseEventClickState,
                self.clicks.count,
            );
        }
        self.post(&event);
        Ok(())
    }

    /// Press or release, with the click count of the current series
    fn button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
        if down {
            self.clicks.press(button, self.at);
        }
        let ty = match (button, down) {
            (MouseButton::Left, true) => CGEventType::LeftMouseDown,
            (MouseButton::Left, false) => CGEventType::LeftMouseUp,
            (MouseButton::Right, true) => CGEventType::RightMouseDown,
            (MouseButton::Right, false) => CGEventType::RightMouseUp,
            (_, true) => CGEventType::OtherMouseDown,
            (_, false) => CGEventType::OtherMouseUp,
        };
        let event = self.mouse_event(ty, button)?;
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::MouseEventClickState,
            self.clicks.count,
        );
        self.buttons[button.index()] = down;
        self.post(&event);
        Ok(())
    }

    /// Scroll by whole lines, carrying the remainder over
    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        self.scroll.0 += dx;
        self.scroll.1 += dy;
        let (lines_x, lines_y) = (
            self.scroll.0 / UNITS_PER_LINE,
            self.scroll.1 / UNITS_PER_LINE,
        );
        self.scroll.0 -= lines_x * UNITS_PER_LINE;
        self.scroll.1 -= lines_y * UNITS_PER_LINE;
        if lines_x == 0 && lines_y == 0 {
            return Ok(());
        }
        // macOS counts horizontal scrolling positive to the left
        let event = CGEvent::new_scroll_wheel_event2(
            Some(&self.source),
            CGScrollEventUnit::Line,
            2,
            lines_y,
            -lines_x,
            0,
        )
        .ok_or_else(|| InputError::Os("cannot create a scroll event".into()))?;
        self.post(&event);
        Ok(())
    }

    /// Press or release; modifiers go out as flag changes, like the
    /// hardware sends them (input methods watch those, e.g. Shift to switch
    /// between Chinese and English)
    fn key(&mut self, usage: u16, down: bool) -> Result<(), InputError> {
        if let Some(key) = nx_key(usage) {
            let repeat = down && !self.media.insert(usage);
            if !down {
                self.media.remove(&usage);
            }
            return self.post_media(key, down, repeat);
        }
        let code = keymap::mac_from_usage(usage).ok_or_else(|| {
            InputError::Os(format!("no macOS key code for HID usage {usage:#04x}"))
        })?;
        let repeat = down && !self.keys.insert(code);
        if !down {
            self.keys.remove(&code);
        }
        let event = CGEvent::new_keyboard_event(Some(&self.source), code, down)
            .ok_or_else(|| InputError::Os("cannot create a keyboard event".into()))?;
        let ev = Some(&*event);
        if MODIFIERS.iter().any(|(modifier, ..)| *modifier == code) {
            CGEvent::set_type(ev, CGEventType::FlagsChanged);
        } else if repeat {
            CGEvent::set_integer_value_field(ev, CGEventField::KeyboardEventAutorepeat, 1);
        }
        self.post(&event);
        Ok(())
    }
}

/// The CoreGraphics number of a button
fn cg_button(button: MouseButton) -> CGMouseButton {
    match button {
        MouseButton::Left => CGMouseButton::Left,
        MouseButton::Right => CGMouseButton::Right,
        MouseButton::Middle => CGMouseButton::Center,
        MouseButton::Back => CGMouseButton(3),
        MouseButton::Forward => CGMouseButton(4),
    }
}

/// Counts presses of one button in quick succession, as the hardware path
/// would: 1 for a click, 2 for a double click, ...
#[derive(Debug, Default)]
struct Clicks {
    /// Count of the current series (0 before any press)
    count: i64,
    /// Button, time and position of the series' last press, position of its
    /// first
    last: Option<(MouseButton, Instant, CGPoint)>,
}

impl Clicks {
    /// A press of `button` at `at`: continue the series or start a new one
    fn press(&mut self, button: MouseButton, at: CGPoint) {
        let continues = self.last.is_some_and(|(last, when, first)| {
            last == button
                && when.elapsed() <= MULTI_CLICK_TIME
                && (at.x - first.x).abs() <= MULTI_CLICK_DISTANCE
                && (at.y - first.y).abs() <= MULTI_CLICK_DISTANCE
        });
        if continues {
            self.count += 1;
            if let Some((_, when, _)) = &mut self.last {
                *when = Instant::now();
            }
        } else {
            self.count = 1;
            self.last = Some((button, Instant::now(), at));
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
    /// Not input Lanroam handles at all: it always passes, even while
    /// another device is controlled (system-defined events other than the
    /// media keys, e.g. brightness)
    Foreign,
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
        SYSTEM_DEFINED => media_key(event).map_or(Translated::Foreign, Translated::Event),
        _ => Translated::Ignored,
    }
}

/// The media key a system-defined event reports, if it is one Lanroam
/// forwards
fn media_key(event: &CGEvent) -> Option<InputEvent> {
    // NSEvent hands out autoreleased objects; see `post_media`
    let (subtype, data1) = autoreleasepool(|_| {
        NSEvent::eventWithCGEvent(event).map(|event| (event.subtype().0, event.data1()))
    })?;
    if subtype != AUX_CONTROL_BUTTONS {
        return None;
    }
    let key = (data1 >> 16) & 0xFFFF;
    let down = match (data1 >> 8) & 0xFF {
        MEDIA_DOWN => true,
        MEDIA_UP => false,
        _ => return None,
    };
    let &(_, usage) = MEDIA_KEYS.iter().find(|(nx, _)| *nx == key)?;
    Some(InputEvent::Key { usage, down })
}

/// The `NX_KEYTYPE_*` to post for a media key's HID usage
fn nx_key(usage: u16) -> Option<isize> {
    MEDIA_KEYS
        .iter()
        .find(|(_, media)| *media == usage)
        .map(|&(key, _)| key)
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

    /// Quick presses of one button in one place make a series; another
    /// button, a pause or a move starts over
    #[test]
    fn multi_clicks() {
        let at = CGPoint { x: 10.0, y: 10.0 };
        let mut clicks = Clicks::default();
        clicks.press(MouseButton::Left, at);
        clicks.press(MouseButton::Left, CGPoint { x: 11.0, y: 10.0 });
        clicks.press(MouseButton::Left, at);
        assert_eq!(clicks.count, 3);
        clicks.press(MouseButton::Right, at);
        assert_eq!(clicks.count, 1);
        clicks.press(MouseButton::Right, CGPoint { x: 30.0, y: 10.0 });
        assert_eq!(clicks.count, 1);
    }

    /// Every media key the key map knows can be posted, and posts as the
    /// first code listed
    #[test]
    fn media_keys_are_mapped() {
        for usage in keymap::names().map(|(usage, _)| usage) {
            assert_eq!(
                nx_key(usage).is_some(),
                keymap::is_media(usage),
                "{usage:#x}"
            );
        }
        assert_eq!(nx_key(usage::MEDIA_NEXT), Some(17));
    }

    /// Media key events read back as the keys they were made for
    #[test]
    fn media_key_events() {
        let event = |key: isize, state: isize| {
            autoreleasepool(|_| {
                NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                    NSEventType::SystemDefined,
                    CGPoint { x: 0.0, y: 0.0 },
                    NSEventModifierFlags((state as usize) << 8),
                    0.0,
                    0,
                    None,
                    AUX_CONTROL_BUTTONS,
                    (key << 16) | (state << 8),
                    -1,
                )
                .and_then(|event| event.CGEvent())
                .unwrap()
            })
        };
        assert_eq!(
            media_key(&event(19, MEDIA_DOWN)),
            Some(InputEvent::Key {
                usage: usage::MEDIA_NEXT,
                down: true
            })
        );
        assert_eq!(
            media_key(&event(7, MEDIA_UP)),
            Some(InputEvent::Key {
                usage: usage::VOLUME_MUTE,
                down: false
            })
        );
        // Brightness up: not forwarded
        assert_eq!(media_key(&event(2, MEDIA_DOWN)), None);
    }

    /// Every modifier key code is in the key map
    #[test]
    fn modifiers_are_mapped() {
        for (code, ..) in MODIFIERS {
            assert!(keymap::usage_from_mac(code).is_some(), "{code:#x}");
        }
    }

    /// The injector can post: a move to where the cursor already is changes
    /// nothing on screen. Needs the Accessibility permission, hence ignored
    /// by default (`cargo nextest run --run-ignored all`)
    #[test]
    #[ignore = "needs the Accessibility permission"]
    fn posts_a_harmless_move() {
        let mut injector = injector().unwrap();
        let now = CGEvent::new(None).unwrap();
        let at = CGEvent::location(Some(&now));
        injector.move_to(Point::floor(at.x, at.y)).unwrap();
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
