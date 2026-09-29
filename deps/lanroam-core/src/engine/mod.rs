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
//! actor's; the clipboard, which follows the pointer, the clipboard actor's.

mod clipboard;
mod drag;
mod files;
mod input;
mod mesh;

pub use clipboard::CopiedFiles;
pub use drag::{
    DragBackend, DragEvent, DragSink, Dragging, NoDrag, PlatformDrag, Receiving,
    failed as drag_failed,
};
pub use input::{ControlEvent, InputBackend, InputStatus, PlatformInput};
pub use lanroam_input::switch::Request;

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lan_kit::identity::{self, IdentityError};
use lan_kit::{Peer, PeerEvent, PeerInfo};
use lanroam_clipboard::Clipboard;
use lanroam_input::config::{Chord, EdgeSettings};
use lanroam_input::{Edge, Point};
use thiserror::Error;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::diag;
use crate::group::join::{self, Challenge, JoinError, JoinGate, Verdict};
use crate::group::{ClipboardShare, FileShare, GroupDoc, GroupStore, Standing};
use crate::layout::LayoutError;
use crate::node::{Node, NodeConfig, NodeError};
use crate::protocol::{Purpose, join_denied, reason_code};
use crate::settings::InputSettings;
use crate::transport::{Incoming, Link, Transport, TransportError, closed_because};
use clipboard::Clip;
use input::{Input, InputMsg};
use mesh::{Mesh, Msg, Wiring};

/// How often this device's displays are read, to notice a display being
/// plugged in, removed or rearranged
const SCREEN_POLL: Duration = Duration::from_secs(2);

/// Longest device name, in characters
pub const MAX_NAME_CHARS: usize = 40;

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
    /// The device name could not be saved
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// A device name must not be blank or too long
    #[error("a device name needs 1 to {MAX_NAME_CHARS} characters")]
    InvalidName,
    /// Settings with a hotkey that could take over typing, or a number out
    /// of range
    #[error("these settings cannot be used")]
    InvalidSettings,
    /// The input settings could not be saved
    #[error("cannot save the input settings: {0}")]
    SaveSettings(String),
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
    /// (again as each attempt starts)
    JoinPin {
        /// Who asks
        joiner: PeerInfo,
        /// The PIN to show
        pin: String,
        /// Attempts the joiner has left, the current one included
        attempts_left: u32,
    },
    /// Who controls what changed
    Control(ControlEvent),
    /// Show this device's number in the layout on its screens for a moment
    /// (some member asked, maybe this one)
    Identify,
    /// A key combination was recorded ([`Engine::record`]); `None` when the
    /// user gave up
    Recorded(Option<Chord>),
    /// That join is over (hide the PIN)
    JoinEnded {
        /// Who asked
        joiner: PeerInfo,
        /// Whether it got in
        admitted: bool,
    },
    /// Files dragged here are still arriving, while their drop waits or
    /// after it was made: show how far, where they land (a card per drop,
    /// by [`Receiving::id`])
    Receiving(Receiving),
    /// The drop with this id is over (its files all there, or not coming):
    /// its card goes
    ReceivingEnded(u64),
    /// A drag of files could not come here
    DragFailed {
        /// Why (a [`drag_failed`] code)
        reason: String,
        /// The first file or folder dragged
        name: String,
    },
    /// Files copied on another member were fetched here ahead of a paste,
    /// or not
    CopiedFiles(CopiedFiles),
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
    /// This device's info, sent in handshakes; changes with its name
    info: watch::Receiver<PeerInfo>,
    /// The data directory, where the device name and the input settings
    /// are saved (`None`: nothing is saved)
    data_dir: Option<PathBuf>,
    /// The input settings in use
    input_settings: Mutex<InputSettings>,
    /// Wakes the join being sponsored, to turn it down
    reject: Arc<Notify>,
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
    /// keyboard and mouse of `input`, the clipboard `clipboard` and drags of
    /// files through `drag`, with the stream of events for the user
    pub async fn start(
        config: NodeConfig,
        input: Arc<dyn InputBackend>,
        clipboard: Arc<dyn Clipboard>,
        drag: Arc<dyn DragBackend>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<EngineEvent>), EngineError> {
        let store = GroupStore::new(&config.data_dir);
        let data_dir = Some(config.data_dir.clone());
        let (node, peer_events) = Node::start(config).await?;
        let node = Arc::new(node);
        let transport = Arc::clone(node.transport());
        let info = node.info().clone();
        let peers = Some(peer_events);
        Self::launch(
            store,
            data_dir,
            transport,
            info,
            Some(node),
            peers,
            input,
            clipboard,
            drag,
        )
    }

    /// Wire the mesh, the input and clipboard actors, the accept loop, the
    /// display poller and the discovery feed around a bound transport
    #[allow(clippy::too_many_arguments)] // one call site, and the tests
    fn launch(
        store: GroupStore,
        data_dir: Option<PathBuf>,
        transport: Arc<Transport>,
        info: PeerInfo,
        node: Option<Arc<Node>>,
        peer_events: Option<mpsc::Receiver<PeerEvent>>,
        backend: Arc<dyn InputBackend>,
        clipboard: Arc<dyn Clipboard>,
        drag: Arc<dyn DragBackend>,
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
            move |info: &PeerInfo, group: Option<&str>| {
                if let Some(node) = &node {
                    advertise_group(node, info, group);
                }
            }
        };
        let (local_tx, local_rx) = watch::channel(info.clone());
        let reject = Arc::new(Notify::new());
        let (input_tx, input_inbox) = mpsc::unbounded_channel();
        let (links_tx, links_rx) = watch::channel(Arc::default());
        let (clip_tx, clip_inbox) = mpsc::unbounded_channel();
        let offers = files::Offers::default();
        let clip = Clip::new(
            &info.fingerprint,
            clipboard,
            links_rx.clone(),
            clip_tx.clone(),
            events_tx.clone(),
            offers.clone(),
        );
        let input = Input::new(
            &info.fingerprint,
            Arc::clone(&backend),
            links_rx,
            events_tx.clone(),
            input_tx.clone(),
            clip_tx.clone(),
            drag.as_ref(),
            offers.clone(),
        );
        let input_settings = data_dir
            .as_deref()
            .map(InputSettings::load)
            .unwrap_or_default();
        let _ = input_tx.send(InputMsg::Settings(input_settings.clone()));
        let wiring = Wiring {
            published: doc_tx,
            local: local_tx,
            events: events_tx.clone(),
            inbox: inbox_tx.clone(),
            advertise: Box::new(advertise),
            input: input_tx.clone(),
            links: links_tx,
            clip: clip_tx,
            offers,
        };
        let mesh = Mesh::new(info.clone(), Arc::clone(&transport), store, doc, wiring);
        let mut tasks = vec![
            tokio::spawn(mesh.run(inbox)),
            tokio::spawn(input.run(input_inbox)),
            tokio::spawn(clip.run(clip_inbox)),
            tokio::spawn(poll_screens(backend, inbox_tx.clone())),
            // Folders of drags left over from earlier runs
            tokio::task::spawn_blocking(drag::sweep),
        ];
        tasks.push(tokio::spawn(accept_loop(
            Arc::clone(&transport),
            local_rx.clone(),
            doc_rx.clone(),
            inbox_tx.clone(),
            events_tx,
            Arc::clone(&reject),
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
                info: local_rx,
                data_dir,
                input_settings: Mutex::new(input_settings),
                reject,
                inbox: inbox_tx,
                input: input_tx,
                doc: doc_rx,
                tasks: Mutex::new(tasks),
            }),
        };
        Ok((engine, events))
    }

    /// This device's info
    pub fn info(&self) -> PeerInfo {
        self.inner.info.borrow().clone()
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
            .connect(sponsor, &self.info(), Purpose::Join)
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

    /// Set how fast the pointer goes on this device while another member
    /// controls it, in percent; the controlling member moves it
    pub async fn set_pointer_speed(&self, speed: u32) -> Result<(), EngineError> {
        self.ask(|reply| Msg::SetPointerSpeed { speed, reply })
            .await?
    }

    /// Set what of this device's clipboard is shared with the group: handed
    /// over as the pointer leaves, taken in as it comes
    pub async fn set_clipboard(&self, share: ClipboardShare) -> Result<(), EngineError> {
        self.ask(|reply| Msg::SetClipboard { share, reply }).await?
    }

    /// Set what this device does with files from the group: whether files
    /// are dragged to and from it
    pub async fn set_files(&self, share: FileShare) -> Result<(), EngineError> {
        self.ask(|reply| Msg::SetFiles { share, reply }).await?
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

    /// Turn down the join this device sponsors right now, if any: the
    /// joiner hears [`join_denied::REJECTED`]
    pub fn reject_join(&self) {
        self.inner.reject.notify_waiters();
    }

    /// Go by `name` from now on: saved, advertised on the LAN, and sent to
    /// the group
    pub async fn rename(&self, name: &str) -> Result<(), EngineError> {
        let name = name.trim().to_string();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
            return Err(EngineError::InvalidName);
        }
        if let Some(dir) = self.inner.data_dir.clone() {
            let saved = name.clone();
            tokio::task::spawn_blocking(move || identity::persist_display_name(&dir, Some(&saved)))
                .await
                .map_err(|e| EngineError::Store(std::io::Error::other(e)))??;
        }
        self.ask(|reply| Msg::Rename { name, reply }).await
    }

    /// Have every online member, this one included, show its number in the
    /// layout on its screens for a moment
    pub fn identify(&self) -> Result<(), EngineError> {
        self.inner
            .inbox
            .send(Msg::Identify)
            .map_err(|_| EngineError::Stopped)
    }

    /// Carry out a request (pause, lock, jump) at this device's next input
    /// event
    pub fn request(&self, request: Request) -> Result<(), EngineError> {
        self.inner
            .input
            .send(InputMsg::Request(request))
            .map_err(|_| EngineError::Stopped)
    }

    /// Cancel drop `id` (see [`Receiving::id`]), dropped before its files
    /// were all there: they stop coming, and the app it landed on gets
    /// none
    pub fn cancel_drop(&self, id: u64) -> Result<(), EngineError> {
        self.inner
            .input
            .send(InputMsg::CancelDrop(id))
            .map_err(|_| EngineError::Stopped)
    }

    /// The input settings in use
    pub fn input_settings(&self) -> InputSettings {
        self.inner
            .input_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Use and save new input settings
    pub async fn set_input_settings(&self, settings: InputSettings) -> Result<(), EngineError> {
        if !settings.valid() {
            return Err(EngineError::InvalidSettings);
        }
        if let Some(dir) = self.inner.data_dir.clone() {
            let saved = settings.clone();
            tokio::task::spawn_blocking(move || saved.save(&dir))
                .await
                .map_err(|e| EngineError::SaveSettings(e.to_string()))?
                .map_err(|e| EngineError::SaveSettings(e.to_string()))?;
        }
        *self
            .inner
            .input_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = settings.clone();
        self.inner
            .input
            .send(InputMsg::Settings(settings))
            .map_err(|_| EngineError::Stopped)
    }

    /// Start recording a key combination, or stop: the next key pressed
    /// with modifiers, on this device's keyboard or the controlling
    /// device's, comes back as [`EngineEvent::Recorded`] instead of going
    /// anywhere, and hotkeys wait meanwhile
    pub fn record(&self, on: bool) -> Result<(), EngineError> {
        self.inner
            .input
            .send(InputMsg::Record(on))
            .map_err(|_| EngineError::Stopped)
    }

    /// Set the edge between members `a` and `b`, for the whole group
    pub async fn set_edge(
        &self,
        a: &str,
        b: &str,
        settings: EdgeSettings,
    ) -> Result<(), EngineError> {
        let (a, b) = (a.to_string(), b.to_string());
        self.ask(|reply| Msg::SetEdge {
            a,
            b,
            settings,
            reply,
        })
        .await?
    }

    /// Whether capture and injection run on this device
    pub async fn input_status(&self) -> Result<InputStatus, EngineError> {
        self.ask_input(InputMsg::Status).await
    }

    /// Start whichever of capture and injection does not run, typically
    /// after the user granted a permission, and report
    pub async fn restart_input(&self) -> Result<InputStatus, EngineError> {
        self.ask_input(InputMsg::Restart).await
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

    /// Send the input actor a request and wait for its reply
    async fn ask_input<T>(
        &self,
        msg: impl FnOnce(oneshot::Sender<T>) -> InputMsg,
    ) -> Result<T, EngineError> {
        let (reply, answer) = oneshot::channel();
        self.inner
            .input
            .send(msg(reply))
            .map_err(|_| EngineError::Stopped)?;
        answer.await.map_err(|_| EngineError::Stopped)
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

    /// Resolves when the sponsor ends the join while the PIN is being
    /// typed: with its reason (a [`join_denied`] code such as
    /// [`join_denied::REJECTED`]) when it gave one
    pub fn ended(&self) -> impl Future<Output = Option<String>> + Send + 'static {
        let conn = self.link.connection().clone();
        async move { closed_because(&conn).await }
    }

    /// Answer with a PIN: `Some` with the group once in, `None` when the PIN
    /// was wrong and another attempt is left
    pub async fn answer(&mut self, pin: &str) -> Result<Option<Arc<GroupDoc>>, EngineError> {
        let own_fp = self.engine.inner.info.borrow().fingerprint.clone();
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

/// Advertise this device in discovery, with its group ID (or none)
fn advertise_group(node: &Node, info: &PeerInfo, group: Option<&str>) {
    let mut info = info.clone();
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
    info: watch::Receiver<PeerInfo>,
    doc: watch::Receiver<Option<Arc<GroupDoc>>>,
    inbox: mpsc::UnboundedSender<Msg>,
    events: mpsc::UnboundedSender<EngineEvent>,
    reject: Arc<Notify>,
) {
    let join = JoinWiring {
        gate: Arc::new(Mutex::new(JoinGate::default())),
        reject,
        inbox: inbox.clone(),
        events,
    };
    while let Some(incoming) = transport.accept().await {
        let info = info.borrow().clone();
        tokio::spawn(serve_incoming(
            incoming,
            info,
            doc.clone(),
            inbox.clone(),
            join.clone(),
        ));
    }
}

/// What sponsoring a join needs
#[derive(Clone)]
struct JoinWiring {
    /// One join at a time, with a cooldown after a used-up PIN
    gate: Arc<Mutex<JoinGate>>,
    /// Wakes the join in progress to turn it down
    reject: Arc<Notify>,
    /// The mesh, to admit the joiner
    inbox: mpsc::UnboundedSender<Msg>,
    /// Events for the user (the PIN)
    events: mpsc::UnboundedSender<EngineEvent>,
}

/// Run the handshake of one incoming connection and hand the link to what
/// its purpose calls for
async fn serve_incoming(
    incoming: Incoming,
    info: PeerInfo,
    doc: watch::Receiver<Option<Arc<GroupDoc>>>,
    inbox: mpsc::UnboundedSender<Msg>,
    join: JoinWiring,
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
        Purpose::Join => sponsor(link, &info, &join).await,
        Purpose::Diag => diag::respond(link).await,
    }
}

/// Sponsor one join: show a PIN, verify the joiner knows it, let it in
async fn sponsor(mut link: Link, own: &PeerInfo, wiring: &JoinWiring) {
    let lock = || wiring.gate.lock().unwrap_or_else(PoisonError::into_inner);
    let joiner = link.remote().clone();
    let begun = lock().begin(Instant::now());
    if let Err(code) = begun {
        tracing::debug!(joiner = %joiner.name, "turned a join away: {code}");
        let _ = join::deny(&mut link, code).await;
        link.close_after_flush_because(code).await;
        return;
    }
    let result = sponsor_join(&mut link, own, &joiner, wiring).await;
    if let Err(e) = &result {
        tracing::info!(joiner = %joiner.name, "join failed: {e}");
    }
    lock().end(Instant::now(), &result);
    let _ = wiring.events.send(EngineEvent::JoinEnded {
        joiner,
        admitted: result.is_ok(),
    });
    // The last message (acceptance or denial) must reach the joiner; the
    // reason goes into the close too, for a joiner still typing the PIN
    let reason = match &result {
        Err(JoinError::Denied(code)) => code.as_str(),
        Err(JoinError::PinUsedUp) => join_denied::WRONG_PIN,
        Err(JoinError::Timeout(_)) => join_denied::TIMEOUT,
        _ => "",
    };
    link.close_after_flush_because(reason).await;
}

/// The steps of a sponsored join
async fn sponsor_join(
    link: &mut Link,
    own: &PeerInfo,
    joiner: &PeerInfo,
    wiring: &JoinWiring,
) -> Result<(), JoinError> {
    let pin = join::new_pin()?;
    // Registered before the PIN is shown, so no rejection is missed
    let rejected = wiring.reject.notified();
    let show_pin = |attempts_left| {
        let _ = wiring.events.send(EngineEvent::JoinPin {
            joiner: joiner.clone(),
            pin: pin.clone(),
            attempts_left,
        });
    };
    let verified = tokio::select! {
        verified = join::verify(link, &own.fingerprint, &pin, show_pin) => verified?,
        () = rejected => {
            join::deny(link, join_denied::REJECTED).await?;
            return Err(JoinError::Denied(join_denied::REJECTED.into()));
        }
    };
    let (reply, admitted) = oneshot::channel();
    let _ = wiring.inbox.send(Msg::Admit {
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
