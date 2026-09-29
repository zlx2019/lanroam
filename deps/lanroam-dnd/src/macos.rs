//! macOS: the drag pasteboard, and two panels on the main thread.
//!
//! - **Probe**: a drag started since the press has written the drag
//!   pasteboard (its change count moved); its file URLs are the files.
//!   The catcher, a panel under the cursor, is a drag destination that
//!   refuses everything: the drag, released there once the pointer went
//!   to another device, slides back and drops nothing.
//! - **Arm**: a small panel at the entry point takes the press injected
//!   there and starts an `NSDraggingSession` of the files, which follows
//!   the injected pointer and drops where the button goes up. Cancelling
//!   moves the catcher under the cursor first, so the release drops
//!   nothing.
//! - **Promises**: armed before its files are all there, the session drags
//!   a file promise of each (`NSFilePromiseProvider`) instead of its URL.
//!   The app it drops on (Finder) names where each goes; once the files
//!   are all there, each is moved there, off the main thread, and the app
//!   told. Only Finder is let take such a drop ([`Dnd::takes`]), and one
//!   whose app never asks for the files counts as not dropped.
//!
//! Both panels are nearly transparent (fully transparent pixels would let
//! the pointer through), sit above everything and never activate the app.
//! The window server takes a moment to put a panel where it was ordered:
//! whatever must land on one waits until a press there would hit it.

// AppKit through objc2: every unsafe block says why it holds
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use block2::{DynBlock, RcBlock};
use dispatch2::{DispatchQueue, DispatchTime};
use lanroam_input::{INJECTED_MARKER, Point};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    AllocAnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class,
    msg_send,
};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSDragOperation, NSDraggingContext, NSDraggingDestination,
    NSDraggingInfo, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent,
    NSFilePromiseProvider, NSFilePromiseProviderDelegate, NSPanel, NSPasteboard,
    NSPasteboardNameDrag, NSPasteboardTypeFileURL, NSPasteboardTypeString, NSPasteboardTypeURL,
    NSPasteboardURLReadingFileURLsOnlyKey, NSPasteboardWriting, NSPopUpMenuWindowLevel, NSView,
    NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType,
    CGMainDisplayID, CGMouseButton, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer, kCGWindowOwnerPID,
};
use objc2_foundation::{
    NSArray, NSCocoaErrorDomain, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol,
    NSOperationQueue, NSPoint, NSRect, NSSize, NSString, NSURL, NSUserCancelledError,
};

use crate::{DndError, Event, Listed, Sink};

/// A drag armed before its files are all there drops at once: it promises
/// them
pub(crate) const DROPS_EARLY: bool = true;

/// How long the promises of a drag are kept after it was armed: as long as
/// the engine keeps its files
const PROMISE_LIFE: Duration = Duration::from_secs(10 * 60);

/// How long the app a promise was dropped on has to ask for the files; one
/// that does not has taken nothing
const PROMISE_GRACE: Duration = Duration::from_secs(3);

/// The program of the app that takes a promise dropped on it
const FINDER: &str = "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder";

/// Type of a promised file, and of a promised folder
const FILE_TYPE: &str = "public.data";
/// See [`FILE_TYPE`]
const FOLDER_TYPE: &str = "public.folder";

/// Tells the app a promise was dropped on whether it was kept (no error)
type Completion = RcBlock<dyn Fn(*mut NSError)>;

/// Side of the catcher, in points: the pointer pushing along an edge
/// stays on it while the probe is answered
const CATCHER_SIZE: f64 = 96.0;

/// Side of the panel taking the press that starts a drag, in points
const SOURCE_SIZE: f64 = 24.0;

/// Side of a file's icon in the drag, in points
const ICON_SIZE: f64 = 48.0;

/// Alpha of the panels: the least that still catches the pointer
const NEARLY_CLEAR: f64 = 1.0 / 255.0;

/// How long the catcher stays after the press it served is over: the
/// release it refuses may still be on its way
const CATCHER_GRACE: Duration = Duration::from_millis(800);

/// How long to wait for the window server to put a panel under a point
/// before going ahead anyway
const HIT_WAIT: Duration = Duration::from_millis(300);

/// How often to ask the window server meanwhile
const HIT_POLL: Duration = Duration::from_millis(4);

/// Drag pasteboard change count before any press was seen
const NO_PRESS: isize = isize::MIN;

thread_local! {
    /// Everything the main thread keeps; set by [`Dnd::start`]
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The main thread's side
struct State {
    /// Where events go
    sink: Arc<Sink>,
    /// The catcher, once created
    catcher: Option<Retained<NSPanel>>,
    /// The panel taking the press that starts a drag, once created
    source: Option<Retained<NSPanel>>,
    /// A drag ready to start at the next press on the source panel: its
    /// id and files, and whether they are all there
    armed: Option<(u64, Vec<PathBuf>, bool)>,
    /// The drag started here, while it runs
    dragging: Option<u64>,
    /// Counts the catcher's showings, so that the delayed hide of an older
    /// one leaves a newer one alone
    showing: u64,
    /// The promises of drags armed before their files were all there, by
    /// id
    promised: HashMap<u64, Promised>,
    /// Apps to tell once a promised item is moved where they want it, by
    /// ticket
    keeping: HashMap<u64, Completion>,
    /// The latest ticket
    tickets: u64,
    /// Shared with the handle: the drags promising their files
    promising: Arc<Mutex<HashSet<u64>>>,
}

/// The promises of a drag armed before its files were all there
struct Promised {
    /// Where each item waits for its files: the stand-ins they fill in
    paths: Vec<PathBuf>,
    /// The files are all there (`true`), or will not come
    delivered: Option<bool>,
    /// The delegate of each item's promise, which its provider only holds
    /// weakly
    sources: Vec<Retained<PromiseSource>>,
    /// Promises asked for before the files were there: the item, where the
    /// app wants it, and how to tell it
    asked: Vec<(usize, PathBuf, Completion)>,
    /// When the drag was armed
    since: Instant,
    /// The app dropped on asked for a promise
    taken: bool,
    /// Dropped, and its end not reported yet: the app has
    /// [`PROMISE_GRACE`] to ask
    unclaimed: bool,
}

/// Handle for the engine; the work happens on the main thread
pub(crate) struct Dnd {
    /// The drag pasteboard's change count at the latest press
    baseline: Arc<AtomicIsize>,
    /// The drags promising their files, by id: whether one does is asked
    /// off the main thread ([`Self::takes`])
    promising: Arc<Mutex<HashSet<u64>>>,
}

impl Dnd {
    /// Set up the main thread's side
    pub(crate) fn start(sink: Sink) -> Result<Self, DndError> {
        let sink = Arc::new(sink);
        let promising = Arc::new(Mutex::new(HashSet::new()));
        let shared = Arc::clone(&promising);
        DispatchQueue::main().exec_async(move || {
            STATE.with_borrow_mut(|state| {
                *state = Some(State {
                    sink,
                    catcher: None,
                    source: None,
                    armed: None,
                    dragging: None,
                    showing: 0,
                    promised: HashMap::new(),
                    keeping: HashMap::new(),
                    tickets: 0,
                    promising: shared,
                });
            });
        });
        Ok(Self {
            baseline: Arc::new(AtomicIsize::new(NO_PRESS)),
            promising,
        })
    }

    /// Note the drag pasteboard's change count: a drag started by this
    /// press will move it. Read here and now, before the drag can start
    pub(crate) fn pressed(&self) {
        let count = objc2::rc::autoreleasepool(|_| drag_pasteboard().changeCount());
        self.baseline.store(count, Ordering::Relaxed);
    }

    /// Answer with the files dragged, and put the catcher under the cursor
    pub(crate) fn probe(&self, id: u64) {
        let baseline = self.baseline.load(Ordering::Relaxed);
        on_main(move |mtm, state| {
            let files = dragged_files(baseline);
            tracing::info!(id, files = files.len(), "probed the drag held here");
            let at = NSEvent::mouseLocation();
            let catcher = state.show_catcher(mtm, at);
            // The drag looks again at what it is over once the catcher is
            // there
            once_hit(catcher, at, |_, _| nudge());
            (state.sink)(Event::Probed { id, files });
        });
    }

    /// Take the catcher away, a moment later
    pub(crate) fn unprobe(&self) {
        on_main(|_, state| state.hide_catcher_later());
    }

    /// Put the source panel at `at`, ready to drag `paths`, or promises of
    /// them unless `ready`
    pub(crate) fn arm(&self, id: u64, at: Point, paths: Vec<PathBuf>, ready: bool) {
        if !ready {
            lock(&self.promising).insert(id);
        }
        on_main(move |mtm, state| {
            state.prune_promises();
            if !ready {
                state.promise(mtm, id, &paths);
            }
            let centre = to_cocoa(at);
            let frame = square(centre, SOURCE_SIZE);
            let panel = state.source.get_or_insert_with(|| {
                // SAFETY: NSView's designated initializer
                let view: Retained<SourceView> =
                    unsafe { msg_send![SourceView::alloc(mtm), initWithFrame: frame] };
                panel(mtm, &view)
            });
            panel.setFrame_display(frame, false);
            panel.orderFrontRegardless();
            let number = panel.windowNumber();
            tracing::info!(id, files = paths.len(), ?at, ready, "armed a drag");
            state.armed = Some((id, paths, ready));
            // The press must land on the panel, not on what lies beneath
            once_hit(number, centre, move |_, state| {
                (state.sink)(Event::Armed { id });
            });
        });
    }

    /// Disarm, or have the drag running here refuse its drop: the catcher
    /// goes under the cursor, where the button will go up
    pub(crate) fn cancel(&self, id: u64) {
        lock(&self.promising).remove(&id);
        on_main(move |mtm, state| {
            if state.armed.as_ref().is_some_and(|(armed, ..)| *armed == id) {
                state.armed = None;
                state.promised.remove(&id);
                if let Some(source) = &state.source {
                    source.orderOut(None);
                }
            }
            tracing::info!(id, "cancelling the drag");
            if state.dragging == Some(id) {
                // The release must land on the catcher, which refuses it
                let at = NSEvent::mouseLocation();
                let catcher = state.show_catcher(mtm, at);
                state.hide_catcher_later();
                // The drag looks again at what it is over, so that the
                // release lands on the catcher, not where it was before
                once_hit(catcher, at, move |_, state| {
                    nudge();
                    (state.sink)(Event::Cancelling { id });
                });
            } else {
                (state.sink)(Event::Cancelling { id });
            }
        });
    }

    /// Nothing to do: a promise names its item alone
    pub(crate) fn listed(&self, _id: u64, _entries: Vec<Listed>) {}

    /// See [`crate::Dnd::takes`]: a promise lands only on Finder
    pub(crate) fn takes(&self, id: u64, at: Point) -> bool {
        let promising = lock(&self.promising).contains(&id);
        !promising || finder_at(at)
    }

    /// Keep the promises of drag `id` asked for so far, or break them
    pub(crate) fn deliver(&self, id: u64, ok: bool) {
        on_main(move |_, state| state.deliver(id, ok));
    }
}

impl State {
    /// Show the catcher centred on `at` (Cocoa screen coordinates); its
    /// window number
    fn show_catcher(&mut self, mtm: MainThreadMarker, at: NSPoint) -> isize {
        let frame = square(at, CATCHER_SIZE);
        let panel = self.catcher.get_or_insert_with(|| {
            // SAFETY: NSView's designated initializer
            let view: Retained<CatchView> =
                unsafe { msg_send![CatchView::alloc(mtm), initWithFrame: frame] };
            // SAFETY: constant strings AppKit defines
            let types = unsafe {
                [
                    NSPasteboardTypeFileURL,
                    NSPasteboardTypeURL,
                    NSPasteboardTypeString,
                ]
            };
            view.registerForDraggedTypes(&NSArray::from_slice(&types));
            panel(mtm, &view)
        });
        panel.setFrame_display(frame, false);
        panel.orderFrontRegardless();
        self.showing += 1;
        panel.windowNumber()
    }

    /// Get ready to promise the items at `paths` for drag `id`
    fn promise(&mut self, mtm: MainThreadMarker, id: u64, paths: &[PathBuf]) {
        let sources = paths
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                PromiseSource::new(mtm, Promise { id, index, name })
            })
            .collect();
        let promised = Promised {
            paths: paths.to_vec(),
            delivered: None,
            sources,
            asked: Vec::new(),
            since: Instant::now(),
            taken: false,
            unclaimed: false,
        };
        self.promised.insert(id, promised);
    }

    /// The session of drag `id` ended, `dropped` or not. A promise dropped
    /// counts once its app asks for it, which it has [`PROMISE_GRACE`] to
    /// do
    fn session_ended(&mut self, id: u64, dropped: bool) {
        lock(&self.promising).remove(&id);
        match self.promised.get_mut(&id) {
            Some(promised) if dropped && !promised.taken => {
                tracing::info!(id, "a promise was dropped, waiting for its app to ask");
                promised.unclaimed = true;
                after(PROMISE_GRACE, move |_, state| state.unclaimed(id));
                return;
            }
            Some(_) if !dropped => {
                self.promised.remove(&id);
            }
            _ => {}
        }
        tracing::info!(id, dropped, "the drag ended");
        (self.sink)(Event::Ended { id, dropped });
    }

    /// The app promise `id` was dropped on had its time to ask: if it did
    /// not, it took nothing
    fn unclaimed(&mut self, id: u64) {
        if !self.promised.get(&id).is_some_and(|p| p.unclaimed) {
            return;
        }
        self.promised.remove(&id);
        tracing::info!(id, "the app dropped on never asked for the promised files");
        (self.sink)(Event::Ended { id, dropped: false });
    }

    /// The app drag `id` dropped on wants item `index` at `to` (if it is
    /// a path): moved there once the files are all there
    fn promise_asked(&mut self, id: u64, index: usize, to: Option<PathBuf>, done: Completion) {
        let Some(promised) = self.promised.get_mut(&id) else {
            tracing::warn!(id, "a promise asked for after it was given up");
            complete(&done, false);
            return;
        };
        let (Some(from), Some(to)) = (promised.paths.get(index).cloned(), to) else {
            complete(&done, false);
            return;
        };
        tracing::info!(id, index, "the app dropped on asks for a promised file");
        promised.taken = true;
        if std::mem::replace(&mut promised.unclaimed, false) {
            tracing::info!(id, "the drag ended");
            (self.sink)(Event::Ended { id, dropped: true });
        }
        let Some(promised) = self.promised.get_mut(&id) else {
            return;
        };
        match promised.delivered {
            None => promised.asked.push((index, to, done)),
            Some(true) => self.keep(from, to, done),
            Some(false) => complete(&done, false),
        }
    }

    /// The files of drag `id` are all there (`ok`), or not coming: keep or
    /// break the promises asked for so far
    fn deliver(&mut self, id: u64, ok: bool) {
        let Some(promised) = self.promised.get_mut(&id) else {
            return;
        };
        promised.delivered = Some(ok);
        let asked = std::mem::take(&mut promised.asked);
        let paths = promised.paths.clone();
        tracing::info!(
            id,
            ok,
            asked = asked.len(),
            "the promised files are delivered"
        );
        for (index, to, done) in asked {
            match paths.get(index) {
                Some(from) if ok => self.keep(from.clone(), to, done),
                _ => complete(&done, false),
            }
        }
    }

    /// Move `from` to `to` off the main thread, then tell the app with
    /// `done`
    fn keep(&mut self, from: PathBuf, to: PathBuf, done: Completion) {
        self.tickets += 1;
        let ticket = self.tickets;
        self.keeping.insert(ticket, done);
        let moving = std::thread::Builder::new()
            .name("lanroam-promise".into())
            .spawn(move || {
                let moved = move_item(&from, &to);
                if let Err(e) = &moved {
                    tracing::warn!(to = %to.display(), "cannot keep a file promise: {e}");
                }
                let ok = moved.is_ok();
                on_main(move |_, state| {
                    if let Some(done) = state.keeping.remove(&ticket) {
                        complete(&done, ok);
                    }
                });
            });
        if let Err(e) = moving {
            tracing::warn!("cannot keep a file promise: {e}");
            if let Some(done) = self.keeping.remove(&ticket) {
                complete(&done, false);
            }
        }
    }

    /// Forget the promises of drags armed too long ago, breaking those
    /// still asked for
    fn prune_promises(&mut self) {
        self.promised.retain(|_, promised| {
            let keep = promised.since.elapsed() < PROMISE_LIFE;
            if !keep {
                for (_, _, done) in promised.asked.drain(..) {
                    complete(&done, false);
                }
            }
            keep
        });
    }

    /// Hide the catcher once [`CATCHER_GRACE`] is over, unless it was shown
    /// again meanwhile
    fn hide_catcher_later(&self) {
        let showing = self.showing;
        let Ok(when) = DispatchTime::try_from(CATCHER_GRACE) else {
            return;
        };
        let _ = DispatchQueue::main().after(when, move || {
            with_state(move |_, state| {
                if state.showing == showing
                    && let Some(catcher) = &state.catcher
                {
                    catcher.orderOut(None);
                }
            });
        });
    }
}

/// Work to do with the state on the main thread
type Work = Box<dyn FnOnce(MainThreadMarker, &mut State) + Send>;

/// Run `then` with the state once a press at `at` (Cocoa screen
/// coordinates) would hit the window numbered `window`, or after
/// [`HIT_WAIT`] regardless
fn once_hit(
    window: isize,
    at: NSPoint,
    then: impl FnOnce(MainThreadMarker, &mut State) + Send + 'static,
) {
    poll_hit(window, at, Instant::now() + HIT_WAIT, Box::new(then));
}

/// Ask the window server again in a moment (see [`once_hit`])
fn poll_hit(window: isize, at: NSPoint, deadline: Instant, then: Work) {
    let Ok(when) = DispatchTime::try_from(HIT_POLL) else {
        return;
    };
    let _ = DispatchQueue::main().after(when, move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let hit = NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(at, 0, mtm) == window;
        if hit || Instant::now() >= deadline {
            if !hit {
                tracing::warn!("a drag panel is not under the pointer in time, going ahead");
            }
            with_state(then);
        } else {
            poll_hit(window, at, deadline, then);
        }
    });
}

/// Run `work` with the state on the main thread, after `delay`
fn after(delay: Duration, work: impl FnOnce(MainThreadMarker, &mut State) + Send + 'static) {
    let Ok(when) = DispatchTime::try_from(delay) else {
        return;
    };
    let _ = DispatchQueue::main().after(when, move || with_state(work));
}

/// `mutex` locked, even after a panic elsewhere
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the window at `at` (Quartz coordinates) is Finder's: a folder,
/// or the desktop. Asked of the window server and the kernel, not AppKit:
/// any thread may ask
fn finder_at(at: Point) -> bool {
    let owner = objc2::rc::autoreleasepool(|_| owner_at(at));
    owner
        .and_then(program_of)
        .is_some_and(|program| program == FINDER)
}

/// The program of process `pid`, if it can be asked
fn program_of(pid: i32) -> Option<String> {
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: a buffer of the size given, which the call fills
    let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    let len = usize::try_from(len).ok().filter(|len| *len > 0)?;
    path.truncate(len);
    String::from_utf8(path).ok()
}

/// The process owning the frontmost window at `at` (Quartz coordinates)
/// that a drop there lands on: an app's window or the desktop. Above them
/// lie the Dock's full-screen window, menus, the dragged image and
/// Lanroam's overlays, which take no drop; Lanroam's own are left out too
fn owner_at(at: Point) -> Option<i32> {
    let list = CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenOnly, kCGNullWindowID)?;
    // SAFETY: an array of dictionaries, toll-free bridged, alive as long as
    // `list`
    let windows: &NSArray = unsafe { &*CFRetained::as_ptr(&list).as_ptr().cast::<NSArray>() };
    let own = i32::try_from(std::process::id()).ok();
    let (x, y) = (f64::from(at.x), f64::from(at.y));
    // Front to back
    windows.iter().find_map(|window| {
        let window = window.downcast::<NSDictionary>().ok()?;
        // SAFETY: constant keys CoreGraphics defines
        let (pid, layer, alpha, bounds) = unsafe {
            (
                kCGWindowOwnerPID,
                kCGWindowLayer,
                kCGWindowAlpha,
                kCGWindowBounds,
            )
        };
        // Normal windows are on layer 0, the desktop below
        if number(&window, layer).is_none_or(|layer| layer.intValue() > 0) {
            return None;
        }
        let pid = number(&window, pid)?.intValue();
        if Some(pid) == own || number(&window, alpha).is_some_and(|a| a.doubleValue() <= 0.0) {
            return None;
        }
        let bounds = window
            .objectForKey(bridged(bounds))?
            .downcast::<NSDictionary>()
            .ok()?;
        let side = |name: &str| {
            let value = bounds.objectForKey(&NSString::from_str(name))?;
            Some(value.downcast::<NSNumber>().ok()?.doubleValue())
        };
        let (left, top) = (side("X")?, side("Y")?);
        let (right, bottom) = (left + side("Width")?, top + side("Height")?);
        (x >= left && x < right && y >= top && y < bottom).then_some(pid)
    })
}

/// The number under `key` in `dictionary`, if it is one
fn number(dictionary: &NSDictionary, key: &CFString) -> Option<Retained<NSNumber>> {
    dictionary.objectForKey(bridged(key))?.downcast().ok()
}

/// A CoreFoundation string as the Foundation string it is
fn bridged(key: &CFString) -> &AnyObject {
    // SAFETY: CFString and NSString are toll-free bridged
    unsafe { &*std::ptr::from_ref(key).cast::<AnyObject>() }
}

/// Run `work` with the state on the main thread, later
fn on_main(work: impl FnOnce(MainThreadMarker, &mut State) + Send + 'static) {
    DispatchQueue::main().exec_async(move || with_state(work));
}

/// Run `work` with the state, on the main thread; again later if the state
/// is busy (AppKit calling back into a view while it is borrowed)
fn with_state(work: impl FnOnce(MainThreadMarker, &mut State) + Send + 'static) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    STATE.with(|cell| match cell.try_borrow_mut() {
        Ok(mut state) => {
            if let Some(state) = state.as_mut() {
                work(mtm, state);
            }
        }
        Err(_) => on_main(work),
    });
}

/// The pasteboard drags write to
fn drag_pasteboard() -> Retained<NSPasteboard> {
    // SAFETY: a constant string AppKit defines
    NSPasteboard::pasteboardWithName(unsafe { NSPasteboardNameDrag })
}

/// The files the current drag holds, if one started since the press whose
/// change count is `baseline`
fn dragged_files(baseline: isize) -> Vec<PathBuf> {
    let pasteboard = drag_pasteboard();
    if baseline == NO_PRESS || pasteboard.changeCount() == baseline {
        return Vec::new();
    }
    let classes = NSArray::from_slice(&[NSURL::class()]);
    let yes = NSNumber::new_bool(true);
    // SAFETY: a constant string AppKit defines
    let key = unsafe { NSPasteboardURLReadingFileURLsOnlyKey };
    let options = NSDictionary::from_slices(&[key], &[&*yes as &AnyObject]);
    // SAFETY: NSURL reads from a pasteboard, with an option it knows
    let Some(objects) =
        (unsafe { pasteboard.readObjectsForClasses_options(&classes, Some(&options)) })
    else {
        return Vec::new();
    };
    objects
        .iter()
        .filter_map(|object| object.downcast::<NSURL>().ok())
        // Finder writes file reference URLs: resolved to paths here
        .filter_map(|url| url.filePathURL()?.path())
        .map(|path| PathBuf::from(path.to_string()))
        .collect()
}

/// Post a drag of the left button where the cursor is, so that the drag
/// session looks again at what lies under it (the catcher, just shown).
/// Tagged like every injected event, so a capture leaves it alone
fn nudge() {
    let Some(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
        return;
    };
    CGEventSource::set_user_data(Some(&source), i64::from(INJECTED_MARKER));
    let Some(now) = CGEvent::new(None) else {
        return;
    };
    let at = CGEvent::location(Some(&now));
    let ty = CGEventType::LeftMouseDragged;
    if let Some(event) = CGEvent::new_mouse_event(Some(&source), ty, at, CGMouseButton::Left) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
}

/// `at` in Cocoa screen coordinates (origin at the bottom left of the main
/// display, y up), from Quartz ones (top left, y down)
fn to_cocoa(at: Point) -> NSPoint {
    let height = CGDisplayBounds(CGMainDisplayID()).size.height;
    NSPoint::new(f64::from(at.x), height - f64::from(at.y))
}

/// A square of side `side` centred on `centre`
fn square(centre: NSPoint, side: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(centre.x - side / 2.0, centre.y - side / 2.0),
        NSSize::new(side, side),
    )
}

/// A panel around `view`: nearly transparent, above everything (full
/// screen apps too), on every space, never activating the app
fn panel(mtm: MainThreadMarker, view: &NSView) -> Retained<NSPanel> {
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        view.frame(),
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    panel.setLevel(NSPopUpMenuWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    panel.setOpaque(false);
    panel.setHasShadow(false);
    panel.setHidesOnDeactivate(false);
    panel.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(1.0, NEARLY_CLEAR)));
    panel.setIgnoresMouseEvents(false);
    // SAFETY: the panel is kept by the state and never closed
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setContentView(Some(view));
    panel
}

define_class!(
    /// The catcher's view: a drag destination refusing everything
    // SAFETY: NSView may be subclassed; nothing to drop
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LanroamDragCatcher"]
    struct CatchView;

    unsafe impl NSObjectProtocol for CatchView {}

    unsafe impl NSDraggingDestination for CatchView {
        /// Nothing may be dropped here
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            tracing::info!("a drag is over the catcher");
            NSDragOperation::None
        }

        /// Still nothing
        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            NSDragOperation::None
        }

        /// Refused
        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag_operation(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            tracing::info!("the catcher refused a drop");
            false
        }

        /// Refused
        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            false
        }
    }
);

define_class!(
    /// The source panel's view: its press starts the drag armed
    // SAFETY: NSView may be subclassed; nothing to drop
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LanroamDragSource"]
    struct SourceView;

    unsafe impl NSObjectProtocol for SourceView {}

    impl SourceView {
        /// The press counts even though the app is not active
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        /// Start the drag armed, if any
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.start_drag(event);
        }
    }

    unsafe impl NSDraggingSource for SourceView {
        /// The files are copied, or moved out of their folder (which goes
        /// anyway): on the same disk, a move takes no time
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy | NSDragOperation::Move
        }

        /// Report how it ended, and clear up
        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, _at: NSPoint, operation: NSDragOperation) {
            let dropped = operation != NSDragOperation::None;
            with_state(move |_, state| {
                let Some(id) = state.dragging.take() else {
                    return;
                };
                if let Some(source) = &state.source {
                    source.orderOut(None);
                }
                state.session_ended(id, dropped);
            });
        }
    }
);

impl SourceView {
    /// Begin dragging the files armed, or their promises, with `event`
    /// (the press)
    fn start_drag(&self, event: &NSEvent) {
        let armed = STATE.with(|cell| {
            let mut state = cell.try_borrow_mut().ok()?;
            let state = state.as_mut()?;
            let (id, paths, _) = state.armed.take()?;
            state.dragging = Some(id);
            let sources = state
                .promised
                .get(&id)
                .map(|promised| promised.sources.clone());
            Some((id, paths, sources))
        });
        let Some((id, paths, sources)) = armed else {
            tracing::info!("a press on the source panel with nothing armed");
            return;
        };
        let items: Vec<Retained<NSDraggingItem>> = match &sources {
            Some(sources) => paths
                .iter()
                .zip(sources)
                .enumerate()
                .map(|(i, (path, source))| {
                    let provider = promise_provider(path, source);
                    dragging_item(path, i, ProtocolObject::from_ref(&*provider))
                })
                .collect(),
            None => paths
                .iter()
                .enumerate()
                .map(|(i, path)| {
                    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                    dragging_item(path, i, ProtocolObject::from_ref(&*url))
                })
                .collect(),
        };
        let items = NSArray::from_retained_slice(&items);
        let session = self.beginDraggingSessionWithItems_event_source(
            &items,
            event,
            ProtocolObject::from_ref(self),
        );
        session.setAnimatesToStartingPositionsOnCancelOrFail(true);
        let promised = sources.is_some();
        tracing::info!(id, files = paths.len(), promised, "the drag started");
    }
}

/// The dragging item of the `i`-th file at `path`: `writer` (its URL, or
/// a promise of it), shown as its icon
fn dragging_item(
    path: &Path,
    i: usize,
    writer: &ProtocolObject<dyn NSPasteboardWriting>,
) -> Retained<NSDraggingItem> {
    let path = NSString::from_str(&path.to_string_lossy());
    let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
    let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
    // Stacked a little apart, as Finder does
    let offset = 4.0 * i as f64;
    let frame = NSRect::new(
        NSPoint::new(offset, -offset),
        NSSize::new(ICON_SIZE, ICON_SIZE),
    );
    // SAFETY: an image is what the contents of a dragging frame may be
    unsafe { item.setDraggingFrame_contents(frame, Some(&icon)) };
    item
}

/// A promise of the file or folder at `path`, kept by `source`
fn promise_provider(path: &Path, source: &PromiseSource) -> Retained<NSFilePromiseProvider> {
    let kind = if path.is_dir() {
        FOLDER_TYPE
    } else {
        FILE_TYPE
    };
    NSFilePromiseProvider::initWithFileType_delegate(
        NSFilePromiseProvider::alloc(),
        &NSString::from_str(kind),
        ProtocolObject::from_ref(source),
    )
}

/// Tell an app whether the promise it was dropped was kept; one not kept is
/// withdrawn as cancelled, which the app takes quietly
fn complete(done: &Completion, ok: bool) {
    if ok {
        done.call((std::ptr::null_mut(),));
        return;
    }
    // SAFETY: a constant string Foundation defines, and no user info
    let error = unsafe {
        NSError::errorWithDomain_code_userInfo(NSCocoaErrorDomain, NSUserCancelledError, None)
    };
    done.call((Retained::as_ptr(&error).cast_mut(),));
}

/// Move what is at `from` to `to`: renamed on the same volume, copied to
/// another
fn move_item(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => copy_item(from, to),
        moved => moved,
    }
}

/// Copy the file or folder at `from` to `to`
fn copy_item(from: &Path, to: &Path) -> io::Result<()> {
    if !from.is_dir() {
        return std::fs::copy(from, to).map(|_| ());
    }
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_item(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

/// Which promise a [`PromiseSource`] keeps
struct Promise {
    /// The drag's id
    id: u64,
    /// The item's place among the drag's
    index: usize,
    /// The item's name, as it lands
    name: String,
}

define_class!(
    /// Keeps the promise of one item of a drag armed before its files were
    /// all there, on the main thread
    // SAFETY: NSObject may be subclassed; nothing to drop but the ivars
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LanroamFilePromise"]
    #[ivars = Promise]
    struct PromiseSource;

    unsafe impl NSObjectProtocol for PromiseSource {}

    unsafe impl NSFilePromiseProviderDelegate for PromiseSource {
        /// The item's name
        #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
        fn file_name(
            &self,
            _provider: &NSFilePromiseProvider,
            _file_type: &NSString,
        ) -> Retained<NSString> {
            NSString::from_str(&self.ivars().name)
        }

        /// The app wants the item at `url`: there once the files are
        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn write_promise(
            &self,
            _provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let Promise { id, index, .. } = *self.ivars();
            let to = url.path().map(|path| PathBuf::from(path.to_string()));
            let done = completion.copy();
            STATE.with(|cell| match cell.try_borrow_mut() {
                Ok(mut state) => match state.as_mut() {
                    Some(state) => state.promise_asked(id, index, to, done),
                    None => complete(&done, false),
                },
                Err(_) => {
                    tracing::warn!(id, "a promise asked for while busy");
                    complete(&done, false);
                }
            });
        }

        /// Asked on the main thread, where the promises are kept
        #[unsafe(method_id(operationQueueForFilePromiseProvider:))]
        fn queue(&self, _provider: &NSFilePromiseProvider) -> Retained<NSOperationQueue> {
            NSOperationQueue::mainQueue()
        }
    }
);

impl PromiseSource {
    /// A delegate keeping `promise`
    fn new(mtm: MainThreadMarker, promise: Promise) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(promise);
        // SAFETY: NSObject's designated initializer
        unsafe { msg_send![super(this), init] }
    }
}
