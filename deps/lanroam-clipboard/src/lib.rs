//! lanroam-clipboard: the clipboard layer of Lanroam.
//!
//! ```text
//! ┌─ content ─ what a clipboard holds, in a form that can travel: text, or
//! │            an image as RGBA pixels; its hash, what devices compare,
//! │            and its bytes on the wire (text as UTF-8, images as PNG)
//! ├─ system  ─ this machine's clipboard: read, write, the change stamp,
//! │            and the concealed marker password managers set; on macOS
//! │            images are decoded by the system and made sRGB (pasteboard)
//! └─ memory  ─ a clipboard in memory, for tests
//! ```
//!
//! Only text and images: files on the clipboard are left alone, they come
//! with file transfer. Content a password manager marks as concealed is
//! never read. No networking lives here: the engine (`lanroam-core`)
//! decides when a clipboard goes where.

use thiserror::Error;

mod content;
mod memory;
#[cfg(target_os = "macos")]
mod pasteboard;
mod system;

pub use content::{Content, Image, Kind, MAX_BYTES, MAX_PIXEL_BYTES};
pub use memory::MemoryClipboard;
pub use system::SystemClipboard;

/// Clipboard errors
#[derive(Debug, Error)]
pub enum ClipboardError {
    /// The system clipboard could not be read or written (busy, or its
    /// content could not be converted)
    #[error("clipboard: {0}")]
    Access(#[from] arboard::Error),
    /// An image could not be encoded or decoded
    #[error("clipboard image: {0}")]
    Image(String),
    /// Text that is not UTF-8
    #[error("clipboard text is not UTF-8")]
    Text,
    /// Too big to hand over (see [`MAX_BYTES`], [`MAX_PIXEL_BYTES`])
    #[error("clipboard content too large: {0} bytes")]
    TooLarge(usize),
}

/// A clipboard: this machine's ([`SystemClipboard`]) or a test's
/// ([`MemoryClipboard`]). Every call may block; run them off async
/// executors
pub trait Clipboard: Send + Sync {
    /// A number that changes whenever the clipboard does, cheap to read
    /// (the content is not touched); `None` where the system has none
    fn stamp(&self) -> Option<i64>;
    /// What the clipboard holds, if it is text or an image that may leave
    /// this device: `None` when it is empty, holds files, is concealed, or
    /// is too large
    fn read(&self) -> Result<Option<Content>, ClipboardError>;
    /// Put `content` on the clipboard, as a copy here would; the stamp it
    /// left, read right after
    fn write(&self, content: &Content) -> Result<Option<i64>, ClipboardError>;
}
