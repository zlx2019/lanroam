//! This machine's clipboard: arboard reads and writes it; the change stamp
//! and the concealed marker are asked of the system directly, without
//! opening the clipboard.
//!
//! The concealed marker is the de facto standard password managers set on
//! what they copy (same checks as Lanecho):
//! - macOS: the pasteboard type `org.nspasteboard.ConcealedType`
//!   (1Password, Keychain and others)
//! - Windows: the clipboard format
//!   `ExcludeClipboardContentFromMonitorProcessing` (the cloud clipboard's
//!   convention) or the older `Clipboard Viewer Ignore` (KeePass and
//!   others)
//!
//! Checking the marker and reading are two steps milliseconds apart; a copy
//! landing in between is read as it is, which clipboard tools accept too.

use std::borrow::Cow;

use crate::{Clipboard, ClipboardError, Content, Image, MAX_BYTES};

/// This machine's clipboard
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn stamp(&self) -> Option<i64> {
        stamp()
    }

    fn read(&self) -> Result<Option<Content>, ClipboardError> {
        if concealed() {
            tracing::debug!("the clipboard holds concealed content, left alone");
            return Ok(None);
        }
        // A fresh handle per call: cheap on macOS and Windows, and nothing
        // to share between threads
        let mut clipboard = arboard::Clipboard::new()?;
        // Copied files also come with their names as text: the files are
        // what was copied
        if let Ok(files) = clipboard.get().file_list()
            && !files.is_empty()
        {
            return Ok(Some(Content::Files(files)));
        }
        // Text first: what copies both (cells of a spreadsheet, a
        // document with pictures) is text to its user
        if let Some(text) = available(clipboard.get_text())?
            && !text.is_empty()
        {
            if text.len() > MAX_BYTES {
                tracing::debug!(bytes = text.len(), "the clipboard text is too large");
                return Ok(None);
            }
            return Ok(Some(Content::Text(text)));
        }
        Ok(read_image(&mut clipboard)?.map(Content::Image))
    }

    fn write(&self, content: &Content) -> Result<Option<i64>, ClipboardError> {
        let mut clipboard = arboard::Clipboard::new()?;
        match content {
            Content::Text(text) => clipboard.set_text(text.as_str())?,
            Content::Image(image) => clipboard.set_image(arboard::ImageData {
                width: image.width as usize,
                height: image.height as usize,
                bytes: Cow::Borrowed(&image.rgba),
            })?,
            Content::Files(paths) => clipboard.set().file_list(paths)?,
        }
        Ok(stamp())
    }
}

/// A read's result, `None` when the clipboard has nothing of that kind
fn available<T>(read: Result<T, arboard::Error>) -> Result<Option<T>, ClipboardError> {
    match read {
        Ok(value) => Ok(Some(value)),
        Err(arboard::Error::ContentNotAvailable | arboard::Error::ConversionFailure) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The clipboard's image: decoded by the system and made sRGB (see
/// [`crate::pasteboard`])
#[cfg(target_os = "macos")]
fn read_image(_: &mut arboard::Clipboard) -> Result<Option<Image>, ClipboardError> {
    Ok(crate::pasteboard::read_image())
}

/// The clipboard's image, as arboard reads it
#[cfg(not(target_os = "macos"))]
fn read_image(clipboard: &mut arboard::Clipboard) -> Result<Option<Image>, ClipboardError> {
    Ok(available(clipboard.get_image())?.and_then(to_image))
}

/// An image read by arboard, unless it is empty or too large
#[cfg(not(target_os = "macos"))]
fn to_image(image: arboard::ImageData<'_>) -> Option<Image> {
    let (Ok(width), Ok(height)) = (u32::try_from(image.width), u32::try_from(image.height)) else {
        return None;
    };
    if width == 0 || height == 0 {
        return None;
    }
    if image.bytes.len() > crate::MAX_PIXEL_BYTES {
        tracing::debug!(
            bytes = image.bytes.len(),
            "the clipboard image is too large"
        );
        return None;
    }
    Some(Image {
        width,
        height,
        rgba: image.bytes.into_owned(),
    })
}

/// The pasteboard's change count, which goes up on every write
#[cfg(target_os = "macos")]
fn stamp() -> Option<i64> {
    objc2::rc::autoreleasepool(|_| {
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        Some(pasteboard.changeCount() as i64)
    })
}

/// The clipboard's sequence number, which changes on every write
#[cfg(windows)]
#[allow(unsafe_code)] // a plain query, the clipboard is not opened
fn stamp() -> Option<i64> {
    // SAFETY: no arguments, no pointers
    let number = unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() };
    Some(i64::from(number))
}

/// No change stamp on this system
#[cfg(not(any(target_os = "macos", windows)))]
fn stamp() -> Option<i64> {
    None
}

/// Whether a password manager marked the pasteboard's content concealed
#[cfg(target_os = "macos")]
fn concealed() -> bool {
    use objc2_foundation::{NSArray, NSString};

    // A pool around the autoreleased objects: this runs on a worker
    // thread, which has none of its own
    objc2::rc::autoreleasepool(|_| {
        let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
        let marker = NSString::from_str("org.nspasteboard.ConcealedType");
        let wanted = NSArray::from_slice(&[&*marker]);
        pasteboard.availableTypeFromArray(&wanted).is_some()
    })
}

/// Whether a password manager marked the clipboard's content concealed
#[cfg(windows)]
#[allow(unsafe_code)] // plain queries, the clipboard is not opened
fn concealed() -> bool {
    use windows_sys::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };
    let markers = [
        windows_sys::core::w!("ExcludeClipboardContentFromMonitorProcessing"),
        windows_sys::core::w!("Clipboard Viewer Ignore"),
    ];
    markers.into_iter().any(|name| {
        // SAFETY: `name` is a static NUL-terminated wide string; registering
        // a name again returns the same format, 0 when the table is full
        let format = unsafe { RegisterClipboardFormatW(name) };
        // SAFETY: a plain query about a format number
        format != 0 && unsafe { IsClipboardFormatAvailable(format) } != 0
    })
}

/// No concealed marker on this system
#[cfg(not(any(target_os = "macos", windows)))]
fn concealed() -> bool {
    false
}
