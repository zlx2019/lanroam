//! Clipboard: it follows the pointer.
//!
//! One actor, told by the input actor where the pointer goes:
//! - **Entering**: when this device takes control of another, it offers
//!   its clipboard there ([`Control::ClipOffer`], a summary)
//! - **Leaving**: when a controller lets go of this device, whatever was
//!   copied here meanwhile is offered back to it. Only then: what this
//!   device held before the visit is not what the user carries
//! - **Taking**: an offer of content the clipboard here does not hold is
//!   fetched over a content stream and written, as a copy here would be;
//!   unless the user copied something here since the offer came, and only
//!   the latest offer (a newer one supersedes a fetch under way)
//! - **Passing on**: content taken from one member while this device
//!   controls another goes on to that one (the pointer went a → b → c: what
//!   was copied on b reaches c through a)
//! - **Files**: copied files are offered as a summary
//!   ([`Control::ClipFiles`]) and fetched ahead of a paste with the file
//!   transfer of drags, unless they are larger than this device fetches
//!   ahead. They land in a folder of their own (see [`super::drag`]), which
//!   the clipboard here then holds; it goes a while after the clipboard
//!   moves on (a paste may still be copying from it)
//!
//! Nothing leaves or enters a device against its settings, and nothing
//! goes to a member whose settings refuse it (its profile says, see
//! [`ClipboardShare`]), nor to one it was not offered to: a member cannot
//! fetch by guessing a hash. The clipboard is read only when its stamp moved,
//! always on a blocking thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lan_kit::frame::FrameError;
use lanroam_clipboard::{Clipboard, ClipboardError, Content, Kind, MAX_BYTES, MAX_PIXEL_BYTES};
use thiserror::Error;
use tokio::sync::{mpsc, watch};

use super::EngineEvent;
use super::drag::{self, KEEP_AFTER_DROP, failed};
use super::files::{self, FilesError, Offers};
use super::input::Links;
use crate::group::ClipboardShare;
use crate::protocol::{ClipReply, Control, FRAMING, StreamRequest};

/// Longest a fetch may take, content included
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Priority of content streams, below the control stream's (0): input goes
/// out first
const CONTENT_PRIORITY: i32 = -1;

/// Most bytes of files copied elsewhere fetched ahead of a paste: larger
/// ones may never be pasted, and are better dragged
const PREFETCH_LIMIT: u64 = 32 << 20;

/// Files fetched ahead taking longer than this are said to be ready: the
/// user may be waiting to paste them
const READY_HINT_AFTER: Duration = Duration::from_secs(1);

/// What became of files copied on another member
/// ([`EngineEvent::CopiedFiles`])
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopiedFiles {
    /// Fetched: the clipboard here holds them, ready to paste
    Ready {
        /// The first file or folder copied
        name: String,
        /// How many were copied
        count: usize,
    },
    /// Larger than this device fetches ahead: left where they are
    TooLarge {
        /// The first file or folder copied
        name: String,
        /// How many were copied
        count: usize,
        /// Bytes in all
        bytes: u64,
        /// Most bytes fetched ahead
        limit: u64,
    },
    /// They did not come
    Failed {
        /// The first file or folder copied
        name: String,
        /// How many were copied
        count: usize,
        /// Why (a [`super::drag_failed`] code)
        reason: &'static str,
    },
}

/// Files a member offers, copied there ([`Control::ClipFiles`])
pub(super) struct FilesOffer {
    /// The member
    pub(super) from: String,
    /// The copy's hash
    pub(super) hash: String,
    /// The first file or folder copied
    pub(super) name: String,
    /// How many were copied
    pub(super) count: usize,
    /// Bytes in all
    pub(super) bytes: u64,
    /// What pulls them
    pub(super) token: String,
}

/// The clipboard actor's inbox
pub(super) enum ClipMsg {
    /// This device took control of a member
    Entered(String),
    /// This device stopped controlling a member
    Left(String),
    /// A member took control of this device
    ControlledBy(String),
    /// The member controlling this device let go, or its link dropped
    Freed(String),
    /// Local input took this device back from its controller
    TookBack,
    /// A member offered files copied there
    Files(FilesOffer),
    /// Files being fetched ahead did not come
    FilesFailed {
        /// Which fetch
        number: u64,
        /// Why (a [`super::drag_failed`] code)
        reason: &'static str,
    },
    /// Files being fetched ahead turned out larger than [`PREFETCH_LIMIT`]:
    /// they grew since they were described
    FilesTooLarge {
        /// Which fetch
        number: u64,
        /// Bytes found
        bytes: u64,
    },
    /// A member offered its clipboard
    Offer {
        /// The member
        from: String,
        /// Text or image
        kind: Kind,
        /// Bytes before encoding
        size: u64,
        /// The content's hash
        hash: String,
    },
    /// A member opened a content stream asking for the content with this
    /// hash
    Stream {
        /// The member
        from: String,
        /// Our half
        send: quinn::SendStream,
        /// What it asks for
        hash: String,
    },
    /// A fetch ended
    Fetched {
        /// The member fetched from
        from: String,
        /// Which fetch
        number: u64,
        /// The content, if it came
        content: Option<Content>,
    },
    /// What every member shares, this one included, by fingerprint
    Shares(HashMap<String, ClipboardShare>),
}

/// Why a transfer failed
#[derive(Debug, Error)]
enum TransferError {
    /// The connection failed
    #[error(transparent)]
    Connection(#[from] quinn::ConnectionError),
    /// A frame could not be read or written
    #[error(transparent)]
    Frame(#[from] FrameError),
    /// The content could not be read
    #[error(transparent)]
    Read(#[from] quinn::ReadToEndError),
    /// The content could not be written
    #[error(transparent)]
    Write(#[from] quinn::WriteError),
    /// The stream was closed already
    #[error(transparent)]
    Closed(#[from] quinn::ClosedStream),
    /// The content could not be encoded or decoded
    #[error(transparent)]
    Clipboard(#[from] ClipboardError),
    /// The other side no longer holds it
    #[error("the content is not held any more")]
    Gone,
    /// What came is not what was offered
    #[error("the content does not match the offer")]
    Mismatch,
    /// More than a hand-over may carry
    #[error("{0} bytes is too large")]
    TooLarge(u64),
    /// Took too long
    #[error("timed out")]
    Timeout,
    /// A blocking task died
    #[error("interrupted")]
    Interrupted,
}

/// What the clipboard here holds, as last read or written
#[derive(Default)]
struct Held {
    /// Read or written at all
    known: bool,
    /// Its stamp then
    stamp: Option<i64>,
    /// Its hash and content, if it may leave this device
    content: Option<(String, Arc<Content>)>,
}

/// A member controlling this device
struct Visit {
    /// The member
    controller: String,
    /// The clipboard's stamp when the visit began, or after this device
    /// last wrote to it: anything else is a copy made here
    stamp: Option<i64>,
}

/// The fetch under way
struct Pending {
    /// Which fetch
    number: u64,
    /// The offered content's hash
    hash: String,
    /// The clipboard's stamp when the offer came
    stamp: Option<i64>,
    /// Files being fetched, if that is what was offered
    files: Option<Fetching>,
}

/// Files copied elsewhere, being fetched ahead of a paste
struct Fetching {
    /// The first file or folder copied
    name: String,
    /// How many were copied
    count: usize,
    /// Where they land
    dir: PathBuf,
    /// The transfer
    task: tokio::task::AbortHandle,
    /// Since when
    since: Instant,
}

/// Files copied here, as offered to others
#[derive(Clone)]
struct Summary {
    /// The first file or folder copied
    name: String,
    /// How many were copied
    count: usize,
    /// Bytes in all
    bytes: u64,
}

/// The actor
pub(super) struct Clip {
    /// This device's fingerprint
    local: String,
    /// The clipboard
    clipboard: Arc<dyn Clipboard>,
    /// Member links
    links: watch::Receiver<Links>,
    /// The actor's own inbox, for its fetches to report into
    inbox: mpsc::UnboundedSender<ClipMsg>,
    /// Where the user hears of files copied elsewhere
    events: mpsc::UnboundedSender<EngineEvent>,
    /// Files this device offers, by token
    offers: Offers,
    /// What each member shares
    shares: HashMap<String, ClipboardShare>,
    /// The files copied here last described, by hash
    described: Option<(String, Summary)>,
    /// The folder of the files fetched here that the clipboard holds
    staged: Option<PathBuf>,
    /// The hash of the latest files too large to fetch ahead, told once
    skipped: Option<String>,
    /// What the clipboard here holds
    held: Held,
    /// The stamp right after this device last wrote to the clipboard
    written: Option<i64>,
    /// The hash last offered to each member, the only content it may fetch
    offered: HashMap<String, String>,
    /// The member this device controls
    target: Option<String>,
    /// The member controlling this device
    visit: Option<Visit>,
    /// The fetch under way
    pending: Option<Pending>,
    /// Number of the latest fetch
    fetches: u64,
}

impl Clip {
    /// An actor for the device `local`
    pub(super) fn new(
        local: &str,
        clipboard: Arc<dyn Clipboard>,
        links: watch::Receiver<Links>,
        inbox: mpsc::UnboundedSender<ClipMsg>,
        events: mpsc::UnboundedSender<EngineEvent>,
        offers: Offers,
    ) -> Self {
        Self {
            local: local.to_string(),
            clipboard,
            links,
            inbox,
            events,
            offers,
            shares: HashMap::new(),
            described: None,
            staged: None,
            skipped: None,
            held: Held::default(),
            written: None,
            offered: HashMap::new(),
            target: None,
            visit: None,
            pending: None,
            fetches: 0,
        }
    }

    /// Run until the engine goes away
    pub(super) async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<ClipMsg>) {
        while let Some(msg) = inbox.recv().await {
            self.handle(msg).await;
        }
    }

    /// Dispatch one message
    async fn handle(&mut self, msg: ClipMsg) {
        match msg {
            ClipMsg::Entered(device) => {
                self.target = Some(device.clone());
                self.offer(&device).await;
            }
            ClipMsg::Left(device) => {
                self.target.take_if(|target| *target == device);
            }
            ClipMsg::ControlledBy(controller) => {
                let stamp = self.stamp().await;
                self.visit = Some(Visit { controller, stamp });
            }
            ClipMsg::Freed(controller) => {
                let Some(visit) = self.visit.take_if(|v| v.controller == controller) else {
                    return;
                };
                let now = self.stamp().await;
                if now.is_some() && now != visit.stamp {
                    self.offer(&controller).await;
                }
            }
            ClipMsg::TookBack => self.visit = None,
            ClipMsg::Files(offer) => self.on_files(offer).await,
            ClipMsg::FilesFailed { number, reason } => self.on_files_failed(number, reason),
            ClipMsg::FilesTooLarge { number, bytes } => self.on_files_too_large(number, bytes),
            ClipMsg::Offer {
                from,
                kind,
                size,
                hash,
            } => self.on_offer(from, kind, size, hash).await,
            ClipMsg::Stream { from, send, hash } => self.on_stream(&from, send, hash),
            ClipMsg::Fetched {
                from,
                number,
                content,
            } => self.on_fetched(&from, number, content).await,
            ClipMsg::Shares(shares) => self.shares = shares,
        }
    }

    /// Offer the clipboard to `to`, if both share what it holds
    async fn offer(&mut self, to: &str) {
        let (own, theirs) = (self.share(&self.local), self.share(to));
        if !own.on || !theirs.on {
            return;
        }
        let Some((hash, content)) = self.current().await else {
            return;
        };
        let kind = content.kind();
        if !own.allows(kind) || !theirs.allows(kind) {
            return;
        }
        if let Content::Files(paths) = &*content {
            self.offer_files(to, hash, paths.clone()).await;
            return;
        }
        let size = content.size() as u64;
        self.offered.insert(to.to_string(), hash.clone());
        self.send(to, Control::ClipOffer { kind, size, hash });
    }

    /// Fetch offered content unless the clipboard here holds it already
    async fn on_offer(&mut self, from: String, kind: Kind, size: u64, hash: String) {
        if !self.share(&self.local).allows(kind) {
            return;
        }
        let limit = match kind {
            Kind::Text => MAX_BYTES,
            Kind::Image => MAX_PIXEL_BYTES,
            // Files come as files
            Kind::Files => return,
        };
        if size > limit as u64 {
            tracing::debug!(size, "skipping an offer too large to take");
            return;
        }
        let stamp = self.stamp().await;
        if self.current().await.is_some_and(|(held, _)| held == hash) {
            // Superseded: whatever is under way is older
            self.drop_pending();
            return;
        }
        let Some(conn) = self.links.borrow().get(&from).map(|l| l.conn.clone()) else {
            return;
        };
        self.drop_pending();
        self.fetches += 1;
        let number = self.fetches;
        self.pending = Some(Pending {
            number,
            hash: hash.clone(),
            stamp,
            files: None,
        });
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            let fetched = tokio::time::timeout(FETCH_TIMEOUT, fetch(&conn, hash, kind))
                .await
                .unwrap_or(Err(TransferError::Timeout));
            let content = fetched
                .inspect_err(|e| tracing::debug!("cannot fetch the clipboard: {e}"))
                .ok();
            let _ = inbox.send(ClipMsg::Fetched {
                from,
                number,
                content,
            });
        });
    }

    /// Write fetched content, if it is still wanted, and pass it on
    async fn on_fetched(&mut self, from: &str, number: u64, content: Option<Content>) {
        let Some(mut pending) = self.pending.take_if(|p| p.number == number) else {
            return;
        };
        let fetching = pending.files.take();
        let Some(content) = content else {
            return;
        };
        let now = self.stamp().await;
        if copied_meanwhile(now, pending.stamp, self.written) {
            tracing::debug!("something was copied here meanwhile, keeping it");
            if let Some(fetching) = fetching {
                discard_now(fetching.dir);
            }
            return;
        }
        let content = Arc::new(content);
        let clipboard = Arc::clone(&self.clipboard);
        let written = Arc::clone(&content);
        let wrote = tokio::task::spawn_blocking(move || clipboard.write(&written)).await;
        let stamp = match wrote {
            Ok(Ok(stamp)) => stamp,
            Ok(Err(e)) => {
                tracing::warn!("cannot write the clipboard: {e}");
                if let Some(fetching) = fetching {
                    discard_now(fetching.dir);
                }
                return;
            }
            Err(_) => return,
        };
        tracing::info!(
            kind = ?content.kind(),
            bytes = content.size(),
            from = %from,
            "took the clipboard"
        );
        self.written = stamp;
        self.held = Held {
            known: true,
            stamp,
            content: Some((pending.hash, content)),
        };
        if let Some(fetching) = fetching {
            self.retire_staged();
            self.staged = Some(fetching.dir);
            if fetching.since.elapsed() >= READY_HINT_AFTER {
                self.emit(CopiedFiles::Ready {
                    name: fetching.name,
                    count: fetching.count,
                });
            }
        }
        if let Some(visit) = &mut self.visit {
            visit.stamp = stamp;
        }
        if let Some(target) = self.target.clone()
            && target != from
        {
            self.offer(&target).await;
        }
    }

    /// Serve a member's content stream asking for `hash` with what the
    /// clipboard held when last read, if it was offered to that member and
    /// both sides share it
    fn on_stream(&self, from: &str, send: quinn::SendStream, hash: String) {
        let (own, theirs) = (self.share(&self.local), self.share(from));
        let offered = self.offered.get(from);
        let held = self.held.content.clone().filter(|(hash, content)| {
            offered == Some(hash) && own.allows(content.kind()) && theirs.allows(content.kind())
        });
        tokio::spawn(async move {
            if let Err(e) = serve(send, hash, held).await {
                tracing::debug!("cannot serve the clipboard: {e}");
            }
        });
    }

    /// What the clipboard holds, read again only if its stamp moved (or
    /// there is none)
    async fn current(&mut self) -> Option<(String, Arc<Content>)> {
        let stamp = self.stamp().await;
        if self.held.known && stamp.is_some() && stamp == self.held.stamp {
            return self.held.content.clone();
        }
        let clipboard = Arc::clone(&self.clipboard);
        let read = tokio::task::spawn_blocking(move || {
            let content = clipboard.read()?;
            Ok::<_, ClipboardError>(content.map(|content| (content.hash(), Arc::new(content))))
        })
        .await;
        match read {
            Ok(Ok(content)) => {
                let staged = content.as_ref().is_some_and(|(_, content)| {
                    self.staged
                        .as_deref()
                        .is_some_and(|dir| holds_from(content, dir))
                });
                if !staged {
                    self.retire_staged();
                }
                self.held = Held {
                    known: true,
                    stamp,
                    content,
                };
                self.held.content.clone()
            }
            Ok(Err(e)) => {
                tracing::debug!("cannot read the clipboard: {e}");
                None
            }
            Err(_) => None,
        }
    }

    /// Offer the files copied here (`paths`, with `hash`) to `to`: what
    /// they are, described once per copy, and a new token to pull them with
    async fn offer_files(&mut self, to: &str, hash: String, paths: Vec<PathBuf>) {
        let summary = match &self.described {
            Some((described, summary)) if *described == hash => summary.clone(),
            _ => {
                let listed = paths.clone();
                let Ok(items) = tokio::task::spawn_blocking(move || drag::describe(&listed)).await
                else {
                    return;
                };
                let Some(first) = items.first() else {
                    tracing::debug!("none of the files copied here can be read");
                    return;
                };
                let summary = Summary {
                    name: first.name.clone(),
                    count: items.len(),
                    bytes: items.iter().map(|item| item.size).sum(),
                };
                self.described = Some((hash.clone(), summary.clone()));
                summary
            }
        };
        let token = match self.offers.offer(paths) {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!("cannot offer the files copied here: {e}");
                return;
            }
        };
        self.send(
            to,
            Control::ClipFiles {
                hash,
                name: summary.name,
                count: summary.count,
                bytes: summary.bytes,
                token,
            },
        );
    }

    /// Fetch files copied elsewhere ahead of a paste, unless the clipboard
    /// here holds them already, they are on their way, or they are larger
    /// than [`PREFETCH_LIMIT`]
    async fn on_files(&mut self, offer: FilesOffer) {
        if !self.share(&self.local).allows(Kind::Files) {
            return;
        }
        // Found too large already, and told: the offer comes again at
        // every visit
        if self.skipped.as_ref() == Some(&offer.hash) {
            return;
        }
        if offer.bytes > PREFETCH_LIMIT {
            self.too_large(offer.hash, offer.name, offer.count, offer.bytes);
            return;
        }
        let stamp = self.stamp().await;
        if self
            .current()
            .await
            .is_some_and(|(held, _)| held == offer.hash)
        {
            self.drop_pending();
            return;
        }
        if self.pending.as_ref().is_some_and(|p| p.hash == offer.hash) {
            return;
        }
        let Some(conn) = self.links.borrow().get(&offer.from).map(|l| l.conn.clone()) else {
            return;
        };
        self.drop_pending();
        self.fetches += 1;
        let number = self.fetches;
        let made =
            tokio::task::spawn_blocking(move || drag::folder(&format!("clip-{number}"))).await;
        let dir = match made {
            Ok(Ok(dir)) => dir,
            Ok(Err(e)) => {
                tracing::warn!("cannot make a folder for copied files: {e}");
                return;
            }
            Err(_) => return,
        };
        tracing::info!(
            files = offer.count,
            bytes = offer.bytes,
            "fetching copied files ahead"
        );
        let fetch = fetch_files(
            self.inbox.clone(),
            number,
            offer.from,
            conn,
            offer.token,
            dir.clone(),
            offer.bytes,
        );
        let task = tokio::spawn(fetch).abort_handle();
        self.pending = Some(Pending {
            number,
            hash: offer.hash,
            stamp,
            files: Some(Fetching {
                name: offer.name,
                count: offer.count,
                dir,
                task,
                since: Instant::now(),
            }),
        });
    }

    /// Leave files copied elsewhere (the copy `hash`) where they are, too
    /// large to fetch ahead, and tell the user
    fn too_large(&mut self, hash: String, name: String, count: usize, bytes: u64) {
        let limit = PREFETCH_LIMIT;
        tracing::info!(bytes, limit, "copied files too large to fetch ahead");
        self.skipped = Some(hash);
        self.emit(CopiedFiles::TooLarge {
            name,
            count,
            bytes,
            limit,
        });
    }

    /// Files being fetched ahead proved too large: what of them came goes,
    /// and they are not fetched again
    fn on_files_too_large(&mut self, number: u64, bytes: u64) {
        let Some(pending) = self.pending.take_if(|p| p.number == number) else {
            return;
        };
        let Some(fetching) = pending.files else {
            return;
        };
        discard_now(fetching.dir);
        self.too_large(pending.hash, fetching.name, fetching.count, bytes);
    }

    /// Files being fetched ahead did not come: what of them came goes
    fn on_files_failed(&mut self, number: u64, reason: &'static str) {
        let Some(pending) = self.pending.take_if(|p| p.number == number) else {
            return;
        };
        let Some(fetching) = pending.files else {
            return;
        };
        discard_now(fetching.dir);
        self.emit(CopiedFiles::Failed {
            name: fetching.name,
            count: fetching.count,
            reason,
        });
    }

    /// Give up the fetch under way, and what of its files came
    fn drop_pending(&mut self) {
        if let Some(fetching) = self.pending.take().and_then(|pending| pending.files) {
            fetching.task.abort();
            discard_now(fetching.dir);
        }
    }

    /// The clipboard moved on from the files fetched here: their folder
    /// goes a while later (a paste may still be copying from it)
    fn retire_staged(&mut self) {
        if let Some(dir) = self.staged.take() {
            tokio::spawn(async move {
                tokio::time::sleep(KEEP_AFTER_DROP).await;
                let _ = tokio::task::spawn_blocking(move || drag::discard(&dir)).await;
            });
        }
    }

    /// Tell the user about files copied elsewhere
    fn emit(&self, copied: CopiedFiles) {
        let _ = self.events.send(EngineEvent::CopiedFiles(copied));
    }

    /// The clipboard's stamp
    async fn stamp(&self) -> Option<i64> {
        let clipboard = Arc::clone(&self.clipboard);
        tokio::task::spawn_blocking(move || clipboard.stamp())
            .await
            .ok()
            .flatten()
    }

    /// What `fp` shares (everything, until its profile says otherwise)
    fn share(&self, fp: &str) -> ClipboardShare {
        self.shares.get(fp).copied().unwrap_or_default()
    }

    /// Send a control message to a member, if linked
    fn send(&self, fp: &str, msg: Control) {
        if let Some(link) = self.links.borrow().get(fp) {
            let _ = link.out.send(msg);
        }
    }
}

/// Whether `content` is files fetched into `dir`, told by the folder's
/// name: the clipboard may spell the path to it otherwise (/private/var
/// for /var on macOS, a `\\?\` prefix on Windows)
fn holds_from(content: &Content, dir: &Path) -> bool {
    let Content::Files(paths) = content else {
        return false;
    };
    paths
        .iter()
        .any(|path| path.parent().and_then(Path::file_name) == dir.file_name())
}

/// Remove the folder of files fetched here, off the actor
fn discard_now(dir: PathBuf) {
    tokio::task::spawn_blocking(move || drag::discard(&dir));
}

/// Fetch the files of `token` from `from` into `dir` (`bytes` in all, as
/// described; no more than [`PREFETCH_LIMIT`] are taken), and tell the
/// actor how it went: the files at the top of `dir` on the clipboard, or
/// why not
async fn fetch_files(
    inbox: mpsc::UnboundedSender<ClipMsg>,
    number: u64,
    from: String,
    conn: quinn::Connection,
    token: String,
    dir: PathBuf,
    bytes: u64,
) {
    let failed = |reason| ClipMsg::FilesFailed { number, reason };
    let at = dir.clone();
    let free = tokio::task::spawn_blocking(move || files::available_space(&at))
        .await
        .ok()
        .flatten();
    if free.is_some_and(|free| free < bytes) {
        tracing::info!(bytes, ?free, "no space for the copied files");
        let _ = inbox.send(failed(failed::NO_SPACE));
        return;
    }
    let limit = Some(PREFETCH_LIMIT);
    if let Err(e) = files::pull(&conn, token, &dir, limit, |_| {}, |_, _| {}).await {
        tracing::info!("cannot fetch the copied files: {e}");
        let _ = inbox.send(match e {
            FilesError::TooLarge(bytes) => ClipMsg::FilesTooLarge { number, bytes },
            _ => failed(failed::TRANSFER),
        });
        return;
    }
    let listed = tokio::task::spawn_blocking(move || top_level(&dir)).await;
    let _ = inbox.send(match listed {
        Ok(Ok(paths)) if !paths.is_empty() => ClipMsg::Fetched {
            from,
            number,
            content: Some(Content::Files(paths)),
        },
        _ => failed(failed::TRANSFER),
    });
}

/// What is at the top of `dir`, by name
fn top_level(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}

/// Whether the user copied something since an offer came: the stamp moved
/// from where it was then (`offered`), and not by this device's latest
/// write (`written`, which may have landed since)
fn copied_meanwhile(now: Option<i64>, offered: Option<i64>, written: Option<i64>) -> bool {
    now != offered && now != written
}

/// Fetch the content with `hash` over a new content stream, and check it is
/// what was offered
async fn fetch(
    conn: &quinn::Connection,
    hash: String,
    kind: Kind,
) -> Result<Content, TransferError> {
    let (mut send, mut recv) = conn.open_bi().await?;
    let request = StreamRequest::Clipboard { hash: hash.clone() };
    FRAMING.write(&mut send, &request).await?;
    send.finish()?;
    let len = match FRAMING.read(&mut recv).await? {
        ClipReply::Content { len } => len,
        ClipReply::Gone => return Err(TransferError::Gone),
    };
    let limit = usize::try_from(len)
        .ok()
        .filter(|len| *len <= MAX_BYTES)
        .ok_or(TransferError::TooLarge(len))?;
    let bytes = recv.read_to_end(limit).await?;
    tokio::task::spawn_blocking(move || {
        let content = Content::decode(kind, bytes)?;
        if content.hash() != hash {
            return Err(TransferError::Mismatch);
        }
        Ok(content)
    })
    .await
    .map_err(|_| TransferError::Interrupted)?
}

/// Answer a content stream asking for `hash`: the content, if it is the
/// one `held`
async fn serve(
    mut send: quinn::SendStream,
    hash: String,
    held: Option<(String, Arc<Content>)>,
) -> Result<(), TransferError> {
    let Some(content) = held.filter(|(held, _)| *held == hash).map(|(_, c)| c) else {
        FRAMING.write(&mut send, &ClipReply::Gone).await?;
        send.finish()?;
        return Ok(());
    };
    let bytes = tokio::task::spawn_blocking(move || content.encode())
        .await
        .map_err(|_| TransferError::Interrupted)??;
    send.set_priority(CONTENT_PRIORITY)?;
    let len = bytes.len() as u64;
    FRAMING
        .write(&mut send, &ClipReply::Content { len })
        .await?;
    send.write_all(&bytes).await?;
    send.finish()?;
    // Stay until the content is through (or the asker gave up)
    let _ = send.stopped().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_fetched_here_are_told_by_their_folder() {
        let dir = Path::new("/var/folders/x/T/lanroam-drops/17-clip-3");
        let fetched = |path: &str| Content::Files(vec![PathBuf::from(path)]);
        let spelt = fetched("/private/var/folders/x/T/lanroam-drops/17-clip-3/a.txt");
        assert!(holds_from(&spelt, dir));
        assert!(!holds_from(&fetched("/Users/zero/a.txt"), dir));
        assert!(!holds_from(&Content::Text("a".into()), dir));
    }

    #[test]
    fn a_copy_here_wins_over_an_offer() {
        // Nothing moved, or only by the latest write: take the content
        assert!(!copied_meanwhile(Some(5), Some(5), None));
        assert!(!copied_meanwhile(Some(6), Some(5), Some(6)));
        // The user copied after the offer came
        assert!(copied_meanwhile(Some(6), Some(5), Some(4)));
        // ... or after this device's latest write
        assert!(copied_meanwhile(Some(7), Some(5), Some(6)));
    }
}
