//! lanroam-dnd: native drag and drop for drags of files carried from one
//! device to another.
//!
//! ```text
//! ┌─ probe  ─ the device the drag starts on: whether the left button held
//! │           drags files, and which; a catcher under the cursor refuses
//! │           the drop meanwhile, so that releasing the drag there (once
//! │           the pointer is elsewhere) drops nothing
//! └─ arm    ─ the device the pointer takes it to: the next left press at
//!             the entry point starts a native drag of the files, which the
//!             user drops wherever the button goes up
//! ```
//!
//! | | probe | drag |
//! |---|---|---|
//! | macOS | the drag pasteboard (changed since the press) | `NSDraggingSession` from a panel under the cursor |
//! | Windows | `IDropTarget::DragEnter` on a window under the cursor | `SHDoDragDrop` from a window under the cursor |
//!
//! A drag armed before its files are all there promises them instead
//! ([`Dnd::drops_early`]): it drops as soon as the button goes up, and the
//! app it lands on gets them once they are ([`Dnd::deliver`]), in the
//! background: a file promise on macOS, virtual files on Windows, which
//! lists them first ([`Dnd::listed`]) and adds the files themselves once
//! they are there. Only the file manager (Finder, Explorer, the desktop)
//! is let take such a drop ([`Dnd::takes`]); others may accept it and take
//! nothing, so a drop counts only once its app asks for the files.
//!
//! Windows used here are transparent, above everything and never take
//! focus. On macOS they live on the main thread, which must run the app's
//! event loop; on Windows on a thread of their own. No networking lives
//! here: the engine (`lanroam-core`) decides when a drag goes where.

use std::path::PathBuf;

use lanroam_input::Point;
use thiserror::Error;

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

/// What happens to the drags, reported to the [`Sink`]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Whether the left button held here drags files ([`Dnd::probe`]):
    /// their paths, none if it drags anything else or nothing
    Probed {
        /// The probe's id
        id: u64,
        /// The files and folders dragged
        files: Vec<PathBuf>,
    },
    /// Ready to drag ([`Dnd::arm`]): the next left press at its point
    /// starts the drag
    Armed {
        /// The drag's id
        id: u64,
    },
    /// The drag being cancelled refuses its drop from now on
    /// ([`Dnd::cancel`]): the button may go up
    Cancelling {
        /// The drag's id
        id: u64,
    },
    /// The drag armed with this id ended: dropped somewhere, or not
    Ended {
        /// The drag's id
        id: u64,
        /// Something took the files (a promise: its app asked for them)
        dropped: bool,
    },
}

/// Receives what happens to the drags; called on any thread
pub type Sink = Box<dyn Fn(Event) + Send + Sync>;

/// A file of a drag lands under its name with this appended, and takes its
/// name once it is whole: an app its drop promised it to may read it as it
/// arrives
pub const PART_SUFFIX: &str = ".lanroam.part";

/// A file or folder a drag carries, as it lands ([`Dnd::listed`])
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// Where it lands, under the folder of the paths armed
    pub path: PathBuf,
    /// Its bytes (none for a folder)
    pub size: u64,
    /// A folder
    pub dir: bool,
}

/// Drag and drop errors
#[derive(Debug, Error)]
pub enum DndError {
    /// Not available on this platform
    #[error("dragging files between devices is not supported on this platform")]
    Unsupported,
    /// An OS call failed
    #[error("{0}")]
    Os(String),
}

/// The native drag and drop of this machine
///
/// Every call returns at once; what comes of it is reported to the sink.
pub struct Dnd {
    /// The platform's side
    imp: imp::Dnd,
}

impl Dnd {
    /// Start, reporting to `sink`
    pub fn start(sink: Sink) -> Result<Self, DndError> {
        Ok(Self {
            imp: imp::Dnd::start(sink)?,
        })
    }

    /// The left button went down here: a drag may start from now on
    pub fn pressed(&self) {
        self.imp.pressed();
    }

    /// Whether the left button held here drags files ([`Event::Probed`]);
    /// meanwhile a catcher under the cursor refuses any drop
    pub fn probe(&self, id: u64) {
        self.imp.probe(id);
    }

    /// The press probed is over: the catcher goes, a moment later (the
    /// release it refuses may still be on its way)
    pub fn unprobe(&self) {
        self.imp.unprobe();
    }

    /// Get ready to drag `paths` from `at` (this machine's coordinates,
    /// see `lanroam_input::platform::displays`): the next left press there
    /// starts the drag ([`Event::Armed`] once ready). Unless `ready` (their
    /// files all there), the drag promises them
    pub fn arm(&self, id: u64, at: Point, paths: Vec<PathBuf>, ready: bool) {
        self.imp.arm(id, at, paths, ready);
    }

    /// Everything the drag armed with `id` carries, folders before what
    /// they hold, in the order the files arrive: what an app it drops on
    /// before they are there learns is coming
    pub fn listed(&self, id: u64, entries: Vec<Listed>) {
        self.imp.listed(id, entries);
    }

    /// Cancel the drag armed with `id`: it drops nothing when the button
    /// goes up ([`Event::Cancelling`] once it may)
    pub fn cancel(&self, id: u64) {
        self.imp.cancel(id);
    }

    /// Whether the drag armed with `id`, let go at `at` (this machine's
    /// coordinates), lands where it can: anywhere when it drags the files
    /// themselves; while it promises them, only on the file manager, as
    /// told by the app of the window there
    pub fn takes(&self, id: u64, at: Point) -> bool {
        self.imp.takes(id, at)
    }

    /// Whether a drag armed before its files are all there drops as soon
    /// as the button goes up: it promises them to the app it lands on
    /// ([`Dnd::deliver`]), and the pointer goes on
    pub fn drops_early(&self) -> bool {
        imp::DROPS_EARLY
    }

    /// The files of the drag armed with `id` are all there (`true`), or
    /// will not come (or not be wanted): an app it dropped on early gets
    /// them now, or learns the drop was cancelled (quietly: the user
    /// hears why from Lanroam)
    pub fn deliver(&self, id: u64, ok: bool) {
        self.imp.deliver(id, ok);
    }
}
