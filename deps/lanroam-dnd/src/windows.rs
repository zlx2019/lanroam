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
//!
//! Both windows are layered with an alpha of 1 (fully transparent ones let
//! the pointer through), topmost, and never activate. The thread is
//! per-monitor DPI aware: positions are physical pixels, like Lanroam's.

// Win32 and COM: every unsafe block says why it holds
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use lanroam_input::{INJECTED_MARKER, Point};
use windows::Win32::Foundation::{
    COLORREF, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, LPARAM,
    LRESULT, POINT, POINTL, S_OK, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject, HBRUSH};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
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
    BHID_DataObject, DragQueryFileW, HDROP, ILCreateFromPathW, ILFree, IShellItemArray,
    SHCreateShellItemArrayFromIDLists, SHDoDragDrop,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, HWND_TOPMOST,
    KillTimer, LWA_ALPHA, MA_NOACTIVATE, MSG, PostMessageW, RegisterClassW, SW_HIDE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow,
    TranslateMessage, WM_APP, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, HRESULT, PCWSTR, Ref, implement, w};

use crate::{DndError, Event, Sink};

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

/// A request for the window thread
enum Command {
    /// See [`Dnd::probe`]
    Probe(u64),
    /// See [`Dnd::unprobe`]
    Unprobe,
    /// See [`Dnd::arm`]
    Arm(u64, Point, Vec<PathBuf>),
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

    /// See [`crate::Dnd::arm`]
    pub(crate) fn arm(&self, id: u64, at: Point, paths: Vec<PathBuf>) {
        self.send(Command::Arm(id, at, paths));
    }

    /// See [`crate::Dnd::cancel`]. A drag running is cancelled from here:
    /// the window thread is inside its drag loop meanwhile
    pub(crate) fn cancel(&self, id: u64) {
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
    /// A drag ready to start at the next press on the source window
    armed: Option<(u64, IDataObject)>,
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
                Command::Arm(id, at, paths) => self.arm(id, at, &paths),
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

    /// Get ready to drag `paths` from `at`
    fn arm(&mut self, id: u64, at: Point, paths: &[PathBuf]) {
        match data_object(paths) {
            Ok(data) => {
                self.armed = Some((id, data));
                show(self.source, at, SOURCE_SIZE);
                tracing::info!(id, files = paths.len(), ?at, "armed a drag");
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
        if self.armed.as_ref().is_some_and(|(armed, _)| *armed == id) {
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
        let (id, data) = state.armed.take()?;
        Some((id, data, Arc::clone(&state.shared)))
    });
    let Some((id, data, shared)) = armed else {
        return;
    };
    shared.cancel.store(false, Ordering::SeqCst);
    shared.dragging.store(id, Ordering::SeqCst);
    tracing::info!(id, "the drag started");
    let source: IDropSource = DropSource(Arc::clone(&shared)).into();
    // Copied, or moved out of their folder (which goes anyway): on the same
    // disk, a move takes no time
    let effects = DROPEFFECT_COPY | DROPEFFECT_MOVE;
    // SAFETY: a window of this thread, with the left button down on it
    let effect = unsafe { SHDoDragDrop(Some(hwnd), &data, &source, effects) };
    shared.dragging.store(0, Ordering::SeqCst);
    hide(hwnd);
    let dropped = matches!(effect, Ok(effect) if effect != DROPEFFECT_NONE);
    tracing::info!(id, dropped, "the drag ended");
    (shared.sink)(Event::Ended { id, dropped });
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
struct DropSource(Arc<Shared>);

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() || self.0.cancel.load(Ordering::SeqCst) {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}
