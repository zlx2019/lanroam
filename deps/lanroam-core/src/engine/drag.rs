//! Drags of files carried from one device to another: the native side (the
//! platform's drag and drop, or a test's), and the folders the files of a
//! drag coming here land in.
//!
//! Each drag coming here gets a folder of its own, with an empty stand-in
//! for each file and folder dragged: the native drag needs them from the
//! start, and they fill in as the files arrive (see [`super::files`]).
//! A folder goes a while after its drop (the app it was dropped on may
//! still be copying from it), at once when the drag is cancelled, and at
//! the next start when it is older than a day. Files copied on another
//! device and fetched ahead of a paste land in folders of the same kind
//! (see [`super::clipboard`]).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lanroam_dnd::Listed;
use lanroam_input::Point;

pub use lanroam_dnd::Event as DragEvent;

use super::files;
use crate::protocol::DragItem;

/// How long the folder of a drop stays
pub(super) const KEEP_AFTER_DROP: Duration = Duration::from_secs(10 * 60);

/// Folders of drags older than this are left over: swept at start
const LEFT_OVER: Duration = Duration::from_secs(24 * 60 * 60);

/// Why a drag of files did not come here
/// ([`super::EngineEvent::DragFailed`])
pub mod failed {
    /// Not enough free space for the files
    pub const NO_SPACE: &str = "no_space";
    /// The files did not arrive (the link dropped, or they changed or went
    /// away meanwhile)
    pub const TRANSFER: &str = "transfer";
    /// Let go on an app that cannot take them before they are all there
    /// (only the file manager can), or one that took nothing
    pub const REFUSED: &str = "refused";
}

/// Files dragged here still arriving while their drop waits, or after they
/// were dropped (promised to the app they landed on)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receiving {
    /// The drag ([`super::Engine::cancel_drop`] cancels it by this)
    pub id: u64,
    /// Where they will land (this device's coordinates)
    pub at: Point,
    /// The first file or folder dragged
    pub name: String,
    /// How many were dragged
    pub count: usize,
    /// Bytes there so far
    pub done: u64,
    /// Bytes in all
    pub total: u64,
    /// Esc cancels: the drop waits (once dropped, its card cancels it)
    pub cancel: bool,
}

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
    /// Get ready to drag `paths` from `at`, their files all there or not
    /// (`ready`)
    fn arm(&self, id: u64, at: Point, paths: Vec<PathBuf>, ready: bool);
    /// Everything drag `id` carries (see [`lanroam_dnd::Dnd::listed`])
    fn listed(&self, _id: u64, _entries: Vec<Listed>) {}
    /// Cancel the drag armed with `id`
    fn cancel(&self, id: u64);
    /// Whether drag `id`, let go at `at`, lands where it can (see
    /// [`lanroam_dnd::Dnd::takes`])
    fn takes(&self, _id: u64, _at: Point) -> bool {
        true
    }
    /// Whether a drag armed before its files are all there drops as soon
    /// as the button goes up (see [`lanroam_dnd::Dnd::drops_early`])
    fn drops_early(&self) -> bool {
        false
    }
    /// The files of drag `id` are all there (`true`), or will not come
    fn deliver(&self, _id: u64, _ok: bool) {}
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

    fn arm(&self, id: u64, at: Point, paths: Vec<PathBuf>, ready: bool) {
        lanroam_dnd::Dnd::arm(self, id, at, paths, ready);
    }

    fn listed(&self, id: u64, entries: Vec<Listed>) {
        lanroam_dnd::Dnd::listed(self, id, entries);
    }

    fn cancel(&self, id: u64) {
        lanroam_dnd::Dnd::cancel(self, id);
    }

    fn takes(&self, id: u64, at: Point) -> bool {
        lanroam_dnd::Dnd::takes(self, id, at)
    }

    fn drops_early(&self) -> bool {
        lanroam_dnd::Dnd::drops_early(self)
    }

    fn deliver(&self, id: u64, ok: bool) {
        lanroam_dnd::Dnd::deliver(self, id, ok);
    }
}

/// No drag and drop: a node without a desktop (the command line)
pub struct NoDrag;

impl DragBackend for NoDrag {
    fn start(&self, _sink: DragSink) -> Result<Box<dyn Dragging>, String> {
        Err("no desktop to drag on".into())
    }
}

/// What each of `paths` is, as the device a drag goes to shows it (a
/// folder with the size of everything in it); paths that cannot be read
/// are left out
pub(super) fn describe(paths: &[PathBuf]) -> Vec<DragItem> {
    paths
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            let (dir, size) = files::measure(path).ok()?;
            Some(DragItem { name, size, dir })
        })
        .collect()
}

/// Where the folders of drags coming here go
fn root() -> PathBuf {
    root_for(cfg!(any(target_os = "macos", windows)), |name| {
        std::env::var_os(name)
    })
}

/// [`root`] on a system whose temporary folder is the user's own
/// (`private_temp`: macOS, Windows) or not, reading the environment through
/// `var`. A temporary folder shared by all users (/tmp) would let the
/// others read the files, or put a link where the folders go, so there
/// they go to the user's cache folder
fn root_for(private_temp: bool, var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let temp = || std::env::temp_dir().join("lanroam-drops");
    if private_temp {
        return temp();
    }
    // An unset, empty or relative XDG_CACHE_HOME means ~/.cache
    let cache = var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".cache")));
    cache.map_or_else(temp, |cache| cache.join("lanroam").join("drops"))
}

/// A new, empty folder for files coming here, told apart by `name`
pub(super) fn folder(name: &str) -> io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let dir = root().join(format!("{stamp}-{name}"));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A new folder for drag `id`, with an empty stand-in for each of `items`
/// under the name it lands with; the folder, and the stand-ins' paths
pub(super) fn stage(id: u64, items: &[DragItem]) -> io::Result<(PathBuf, Vec<PathBuf>)> {
    let dir = folder(&id.to_string())?;
    let mut paths = Vec::new();
    for item in items {
        // One name each, as the files will arrive under it
        let Ok(name) = files::safe_path(&item.name) else {
            tracing::warn!(name = %item.name, "a dragged file with no usable name is left out");
            continue;
        };
        let path = dir.join(name);
        if path.parent() != Some(dir.as_path()) || path.exists() {
            continue;
        }
        if item.dir {
            fs::create_dir(&path)?;
        } else {
            fs::write(&path, b"")?;
        }
        paths.push(path);
    }
    Ok((dir, paths))
}

/// Remove the folder of a drag, and what it holds
pub(super) fn discard(dir: &Path) {
    if let Err(e) = fs::remove_dir_all(dir)
        && e.kind() != io::ErrorKind::NotFound
    {
        tracing::debug!(folder = %dir.display(), "cannot remove the folder of a drag: {e}");
    }
}

/// Remove the folders of drags left over from earlier runs
pub(super) fn sweep() {
    let Ok(read) = fs::read_dir(root()) else {
        return;
    };
    for entry in read.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > LEFT_OVER);
        if old {
            discard(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    /// A file item
    fn file(name: &str, size: u64) -> DragItem {
        DragItem {
            name: name.into(),
            size,
            dir: false,
        }
    }

    #[test]
    fn stand_ins_take_the_names() {
        let items = [
            file("notes.txt", 7),
            DragItem {
                name: "photos".into(),
                size: 0,
                dir: true,
            },
            file("../escape.txt", 1),
            file("a/b.txt", 1),
            file("notes.txt", 7),
        ];
        let (dir, paths) = stage(u64::MAX, &items).unwrap();
        assert_eq!(paths, [dir.join("notes.txt"), dir.join("photos")]);
        assert!(paths[0].is_file() && paths[1].is_dir());
        discard(&dir);
        assert!(!dir.exists());
    }

    /// Drops go to the temporary folder where it is the user's own, and to
    /// the user's cache folder where it is shared
    #[cfg(unix)]
    #[test]
    fn drops_stay_out_of_shared_folders() {
        /// An environment holding `vars`
        fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
            move |name| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(*value))
            }
        }

        let temp = std::env::temp_dir().join("lanroam-drops");
        let home = [("HOME", "/home/zero")];
        assert_eq!(root_for(true, env(&home)), temp);
        assert_eq!(
            root_for(false, env(&home)),
            PathBuf::from("/home/zero/.cache/lanroam/drops")
        );
        let xdg = [("HOME", "/home/zero"), ("XDG_CACHE_HOME", "/data/cache")];
        assert_eq!(
            root_for(false, env(&xdg)),
            PathBuf::from("/data/cache/lanroam/drops")
        );
        let relative = [("HOME", "/home/zero"), ("XDG_CACHE_HOME", "cache")];
        assert_eq!(
            root_for(false, env(&relative)),
            PathBuf::from("/home/zero/.cache/lanroam/drops")
        );
        assert_eq!(root_for(false, env(&[])), temp);
    }

    #[test]
    fn folders_are_described_by_what_they_hold() {
        let dir = TempDir::new();
        let photos = dir.0.join("photos");
        fs::create_dir_all(photos.join("inner")).unwrap();
        fs::write(photos.join("a.jpg"), b"123").unwrap();
        fs::write(photos.join("inner/b.jpg"), b"45").unwrap();
        fs::write(dir.0.join("notes.txt"), b"hello").unwrap();
        let paths = [photos, dir.0.join("notes.txt"), dir.0.join("missing")];
        assert_eq!(
            describe(&paths),
            [
                DragItem {
                    name: "photos".into(),
                    size: 5,
                    dir: true
                },
                file("notes.txt", 5)
            ]
        );
    }
}
