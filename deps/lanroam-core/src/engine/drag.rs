//! Drags of files carried from one device to another: the native side (the
//! platform's drag and drop, or a test's), and where the files of a drag
//! coming here are put.
//!
//! The files themselves do not travel yet: a drag coming here drags
//! stand-ins, empty files and folders with their names, ready to drop at
//! once. That is enough to try the drag itself on every desktop. Tests give
//! the stand-ins a transfer time ([`STAND_IN_TRANSFER`]), so that a release
//! before the files are ready is covered too.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lanroam_input::Point;

pub use lanroam_dnd::Event as DragEvent;

use crate::protocol::DragItem;

/// How long the stand-in transfer of a drag coming here takes: released
/// sooner, its drop waits until then
#[cfg(not(test))]
pub(super) const STAND_IN_TRANSFER: Duration = Duration::ZERO;
/// How long the stand-in transfer takes in tests
#[cfg(test)]
pub(super) const STAND_IN_TRANSFER: Duration = Duration::from_millis(200);

/// Hands what the native side reports to the engine; called on any thread
pub type DragSink = Box<dyn Fn(DragEvent) + Send + Sync>;

/// Native drag and drop (the platform's, or a test's)
pub trait DragBackend: Send + Sync {
    /// Start, reporting to `sink`; why not where this device cannot drag
    /// files between devices
    fn start(&self, sink: DragSink) -> Result<Box<dyn Dragging>, String>;
}

/// A running native drag and drop; every call returns at once, and what
/// comes of it goes to the sink (see [`lanroam_dnd::Dnd`] for each)
pub trait Dragging: Send {
    /// The left button went down here
    fn pressed(&self);
    /// Whether the left button held here drags files
    fn probe(&self, id: u64);
    /// The press probed is over
    fn unprobe(&self);
    /// Get ready to drag `paths` from `at`
    fn arm(&self, id: u64, at: Point, paths: Vec<PathBuf>);
    /// Cancel the drag armed with `id`
    fn cancel(&self, id: u64);
}

/// This machine's drag and drop
pub struct PlatformDrag;

impl DragBackend for PlatformDrag {
    fn start(&self, sink: DragSink) -> Result<Box<dyn Dragging>, String> {
        let dnd = lanroam_dnd::Dnd::start(sink).map_err(|e| e.to_string())?;
        Ok(Box::new(dnd))
    }
}

impl Dragging for lanroam_dnd::Dnd {
    fn pressed(&self) {
        lanroam_dnd::Dnd::pressed(self);
    }

    fn probe(&self, id: u64) {
        lanroam_dnd::Dnd::probe(self, id);
    }

    fn unprobe(&self) {
        lanroam_dnd::Dnd::unprobe(self);
    }

    fn arm(&self, id: u64, at: Point, paths: Vec<PathBuf>) {
        lanroam_dnd::Dnd::arm(self, id, at, paths);
    }

    fn cancel(&self, id: u64) {
        lanroam_dnd::Dnd::cancel(self, id);
    }
}

/// No drag and drop: a node without a desktop (the command line)
pub struct NoDrag;

impl DragBackend for NoDrag {
    fn start(&self, _sink: DragSink) -> Result<Box<dyn Dragging>, String> {
        Err("no desktop to drag on".into())
    }
}

/// What each of `paths` is, as the device a drag goes to shows it; paths
/// that cannot be read are left out
pub(super) fn describe(paths: &[PathBuf]) -> Vec<DragItem> {
    paths
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            let meta = fs::metadata(path).ok()?;
            let dir = meta.is_dir();
            Some(DragItem {
                name,
                size: if dir { 0 } else { meta.len() },
                dir,
            })
        })
        .collect()
}

/// Put stand-ins for the files of drag `id` in a folder of their own:
/// empty files and folders with their names. Their paths; names that are
/// not safe to use are left out
pub(super) fn stage_stand_ins(id: u64, items: &[DragItem]) -> io::Result<Vec<PathBuf>> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let dir = std::env::temp_dir()
        .join("lanroam-drops")
        .join(format!("{stamp}-{id}"));
    fs::create_dir_all(&dir)?;
    let mut paths = Vec::new();
    for item in items {
        let Some(name) = safe_name(&item.name) else {
            tracing::warn!(name = %item.name, "a dragged file with an unsafe name is left out");
            continue;
        };
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        if item.dir {
            fs::create_dir(&path)?;
        } else {
            fs::write(&path, b"")?;
        }
        paths.push(path);
    }
    Ok(paths)
}

/// `name` if it is a plain file name that is safe to put in a folder on
/// any system: no separators or parent, no control characters, nothing
/// Windows would trim
fn safe_name(name: &str) -> Option<&str> {
    let unsafe_char = |c: char| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
    let bad = name.is_empty()
        || name.len() > 255
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| c.is_control() || unsafe_char(c));
    (!bad).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file item
    fn file(name: &str) -> DragItem {
        DragItem {
            name: name.into(),
            size: 7,
            dir: false,
        }
    }

    #[test]
    fn only_plain_names_are_safe() {
        for name in ["报告 final.pdf", ".hidden", "a..b", "photos"] {
            assert_eq!(safe_name(name), Some(name));
        }
        for name in [
            "", ".", "..", "../x", "a/b", "a\\b", "C:x", "x\n", "tail.", "tail ",
        ] {
            assert_eq!(safe_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn stand_ins_take_the_names() {
        let items = [
            file("notes.txt"),
            DragItem {
                name: "photos".into(),
                size: 0,
                dir: true,
            },
            file("../escape.txt"),
            file("notes.txt"),
        ];
        let paths = stage_stand_ins(u64::MAX, &items).unwrap();
        let dir = paths[0].parent().unwrap().to_path_buf();
        assert_eq!(paths, [dir.join("notes.txt"), dir.join("photos")]);
        assert!(paths[0].is_file() && paths[1].is_dir());
        assert_eq!(
            describe(&paths),
            [file("notes.txt"), items[1].clone()].map(|mut item| {
                item.size = 0;
                item
            })
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
