//! Windows backend.
//!
//! - **Injection**: `SendInput`. Keys go as scan codes, so the target's own
//!   layout and input method interpret them; the pointer goes to absolute
//!   positions over the virtual desktop.
//! - **Displays**: monitor rectangles in physical pixels. The process is
//!   made per-monitor DPI aware first, or Windows would report (and expect)
//!   scaled coordinates.
//! - **Capture**: M1 step 2.
//!
//! Injection cannot reach windows of elevated processes (UIPI), the secure
//! desktop (UAC prompts, Ctrl+Alt+Del) or the lock screen; such calls fail
//! and are counted by the caller.

#![allow(unsafe_code)] // user32 FFI

use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex, Once};

use windows_sys::Win32::Foundation::{LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VK_PAUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    XBUTTON1, XBUTTON2,
};
use windows_sys::core::BOOL;

use super::{Capture, EmitSink};
use crate::event::MouseButton;
use crate::geometry::{Point, Rect};
use crate::inject::Injector;
use crate::keymap::{self, usage};
use crate::switch::Switch;
use crate::{INJECTED_MARKER, InputError};

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

/// Capture arrives in M1 step 2
pub(super) fn start_capture(
    _switch: Arc<Mutex<Switch>>,
    _sink: EmitSink,
) -> Result<Capture, InputError> {
    Err(InputError::Unsupported("input capture on Windows"))
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
    // SAFETY: plain metric queries
    let (left, top, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    (normalize(at.x - left, width), normalize(at.y - top, height))
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
