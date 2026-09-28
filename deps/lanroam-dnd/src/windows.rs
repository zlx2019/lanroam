//! Windows: OLE drag and drop, on a thread of its own with two windows.
//!
//! - **Probe**: the catcher, a window under the cursor, is a drop target.
//!   The drag held here enters it (a zero move makes the drag loop look
//!   again) and hands over its `CF_HDROP` file list; it refuses every drop,
//!   so the drag, released there once the pointer went to another device,
//!   drops nothing.
//! - **Arm**: the source window at the entry point takes the press
//!   injected there and starts `SHDoDragDrop` with a shell data object of
//!   the files (the same Explorer builds), which follows the injected
//!   pointer and drops where the button goes up. Its drop source gives up
//!   instead once cancelled.
//! - **Promises**: armed before its files are all there, the data object
//!   is wrapped. It offers virtual files instead (`FileGroupDescriptorW`,
//!   listing everything, and `FileContents`, a stream of each file read as
//!   it arrives), and the app dropped on may take them in the background
//!   (`IDataObjectAsyncCapability`), as Explorer does: its window never
//!   waits. The files' own formats (their paths) join once they are all
//!   there ([`crate::Dnd::deliver`]). A stream read ahead of its file
//!   waits, serving this thread's messages meanwhile.
//!
//! Both windows are layered with an alpha of 1 (fully transparent ones let
//! the pointer through), topmost, and never activate. The thread is
//! per-monitor DPI aware: positions are physical pixels, like Lanroam's.

// Win32 and COM: every unsafe block says why it holds
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::mem::ManuallyDrop;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};

use lanroam_input::{INJECTED_MARKER, Point};
use windows::Win32::Foundation::{
    COLORREF, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC,
    DV_E_LINDEX, DV_E_TYMED, E_NOTIMPL, ERROR_CANCELLED, FILETIME, GlobalFree, HGLOBAL, HWND,
    LPARAM, LRESULT, POINT, POINTL, S_FALSE, S_OK, STG_E_ACCESSDENIED, STG_E_INVALIDFUNCTION,
    STG_E_INVALIDPOINTER, STG_E_READFAULT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject, HBRUSH};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemAlloc, CoTaskMemFree, DATADIR_GET,
    DVASPECT_CONTENT, FORMATETC, IAdviseSink, IBindCtx, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumSTATDATA, ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE,
    STATFLAG, STATFLAG_NONAME, STATSTG, STGC, STGM_READ, STGMEDIUM, STGMEDIUM_0, STGTY_STREAM,
    STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE, IDropSource,
    IDropSource_Impl, IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop,
    ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_DataObject, CLSID_DragDropHelper, DragQueryFileW, FD_ATTRIBUTES, FD_FILESIZE, FD_UNICODE,
    FILEDESCRIPTORW, HDROP, IDataObjectAsyncCapability, IDataObjectAsyncCapability_Impl,
    IDragSourceHelper, ILCreateFromPathW, ILFree, IShellItemArray,
    SHCreateShellItemArrayFromIDLists, SHCreateStdEnumFmtEtc, SHDoDragDrop,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, HWND_TOPMOST,
    KillTimer, LWA_ALPHA, MA_NOACTIVATE, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx,
    PM_REMOVE, PeekMessageW, PostMessageW, QS_ALLINPUT, RegisterClassW, SW_HIDE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow,
    TranslateMessage, WM_APP, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, HRESULT, PCWSTR, PWSTR, Ref, implement, w};

use crate::{DndError, Event, Listed, PART_SUFFIX, Sink};

/// Side of the catcher, in pixels: the pointer pushing along an edge stays
/// on it while the probe is answered
const CATCHER_SIZE: i32 = 128;

/// Side of the window taking the press that starts a drag, in pixels
const SOURCE_SIZE: i32 = 32;

/// Posted to the catcher: commands are waiting
const WM_COMMANDS: u32 = WM_APP + 1;

/// Timer of the catcher: no drag entered it in time
const PROBE_TIMER: usize = 1;

/// How long a probe waits for the drag held here to enter the catcher, in
/// milliseconds; nothing entering it means nothing that OLE drags
const PROBE_WAIT_MS: u32 = 1500;

/// Timer of the catcher: time to hide it
const HIDE_TIMER: usize = 2;

/// How long the catcher stays after the press it served is over, in
/// milliseconds: the release it refuses may still be on its way
const CATCHER_GRACE_MS: u32 = 800;

/// A drag armed before its files are all there drops at once: it promises
/// them
pub(crate) const DROPS_EARLY: bool = true;

/// How often an app reading ahead of the files looks again whether they
/// are there, in milliseconds (messages wake it sooner)
const WAIT_POLL_MS: u32 = 25;

/// Longest path of a virtual file, in UTF-16 units: what a file descriptor
/// holds, less its terminating NUL
const MAX_VIRTUAL_PATH: usize = 259;

/// Attribute of a folder in a file descriptor
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// Attribute of a plain file in a file descriptor
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

/// How much of a file a stream copies to another at once
const COPY_CHUNK: usize = 256 * 1024;

/// [`Gate::state`]: the files are on their way
const WAITING: u8 = 0;
/// [`Gate::state`]: the files are all there
const OPEN: u8 = 1;
/// [`Gate::state`]: the files will not come
const SHUT: u8 = 2;

/// A request for the window thread
enum Command {
    /// See [`Dnd::probe`]
    Probe(u64),
    /// See [`Dnd::unprobe`]
    Unprobe,
    /// See [`Dnd::arm`], with the drag's gate unless its files are all
    /// there
    Arm(u64, Point, Vec<PathBuf>, Option<Arc<Gate>>),
    /// See [`Dnd::cancel`], for a drag not started yet
    Disarm(u64),
}

/// Shared between the window thread and the handle
struct Shared {
    /// Where events go
    sink: Sink,
    /// The id of the drag running here, 0 if none
    dragging: AtomicU64,
    /// The drag running here gives up at the next change of the buttons
    cancel: AtomicBool,
    /// The gates of the drags armed or dropped whose files are still on
    /// their way, by id
    gates: Mutex<HashMap<u64, Arc<Gate>>>,
}

impl Shared {
    /// The gates, even after a panic elsewhere
    fn gates(&self) -> MutexGuard<'_, HashMap<u64, Arc<Gate>>> {
        lock(&self.gates)
    }
}

/// What a drag armed before its files were all there knows of them:
/// whether they are, and what is coming
struct Gate {
    /// [`WAITING`], [`OPEN`] or [`SHUT`]
    state: AtomicU8,
    /// The button went up: the drop is under way
    dropped: AtomicBool,
    /// The app dropped on asked for the files, or took them on in the
    /// background
    asked: AtomicBool,
    /// Where the files land: the folder of the paths armed
    root: PathBuf,
    /// Everything the drag carries as virtual files, once listed
    listing: Mutex<Option<Arc<Vec<Listed>>>>,
}

impl Gate {
    /// The gate of a drag whose files land in `root`
    fn new(root: PathBuf) -> Self {
        Self {
            state: AtomicU8::new(WAITING),
            dropped: AtomicBool::new(false),
            asked: AtomicBool::new(false),
            root,
            listing: Mutex::new(None),
        }
    }

    /// The files are all there
    fn open(&self) -> bool {
        self.state.load(Ordering::SeqCst) == OPEN
    }

    /// Note that the app dropped on asks for the files
    fn ask(&self) {
        if self.dropped.load(Ordering::SeqCst) {
            self.asked.store(true, Ordering::SeqCst);
        }
    }
}

/// Handle for the engine; the work happens on the window thread
pub(crate) struct Dnd {
    /// Requests for the thread
    commands: mpsc::Sender<Command>,
    /// The catcher, woken with [`WM_COMMANDS`] (a window handle is not
    /// `Send`; its value is)
    catcher: isize,
    /// Shared with the thread
    shared: Arc<Shared>,
}

impl Dnd {
    /// Start the window thread; returns once its windows exist
    pub(crate) fn start(sink: Sink) -> Result<Self, DndError> {
        let shared = Arc::new(Shared {
            sink,
            dragging: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            gates: Mutex::new(HashMap::new()),
        });
        let (commands, inbox) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("lanroam-dnd".into())
            .spawn(move || run(&thread_shared, inbox, &ready_tx))
            .map_err(|e| DndError::Os(format!("cannot start the drag thread: {e}")))?;
        let catcher = ready
            .recv()
            .map_err(|_| DndError::Os("the drag thread ended during startup".into()))??;
        Ok(Self {
            commands,
            catcher,
            shared,
        })
    }

    /// Nothing to note: the catcher sees the drag itself
    pub(crate) fn pressed(&self) {}

    /// See [`crate::Dnd::probe`]
    pub(crate) fn probe(&self, id: u64) {
        self.send(Command::Probe(id));
    }

    /// See [`crate::Dnd::unprobe`]
    pub(crate) fn unprobe(&self) {
        self.send(Command::Unprobe);
    }

    /// See [`crate::Dnd::arm`]. The gate of a drag whose files are not
    /// all there is kept here, where [`Self::listed`] and
    /// [`Self::deliver`] find it at once
    pub(crate) fn arm(&self, id: u64, at: Point, paths: Vec<PathBuf>, ready: bool) {
        let gate = (!ready).then(|| {
            let root = paths
                .first()
                .and_then(|path| path.parent())
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let gate = Arc::new(Gate::new(root));
            self.shared.gates().insert(id, Arc::clone(&gate));
            gate
        });
        self.send(Command::Arm(id, at, paths, gate));
    }

    /// See [`crate::Dnd::listed`]. What a virtual file cannot name (a path
    /// too long) is left out, with what it holds
    pub(crate) fn listed(&self, id: u64, mut entries: Vec<Listed>) {
        let Some(gate) = self.shared.gates().get(&id).cloned() else {
            return;
        };
        let all = entries.len();
        entries.retain(|entry| entry.path.as_os_str().encode_wide().count() <= MAX_VIRTUAL_PATH);
        if entries.len() < all {
            tracing::warn!(
                id,
                left_out = all - entries.len(),
                "paths too long for virtual files"
            );
        }
        tracing::info!(
            id,
            entries = entries.len(),
            "listed what a promised drag carries"
        );
        *lock(&gate.listing) = Some(Arc::new(entries));
    }

    /// See [`crate::Dnd::deliver`]. From here: the window thread may be
    /// busy with an app waiting for the files
    pub(crate) fn deliver(&self, id: u64, ok: bool) {
        if let Some(gate) = self.shared.gates().remove(&id) {
            gate.state
                .store(if ok { OPEN } else { SHUT }, Ordering::SeqCst);
            if gate.dropped.load(Ordering::SeqCst) {
                tracing::info!(id, ok, "the files of a drop are delivered");
            }
        }
    }

    /// See [`crate::Dnd::cancel`]. A drag running is cancelled from here:
    /// the window thread is inside its drag loop meanwhile
    pub(crate) fn cancel(&self, id: u64) {
        if let Some(gate) = self.shared.gates().remove(&id) {
            gate.state.store(SHUT, Ordering::SeqCst);
        }
        if self.shared.dragging.load(Ordering::SeqCst) == id {
            self.shared.cancel.store(true, Ordering::SeqCst);
            tracing::info!(id, "cancelling the drag");
            (self.shared.sink)(Event::Cancelling { id });
        } else {
            self.send(Command::Disarm(id));
        }
    }

    /// Hand a command to the window thread and wake it
    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            let catcher = HWND(self.catcher as _);
            // SAFETY: posting to a window of the thread, which lives as
            // long as the process
            if let Err(e) =
                unsafe { PostMessageW(Some(catcher), WM_COMMANDS, WPARAM(0), LPARAM(0)) }
            {
                tracing::warn!("cannot wake the drag thread: {e}");
            }
        }
    }
}

thread_local! {
    /// The window thread's state
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The window thread's side
struct State {
    /// Shared with the handle
    shared: Arc<Shared>,
    /// Requests from the handle
    inbox: mpsc::Receiver<Command>,
    /// The catcher window
    catcher: HWND,
    /// The source window
    source: HWND,
    /// A probe waiting for the drag held here to enter the catcher
    probing: Option<u64>,
    /// A drag ready to start at the next press on the source window, with
    /// its gate unless its files are all there
    armed: Option<(u64, IDataObject, Option<Arc<Gate>>)>,
}

/// Body of the window thread: set up, report, and serve messages for good
fn run(
    shared: &Arc<Shared>,
    inbox: mpsc::Receiver<Command>,
    ready: &mpsc::Sender<Result<isize, DndError>>,
) {
    match set_up(shared, inbox) {
        Ok(catcher) => {
            let _ = ready.send(Ok(catcher.0 as isize));
        }
        Err(e) => {
            let _ = ready.send(Err(DndError::Os(e.to_string())));
            return;
        }
    }
    let mut msg = MSG::default();
    // SAFETY: a plain message loop on this thread's queue
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// OLE, the window class, both windows, and the catcher as a drop target
fn set_up(shared: &Arc<Shared>, inbox: mpsc::Receiver<Command>) -> windows::core::Result<HWND> {
    // SAFETY: plain calls on this thread; OLE stays initialized for its
    // whole life
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        OleInitialize(None)?;
    }
    // SAFETY: this module's handle, a static class name, a window
    // procedure of the right type and a stock brush
    let class = unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("LanroamDnd"),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        class
    };
    let catcher = window(&class)?;
    let source = window(&class)?;
    let target: IDropTarget = Catcher.into();
    // SAFETY: a window of this thread, which has OLE initialized
    unsafe { RegisterDragDrop(catcher, &target)? };
    STATE.with_borrow_mut(|state| {
        *state = Some(State {
            shared: Arc::clone(shared),
            inbox,
            catcher,
            source,
            probing: None,
            armed: None,
        });
    });
    Ok(catcher)
}

/// A hidden window of `class`: layered and nearly transparent, topmost,
/// never activating, not on the taskbar
fn window(class: &WNDCLASSW) -> windows::core::Result<HWND> {
    let style = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    // SAFETY: a registered class and a static title
    unsafe {
        let hwnd = CreateWindowExW(
            style,
            class.lpszClassName,
            w!("Lanroam"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(class.hInstance),
            None,
        )?;
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA)?;
        Ok(hwnd)
    }
}

/// The window procedure of both windows
extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMANDS => with_state(State::run_commands),
        WM_TIMER => with_state(|state| state.on_timer(wparam.0)),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => start_drag(hwnd),
        // SAFETY: the default handling of a message of this window
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

/// Run `work` with the state, unless it is busy (a message dispatched
/// while it is borrowed)
fn with_state(work: impl FnOnce(&mut State)) {
    STATE.with(|cell| match cell.try_borrow_mut() {
        Ok(mut state) => {
            if let Some(state) = state.as_mut() {
                work(state);
            }
        }
        Err(_) => tracing::debug!("the drag thread is busy, a message is skipped"),
    });
}

impl State {
    /// Carry out the commands waiting
    fn run_commands(&mut self) {
        while let Ok(command) = self.inbox.try_recv() {
            match command {
                Command::Probe(id) => self.probe(id),
                Command::Unprobe => self.unprobe(),
                Command::Arm(id, at, paths, gate) => self.arm(id, at, &paths, gate),
                Command::Disarm(id) => self.disarm(id),
            }
        }
    }

    /// Put the catcher under the cursor and wait for the drag to enter it
    fn probe(&mut self, id: u64) {
        let mut at = POINT::default();
        // SAFETY: a plain query into a local
        if unsafe { GetCursorPos(&mut at) }.is_err() {
            self.answer(id, Vec::new());
            return;
        }
        self.probing = Some(id);
        show(self.catcher, Point::new(at.x, at.y), CATCHER_SIZE);
        // SAFETY: timers of this thread's window
        unsafe {
            let _ = KillTimer(Some(self.catcher), HIDE_TIMER);
            SetTimer(Some(self.catcher), PROBE_TIMER, PROBE_WAIT_MS, None);
        }
        nudge();
        tracing::info!(id, "probing the drag held here");
    }

    /// The press probed is over: hide the catcher after the grace time
    fn unprobe(&mut self) {
        self.probing = None;
        // SAFETY: timers of this thread's window
        unsafe {
            let _ = KillTimer(Some(self.catcher), PROBE_TIMER);
            SetTimer(Some(self.catcher), HIDE_TIMER, CATCHER_GRACE_MS, None);
        }
    }

    /// A timer of the catcher went off
    fn on_timer(&mut self, timer: usize) {
        // SAFETY: a timer of this thread's window
        unsafe {
            let _ = KillTimer(Some(self.catcher), timer);
        }
        match timer {
            PROBE_TIMER => {
                if let Some(id) = self.probing.take() {
                    tracing::info!(id, "no drag entered the catcher");
                    self.answer(id, Vec::new());
                }
            }
            HIDE_TIMER => hide(self.catcher),
            _ => {}
        }
    }

    /// Tell the engine what the probe found
    fn answer(&self, id: u64, files: Vec<PathBuf>) {
        (self.shared.sink)(Event::Probed { id, files });
    }

    /// Get ready to drag `paths` from `at`, or promises of them with
    /// `gate`
    fn arm(&mut self, id: u64, at: Point, paths: &[PathBuf], gate: Option<Arc<Gate>>) {
        let data = data_object(paths).map(|data| match &gate {
            Some(gate) => {
                // Drawn while the files' own formats still show
                draw_image(self.source, &data);
                Promised {
                    inner: data,
                    gate: Arc::clone(gate),
                    taking: AtomicBool::new(false),
                }
                .into()
            }
            None => data,
        });
        match data {
            Ok(data) => {
                let promised = gate.is_some();
                self.armed = Some((id, data, gate));
                show(self.source, at, SOURCE_SIZE);
                tracing::info!(id, files = paths.len(), ?at, promised, "armed a drag");
                (self.shared.sink)(Event::Armed { id });
            }
            Err(e) => {
                tracing::warn!("cannot drag the files: {e}");
                (self.shared.sink)(Event::Ended { id, dropped: false });
            }
        }
    }

    /// Forget a drag not started yet
    fn disarm(&mut self, id: u64) {
        if self.armed.as_ref().is_some_and(|(armed, ..)| *armed == id) {
            self.armed = None;
            hide(self.source);
        }
        (self.shared.sink)(Event::Cancelling { id });
    }
}

/// Show `hwnd` centred on `at`, `size` pixels wide and high, above
/// everything
fn show(hwnd: HWND, at: Point, size: i32) {
    let (x, y) = (at.x - size / 2, at.y - size / 2);
    let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
    // SAFETY: a window of this thread
    if let Err(e) = unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, size, size, flags) } {
        tracing::warn!("cannot show a drag window: {e}");
    }
}

/// Hide `hwnd`
fn hide(hwnd: HWND) {
    // SAFETY: a window of this thread
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// A move by nothing, so that the drag loop looks again at what lies under
/// the cursor (the catcher, just shown). Tagged like every injected event,
/// so a capture leaves it alone
fn nudge() {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: MOUSEEVENTF_MOVE,
                dwExtraInfo: INJECTED_MARKER as usize,
                ..Default::default()
            },
        },
    };
    // SAFETY: one well-formed input
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
}

/// The press on the source window: drag the files armed until the button
/// goes up, then report how it ended
fn start_drag(hwnd: HWND) {
    // Taken out of the state: the drag loop below dispatches messages
    // that need it
    let armed = STATE.with(|cell| {
        let mut state = cell.try_borrow_mut().ok()?;
        let state = state.as_mut()?;
        (state.source == hwnd).then_some(())?;
        let (id, data, gate) = state.armed.take()?;
        Some((id, data, gate, Arc::clone(&state.shared)))
    });
    let Some((id, data, gate, shared)) = armed else {
        return;
    };
    shared.cancel.store(false, Ordering::SeqCst);
    shared.dragging.store(id, Ordering::SeqCst);
    tracing::info!(id, "the drag started");
    let source: IDropSource = DropSource {
        shared: Arc::clone(&shared),
        gate: gate.clone(),
    }
    .into();
    // Copied, or moved out of their folder (which goes anyway): on the same
    // disk, a move takes no time
    let effects = DROPEFFECT_COPY | DROPEFFECT_MOVE;
    // SAFETY: a window of this thread, with the left button down on it
    let effect = unsafe { SHDoDragDrop(Some(hwnd), &data, &source, effects) };
    shared.dragging.store(0, Ordering::SeqCst);
    hide(hwnd);
    // An app taking the files in the background may report no effect yet
    let dropped = matches!(effect, Ok(effect) if effect != DROPEFFECT_NONE)
        || gate.is_some_and(|gate| gate.asked.load(Ordering::SeqCst));
    if !dropped {
        shared.gates().remove(&id);
    }
    tracing::info!(id, dropped, "the drag ended");
    (shared.sink)(Event::Ended { id, dropped });
}

/// Give `data` the image the shell draws for a drag of its files (from
/// their paths, which a promise holds back later on)
fn draw_image(hwnd: HWND, data: &IDataObject) {
    // SAFETY: the shell's drag image helper, created in this thread's
    // apartment, with a window of this thread
    let drawn = unsafe {
        CoCreateInstance::<_, IDragSourceHelper>(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER)
            .and_then(|helper| helper.InitializeFromWindow(Some(hwnd), None, data))
    };
    if let Err(e) = drawn {
        tracing::debug!("no image for a promised drag: {e}");
    }
}

/// A shell data object of `paths`, the same Explorer builds for a drag
fn data_object(paths: &[PathBuf]) -> windows::core::Result<IDataObject> {
    let lists: Vec<*mut ITEMIDLIST> = paths
        .iter()
        .map(|path| {
            let wide = wide(path);
            // SAFETY: a NUL-terminated path that outlives the call
            unsafe { ILCreateFromPathW(PCWSTR(wide.as_ptr())) }
        })
        .collect();
    let array = if lists.iter().any(|list| list.is_null()) {
        Err(windows::core::Error::from_thread())
    } else {
        let lists: Vec<*const ITEMIDLIST> = lists.iter().map(|list| list.cast_const()).collect();
        // SAFETY: valid item ID lists, only read during the call
        unsafe { SHCreateShellItemArrayFromIDLists(&lists) }
    };
    for list in lists {
        // SAFETY: allocated above, freed once
        unsafe { ILFree(Some(list.cast_const())) };
    }
    let array: IShellItemArray = array?;
    // SAFETY: a shell item array's own data object
    unsafe { array.BindToHandler(None, &BHID_DataObject) }
}

/// `path` as a NUL-terminated wide string
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// The files `data` carries (`CF_HDROP`), none if it carries no files
fn dragged_files(data: &IDataObject) -> Vec<PathBuf> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    // SAFETY: a well-formed format; the medium is released below
    let Ok(mut medium) = (unsafe { data.GetData(&format) }) else {
        return Vec::new();
    };
    // SAFETY: CF_HDROP travels as an HDROP in the medium's global memory
    let hdrop = HDROP(unsafe { medium.u.hGlobal.0 });
    // SAFETY: counting, then reading each name into a buffer of its size
    let files = unsafe {
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        (0..count)
            .map(|i| {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut name = vec![0u16; len + 1];
                DragQueryFileW(hdrop, i, Some(&mut name));
                PathBuf::from(OsString::from_wide(&name[..len]))
            })
            .collect()
    };
    // SAFETY: the medium GetData handed over, released once
    unsafe { ReleaseStgMedium(&mut medium) };
    files
}

/// The catcher as a drop target: it learns what the drag held here
/// carries, and refuses to take anything
#[implement(IDropTarget)]
struct Catcher;

impl IDropTarget_Impl for Catcher_Impl {
    fn DragEnter(
        &self,
        data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _at: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        refuse(effect);
        let files = data.as_ref().map(dragged_files).unwrap_or_default();
        with_state(|state| {
            if let Some(id) = state.probing.take() {
                // SAFETY: a timer of this thread's window
                unsafe {
                    let _ = KillTimer(Some(state.catcher), PROBE_TIMER);
                }
                tracing::info!(id, files = files.len(), "a drag entered the catcher");
                state.answer(id, files);
            }
        });
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        _at: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        refuse(effect);
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        _data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _at: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        tracing::info!("the catcher refused a drop");
        refuse(effect);
        Ok(())
    }
}

/// Answer a drop target call: nothing may be dropped here
fn refuse(effect: *mut DROPEFFECT) {
    if !effect.is_null() {
        // SAFETY: OLE passes a valid pointer to write the effect to
        unsafe { *effect = DROPEFFECT_NONE };
    }
}

/// The drop source of the drags started here: drops when the left button
/// goes up, unless cancelled (or Esc)
#[implement(IDropSource)]
struct DropSource {
    /// Shared with the handle
    shared: Arc<Shared>,
    /// The gate of a drag promising its files
    gate: Option<Arc<Gate>>,
}

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() || self.shared.cancel.load(Ordering::SeqCst) {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            if let Some(gate) = &self.gate {
                gate.dropped.store(true, Ordering::SeqCst);
            }
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// The shell's data object of the files of a drag armed before they were
/// all there, promising them: virtual files, and the files' own formats
/// (their paths) once they are all there (see [`Gate`]); all else goes to
/// the shell's object as it is
#[implement(IDataObject, IDataObjectAsyncCapability)]
struct Promised {
    /// The shell's data object
    inner: IDataObject,
    /// What is known of the files
    gate: Arc<Gate>,
    /// The app dropped on takes the files in the background right now
    taking: AtomicBool,
}

impl Promised_Impl {
    /// Whether `format` is one of the files' own, held back until they are
    /// all there (virtual files stand in for them)
    fn holds(&self, format: &FORMATETC) -> bool {
        let formats = formats();
        let virtual_file = [formats.descriptor, formats.contents].contains(&format.cfFormat);
        virtual_file || (!self.gate.open() && formats.held.contains(&format.cfFormat))
    }

    /// The virtual files: everything listed, as a file group descriptor
    fn descriptor(&self) -> windows::core::Result<STGMEDIUM> {
        self.gate.ask();
        let Some(listing) = wait_for_listing(&self.gate) else {
            tracing::warn!("a promised drop without the list of its files");
            return Err(DV_E_FORMATETC.into());
        };
        tracing::info!(
            entries = listing.len(),
            "the app dropped on reads the virtual files"
        );
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 {
                hGlobal: descriptor(&listing)?,
            },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }

    /// The contents of the virtual file at `index`, read as it arrives
    fn contents(&self, index: i32) -> windows::core::Result<STGMEDIUM> {
        self.gate.ask();
        let listing = wait_for_listing(&self.gate).ok_or(DV_E_FORMATETC)?;
        let entry = usize::try_from(index)
            .ok()
            .and_then(|index| listing.get(index))
            .filter(|entry| !entry.dir)
            .ok_or(DV_E_LINDEX)?;
        let stream: IStream = FileStream {
            gate: Arc::clone(&self.gate),
            path: self.gate.root.join(&entry.path),
            size: entry.size,
            reading: Mutex::new(Reading::default()),
        }
        .into();
        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 {
                pstm: ManuallyDrop::new(Some(stream)),
            },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }
}

impl IDataObject_Impl for Promised_Impl {
    fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        // SAFETY: OLE passes a valid FORMATETC, or null
        if let Some(asked) = unsafe { format.as_ref() } {
            let formats = formats();
            if asked.cfFormat == formats.descriptor {
                return self.descriptor();
            }
            if asked.cfFormat == formats.contents {
                return self.contents(asked.lindex);
            }
            if self.holds(asked) {
                return Err(DV_E_FORMATETC.into());
            }
        }
        // SAFETY: the caller's arguments, passed on
        unsafe { self.inner.GetData(format) }
    }

    fn GetDataHere(
        &self,
        format: *const FORMATETC,
        medium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        // SAFETY: OLE passes a valid FORMATETC, or null
        if let Some(asked) = unsafe { format.as_ref() }
            && self.holds(asked)
        {
            return Err(DV_E_FORMATETC.into());
        }
        // SAFETY: the caller's arguments, passed on
        unsafe { self.inner.GetDataHere(format, medium) }
    }

    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        // SAFETY: OLE passes a valid FORMATETC, or null
        if let Some(asked) = unsafe { format.as_ref() } {
            let formats = formats();
            let medium = if asked.cfFormat == formats.descriptor {
                Some(TYMED_HGLOBAL.0)
            } else if asked.cfFormat == formats.contents {
                Some(TYMED_ISTREAM.0)
            } else {
                None
            };
            match medium {
                Some(medium) if asked.tymed & medium as u32 != 0 => return S_OK,
                Some(_) => return DV_E_TYMED,
                None if self.holds(asked) => return DV_E_FORMATETC,
                None => {}
            }
        }
        // SAFETY: the caller's argument, passed on
        unsafe { self.inner.QueryGetData(format) }
    }

    fn GetCanonicalFormatEtc(&self, format: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
        // SAFETY: the caller's arguments, passed on
        unsafe { self.inner.GetCanonicalFormatEtc(format, out) }
    }

    fn SetData(
        &self,
        format: *const FORMATETC,
        medium: *const STGMEDIUM,
        release: BOOL,
    ) -> windows::core::Result<()> {
        // SAFETY: the caller's arguments, passed on
        unsafe { self.inner.SetData(format, medium, release.as_bool()) }
    }

    /// The virtual files, and the shell's formats but those held back:
    /// the virtual files first while the files are not all there, last
    /// once they are (an app taking the first it knows takes the files
    /// themselves then)
    fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
        // SAFETY: the caller's argument, passed on
        let inner = unsafe { self.inner.EnumFormatEtc(direction)? };
        if direction != DATADIR_GET.0 as u32 {
            return Ok(inner);
        }
        let formats = formats();
        let virtual_files = [
            format_etc(formats.descriptor, TYMED_HGLOBAL.0),
            format_etc(formats.contents, TYMED_ISTREAM.0),
        ];
        let open = self.gate.open();
        let mut offered = Vec::new();
        if !open {
            offered.extend(virtual_files);
        }
        let mut dropped = Vec::new();
        loop {
            let mut one = [FORMATETC::default()];
            let mut fetched = 0;
            // SAFETY: room for one, and its count
            if unsafe { inner.Next(&mut one, Some(&mut fetched)) } != S_OK || fetched == 0 {
                break;
            }
            if self.holds(&one[0]) {
                dropped.push(one[0]);
            } else {
                offered.push(one[0]);
            }
        }
        if open {
            offered.extend(virtual_files);
        }
        for format in &dropped {
            if !format.ptd.is_null() {
                // SAFETY: the device of a format the enumerator handed
                // over, freed once. Those offered on keep theirs: the new
                // enumerator may point to it
                unsafe { CoTaskMemFree(Some(format.ptd.cast_const().cast())) };
            }
        }
        // SAFETY: well-formed formats, copied by the enumerator
        unsafe { SHCreateStdEnumFmtEtc(&offered) }
    }

    fn DAdvise(
        &self,
        format: *const FORMATETC,
        advf: u32,
        sink: Ref<'_, IAdviseSink>,
    ) -> windows::core::Result<u32> {
        // SAFETY: the caller's arguments, passed on
        unsafe { self.inner.DAdvise(format, advf, sink.as_ref()) }
    }

    fn DUnadvise(&self, connection: u32) -> windows::core::Result<()> {
        // SAFETY: the caller's argument, passed on
        unsafe { self.inner.DUnadvise(connection) }
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        // SAFETY: a plain call on the inner object
        unsafe { self.inner.EnumDAdvise() }
    }
}

/// The app dropped on may take the files in the background: then it waits
/// for them there, not on its window
impl IDataObjectAsyncCapability_Impl for Promised_Impl {
    fn SetAsyncMode(&self, _async: BOOL) -> windows::core::Result<()> {
        Ok(())
    }

    fn GetAsyncMode(&self) -> windows::core::Result<BOOL> {
        Ok(true.into())
    }

    fn StartOperation(&self, _reserved: Ref<'_, IBindCtx>) -> windows::core::Result<()> {
        tracing::info!("the app dropped on takes the files in the background");
        self.gate.ask();
        self.taking.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn InOperation(&self) -> windows::core::Result<BOOL> {
        Ok(self.taking.load(Ordering::SeqCst).into())
    }

    fn EndOperation(
        &self,
        result: HRESULT,
        _reserved: Ref<'_, IBindCtx>,
        effects: u32,
    ) -> windows::core::Result<()> {
        self.taking.store(false, Ordering::SeqCst);
        tracing::info!(
            ok = result.is_ok(),
            effects,
            "the app dropped on took the files"
        );
        Ok(())
    }
}

/// The registered formats a promise deals in
struct Formats {
    /// Virtual files: the file group descriptor
    descriptor: u16,
    /// Virtual files: the contents of one
    contents: u16,
    /// The files' own: held back until they are all there
    held: Vec<u16>,
}

/// The registered formats a promise deals in, registered once
fn formats() -> &'static Formats {
    static FORMATS: OnceLock<Formats> = OnceLock::new();
    FORMATS.get_or_init(|| {
        // SAFETY: NUL-terminated constant names; registering one that
        // exists returns its number (0 on failure, which matches nothing)
        let register = |name| u16::try_from(unsafe { RegisterClipboardFormatW(name) }).unwrap_or(0);
        let named = [
            w!("Shell IDList Array"),
            w!("FileNameW"),
            w!("FileName"),
            w!("FileNameMapW"),
            w!("FileNameMap"),
            w!("FileGroupDescriptor"),
        ];
        Formats {
            descriptor: register(w!("FileGroupDescriptorW")),
            contents: register(w!("FileContents")),
            held: named
                .into_iter()
                .map(register)
                .filter(|format| *format != 0)
                .chain(Some(CF_HDROP.0))
                .collect(),
        }
    })
}

/// A format of the content in `medium` (a `TYMED` value), any index
fn format_etc(format: u16, medium: i32) -> FORMATETC {
    FORMATETC {
        cfFormat: format,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: medium as u32,
    }
}

/// `entries` as a file group descriptor in global memory: each folder and
/// file by its path under the drop, with its size
fn descriptor(entries: &[Listed]) -> windows::core::Result<HGLOBAL> {
    let count = u32::try_from(entries.len()).map_err(|_| DV_E_FORMATETC)?;
    let size = size_of::<u32>() + entries.len() * size_of::<FILEDESCRIPTORW>();
    // SAFETY: a plain allocation, handed over with the medium
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, size)? };
    // SAFETY: the memory just allocated
    let base = unsafe { GlobalLock(memory) }.cast::<u8>();
    if base.is_null() {
        let error = windows::core::Error::from_thread();
        // SAFETY: allocated above, freed once
        let _ = unsafe { GlobalFree(Some(memory)) };
        return Err(error);
    }
    // SAFETY: `size` bytes at `base`: the count, then a descriptor each
    // (packed, so unaligned writes)
    unsafe {
        base.cast::<u32>().write_unaligned(count);
        let first = base.add(size_of::<u32>()).cast::<FILEDESCRIPTORW>();
        for (i, entry) in entries.iter().enumerate() {
            first.add(i).write_unaligned(file_descriptor(entry));
        }
        // Fails once unlocked, as it is now
        let _ = GlobalUnlock(memory);
    }
    Ok(memory)
}

/// The descriptor of one folder or file: its path under the drop, its
/// size, that it is a folder
fn file_descriptor(entry: &Listed) -> FILEDESCRIPTORW {
    // No FD_PROGRESSUI: Lanroam's card shows how far they are, and cancels
    let flags = FD_ATTRIBUTES.0 | FD_FILESIZE.0 | FD_UNICODE.0;
    let mut descriptor = FILEDESCRIPTORW {
        dwFlags: flags as u32,
        dwFileAttributes: if entry.dir {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            FILE_ATTRIBUTE_NORMAL
        },
        nFileSizeHigh: (entry.size >> 32) as u32,
        nFileSizeLow: entry.size as u32,
        ..Default::default()
    };
    // Paths too long were left out when listed (see Dnd::listed)
    let mut name = descriptor.cFileName;
    for (to, unit) in name
        .iter_mut()
        .zip(entry.path.as_os_str().encode_wide().take(MAX_VIRTUAL_PATH))
    {
        *to = unit;
    }
    descriptor.cFileName = name;
    descriptor
}

/// Everything the drag of `gate` carries, waiting for it while it may come
/// (serving this thread's messages); none if it will not
fn wait_for_listing(gate: &Gate) -> Option<Arc<Vec<Listed>>> {
    let mut waited = false;
    loop {
        if let Some(listing) = lock(&gate.listing).clone() {
            return Some(listing);
        }
        if gate.state.load(Ordering::SeqCst) != WAITING {
            return None;
        }
        if !std::mem::replace(&mut waited, true) {
            tracing::info!("the app dropped on waits for the list of the files");
        }
        pump();
    }
}

/// Wait a moment for something to change, serving this thread's messages
/// meanwhile (the calls of COM, the commands that come in)
fn pump() {
    let mut msg = MSG::default();
    // SAFETY: plain calls on this thread's own queue
    unsafe {
        MsgWaitForMultipleObjectsEx(None, WAIT_POLL_MS, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// `mutex` locked, even after a panic elsewhere
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The contents of one file of a promised drag, read as it arrives: a read
/// ahead of it waits for it
#[implement(IStream)]
struct FileStream {
    /// What is known of the drag's files
    gate: Arc<Gate>,
    /// Where the file lands
    path: PathBuf,
    /// Its bytes
    size: u64,
    /// How far the reading is
    reading: Mutex<Reading>,
}

/// How far a [`FileStream`] is
#[derive(Default)]
struct Reading {
    /// The file, once it started to arrive
    file: Option<File>,
    /// Where the next read starts
    pos: u64,
}

impl FileStream_Impl {
    /// Fill `buf` from where the reading is, waiting where the file has not
    /// arrived yet; how much, and `S_OK`, `S_FALSE` at the end of the file,
    /// or an error once it will not arrive
    fn fill(&self, buf: &mut [u8]) -> (usize, HRESULT) {
        let mut filled = 0;
        while filled < buf.len() {
            match self.read_arrived(&mut buf[filled..]) {
                Ok(Some(0)) => return (filled, S_FALSE),
                Ok(Some(n)) => filled += n,
                // Withdrawn as cancelled, which the app takes quietly
                Ok(None) if self.gate.state.load(Ordering::SeqCst) == SHUT => {
                    tracing::info!(file = %self.path.display(), "a promised file will not arrive");
                    return (filled, ERROR_CANCELLED.to_hresult());
                }
                Ok(None) => pump(),
                Err(e) => {
                    tracing::warn!(file = %self.path.display(), "cannot read a promised file: {e}");
                    return (filled, STG_E_READFAULT);
                }
            }
        }
        (filled, S_OK)
    }

    /// Read what has arrived into `buf`: how much (none at the end of the
    /// file), or `None` when nothing more is there yet
    fn read_arrived(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        let mut reading = lock(&self.reading);
        let left = self.size.saturating_sub(reading.pos);
        if left == 0 {
            return Ok(Some(0));
        }
        // Asked before reading: all there then, the file is whole
        let whole = self.gate.open();
        if reading.file.is_none() {
            let Some(mut file) = self.open()? else {
                return Ok(None);
            };
            file.seek(SeekFrom::Start(reading.pos))?;
            reading.file = Some(file);
        }
        let want = usize::try_from(left).map_or(buf.len(), |left| left.min(buf.len()));
        let Some(file) = reading.file.as_mut() else {
            return Ok(None);
        };
        match file.read(&mut buf[..want])? {
            0 if whole => Err(io::ErrorKind::UnexpectedEof.into()),
            0 => Ok(None),
            n => {
                reading.pos += n as u64;
                Ok(Some(n))
            }
        }
    }

    /// The file where it is now: arriving under its part's name, or whole
    /// under its own (not the empty stand-in there before); none while it
    /// has not started to arrive. Once all are there, it must be
    fn open(&self) -> io::Result<Option<File>> {
        let mut part = self.path.as_os_str().to_os_string();
        part.push(PART_SUFFIX);
        match File::open(&part) {
            Ok(file) => return Ok(Some(file)),
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            Err(_) => {}
        }
        let whole = self.gate.open();
        match File::open(&self.path) {
            Ok(file) if whole || file.metadata()?.len() == self.size => Ok(Some(file)),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound && !whole => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl ISequentialStream_Impl for FileStream_Impl {
    fn Read(&self, pv: *mut core::ffi::c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        if pv.is_null() {
            return STG_E_INVALIDPOINTER;
        }
        // SAFETY: the caller's buffer of `cb` bytes
        let buf = unsafe { std::slice::from_raw_parts_mut(pv.cast::<u8>(), cb as usize) };
        let (filled, result) = self.fill(buf);
        if !pcbread.is_null() {
            // SAFETY: the caller's count, where it wants it
            unsafe { *pcbread = filled as u32 };
        }
        result
    }

    fn Write(&self, _pv: *const core::ffi::c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        STG_E_ACCESSDENIED
    }
}

impl IStream_Impl for FileStream_Impl {
    fn Seek(
        &self,
        dlibmove: i64,
        dworigin: STREAM_SEEK,
        plibnewposition: *mut u64,
    ) -> windows::core::Result<()> {
        let mut reading = lock(&self.reading);
        let base = match dworigin {
            STREAM_SEEK_SET => 0,
            STREAM_SEEK_CUR => reading.pos,
            STREAM_SEEK_END => self.size,
            _ => return Err(STG_E_INVALIDFUNCTION.into()),
        };
        let pos = base
            .checked_add_signed(dlibmove)
            .ok_or(STG_E_INVALIDFUNCTION)?;
        if let Some(file) = reading.file.as_mut() {
            file.seek(SeekFrom::Start(pos))
                .map_err(|_| windows::core::Error::from(STG_E_READFAULT))?;
        }
        reading.pos = pos;
        if !plibnewposition.is_null() {
            // SAFETY: the caller's position, where it wants it
            unsafe { *plibnewposition = pos };
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> windows::core::Result<()> {
        Err(STG_E_ACCESSDENIED.into())
    }

    /// Read up to `cb` bytes into `pstm`
    fn CopyTo(
        &self,
        pstm: Ref<'_, IStream>,
        cb: u64,
        pcbread: *mut u64,
        pcbwritten: *mut u64,
    ) -> windows::core::Result<()> {
        let to = pstm.ok()?;
        let (mut read, mut written) = (0u64, 0u64);
        let mut buf = vec![0u8; COPY_CHUNK];
        let copied = loop {
            let want = usize::try_from(cb - read).map_or(buf.len(), |left| left.min(buf.len()));
            if want == 0 {
                break Ok(());
            }
            let (n, result) = self.fill(&mut buf[..want]);
            read += n as u64;
            let mut out = 0;
            // SAFETY: `n` bytes of the buffer, and a count to write to
            let wrote = unsafe { to.Write(buf.as_ptr().cast(), n as u32, Some(&mut out)) };
            written += u64::from(out);
            if wrote.is_err() {
                break Err(wrote.into());
            }
            if result != S_OK {
                break result.ok();
            }
        };
        // SAFETY: the caller's counts, where it wants them
        unsafe {
            if !pcbread.is_null() {
                *pcbread = read;
            }
            if !pcbwritten.is_null() {
                *pcbwritten = written;
            }
        }
        copied
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn LockRegion(
        &self,
        _liboffset: u64,
        _cb: u64,
        _dwlocktype: &LOCKTYPE,
    ) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn UnlockRegion(
        &self,
        _liboffset: u64,
        _cb: u64,
        _dwlocktype: u32,
    ) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    /// Its size, and its name unless not asked for
    fn Stat(&self, pstatstg: *mut STATSTG, grfstatflag: &STATFLAG) -> windows::core::Result<()> {
        if pstatstg.is_null() {
            return Err(STG_E_INVALIDPOINTER.into());
        }
        let name = match self.path.file_name() {
            Some(name) if *grfstatflag != STATFLAG_NONAME => task_string(name),
            _ => PWSTR::null(),
        };
        let stat = STATSTG {
            pwcsName: name,
            r#type: STGTY_STREAM.0 as u32,
            cbSize: self.size,
            mtime: FILETIME::default(),
            ctime: FILETIME::default(),
            atime: FILETIME::default(),
            grfMode: STGM_READ,
            grfLocksSupported: 0,
            clsid: windows::core::GUID::zeroed(),
            grfStateBits: 0,
            reserved: 0,
        };
        // SAFETY: the caller's structure, filled in
        unsafe { *pstatstg = stat };
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        Err(E_NOTIMPL.into())
    }
}

/// `text` as a NUL-terminated wide string in COM's task memory, which the
/// caller frees; null if there is no memory
fn task_string(text: &OsStr) -> PWSTR {
    let wide: Vec<u16> = text.encode_wide().chain(Some(0)).collect();
    // SAFETY: a plain allocation of the string's size
    let memory = unsafe { CoTaskMemAlloc(wide.len() * size_of::<u16>()) }.cast::<u16>();
    if !memory.is_null() {
        // SAFETY: room for the whole string
        unsafe { memory.copy_from_nonoverlapping(wide.as_ptr(), wide.len()) };
    }
    PWSTR(memory)
}
