//! The engine: one device running in its desk group.
//!
//! It owns the node (identity, QUIC endpoint, discovery) and one actor task,
//! the mesh, which holds the group document and a link to
//! every online member. Everything that changes group state goes through the
//! mesh's inbox, so there are no locks around the document; the rest of the
//! engine reads the latest copy from a watch channel.
//!
//! Incoming connections are sorted by the purpose they declare: member links
//! go to the mesh, joins run a sponsor task that shows a PIN, diagnostics
//! answer pings. Keyboard and mouse, in both directions, are the input
//! actor's.

mod input;
mod mesh;

pub use input::{ControlEvent, InputBackend, PlatformInput};

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lan_kit::{Peer, PeerEvent, PeerInfo};
use lanroam_input::{Edge, Point};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::diag;
use crate::group::join::{self, Challenge, JoinError, JoinGate, Verdict};
use crate::group::{GroupDoc, GroupStore, Standing};
use crate::layout::LayoutError;
use crate::node::{Node, NodeConfig, NodeError};
use crate::protocol::{Purpose, join_denied, reason_code};
use crate::transport::{Incoming, Link, Transport, TransportError};
use input::{Input, InputMsg};
use mesh::{Mesh, Msg, Wiring};

/// How often this device's displays are read, to notice a display being
/// plugged in, removed or rearranged
const SCREEN_POLL: Duration = Duration::from_secs(2);

/// Engine errors
#[derive(Debug, Error)]
pub enum EngineError {
    /// The node could not start
    #[error(transparent)]
    Node(#[from] NodeError),
    /// Reading or writing the group document failed
    #[error("group document: {0}")]
    Store(#[from] std::io::Error),
    /// A connection failed
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// Joining failed
    #[error(transparent)]
    Join(#[from] JoinError),
    /// A layout change was refused
    #[error(transparent)]
    Layout(#[from] LayoutError),
    /// This device is in a group already (leave it first)
    #[error("this device is in a desk group already; leave it first")]
    Grouped,
    /// This device is in no group
    #[error("this device is in no desk group")]
    NoGroup,
    /// The device is not a current member
    #[error("{0} is not a member of the desk group")]
    NotAMember(String),
    /// The engine has stopped
    #[error("the engine has stopped")]
    Stopped,
}

/// What the engine tells its user
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A member is linked
    Online(PeerInfo),
    /// A member's link is gone
    Offline {
        /// Its fingerprint
        fingerprint: String,
        /// Its name
        name: String,
    },
    /// The group document changed; `None`: this device is in no group now
    Group(Option<Arc<GroupDoc>>),
    /// Another member removed this device from the group
    Kicked,
    /// A device asks to join through this one: show the PIN to the user
    JoinPin {
        /// Who asks
        joiner: PeerInfo,
        /// The PIN to show
        pin: String,
    },
    /// Who controls what changed
    Control(ControlEvent),
    /// That join is over (hide the PIN)
    JoinEnded {
        /// Who asked
        joiner: PeerInfo,
        /// Whether it got in
        admitted: bool,
    },
}

/// Where to put a device on the layout canvas
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spot {
    /// Its origin at this canvas position
    At(Point),
    /// On `side` of the device `anchor`, shifted `offset` logical pixels
    /// along the shared edge
    Beside {
        /// Which side of the anchor
        side: Edge,
        /// Fingerprint of the anchor
        anchor: String,
        /// Shift along the edge (right or down)
        offset: i32,
    },
}

/// Group state at a glance
#[derive(Debug, Clone)]
pub struct Status {
    /// The group document; `None` outside a group
    pub doc: Option<Arc<GroupDoc>>,
    /// Members linked right now
    pub online: Vec<PeerInfo>,
}

/// A running engine; clones share it
#[derive(Clone)]
pub struct Engine {
    /// Shared state
    inner: Arc<Inner>,
}

/// State shared by the engine's clones
struct Inner {
    /// The node (`None` in tests, which run without discovery)
    node: Option<Arc<Node>>,
    /// QUIC endpoint
    transport: Arc<Transport>,
    /// This device's info, sent in handshakes
    info: PeerInfo,
    /// The mesh's inbox
    inbox: mpsc::UnboundedSender<Msg>,
    /// The input actor's inbox
    input: mpsc::UnboundedSender<InputMsg>,
    /// Latest group document
    doc: watch::Receiver<Option<Arc<GroupDoc>>>,
    /// Background tasks, stopped on shutdown
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Engine {
    /// Start a node and run it in its desk group (if any), sharing the
    /// keyboard and mouse of `input`, with the stream of events for the
    /// user
    pub async fn start(
        config: NodeConfig,
        input: Arc<dyn InputBackend>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<EngineEvent>), EngineError> {
        let store = GroupStore::new(&config.data_dir);
        let (node, peer_events) = Node::start(config).await?;
        let node = Arc::new(node);
        let transport = Arc::clone(node.transport());
        let info = node.info().clone();
        Self::launch(store, transport, info, Some(node), Some(peer_events), input)
    }

    /// Wire the mesh, the input actor, the accept loop, the display poller
    /// and the discovery feed around a bound transport
    fn launch(
        store: GroupStore,
        transport: Arc<Transport>,
        info: PeerInfo,
        node: Option<Arc<Node>>,
        peer_events: Option<mpsc::Receiver<PeerEvent>>,
        backend: Arc<dyn InputBackend>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<EngineEvent>), EngineError> {
        let doc = store.load()?;
        let (events_tx, events) = mpsc::unbounded_channel();
        let (inbox_tx, inbox) = mpsc::unbounded_channel();
        // Readable at once; the mesh drops a document that no longer lists
        // this device when it starts
        let initial = doc
            .as_ref()
            .filter(|doc| doc.is_member(&info.fingerprint))
            .map(|doc| Arc::new(doc.clone()));
        let (doc_tx, doc_rx) = watch::channel(initial);
        let advertise = {
            let node = node.clone();
            move |group: Option<&str>| {
                if let Some(node) = &node {
                    advertise_group(node, group);
                }
            }
        };
        let (input_tx, input_inbox) = mpsc::unbounded_channel();
        let (links_tx, links_rx) = watch::channel(Arc::default());
        let input = Input::new(
            &info.fingerprint,
            backend.as_ref(),
            links_rx,
            events_tx.clone(),
            input_tx.clone(),
        );
        let wiring = Wiring {
            published: doc_tx,
            events: events_tx.clone(),
            inbox: inbox_tx.clone(),
            advertise: Box::new(advertise),
            input: input_tx.clone(),
            links: links_tx,
        };
        let mesh = Mesh::new(info.clone(), Arc::clone(&transport), store, doc, wiring);
        let mut tasks = vec![
            tokio::spawn(mesh.run(inbox)),
            tokio::spawn(input.run(input_inbox)),
            tokio::spawn(poll_screens(backend, inbox_tx.clone())),
        ];
        tasks.push(tokio::spawn(accept_loop(
            Arc::clone(&transport),
            info.clone(),
            doc_rx.clone(),
            inbox_tx.clone(),
            events_tx,
        )));
        if let Some(mut peer_events) = peer_events {
            let inbox = inbox_tx.clone();
            tasks.push(tokio::spawn(async move {
                while let Some(event) = peer_events.recv().await {
                    if inbox.send(Msg::Peer(event)).is_err() {
                        break;
                    }
                }
            }));
        }
        let engine = Self {
            inner: Arc::new(Inner {
                node,
                transport,
                info,
                inbox: inbox_tx,
                input: input_tx,
                doc: doc_rx,
                tasks: Mutex::new(tasks),
            }),
        };
        Ok((engine, events))
    }

    /// This device's info
    pub fn info(&self) -> &PeerInfo {
        &self.inner.info
    }

    /// Port of the QUIC endpoint
    pub fn local_port(&self) -> u16 {
        self.inner.transport.local_port()
    }

    /// The group document; `None` outside a group
    pub fn group(&self) -> Option<Arc<GroupDoc>> {
        self.inner.doc.borrow().clone()
    }

    /// Nodes discovery sees on the LAN, group members or not
    pub fn nearby(&self) -> Vec<Peer> {
        self.inner
            .node
            .as_ref()
            .map(|node| node.discovery().peers())
            .unwrap_or_default()
    }

    /// Group state at a glance
    pub async fn status(&self) -> Result<Status, EngineError> {
        self.ask(|reply| Msg::Status { reply }).await
    }

    /// Ask `sponsor` to let this device into its group (founding one with
    /// it if it has none)
    ///
    /// Returns once the sponsor shows its PIN; answer with it through
    /// [`Joining::answer`]. Dropping the [`Joining`] cancels.
    pub async fn join(&self, sponsor: &Peer) -> Result<Joining, EngineError> {
        if self.group().is_some() {
            return Err(EngineError::Grouped);
        }
        let mut link = self
            .inner
            .transport
            .connect(sponsor, &self.inner.info, Purpose::Join)
            .await?;
        let challenge = join::recv_challenge(&mut link).await?;
        Ok(Joining {
            link,
            challenge,
            engine: self.clone(),
        })
    }

    /// Move a member on the layout canvas
    pub async fn place(&self, fingerprint: &str, spot: Spot) -> Result<(), EngineError> {
        let fingerprint = fingerprint.to_string();
        self.ask(|reply| Msg::Place {
            fingerprint,
            spot,
            reply,
        })
        .await?
    }

    /// Turn on or off swapping Command and Control on input into this
    /// device from a device of the other platform
    pub async fn set_swap(&self, on: bool) -> Result<(), EngineError> {
        self.ask(|reply| Msg::SetSwap { on, reply }).await?
    }

    /// Remove a member from the group
    pub async fn kick(&self, fingerprint: &str) -> Result<(), EngineError> {
        let fingerprint = fingerprint.to_string();
        self.ask(|reply| Msg::Kick { fingerprint, reply }).await?
    }

    /// Leave the group
    pub async fn leave(&self) -> Result<(), EngineError> {
        self.ask(|reply| Msg::Leave { reply }).await?
    }

    /// Stop the engine: give the keyboard and mouse back, close every
    /// link, say goodbye on the LAN
    pub async fn shutdown(&self) {
        let (reply, done) = oneshot::channel();
        if self.inner.input.send(InputMsg::Shutdown(reply)).is_ok() {
            let _ = done.await;
        }
        let (reply, done) = oneshot::channel();
        if self.inner.inbox.send(Msg::Shutdown { reply }).is_ok() {
            let _ = done.await;
        }
        let tasks = std::mem::take(&mut *self.lock_tasks());
        for task in tasks {
            task.abort();
        }
        match &self.inner.node {
            Some(node) => node.shutdown().await,
            None => {
                // Tests only: draining may take seconds and nobody waits
                // for the goodbye to arrive
                self.inner.transport.close();
                let idle = self.inner.transport.wait_idle();
                let _ = tokio::time::timeout(Duration::from_millis(200), idle).await;
            }
        }
    }

    /// The background tasks
    fn lock_tasks(&self) -> std::sync::MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.inner
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Send the mesh a request and wait for its reply
    async fn ask<T>(&self, msg: impl FnOnce(oneshot::Sender<T>) -> Msg) -> Result<T, EngineError> {
        let (reply, answer) = oneshot::channel();
        self.inner
            .inbox
            .send(msg(reply))
            .map_err(|_| EngineError::Stopped)?;
        answer.await.map_err(|_| EngineError::Stopped)
    }
}

/// A join in progress, waiting for the PIN the sponsor shows
pub struct Joining {
    /// The join link to the sponsor
    link: Link,
    /// The attempt to answer
    challenge: Challenge,
    /// The engine to adopt the group into
    engine: Engine,
}

impl Joining {
    /// The sponsor
    pub fn sponsor(&self) -> &PeerInfo {
        self.link.remote()
    }

    /// Attempts left for this PIN, the next one included
    pub fn attempts_left(&self) -> u32 {
        self.challenge.attempts_left
    }

    /// Answer with a PIN: `Some` with the group once in, `None` when the PIN
    /// was wrong and another attempt is left
    pub async fn answer(&mut self, pin: &str) -> Result<Option<Arc<GroupDoc>>, EngineError> {
        let own_fp = self.engine.inner.info.fingerprint.clone();
        match join::answer(&mut self.link, &own_fp, &self.challenge, pin).await? {
            Verdict::Retry(next) => {
                self.challenge = next;
                Ok(None)
            }
            Verdict::Accepted(doc) => {
                self.link.close();
                let shared = Arc::new(doc.clone());
                self.engine.ask(|reply| Msg::Adopt { doc, reply }).await??;
                Ok(Some(shared))
            }
        }
    }
}

/// Read this device's displays every [`SCREEN_POLL`] and report changes to
/// the mesh
async fn poll_screens(backend: Arc<dyn InputBackend>, inbox: mpsc::UnboundedSender<Msg>) {
    let mut last = None;
    let mut warned = false;
    loop {
        let backend = Arc::clone(&backend);
        let read = tokio::task::spawn_blocking(move || backend.screens())
            .await
            .unwrap_or_else(|e| Err(lanroam_input::InputError::Os(e.to_string())));
        match read {
            Ok(screens) if last.as_ref() != Some(&screens) => {
                let (displays, scale) = screens.clone();
                last = Some(screens);
                if inbox.send(Msg::Screens { displays, scale }).is_err() {
                    return;
                }
            }
            Ok(_) => {}
            Err(e) if !warned => {
                warned = true;
                tracing::warn!(
                    "cannot read the displays, the layout will not show this device: {e}"
                );
            }
            Err(_) => {}
        }
        tokio::time::sleep(SCREEN_POLL).await;
    }
}

/// Advertise the group ID in discovery (or stop advertising it)
fn advertise_group(node: &Node, group: Option<&str>) {
    let mut info = node.info().clone();
    match group {
        Some(id) => {
            info.props
                .insert(crate::protocol::PROP_GROUP.to_string(), id.to_string());
        }
        None => {
            info.props.remove(crate::protocol::PROP_GROUP);
        }
    }
    if let Err(e) = node.discovery().update_info(&info) {
        tracing::warn!("cannot advertise the group: {e}");
    }
}

/// Whether a verified peer may connect for `purpose`: anyone may ask to
/// join or measure the link, only current members may link up
fn admission(
    doc: Option<&GroupDoc>,
    peer: &PeerInfo,
    purpose: Purpose,
) -> Result<(), &'static str> {
    match purpose {
        Purpose::Join | Purpose::Diag => Ok(()),
        Purpose::Member => match doc.map(|doc| doc.standing(&peer.fingerprint)) {
            Some(Standing::Member) => Ok(()),
            Some(Standing::Removed) => Err(reason_code::REMOVED),
            _ => Err(reason_code::NOT_A_MEMBER),
        },
    }
}

/// Accept connections until the endpoint closes, each in its own task
async fn accept_loop(
    transport: Arc<Transport>,
    info: PeerInfo,
    doc: watch::Receiver<Option<Arc<GroupDoc>>>,
    inbox: mpsc::UnboundedSender<Msg>,
    events: mpsc::UnboundedSender<EngineEvent>,
) {
    let gate = Arc::new(Mutex::new(JoinGate::default()));
    while let Some(incoming) = transport.accept().await {
        tokio::spawn(serve_incoming(
            incoming,
            info.clone(),
            doc.clone(),
            inbox.clone(),
            events.clone(),
            Arc::clone(&gate),
        ));
    }
}

/// Run the handshake of one incoming connection and hand the link to what
/// its purpose calls for
async fn serve_incoming(
    incoming: Incoming,
    info: PeerInfo,
    doc: watch::Receiver<Option<Arc<GroupDoc>>>,
    inbox: mpsc::UnboundedSender<Msg>,
    events: mpsc::UnboundedSender<EngineEvent>,
    gate: Arc<Mutex<JoinGate>>,
) {
    let from = incoming.remote_address();
    let admit = |peer: &PeerInfo, purpose| admission(doc.borrow().as_deref(), peer, purpose);
    let link = match incoming.handshake(&info, admit).await {
        Ok(link) => link,
        // Discovery probing our identity
        Err(e) if e.is_probe() => return,
        Err(e) => {
            tracing::debug!(%from, "refused a connection: {e}");
            return;
        }
    };
    match link.purpose() {
        Purpose::Member => {
            let _ = inbox.send(Msg::LinkUp {
                link,
                dialed: false,
            });
        }
        Purpose::Join => sponsor(link, &info, &gate, &inbox, &events).await,
        Purpose::Diag => diag::respond(link).await,
    }
}

/// Sponsor one join: show a PIN, verify the joiner knows it, let it in
async fn sponsor(
    mut link: Link,
    own: &PeerInfo,
    gate: &Mutex<JoinGate>,
    inbox: &mpsc::UnboundedSender<Msg>,
    events: &mpsc::UnboundedSender<EngineEvent>,
) {
    let lock = || gate.lock().unwrap_or_else(PoisonError::into_inner);
    let joiner = link.remote().clone();
    let begun = lock().begin(Instant::now());
    if let Err(code) = begun {
        tracing::debug!(joiner = %joiner.name, "turned a join away: {code}");
        let _ = join::deny(&mut link, code).await;
        link.close_after_flush().await;
        return;
    }
    let result = sponsor_join(&mut link, own, &joiner, inbox, events).await;
    if let Err(e) = &result {
        tracing::info!(joiner = %joiner.name, "join failed: {e}");
    }
    lock().end(Instant::now(), &result);
    let _ = events.send(EngineEvent::JoinEnded {
        joiner,
        admitted: result.is_ok(),
    });
    // The last message (acceptance or denial) must reach the joiner
    link.close_after_flush().await;
}

/// The steps of a sponsored join
async fn sponsor_join(
    link: &mut Link,
    own: &PeerInfo,
    joiner: &PeerInfo,
    inbox: &mpsc::UnboundedSender<Msg>,
    events: &mpsc::UnboundedSender<EngineEvent>,
) -> Result<(), JoinError> {
    let pin = join::new_pin()?;
    let _ = events.send(EngineEvent::JoinPin {
        joiner: joiner.clone(),
        pin: pin.clone(),
    });
    let verified = join::verify(link, &own.fingerprint, &pin).await?;
    let (reply, admitted) = oneshot::channel();
    let _ = inbox.send(Msg::Admit {
        joiner: joiner.clone(),
        reply,
    });
    let Ok(doc) = admitted.await else {
        join::deny(link, join_denied::INTERNAL).await?;
        return Err(JoinError::Denied(join_denied::INTERNAL.into()));
    };
    join::accept(link, verified, &doc).await
}

#[cfg(test)]
mod tests;
