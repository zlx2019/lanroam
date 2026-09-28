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
//!
//! Both panels are nearly transparent (fully transparent pixels would let
//! the pointer through), sit above everything and never activate the app.

// AppKit through objc2: every unsafe block says why it holds
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchTime};
use lanroam_input::{INJECTED_MARKER, Point};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AllocAnyThread, ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSDragOperation, NSDraggingContext, NSDraggingDestination,
    NSDraggingInfo, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent, NSPanel,
    NSPasteboard, NSPasteboardNameDrag, NSPasteboardTypeFileURL, NSPasteboardTypeString,
    NSPasteboardTypeURL, NSPasteboardURLReadingFileURLsOnlyKey, NSPopUpMenuWindowLevel, NSView,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType,
    CGMainDisplayID, CGMouseButton,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};

use crate::{DndError, Event, Sink};

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
    /// id and files
    armed: Option<(u64, Vec<PathBuf>)>,
    /// The drag started here, while it runs
    dragging: Option<u64>,
    /// Counts the catcher's showings, so that the delayed hide of an older
    /// one leaves a newer one alone
    showing: u64,
}

/// Handle for the engine; the work happens on the main thread
pub(crate) struct Dnd {
    /// The drag pasteboard's change count at the latest press
    baseline: Arc<AtomicIsize>,
}

impl Dnd {
    /// Set up the main thread's side
    pub(crate) fn start(sink: Sink) -> Result<Self, DndError> {
        let sink = Arc::new(sink);
        DispatchQueue::main().exec_async(move || {
            STATE.with_borrow_mut(|state| {
                *state = Some(State {
                    sink,
                    catcher: None,
                    source: None,
                    armed: None,
                    dragging: None,
                    showing: 0,
                });
            });
        });
        Ok(Self {
            baseline: Arc::new(AtomicIsize::new(NO_PRESS)),
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
            state.show_catcher(mtm, NSEvent::mouseLocation());
            nudge();
            (state.sink)(Event::Probed { id, files });
        });
    }

    /// Take the catcher away, a moment later
    pub(crate) fn unprobe(&self) {
        on_main(|_, state| state.hide_catcher_later());
    }

    /// Put the source panel at `at`, ready to drag `paths`
    pub(crate) fn arm(&self, id: u64, at: Point, paths: Vec<PathBuf>) {
        on_main(move |mtm, state| {
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
            tracing::info!(id, files = paths.len(), ?at, "armed a drag");
            state.armed = Some((id, paths));
            (state.sink)(Event::Armed { id });
        });
    }

    /// Disarm, or have the drag running here refuse its drop: the catcher
    /// goes under the cursor, where the button will go up
    pub(crate) fn cancel(&self, id: u64) {
        on_main(move |mtm, state| {
            if state.armed.as_ref().is_some_and(|(armed, _)| *armed == id) {
                state.armed = None;
                if let Some(source) = &state.source {
                    source.orderOut(None);
                }
            }
            if state.dragging == Some(id) {
                state.show_catcher(mtm, NSEvent::mouseLocation());
                state.hide_catcher_later();
            }
            tracing::info!(id, "cancelling the drag");
            (state.sink)(Event::Cancelling { id });
        });
    }
}

impl State {
    /// Show the catcher centred on `at` (Cocoa screen coordinates)
    fn show_catcher(&mut self, mtm: MainThreadMarker, at: NSPoint) {
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
        /// The files are copied wherever they go
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
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
                tracing::info!(id, dropped, "the drag ended");
                (state.sink)(Event::Ended { id, dropped });
            });
        }
    }
);

impl SourceView {
    /// Begin dragging the files armed, with `event` (the press)
    fn start_drag(&self, event: &NSEvent) {
        let armed = STATE.with(|cell| {
            let mut state = cell.try_borrow_mut().ok()?;
            let state = state.as_mut()?;
            let (id, paths) = state.armed.take()?;
            state.dragging = Some(id);
            Some((id, paths))
        });
        let Some((id, paths)) = armed else {
            tracing::info!("a press on the source panel with nothing armed");
            return;
        };
        let items: Vec<Retained<NSDraggingItem>> = paths
            .iter()
            .enumerate()
            .map(|(i, path)| dragging_item(path, i))
            .collect();
        let items = NSArray::from_retained_slice(&items);
        let session = self.beginDraggingSessionWithItems_event_source(
            &items,
            event,
            ProtocolObject::from_ref(self),
        );
        session.setAnimatesToStartingPositionsOnCancelOrFail(true);
        tracing::info!(id, files = paths.len(), "the drag started");
    }
}

/// The dragging item of the `i`-th file: its URL, shown as its icon
fn dragging_item(path: &Path, i: usize) -> Retained<NSDraggingItem> {
    let path = NSString::from_str(&path.to_string_lossy());
    let url = NSURL::fileURLWithPath(&path);
    let writer = ProtocolObject::from_ref(&*url);
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
