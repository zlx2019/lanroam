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
//!
//! Nothing leaves or enters a device against its settings, and nothing
//! goes to a member whose settings refuse it (its profile says, see
//! [`ClipboardShare`]), nor to one it was not offered to: a member cannot
//! fetch by guessing a hash. The clipboard is read only when its stamp moved,
//! always on a blocking thread.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use lan_kit::frame::FrameError;
use lanroam_clipboard::{Clipboard, ClipboardError, Content, Kind, MAX_BYTES, MAX_PIXEL_BYTES};
use thiserror::Error;
use tokio::sync::{mpsc, watch};

use super::input::Links;
use crate::group::ClipboardShare;
use crate::protocol::{ClipReply, Control, FRAMING, StreamRequest};

/// Longest a fetch may take, content included
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the opener of a content stream has to say what it wants
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Priority of content streams, below the control stream's (0): input goes
/// out first
const CONTENT_PRIORITY: i32 = -1;

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
    /// A member opened a content stream
    Stream {
        /// The member
        from: String,
        /// Our half
        send: quinn::SendStream,
        /// Theirs
        recv: quinn::RecvStream,
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
    /// What each member shares
    shares: HashMap<String, ClipboardShare>,
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
    ) -> Self {
        Self {
            local: local.to_string(),
            clipboard,
            links,
            inbox,
            shares: HashMap::new(),
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
            ClipMsg::Offer {
                from,
                kind,
                size,
                hash,
            } => self.on_offer(from, kind, size, hash).await,
            ClipMsg::Stream { from, send, recv } => self.on_stream(&from, send, recv),
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
        };
        if size > limit as u64 {
            tracing::debug!(size, "skipping an offer too large to take");
            return;
        }
        let stamp = self.stamp().await;
        if self.current().await.is_some_and(|(held, _)| held == hash) {
            // Superseded: whatever is under way is older
            self.pending = None;
            return;
        }
        let Some(conn) = self.links.borrow().get(&from).map(|l| l.conn.clone()) else {
            return;
        };
        self.fetches += 1;
        let number = self.fetches;
        self.pending = Some(Pending {
            number,
            hash: hash.clone(),
            stamp,
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
        let Some(pending) = self.pending.take_if(|p| p.number == number) else {
            return;
        };
        let Some(content) = content else {
            return;
        };
        let now = self.stamp().await;
        if copied_meanwhile(now, pending.stamp, self.written) {
            tracing::debug!("something was copied here meanwhile, keeping it");
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
        if let Some(visit) = &mut self.visit {
            visit.stamp = stamp;
        }
        if let Some(target) = self.target.clone()
            && target != from
        {
            self.offer(&target).await;
        }
    }

    /// Serve a member's content stream with what the clipboard held when
    /// last read, if it was offered to that member and both sides share it
    fn on_stream(&self, from: &str, send: quinn::SendStream, recv: quinn::RecvStream) {
        let (own, theirs) = (self.share(&self.local), self.share(from));
        let offered = self.offered.get(from);
        let held = self.held.content.clone().filter(|(hash, content)| {
            offered == Some(hash) && own.allows(content.kind()) && theirs.allows(content.kind())
        });
        tokio::spawn(async move {
            if let Err(e) = serve(send, recv, held).await {
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

/// Answer a content stream: the content asked for if it is `held`
async fn serve(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    held: Option<(String, Arc<Content>)>,
) -> Result<(), TransferError> {
    let request = tokio::time::timeout(REQUEST_TIMEOUT, FRAMING.read(&mut recv))
        .await
        .map_err(|_| TransferError::Timeout)??;
    let StreamRequest::Clipboard { hash } = request;
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
