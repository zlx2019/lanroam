//! The files of a drag, from the device they are on to the device the
//! pointer took the drag to.
//!
//! The device the files are on offers them under a token ([`Offers`]),
//! which travels with the drag to whichever device drags them on. That
//! device pulls them over one content stream ([`pull`]; [`serve`] on the
//! other side): their count and size, a listing of every folder and file
//! when asked (what an app a drop lands on before they are all there
//! learns is coming), then folder by folder and file by file, each file
//! followed by its BLAKE3. A file lands as `.part` and takes its name once
//! its hash matches. Paths from the other side are
//! made safe for this system first: nothing gets out of the folder the
//! files land in, and no name is one Windows refuses.
//!
//! Nothing resumes: a drag that breaks off is dragged again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lan_kit::frame::FrameError;
use lanroam_dnd::{Listed, PART_SUFFIX};
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::protocol::{DragPart, DragReply, FRAMING, StreamRequest};

/// How much of a file moves at once
const CHUNK: usize = 256 * 1024;

/// Longest a stream may stall before the transfer gives up
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a whole file tries to take its name, and how long it waits
/// in between
const RENAME_TRIES: u32 = 20;
/// See [`RENAME_TRIES`]
const RENAME_WAIT: Duration = Duration::from_millis(50);

/// How long files stay offered
const OFFER_LIFE: Duration = Duration::from_secs(60 * 60);

/// Priority of the stream, below the control stream's (0): input goes out
/// first
const CONTENT_PRIORITY: i32 = -1;

/// Windows device names, refused as file names with any extension
const WINDOWS_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Why the files of a drag did not get through
#[derive(Debug, thiserror::Error)]
pub(super) enum FilesError {
    /// Reading or writing the files
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A frame could not be read or written
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// The connection failed
    #[error(transparent)]
    Connection(#[from] quinn::ConnectionError),
    /// The stream was closed already
    #[error(transparent)]
    Closed(#[from] quinn::ClosedStream),
    /// Not offered (any more)
    #[error("the files are not offered any more")]
    Gone,
    /// A path that would leave the folder, or name nothing
    #[error("unsafe path {0:?}")]
    Path(String),
    /// A file whose hash does not match
    #[error("{0} arrived damaged")]
    Mismatch(String),
    /// Nothing moved for too long
    #[error("the transfer stalled")]
    Stalled,
    /// More bytes than the pull takes: those found
    #[error("too large: {0} bytes")]
    TooLarge(u64),
    /// A part out of place
    #[error("unexpected {0} in the stream")]
    Unexpected(&'static str),
    /// No random token
    #[error("no random token: {0}")]
    Random(String),
}

/// Files this device offers for drags, by token
#[derive(Debug, Clone, Default)]
pub(crate) struct Offers(Arc<Mutex<HashMap<String, Offer>>>);

/// Files offered for one drag
#[derive(Debug)]
struct Offer {
    /// What was dragged
    paths: Vec<PathBuf>,
    /// Since when
    since: Instant,
}

impl Offers {
    /// Offer `paths` under a new token; older offers past their life go
    pub(super) fn offer(&self, paths: Vec<PathBuf>) -> Result<String, FilesError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| FilesError::Random(e.to_string()))?;
        let token = hex::encode(bytes);
        let mut offers = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        offers.retain(|_, offer| offer.since.elapsed() < OFFER_LIFE);
        let offer = Offer {
            paths,
            since: Instant::now(),
        };
        offers.insert(token.clone(), offer);
        Ok(token)
    }

    /// The files offered under `token`, while it lasts
    fn paths(&self, token: &str) -> Option<Vec<PathBuf>> {
        let offers = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        offers
            .get(token)
            .filter(|offer| offer.since.elapsed() < OFFER_LIFE)
            .map(|offer| offer.paths.clone())
    }
}

/// One thing a drag sends
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Entry {
    /// A folder, by its path in the drag
    Dir(String),
    /// A file: where it is, its path in the drag, its size
    File(PathBuf, String, u64),
}

/// Everything a drag of `paths` sends, folders before what they hold.
/// Links inside folders are left out (they could loop), and so is what
/// cannot be read there; what was dragged itself must be readable
pub(super) fn collect(paths: &[PathBuf]) -> std::io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for path in paths {
        let meta = std::fs::metadata(path)?;
        let Some(name) = path.file_name() else {
            continue;
        };
        let name = name.to_string_lossy().into_owned();
        if meta.is_dir() {
            entries.push(Entry::Dir(name.clone()));
            walk(path, &name, &mut entries);
        } else if meta.is_file() {
            entries.push(Entry::File(path.clone(), name, meta.len()));
        }
    }
    Ok(entries)
}

/// Add what folder `dir` holds, as `rel/...`
fn walk(dir: &Path, rel: &str, entries: &mut Vec<Entry>) {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) => {
            tracing::warn!(folder = %dir.display(), "a dragged folder cannot be read: {e}");
            return;
        }
    };
    for entry in read.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        let rel = format!("{rel}/{}", entry.file_name().to_string_lossy());
        if kind.is_dir() {
            entries.push(Entry::Dir(rel.clone()));
            walk(&path, &rel, entries);
        } else if kind.is_file()
            && let Ok(meta) = entry.metadata()
        {
            entries.push(Entry::File(path, rel, meta.len()));
        }
    }
}

/// Whether `path` is a folder, and the bytes a drag of it sends, counted by
/// the rules of [`collect`] without gathering every entry: describing a
/// large folder takes no memory per file
pub(super) fn measure(path: &Path) -> std::io::Result<(bool, u64)> {
    let meta = std::fs::metadata(path)?;
    if meta.is_dir() {
        Ok((true, folder_bytes(path)))
    } else if meta.is_file() {
        Ok((false, meta.len()))
    } else {
        Ok((false, 0))
    }
}

/// The bytes of the files in folder `dir`, by the rules of [`walk`]
fn folder_bytes(dir: &Path) -> u64 {
    let Ok(read) = std::fs::read_dir(dir) else {
        return 0;
    };
    read.flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => folder_bytes(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |meta| meta.len()),
            _ => 0,
        })
        .sum()
}

/// How many files `entries` hold, and their bytes
pub(super) fn totals(entries: &[Entry]) -> (u64, u64) {
    entries
        .iter()
        .fold((0, 0), |(count, bytes), entry| match entry {
            Entry::Dir(_) => (count, bytes),
            Entry::File(_, _, size) => (count + 1, bytes + size),
        })
}

/// `rel`, a path from the other side, as a path safe to join to a folder
/// here: `/` or `\` separated, only plain names, each made valid on this
/// system
pub(super) fn safe_path(rel: &str) -> Result<PathBuf, FilesError> {
    safe_path_for(rel, cfg!(windows))
}

/// [`safe_path`] with Windows' rules or not. Split by hand: parsed as a
/// path first, `C:` would be a drive on Windows, and joined it would
/// replace the folder
fn safe_path_for(rel: &str, windows: bool) -> Result<PathBuf, FilesError> {
    let unsafe_path = || FilesError::Path(rel.to_string());
    let normalized = rel.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(unsafe_path());
    }
    let mut out = PathBuf::new();
    for name in normalized.split('/') {
        match name {
            "" | "." => {}
            ".." => return Err(unsafe_path()),
            name => out.push(safe_name(name, windows)),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(unsafe_path());
    }
    Ok(out)
}

/// One name made valid: control characters go everywhere; on Windows also
/// the characters it refuses, trailing dots and spaces (which it would
/// drop), and device names (which get a `_` in front)
fn safe_name(name: &str, windows: bool) -> String {
    let refused = |c: char| windows && matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*');
    let mut clean: String = name
        .chars()
        .map(|c| if c.is_control() || refused(c) { '_' } else { c })
        .collect();
    if !windows {
        return clean;
    }
    let trimmed = clean.trim_end_matches(['.', ' ']);
    if trimmed.len() != clean.len() {
        clean = if trimmed.is_empty() {
            "_".to_string()
        } else {
            trimmed.to_string()
        };
    }
    let stem = clean.split('.').next().unwrap_or_default();
    if WINDOWS_RESERVED
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        clean.insert(0, '_');
    }
    clean
}

/// Answer a content stream asking for the files of `token`, listed first
/// if `listing`
pub(super) async fn serve(
    mut send: quinn::SendStream,
    token: String,
    listing: bool,
    offers: Offers,
) -> Result<(), FilesError> {
    let Some(paths) = offers.paths(&token) else {
        FRAMING.write(&mut send, &DragReply::Gone).await?;
        send.finish()?;
        return Ok(());
    };
    send.set_priority(CONTENT_PRIORITY)?;
    let entries = tokio::task::spawn_blocking(move || collect(&paths))
        .await
        .map_err(|_| FilesError::Unexpected("stop"))?;
    let entries = match entries {
        Ok(entries) => entries,
        // Gone since they were dragged
        Err(e) => {
            FRAMING.write(&mut send, &DragReply::Gone).await?;
            send.finish()?;
            return Err(e.into());
        }
    };
    send_entries(&mut send, &entries, listing).await?;
    send.finish()?;
    // Stay until everything is through (or the other side gave up)
    let _ = send.stopped().await;
    Ok(())
}

/// Pull the files of `token` from the device behind `conn` into `dir`, up
/// to `limit` bytes if given, telling `listed` what is coming (unless the
/// other side is older than that) and `progress` the bytes so far and in
/// all
pub(super) async fn pull(
    conn: &quinn::Connection,
    token: String,
    dir: &Path,
    limit: Option<u64>,
    listed: impl FnOnce(Vec<Listed>),
    progress: impl FnMut(u64, u64),
) -> Result<(), FilesError> {
    let (mut send, mut recv) = conn.open_bi().await?;
    let request = StreamRequest::Drag {
        token,
        listing: true,
    };
    FRAMING.write(&mut send, &request).await?;
    send.finish()?;
    receive_entries(&mut recv, dir, limit, listed, progress).await
}

/// Write `entries` as a [`DragReply`] and [`DragPart`]s, listed first if
/// `listing`, the files' bytes in between
async fn send_entries<W>(w: &mut W, entries: &[Entry], listing: bool) -> Result<(), FilesError>
where
    W: AsyncWrite + Unpin,
{
    let (count, bytes) = totals(entries);
    if listing {
        FRAMING
            .write(w, &DragReply::Listing { count, bytes })
            .await?;
        for entry in entries {
            FRAMING.write(w, &header(entry)).await?;
        }
        FRAMING.write(w, &DragPart::Listed).await?;
    } else {
        FRAMING.write(w, &DragReply::Files { count, bytes }).await?;
    }
    let mut buf = vec![0u8; CHUNK];
    for entry in entries {
        FRAMING.write(w, &header(entry)).await?;
        if let Entry::File(from, _, size) = entry {
            let hash = send_file(w, from, *size, &mut buf).await?;
            FRAMING.write(w, &DragPart::Hash { hash }).await?;
        }
    }
    FRAMING.write(w, &DragPart::Done).await?;
    Ok(())
}

/// The part announcing `entry`
fn header(entry: &Entry) -> DragPart {
    match entry {
        Entry::Dir(path) => DragPart::Dir { path: path.clone() },
        Entry::File(_, path, size) => DragPart::File {
            path: path.clone(),
            size: *size,
        },
    }
}

/// Write `size` bytes of the file at `path`; their hash
async fn send_file<W>(
    w: &mut W,
    path: &Path,
    size: u64,
    buf: &mut [u8],
) -> Result<String, FilesError>
where
    W: AsyncWrite + Unpin,
{
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = blake3::Hasher::new();
    let mut left = size;
    while left > 0 {
        let want = usize::try_from(left).map_or(buf.len(), |left| left.min(buf.len()));
        let n = file.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("{} got shorter while it was sent", path.display()),
            )
            .into());
        }
        hasher.update(&buf[..n]);
        tokio::time::timeout(IDLE_TIMEOUT, w.write_all(&buf[..n]))
            .await
            .map_err(|_| FilesError::Stalled)??;
        left -= n as u64;
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Read what [`send_entries`] writes into `dir`, telling `listed` about a
/// listing; past `limit` bytes, if given, it stops before writing more:
/// told so by the total, or by the file that would go past
async fn receive_entries<R>(
    r: &mut R,
    dir: &Path,
    limit: Option<u64>,
    listed: impl FnOnce(Vec<Listed>),
    mut progress: impl FnMut(u64, u64),
) -> Result<(), FilesError>
where
    R: AsyncRead + Unpin,
{
    let over = |bytes: u64| limit.is_some_and(|limit| bytes > limit);
    let (total, listing) = match read_frame(r).await? {
        DragReply::Files { bytes, .. } => (bytes, false),
        DragReply::Listing { bytes, .. } => (bytes, true),
        DragReply::Gone => return Err(FilesError::Gone),
    };
    if over(total) {
        return Err(FilesError::TooLarge(total));
    }
    if listing {
        listed(receive_listing(r).await?);
    }
    let mut done: u64 = 0;
    let mut buf = vec![0u8; CHUNK];
    loop {
        match read_frame(r).await? {
            DragPart::Dir { path } => {
                tokio::fs::create_dir_all(dir.join(safe_path(&path)?)).await?;
            }
            DragPart::File { path, size } => {
                let after = done.saturating_add(size);
                if over(after) {
                    return Err(FilesError::TooLarge(after));
                }
                let to = dir.join(safe_path(&path)?);
                if let Some(parent) = to.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let part = part_of(&to);
                let hash = receive_file(r, &part, size, &mut buf, |n| {
                    done += n;
                    progress(done, total);
                })
                .await;
                let hash = match hash {
                    Ok(hash) => hash,
                    Err(e) => {
                        let _ = tokio::fs::remove_file(&part).await;
                        return Err(e);
                    }
                };
                let DragPart::Hash { hash: sent } = read_frame(r).await? else {
                    return Err(FilesError::Unexpected("part instead of a hash"));
                };
                if hash != sent {
                    let _ = tokio::fs::remove_file(&part).await;
                    return Err(FilesError::Mismatch(path));
                }
                settle(&part, &to).await?;
            }
            DragPart::Hash { .. } => return Err(FilesError::Unexpected("hash")),
            DragPart::Listed => return Err(FilesError::Unexpected("end of a listing")),
            DragPart::Done => return Ok(()),
        }
    }
}

/// Read a listing up to its end, each path made safe
async fn receive_listing<R>(r: &mut R) -> Result<Vec<Listed>, FilesError>
where
    R: AsyncRead + Unpin,
{
    let mut entries = Vec::new();
    loop {
        let (path, size, dir) = match read_frame(r).await? {
            DragPart::Dir { path } => (path, 0, true),
            DragPart::File { path, size } => (path, size, false),
            DragPart::Listed => return Ok(entries),
            DragPart::Hash { .. } | DragPart::Done => {
                return Err(FilesError::Unexpected("part in a listing"));
            }
        };
        let path = safe_path(&path)?;
        entries.push(Listed { path, size, dir });
    }
}

/// Read `size` bytes into a new file at `path`, telling `arrived` about
/// each chunk; their hash
async fn receive_file<R>(
    r: &mut R,
    path: &Path,
    size: u64,
    buf: &mut [u8],
    mut arrived: impl FnMut(u64),
) -> Result<String, FilesError>
where
    R: AsyncRead + Unpin,
{
    let mut file = tokio::fs::File::create(path).await?;
    let mut hasher = blake3::Hasher::new();
    let mut left = size;
    while left > 0 {
        let want = usize::try_from(left).map_or(buf.len(), |left| left.min(buf.len()));
        let n = tokio::time::timeout(IDLE_TIMEOUT, r.read(&mut buf[..want]))
            .await
            .map_err(|_| FilesError::Stalled)??;
        if n == 0 {
            return Err(FilesError::Unexpected("end of the stream"));
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await?;
        left -= n as u64;
        arrived(n as u64);
    }
    file.flush().await?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Give the whole file at `part` its name `to`, over the stand-in the drag
/// has shown until now. Tried a few times: Windows refuses while someone
/// has the stand-in open (the shell drawing the drag's icon)
async fn settle(part: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match tokio::fs::rename(part, to).await {
            Ok(()) => return Ok(()),
            Err(_) if tries < RENAME_TRIES => {
                tries += 1;
                tokio::time::sleep(RENAME_WAIT).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// One frame, unless the stream stalls
async fn read_frame<R, T>(r: &mut R) -> Result<T, FilesError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    tokio::time::timeout(IDLE_TIMEOUT, FRAMING.read(r))
        .await
        .map_err(|_| FilesError::Stalled)?
        .map_err(Into::into)
}

/// Where the file for `path` is written until it is whole
fn part_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(PART_SUFFIX);
    path.with_file_name(name)
}

/// Bytes free where `dir` is (or its nearest existing parent), if known
pub(super) fn available_space(dir: &Path) -> Option<u64> {
    let mut probe = dir;
    loop {
        if probe.exists() {
            return fs4::available_space(probe).ok();
        }
        probe = probe.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    /// A tree to drag: a file, and a folder with a file, a subfolder and
    /// an empty folder
    fn tree(dir: &Path) -> Vec<PathBuf> {
        std::fs::write(dir.join("notes.txt"), b"hello").unwrap();
        let photos = dir.join("photos");
        std::fs::create_dir_all(photos.join("2026/trip")).unwrap();
        std::fs::create_dir(photos.join("empty")).unwrap();
        std::fs::write(photos.join("cover.jpg"), vec![7u8; CHUNK + 3]).unwrap();
        std::fs::write(photos.join("2026/trip/a.jpg"), b"a").unwrap();
        vec![dir.join("notes.txt"), photos]
    }

    #[test]
    fn paths_are_made_safe() {
        let ok = |rel: &str, windows: bool| safe_path_for(rel, windows).unwrap();
        let path = |parts: &[&str]| parts.iter().collect::<PathBuf>();
        for windows in [false, true] {
            assert_eq!(ok("photos/a.jpg", windows), path(&["photos", "a.jpg"]));
            assert_eq!(ok("photos\\a.jpg", windows), path(&["photos", "a.jpg"]));
            assert_eq!(ok("./a/./b", windows), path(&["a", "b"]));
            assert_eq!(ok("x\ty", windows), path(&["x_y"]));
            for bad in ["", ".", "../x", "a/../../x", "/etc/passwd", "\\\\?\\C:\\x"] {
                assert!(safe_path_for(bad, windows).is_err(), "{bad:?}");
            }
        }
        assert_eq!(ok("a:b?.txt", true), path(&["a_b_.txt"]));
        assert_eq!(ok("C:\\x", true), path(&["C_", "x"]));
        assert_eq!(ok("con.txt", true), path(&["_con.txt"]));
        assert_eq!(ok("tail. ", true), path(&["tail"]));
        assert_eq!(ok("... ", true), path(&["_"]));
        #[cfg(not(windows))]
        assert_eq!(ok("a:b?.txt", false), path(&["a:b?.txt"]));
    }

    #[test]
    fn a_tree_is_collected_folders_first() {
        let dir = TempDir::new();
        let mut entries = collect(&tree(&dir.0)).unwrap();
        entries.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        let names: Vec<String> = entries
            .iter()
            .map(|entry| match entry {
                Entry::Dir(path) => format!("{path}/"),
                Entry::File(_, path, size) => format!("{path} {size}"),
            })
            .collect();
        let big = CHUNK + 3;
        assert_eq!(
            names,
            [
                "photos/".to_string(),
                "photos/2026/".into(),
                "photos/2026/trip/".into(),
                "photos/empty/".into(),
                "notes.txt 5".into(),
                "photos/2026/trip/a.jpg 1".into(),
                format!("photos/cover.jpg {big}"),
            ]
        );
        assert_eq!(totals(&entries), (3, 5 + 1 + big as u64));
    }

    /// What is dragged measures what a drag of it sends, links inside a
    /// folder left out
    #[test]
    fn a_drag_measures_what_it_sends() {
        let dir = TempDir::new();
        let paths = tree(&dir.0);
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.0.join("notes.txt"), dir.0.join("photos/link.txt")).unwrap();
        for path in &paths {
            let entries = collect(std::slice::from_ref(path)).unwrap();
            let folder = matches!(entries.first(), Some(Entry::Dir(_)));
            let (_, bytes) = totals(&entries);
            assert_eq!(measure(path).unwrap(), (folder, bytes), "{path:?}");
        }
        assert!(measure(&dir.0.join("missing")).is_err());
    }

    #[tokio::test]
    async fn a_tree_travels_whole() {
        let (from, to) = (TempDir::new(), TempDir::new());
        let entries = collect(&tree(&from.0)).unwrap();
        let (mut w, mut r) = tokio::io::duplex(64 * 1024);
        let sending = tokio::spawn(async move { send_entries(&mut w, &entries, false).await });
        let mut seen = Vec::new();
        let mut listing = None;
        receive_entries(
            &mut r,
            &to.0,
            None,
            |entries| listing = Some(entries),
            |done, total| seen.push((done, total)),
        )
        .await
        .unwrap();
        sending.await.unwrap().unwrap();
        // Not asked for
        assert_eq!(listing, None);

        let read = |rel: &str| std::fs::read(to.0.join(rel)).unwrap();
        assert_eq!(read("notes.txt"), b"hello");
        assert_eq!(read("photos/cover.jpg"), vec![7u8; CHUNK + 3]);
        assert_eq!(read("photos/2026/trip/a.jpg"), b"a");
        assert!(to.0.join("photos/empty").is_dir());
        let total = (5 + 1 + CHUNK + 3) as u64;
        assert_eq!(seen.last(), Some(&(total, total)));
        let parts = std::fs::read_dir(to.0.join("photos")).unwrap();
        assert!(
            parts
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(PART_SUFFIX))
        );
    }

    #[tokio::test]
    async fn a_listing_comes_first() {
        let (from, to) = (TempDir::new(), TempDir::new());
        let entries = collect(&tree(&from.0)).unwrap();
        let expected: Vec<Listed> = entries
            .iter()
            .map(|entry| match entry {
                Entry::Dir(path) => Listed {
                    path: safe_path(path).unwrap(),
                    size: 0,
                    dir: true,
                },
                Entry::File(_, path, size) => Listed {
                    path: safe_path(path).unwrap(),
                    size: *size,
                    dir: false,
                },
            })
            .collect();
        let (mut w, mut r) = tokio::io::duplex(64 * 1024);
        let sending = tokio::spawn(async move { send_entries(&mut w, &entries, true).await });
        let mut listing = None;
        let mut first_bytes = None;
        receive_entries(
            &mut r,
            &to.0,
            None,
            |entries| listing = Some(entries),
            |done, _| {
                first_bytes.get_or_insert(done);
            },
        )
        .await
        .unwrap();
        sending.await.unwrap().unwrap();
        assert_eq!(listing, Some(expected));
        assert!(first_bytes.is_some());
        assert_eq!(std::fs::read(to.0.join("notes.txt")).unwrap(), b"hello");
        assert!(to.0.join("photos/empty").is_dir());
    }

    #[tokio::test]
    async fn a_damaged_file_is_refused() {
        let to = TempDir::new();
        let (mut w, mut r) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let parts = (
                DragReply::Files { count: 1, bytes: 3 },
                DragPart::File {
                    path: "x.bin".into(),
                    size: 3,
                },
            );
            FRAMING.write(&mut w, &parts.0).await.unwrap();
            FRAMING.write(&mut w, &parts.1).await.unwrap();
            w.write_all(b"abc").await.unwrap();
            let hash = DragPart::Hash {
                hash: blake3::hash(b"abx").to_hex().to_string(),
            };
            FRAMING.write(&mut w, &hash).await.unwrap();
        });
        let got = receive_entries(&mut r, &to.0, None, |_| {}, |_, _| {}).await;
        assert!(
            matches!(got, Err(FilesError::Mismatch(ref path)) if path == "x.bin"),
            "{got:?}"
        );
        assert_eq!(std::fs::read_dir(&to.0).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn gone_and_unsafe_are_refused() {
        let to = TempDir::new();
        let (mut w, mut r) = tokio::io::duplex(1024);
        FRAMING.write(&mut w, &DragReply::Gone).await.unwrap();
        let got = receive_entries(&mut r, &to.0, None, |_| {}, |_, _| {}).await;
        assert!(matches!(got, Err(FilesError::Gone)), "{got:?}");

        let (mut w, mut r) = tokio::io::duplex(1024);
        FRAMING
            .write(&mut w, &DragReply::Files { count: 0, bytes: 0 })
            .await
            .unwrap();
        let escape = DragPart::Dir {
            path: "../out".into(),
        };
        FRAMING.write(&mut w, &escape).await.unwrap();
        let got = receive_entries(&mut r, &to.0, None, |_| {}, |_, _| {}).await;
        assert!(matches!(got, Err(FilesError::Path(_))), "{got:?}");

        // In a listing too
        let (mut w, mut r) = tokio::io::duplex(1024);
        FRAMING
            .write(&mut w, &DragReply::Listing { count: 0, bytes: 0 })
            .await
            .unwrap();
        FRAMING.write(&mut w, &escape).await.unwrap();
        let got = receive_entries(&mut r, &to.0, None, |_| {}, |_, _| {}).await;
        assert!(matches!(got, Err(FilesError::Path(_))), "{got:?}");
    }

    /// A pull takes up to its limit; past it, it stops before writing
    /// more, told so by the total or by the file that would go past
    #[tokio::test]
    async fn past_the_limit_is_refused() {
        let from = TempDir::new();
        let entries = collect(&tree(&from.0)).unwrap();
        let (_, bytes) = totals(&entries);
        let pull = |limit: u64| {
            let entries = entries.clone();
            async move {
                let to = TempDir::new();
                let (mut w, mut r) = tokio::io::duplex(64 * 1024);
                tokio::spawn(async move { send_entries(&mut w, &entries, true).await });
                let got = receive_entries(&mut r, &to.0, Some(limit), |_| {}, |_, _| {}).await;
                (got, to)
            }
        };
        let (got, _to) = pull(bytes).await;
        assert!(got.is_ok(), "{got:?}");
        let (got, to) = pull(bytes - 1).await;
        assert!(
            matches!(got, Err(FilesError::TooLarge(b)) if b == bytes),
            "{got:?}"
        );
        assert_eq!(std::fs::read_dir(&to.0).unwrap().count(), 0);

        // Files adding up to more than the total they came with
        let to = TempDir::new();
        let (mut w, mut r) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let file = |path: &str, size| DragPart::File {
                path: path.into(),
                size,
            };
            let hash = blake3::hash(b"a").to_hex().to_string();
            FRAMING
                .write(&mut w, &DragReply::Files { count: 2, bytes: 2 })
                .await
                .unwrap();
            FRAMING.write(&mut w, &file("a.bin", 1)).await.unwrap();
            w.write_all(b"a").await.unwrap();
            FRAMING
                .write(&mut w, &DragPart::Hash { hash })
                .await
                .unwrap();
            FRAMING.write(&mut w, &file("b.bin", 100)).await.unwrap();
        });
        let got = receive_entries(&mut r, &to.0, Some(10), |_| {}, |_, _| {}).await;
        assert!(matches!(got, Err(FilesError::TooLarge(101))), "{got:?}");
        let landed: Vec<String> = std::fs::read_dir(&to.0)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(landed, ["a.bin"]);
    }

    #[test]
    fn offers_are_found_by_token() {
        let offers = Offers::default();
        let token = offers.offer(vec![PathBuf::from("/a")]).unwrap();
        assert_eq!(token.len(), 32);
        assert_eq!(offers.paths(&token), Some(vec![PathBuf::from("/a")]));
        assert_eq!(offers.paths("guess"), None);
        assert_ne!(offers.offer(vec![]).unwrap(), token);
    }
}
