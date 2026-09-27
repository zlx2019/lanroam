//! Windows backend.
//!
//! - **Injection**: `SendInput`. Keys go as scan codes, so the target's own
//!   layout and input method interpret them; the pointer goes to absolute
//!   positions over the virtual desktop.
//! - **Displays**: monitor rectangles in physical pixels. The process is
//!   made per-monitor DPI aware first, or Windows would report (and expect)
//!   scaled coordinates.
//! - **Capture**: low-level keyboard and mouse hooks on a thread of their
//!   own. Hook positions are not clipped to the desktop yet, so pushing
//!   against an edge still moves them outwards. While the target is
//!   controlled every event is blocked, which keeps the cursor where it
//!   crossed, and motion is the offset from there (lan-mouse's approach).
//!   The cursor stays visible meanwhile.
//!
//! Injection cannot reach windows of elevated processes (UIPI), the secure
//! desktop (UAC prompts, Ctrl+Alt+Del) or the lock screen; such calls fail
//! and are counted by the caller.

#![allow(unsafe_code)] // user32 FFI

use std::cell::RefCell;
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC_EX,
    MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MapVirtualKeyW, SendInput, VK_PAUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCursorPos, GetMessageW, GetSystemMetrics, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
    KillTimer, LLKHF_EXTENDED, LLKHF_UP, MSG, MSLLHOOKSTRUCT, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetCursorPos, SetTimer,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};
use windows_sys::core::BOOL;

use super::{Capture, EmitSink, Ready};
use crate::event::{InputEvent, MouseButton};
use crate::geometry::{Point, Rect};
use crate::inject::Injector;
use crate::keymap::{self, usage};
use crate::switch::{CursorAction, Emit, Switch, Verdict};
use crate::{INJECTED_MARKER, InputError};

/// How often the capture thread checks whether it should stop, in
/// milliseconds
const STOP_POLL_MS: u32 = 200;

thread_local! {
    /// State of the hooks. Low-level hooks run on the thread that installed
    /// them and receive no context pointer, hence a thread-local
    static HOOKS: RefCell<Option<Hooks>> = const { RefCell::new(None) };
}

/// Make this process see physical pixels (done once)
fn dpi_aware() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: plain call; it fails harmlessly when the awareness was
        // already set, e.g. by a manifest
        let ok =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if ok == 0 {
            tracing::debug!("DPI awareness was already set for this process");
        }
    });
}

/// The primary monitor's DPI scale in percent (physical pixels per logical
/// pixel)
pub(super) fn scale() -> Result<u32, InputError> {
    dpi_aware();
    // SAFETY: plain call; with MONITOR_DEFAULTTOPRIMARY it always returns a
    // monitor
    let primary = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    // SAFETY: both out-pointers are valid for the call
    let hr = unsafe { GetDpiForMonitor(primary, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    if hr < 0 || dpi_x == 0 {
        return Err(InputError::Os(format!("GetDpiForMonitor failed ({hr:#x})")));
    }
    Ok(dpi_x * 100 / 96)
}

/// Monitor rectangles, in physical pixels
pub(super) fn displays() -> Result<Vec<Rect>, InputError> {
    dpi_aware();
    let mut rects: Vec<Rect> = Vec::new();
    // SAFETY: the callback only runs during this call, and `rects` outlives
    // it
    let ok = unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect_monitor),
            (&raw mut rects) as LPARAM,
        )
    };
    if ok == 0 || rects.is_empty() {
        return Err(InputError::Os(
            "EnumDisplayMonitors found no monitor".into(),
        ));
    }
    Ok(rects)
}

/// `EnumDisplayMonitors` callback: append the monitor's rectangle
///
/// # Safety
///
/// `data` must be the `Vec<Rect>` passed by [`displays`].
unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: see above; the enumeration is synchronous
    let rects = unsafe { &mut *(data as *mut Vec<Rect>) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a MONITORINFO with its size set
    if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
        let r = info.rcMonitor;
        rects.push(Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top));
    }
    TRUE
}

/// Start the low-level hooks on their own thread; returns once they run
pub(super) fn start_capture(
    switch: Arc<Mutex<Switch>>,
    sink: EmitSink,
) -> Result<Capture, InputError> {
    dpi_aware();
    super::spawn_capture(move |stop, ready| capture_thread(switch, sink, stop, ready))
}

/// Body of the capture thread: install the hooks and pump messages (the
/// hooks are called from within `GetMessageW`) until asked to stop
fn capture_thread(switch: Arc<Mutex<Switch>>, sink: EmitSink, stop: &AtomicBool, ready: &Ready) {
    HOOKS.set(Some(Hooks {
        switch,
        sink,
        out: Vec::with_capacity(8),
        last: cursor_position(),
        parked: None,
    }));
    // SAFETY: both procedures follow the HOOKPROC contract, and the hooks are
    // removed below, before this thread ends
    let (mouse, keyboard) = unsafe {
        (
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), null_mut(), 0),
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), null_mut(), 0),
        )
    };
    if mouse.is_null() || keyboard.is_null() {
        unhook(mouse);
        unhook(keyboard);
        HOOKS.set(None);
        let _ = ready.send(Err(InputError::Os("cannot install the input hooks".into())));
        return;
    }
    // A thread timer (no window) wakes the loop to check the stop flag
    // SAFETY: plain call
    let timer = unsafe { SetTimer(null_mut(), 0, STOP_POLL_MS, None) };
    let _ = ready.send(Ok(()));

    let mut msg = MSG::default();
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: `msg` is a valid MSG to fill
        if unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } <= 0 {
            break;
        }
    }

    // SAFETY: the timer belongs to this thread
    unsafe { KillTimer(null_mut(), timer) };
    // Blocking ends with the hooks; a parked cursor is free to move again
    unhook(mouse);
    unhook(keyboard);
    HOOKS.set(None);
}

/// Remove a hook, if it was installed
fn unhook(hook: HHOOK) {
    if !hook.is_null() {
        // SAFETY: `hook` was installed by this thread and not removed yet
        unsafe { UnhookWindowsHookEx(hook) };
    }
}

/// Low-level mouse hook
///
/// # Safety
///
/// Called by Windows with the hook arguments; for `HC_ACTION`, `lparam`
/// points to an `MSLLHOOKSTRUCT`.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: see above
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if with_hooks(|hooks| hooks.on_mouse(wparam as u32, info)) == Verdict::Swallow {
            return 1;
        }
    }
    // SAFETY: the arguments are passed on unchanged
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

/// Low-level keyboard hook
///
/// # Safety
///
/// Called by Windows with the hook arguments; for `HC_ACTION`, `lparam`
/// points to a `KBDLLHOOKSTRUCT`.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: see above
        let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if with_hooks(|hooks| hooks.on_key(info)) == Verdict::Swallow {
            return 1;
        }
    }
    // SAFETY: the arguments are passed on unchanged
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

/// Run `f` on the hook state
///
/// Anything unusual (no state, a re-entrant call, a panic, which must not
/// unwind into Windows) lets the event pass: a bug fails open instead of
/// eating the user's input.
fn with_hooks(f: impl FnOnce(&mut Hooks) -> Verdict) -> Verdict {
    catch_unwind(AssertUnwindSafe(|| {
        HOOKS.with(|cell| match cell.try_borrow_mut() {
            Ok(mut hooks) => hooks.as_mut().map_or(Verdict::Pass, f),
            Err(_) => Verdict::Pass,
        })
    }))
    .unwrap_or(Verdict::Pass)
}

/// State of the hooks, owned by the capture thread
struct Hooks {
    /// Decides each event
    switch: Arc<Mutex<Switch>>,
    /// Receives the messages for the target
    sink: EmitSink,
    /// Reused buffer for the switch's messages
    out: Vec<Emit>,
    /// Where the cursor is
    last: Point,
    /// Where the cursor is held while controlling the target
    parked: Option<Point>,
}

impl Hooks {
    /// Decide one mouse event
    fn on_mouse(&mut self, message: u32, info: &MSLLHOOKSTRUCT) -> Verdict {
        if info.dwExtraInfo == INJECTED_MARKER as usize {
            return Verdict::Pass;
        }
        let at = Point::new(info.pt.x, info.pt.y);
        let button = |button, down| Some(InputEvent::Button { button, down });
        let event = match message {
            WM_MOUSEMOVE => {
                let from = self.parked.unwrap_or(self.last);
                Some(InputEvent::Motion {
                    at,
                    dx: f64::from(at.x - from.x),
                    dy: f64::from(at.y - from.y),
                })
            }
            WM_LBUTTONDOWN => button(MouseButton::Left, true),
            WM_LBUTTONUP => button(MouseButton::Left, false),
            WM_RBUTTONDOWN => button(MouseButton::Right, true),
            WM_RBUTTONUP => button(MouseButton::Right, false),
            WM_MBUTTONDOWN => button(MouseButton::Middle, true),
            WM_MBUTTONUP => button(MouseButton::Middle, false),
            WM_XBUTTONDOWN | WM_XBUTTONUP => {
                let down = message == WM_XBUTTONDOWN;
                match (info.mouseData >> 16) as u16 {
                    XBUTTON1 => button(MouseButton::Back, down),
                    XBUTTON2 => button(MouseButton::Forward, down),
                    _ => None,
                }
            }
            WM_MOUSEWHEEL => Some(InputEvent::Wheel {
                dx: 0,
                dy: wheel_amount(info.mouseData),
            }),
            WM_MOUSEHWHEEL => Some(InputEvent::Wheel {
                dx: wheel_amount(info.mouseData),
                dy: 0,
            }),
            _ => None,
        };
        let verdict = self.decide(event);
        // A motion that goes through moves the cursor, clipped to the desktop
        if message == WM_MOUSEMOVE && verdict == Verdict::Pass && self.parked.is_none() {
            self.last = clip_to_desktop(at);
        }
        verdict
    }

    /// Decide one keyboard event
    fn on_key(&mut self, info: &KBDLLHOOKSTRUCT) -> Verdict {
        if info.dwExtraInfo == INJECTED_MARKER as usize {
            return Verdict::Pass;
        }
        let down = info.flags & LLKHF_UP == 0;
        let event = key_usage(info).map(|usage| InputEvent::Key { usage, down });
        self.decide(event)
    }

    /// Run the switch and apply its cursor change: parking needs nothing
    /// but the blocking itself; coming back puts the cursor in place
    fn decide(&mut self, event: Option<InputEvent>) -> Verdict {
        let decision = super::decide(&self.switch, event, &mut self.out, &mut self.sink);
        match decision.cursor {
            Some(CursorAction::Park) => self.parked = Some(self.last),
            Some(CursorAction::Release(at)) => {
                self.parked = None;
                self.last = at;
                // SAFETY: plain call
                unsafe { SetCursorPos(at.x, at.y) };
            }
            None => {}
        }
        decision.verdict
    }
}

/// Signed wheel amount from a hook's `mouseData` (high word)
fn wheel_amount(mouse_data: u32) -> i32 {
    i32::from((mouse_data >> 16) as u16 as i16)
}

/// HID usage of a hooked key, from its scan code
///
/// Codes the key map does not know are dropped. That includes the fake
/// Shift presses (0xE02A, 0xE036) Windows wraps around navigation keys
/// while Num Lock is on, which must not reach the target.
fn key_usage(info: &KBDLLHOOKSTRUCT) -> Option<u16> {
    let scan = if info.scanCode == 0 {
        // Some keys (media keys, software keyboards) come without a scan
        // code; the extended variant carries the 0xE0 prefix
        // SAFETY: plain call
        unsafe { MapVirtualKeyW(info.vkCode, MAPVK_VK_TO_VSC_EX) }
    } else if info.flags & LLKHF_EXTENDED != 0 {
        0xE000 | (info.scanCode & 0xFF)
    } else {
        info.scanCode & 0xFF
    };
    keymap::usage_from_win(u16::try_from(scan).ok()?)
}

/// Where the cursor is now
fn cursor_position() -> Point {
    let mut pt = POINT::default();
    // SAFETY: `pt` is a valid POINT to fill
    unsafe { GetCursorPos(&mut pt) };
    Point::new(pt.x, pt.y)
}

/// The virtual desktop: every monitor's bounding box
fn virtual_screen() -> Rect {
    // SAFETY: plain metric queries
    unsafe {
        Rect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// Clip a hook position to the desktop, as Windows will
fn clip_to_desktop(p: Point) -> Point {
    virtual_screen().clamp(p)
}

/// An injector for the interactive session
pub(super) fn injector() -> Result<Box<dyn Injector>, InputError> {
    dpi_aware();
    Ok(Box::new(SendInputInjector))
}

/// Injects through `SendInput`
struct SendInputInjector;

impl Injector for SendInputInjector {
    /// Absolute move over the virtual desktop
    fn move_to(&mut self, at: Point) -> Result<(), InputError> {
        let (x, y) = absolute(at);
        send(&[mouse(
            x,
            y,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            0,
        )])
    }

    /// Button press or release where the cursor is
    fn button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
        let (flags, data) = match (button, down) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2),
        };
        send(&[mouse(0, 0, flags, u32::from(data))])
    }

    /// Wheel and tilt, already in WHEEL_DELTA units
    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        // Negative amounts travel as the two's complement in the DWORD
        let inputs: Vec<INPUT> = [(MOUSEEVENTF_WHEEL, dy), (MOUSEEVENTF_HWHEEL, dx)]
            .into_iter()
            .filter(|&(_, amount)| amount != 0)
            .map(|(flags, amount)| mouse(0, 0, flags, amount as u32))
            .collect();
        send(&inputs)
    }

    /// Scan code press or release; Pause has no plain scan code and goes as
    /// a virtual key
    fn key(&mut self, usage: u16, down: bool) -> Result<(), InputError> {
        let up = if down { 0 } else { KEYEVENTF_KEYUP };
        if usage == usage::PAUSE {
            return send(&[keyboard(VK_PAUSE, 0, up)]);
        }
        let scan = keymap::win_from_usage(usage).ok_or_else(|| {
            InputError::Os(format!("no Windows scan code for HID usage {usage:#04x}"))
        })?;
        let extended = if scan & 0xFF00 == 0xE000 {
            KEYEVENTF_EXTENDEDKEY
        } else {
            0
        };
        send(&[keyboard(0, scan & 0xFF, KEYEVENTF_SCANCODE | extended | up)])
    }
}

/// A desktop position in `SendInput`'s absolute units (0..=65535 over the
/// virtual desktop)
fn absolute(at: Point) -> (i32, i32) {
    let desktop = virtual_screen();
    (
        normalize(at.x - desktop.x, desktop.width),
        normalize(at.y - desktop.y, desktop.height),
    )
}

/// Map a pixel offset within a span of `span` pixels to 0..=65535
///
/// Windows maps back with `floor(n * span / 65536)`; one unit past the exact
/// boundary guarantees that lands on the intended pixel rather than the one
/// before it (the formula AutoHotkey has long used).
fn normalize(offset: i32, span: i32) -> i32 {
    if span <= 0 {
        return 0;
    }
    let offset = i64::from(offset.clamp(0, span - 1));
    let n = offset * 65536 / i64::from(span) + 1;
    n.min(65535) as i32
}

/// A mouse `INPUT`, tagged as Lanroam's
fn mouse(dx: i32, dy: i32, flags: MOUSE_EVENT_FLAGS, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_MARKER as usize,
            },
        },
    }
}

/// A keyboard `INPUT`, tagged as Lanroam's
fn keyboard(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_MARKER as usize,
            },
        },
    }
}

/// Insert events into the input stream, all or nothing
fn send(inputs: &[INPUT]) -> Result<(), InputError> {
    if inputs.is_empty() {
        return Ok(());
    }
    let count = u32::try_from(inputs.len()).unwrap_or(u32::MAX);
    // SAFETY: `inputs` is a slice of initialized INPUT structures
    let sent = unsafe { SendInput(count, inputs.as_ptr(), size_of::<INPUT>() as i32) };
    if sent == count {
        Ok(())
    } else {
        Err(InputError::Os(format!(
            "SendInput inserted {sent} of {count} events (an elevated window or the secure desktop has focus?)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wheel amount is the signed high word
    #[test]
    fn wheel_amounts() {
        assert_eq!(wheel_amount(120 << 16), 120);
        assert_eq!(wheel_amount(((-240i16) as u16 as u32) << 16), -240);
        assert_eq!(wheel_amount(0xFFFF), 0);
    }

    /// Scan codes map with and without the extended flag; fake shifts and
    /// unknown codes are dropped
    #[test]
    fn hooked_key_codes() {
        let key = |scan, flags| KBDLLHOOKSTRUCT {
            vkCode: 0,
            scanCode: scan,
            flags,
            time: 0,
            dwExtraInfo: 0,
        };
        assert_eq!(key_usage(&key(0x1E, 0)), Some(0x04));
        assert_eq!(
            key_usage(&key(0x1D, LLKHF_EXTENDED)),
            Some(usage::RIGHT_CTRL)
        );
        assert_eq!(key_usage(&key(0x1D, LLKHF_UP)), Some(usage::LEFT_CTRL));
        assert_eq!(key_usage(&key(0x2A, LLKHF_EXTENDED)), None);
    }

    /// Every pixel survives the round trip through absolute units
    #[test]
    fn normalize_roundtrips() {
        for span in [1, 800, 1366, 1920, 2560, 3840, 5120, 7680] {
            for offset in 0..span {
                let n = i64::from(normalize(offset, span));
                assert!((0..=65535).contains(&n));
                assert_eq!(
                    n * i64::from(span) / 65536,
                    i64::from(offset),
                    "span {span}"
                );
            }
        }
        assert_eq!(normalize(-5, 1920), normalize(0, 1920));
        assert_eq!(normalize(5000, 1920), normalize(1919, 1920));
    }
}
