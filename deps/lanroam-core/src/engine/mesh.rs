//! The mesh: the actor holding the group document and one link per online
//! member.
//!
//! - **Dialing**: every member visible in discovery gets dialed until a link
//!   stands, retrying with backoff. Both sides dial, since a firewall may
//!   block one direction; the side with the smaller fingerprint dials first
//!   and the other waits [`DIAL_DEFER`], so they rarely dial at once
//! - **Duplicates**: when two links to one peer exist anyway, both sides
//!   keep the same one: the one dialed by the smaller fingerprint. The newer
//!   one wins instead when the same side dialed both, or when the older one
//!   is past [`RACE_WINDOW`]: a peer only dials when it has no link, so it
//!   restarted or lost the old one without us noticing yet
//! - **Sync**: each link starts with both sides sending their document; a
//!   merge that changes ours is saved and sent on to every link, so changes
//!   spread through the group and stop once everyone agrees
//! - **Input**: input messages and datagrams go to the input actor, which
//!   also gets the links to send on and the online
//!   part of the layout: a device that is offline cannot be crossed into

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use lan_kit::frame::FrameError;
use lan_kit::{Peer, PeerEvent, PeerInfo};
use lanroam_input::Rect;
use lanroam_input::config::{EdgeSettings, MAX_POINTER_SPEED, MIN_SPEED, valid_pointer_speed};
use lanroam_input::world::World;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior};

use super::input::{InputMsg, LinkHandle, Links};
use super::{EngineError, EngineEvent, Spot, Status};
use crate::group::{GroupDoc, GroupStore, Profile};
use crate::layout;
use crate::protocol::{Control, Datagram, FRAMING, Purpose, reason_code};
use crate::transport::{Link, Transport, TransportError, close_code};

/// First retry delay after a failed dial or a lost link
const RETRY_MIN: Duration = Duration::from_secs(1);

/// Longest retry delay
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Head start of the side with the smaller fingerprint when both could dial
const DIAL_DEFER: Duration = Duration::from_secs(2);

/// Links to one peer set up within this span of each other come from both
/// sides dialing at once; a link arriving later replaces the old one
const RACE_WINDOW: Duration = Duration::from_secs(5);

/// How long a member whose link was closed as a duplicate may stay unlinked
/// before it counts as offline (the surviving link is usually on its way)
const DUPLICATE_GRACE: Duration = Duration::from_secs(3);

/// Period of the housekeeping (due dials, expired graces)
const TICK: Duration = Duration::from_secs(1);

/// How long a closing link may take to deliver its last messages
const LINGER: Duration = Duration::from_secs(1);

/// The mesh's inbox
pub(super) enum Msg {
    /// Discovery news
    Peer(PeerEvent),
    /// A member link passed the Hello gate
    LinkUp {
        /// The link
        link: Link,
        /// Whether this side dialed it
        dialed: bool,
    },
    /// Dialing a member failed
    DialFailed {
        /// The member
        fingerprint: String,
        /// Which dial
        attempt: u64,
        /// Why
        error: TransportError,
    },
    /// A control message arrived on a link
    Received {
        /// The link
        id: u64,
        /// The message
        msg: Control,
    },
    /// A link is gone
    LinkDown {
        /// The link
        id: u64,
        /// The peer closed it in favor of another link
        duplicate: bool,
    },
    /// A sponsored joiner proved the PIN: add it (founding a group if
    /// needed) and reply with the document to hand over
    Admit {
        /// The joiner
        joiner: PeerInfo,
        /// The document, the joiner included
        reply: oneshot::Sender<GroupDoc>,
    },
    /// This device joined: take the sponsor's document
    Adopt {
        /// The document
        doc: GroupDoc,
        /// Done
        reply: oneshot::Sender<Result<(), EngineError>>,
    },
    /// This device's displays, as last read
    Screens {
        /// Display rectangles, in device coordinates
        displays: Vec<Rect>,
        /// Device units per logical pixel, in percent
        scale: u32,
    },
    /// Move a member on the layout canvas
    Place {
        /// The member
        fingerprint: String,
        /// Where to
        spot: Spot,
        /// Done
        reply: oneshot::Sender<Result<(), EngineError>>,
    },
    /// Turn the Command / Control swap for input into this device on or off
    SetSwap {
        /// On
        on: bool,
        /// Done
        reply: oneshot::Sender<Result<(), EngineError>>,
    },
    /// Set how fast the pointer goes on this device while controlled
    SetPointerSpeed {
        /// Percent
        speed: u32,
        /// Done
        reply: oneshot::Sender<Result<(), EngineError>>,
    },
    /// Set the edge between two members
    SetEdge {
        /// One member
        a: String,
        /// The other
        b: String,
        /// The settings
        settings: EdgeSettings,
        /// Done
        reply: oneshot::Sender<Result<(), EngineError>>,
    },
    /// Remove a member
    Kick {
        /// The member
        fingerprint: String,
        /// Done
        reply: oneshot::Sender<Result<(), EngineError>>,
    },
    /// Leave the group
    Leave {
        /// Done
        reply: oneshot::Sender<Result<(), EngineError>>,
    },
    /// Report the state
    Status {
        /// The report
        reply: oneshot::Sender<Status>,
    },
    /// Every online member, this one included, shows its number
    Identify,
    /// This device got a new name (already persisted)
    Rename {
        /// The name
        name: String,
        /// Done
        reply: oneshot::Sender<()>,
    },
    /// Close everything and stop
    Shutdown {
        /// Done
        reply: oneshot::Sender<()>,
    },
}

/// Advertises this device in discovery, with its group ID (`None`: no
/// group)
pub(super) type Advertise = Box<dyn Fn(&PeerInfo, Option<&str>) + Send>;

/// Channels the mesh talks through
pub(super) struct Wiring {
    /// Latest document, for the rest of the engine
    pub(super) published: watch::Sender<Option<Arc<GroupDoc>>>,
    /// This device's info as it changes (its name), for the rest of the
    /// engine
    pub(super) local: watch::Sender<PeerInfo>,
    /// Events for the user
    pub(super) events: mpsc::UnboundedSender<EngineEvent>,
    /// The mesh's own inbox, handed to the tasks it spawns
    pub(super) inbox: mpsc::UnboundedSender<Msg>,
    /// Advertise the group ID in discovery
    pub(super) advertise: Advertise,
    /// The input actor's inbox
    pub(super) input: mpsc::UnboundedSender<InputMsg>,
    /// Member links, for the input actor
    pub(super) links: watch::Sender<Links>,
}

/// A registered member link
struct LinkEntry {
    /// Tells this link from earlier ones to the same peer
    id: u64,
    /// The member
    info: PeerInfo,
    /// Whether this side dialed it
    dialed: bool,
    /// When it was registered
    since: Instant,
    /// Messages to send; dropping it closes the link once they are out
    out: mpsc::UnboundedSender<Control>,
    /// The connection, for closing a duplicate at once
    conn: quinn::Connection,
}

/// Dialing state of one member
struct Dial {
    /// When to dial next
    due: Instant,
    /// Delay after the next failure
    backoff: Duration,
    /// A dial is running
    in_flight: bool,
    /// Number of the latest dial, so the outcome of an abandoned one is
    /// ignored
    attempt: u64,
}

/// The actor
pub(super) struct Mesh {
    /// This device
    info: PeerInfo,
    /// QUIC endpoint, for dialing
    transport: Arc<Transport>,
    /// The document on disk
    store: GroupStore,
    /// The document; `None` outside a group
    doc: Option<GroupDoc>,
    /// This device's displays (device coordinates) and scale (percent);
    /// `None` until first read
    screens: Option<(Vec<Rect>, u32)>,
    /// The Command / Control swap setting, once changed while running
    swap_cmd_ctrl: Option<bool>,
    /// The pointer speed, once changed while running
    pointer_speed: Option<u32>,
    /// Channels
    wiring: Wiring,
    /// Group ID currently advertised
    advertised: Option<String>,
    /// Nodes discovery sees, by fingerprint
    peers: HashMap<String, Peer>,
    /// Member links, by fingerprint
    links: HashMap<String, LinkEntry>,
    /// Members to dial, by fingerprint
    dials: HashMap<String, Dial>,
    /// Members announced online; `Some` since when a duplicate close left
    /// one unlinked
    announced: HashMap<String, Option<Instant>>,
    /// Next link ID
    next_id: u64,
    /// Number of the latest dial
    next_attempt: u64,
}

impl Mesh {
    /// A mesh around the loaded document
    pub(super) fn new(
        info: PeerInfo,
        transport: Arc<Transport>,
        store: GroupStore,
        doc: Option<GroupDoc>,
        wiring: Wiring,
    ) -> Self {
        Self {
            info,
            transport,
            store,
            doc,
            screens: None,
            swap_cmd_ctrl: None,
            pointer_speed: None,
            wiring,
            advertised: None,
            peers: HashMap::new(),
            links: HashMap::new(),
            dials: HashMap::new(),
            announced: HashMap::new(),
            next_id: 0,
            next_attempt: 0,
        }
    }

    /// Run until shut down
    pub(super) async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Msg>) {
        self.open();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                msg = inbox.recv() => match msg {
                    Some(Msg::Shutdown { reply }) => {
                        // Dropping the entries closes the links
                        self.links.clear();
                        self.publish_links();
                        let _ = reply.send(());
                        break;
                    }
                    Some(msg) => self.handle(msg),
                    None => break,
                },
                _ = tick.tick() => self.maintain(),
            }
        }
    }

    /// Settle the loaded document: drop it if it no longer lists this
    /// device, refresh this device's profile, publish it
    fn open(&mut self) {
        let fp = &self.info.fingerprint;
        if self.doc.as_ref().is_some_and(|doc| !doc.is_member(fp)) {
            tracing::warn!("the saved group document does not list this device; ignoring it");
            self.doc = None;
            self.save();
        }
        if self.doc.is_some() {
            self.commit();
        }
    }

    /// Dispatch one inbox message
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Peer(event) => self.on_peer(event),
            Msg::LinkUp { link, dialed } => self.on_link_up(link, dialed),
            Msg::DialFailed {
                fingerprint,
                attempt,
                error,
            } => self.on_dial_failed(&fingerprint, attempt, &error),
            Msg::Received { id, msg } => self.on_received(id, msg),
            Msg::LinkDown { id, duplicate } => self.on_link_down(id, duplicate),
            Msg::Admit { joiner, reply } => {
                self.admit(&joiner);
                if let Some(doc) = &self.doc {
                    let _ = reply.send(doc.clone());
                }
            }
            Msg::Screens { displays, scale } => {
                let screens = Some((displays, scale));
                if screens != self.screens {
                    self.screens = screens;
                    self.commit();
                }
            }
            Msg::Place {
                fingerprint,
                spot,
                reply,
            } => {
                let _ = reply.send(self.place(&fingerprint, spot));
            }
            Msg::SetSwap { on, reply } => {
                let result = match self.doc {
                    Some(_) => {
                        self.swap_cmd_ctrl = Some(on);
                        self.commit();
                        Ok(())
                    }
                    None => Err(EngineError::NoGroup),
                };
                let _ = reply.send(result);
            }
            Msg::SetPointerSpeed { speed, reply } => {
                let _ = reply.send(self.set_pointer_speed(speed));
            }
            Msg::SetEdge {
                a,
                b,
                settings,
                reply,
            } => {
                let _ = reply.send(self.set_edge(&a, &b, settings));
            }
            Msg::Adopt { doc, reply } => {
                let _ = reply.send(self.adopt(doc));
            }
            Msg::Kick { fingerprint, reply } => {
                let _ = reply.send(self.kick(&fingerprint));
            }
            Msg::Leave { reply } => {
                let _ = reply.send(self.leave());
            }
            Msg::Status { reply } => {
                let _ = reply.send(Status {
                    doc: self.wiring.published.borrow().clone(),
                    online: self.links.values().map(|l| l.info.clone()).collect(),
                });
            }
            Msg::Rename { name, reply } => {
                self.rename(name);
                let _ = reply.send(());
            }
            Msg::Identify => {
                for entry in self.links.values() {
                    let _ = entry.out.send(Control::Identify);
                }
                self.emit(EngineEvent::Identify);
            }
            // Handled by the run loop
            Msg::Shutdown { .. } => {}
        }
    }

    /// Track the nodes discovery sees; a member showing up gets dialed
    fn on_peer(&mut self, event: PeerEvent) {
        match event {
            PeerEvent::Up(peer) => {
                let fp = peer.info.fingerprint.clone();
                let moved = self
                    .peers
                    .insert(fp.clone(), peer.clone())
                    .is_some_and(|old| (&old.addrs, old.port) != (&peer.addrs, peer.port));
                if moved {
                    // A dial to the old address may hang until its timeout;
                    // start over at the new one
                    self.dials.remove(&fp);
                }
                self.maintain();
            }
            PeerEvent::Down(fp) => {
                self.peers.remove(&fp);
                self.dials.remove(&fp);
            }
        }
    }

    /// Whether this device should be linked to `fp` but is not
    fn wants_link(&self, fp: &str) -> bool {
        fp != self.info.fingerprint
            && !self.links.contains_key(fp)
            && self.doc.as_ref().is_some_and(|doc| doc.is_member(fp))
    }

    /// Whether this side dials first to `fp`
    fn dials_first(&self, fp: &str) -> bool {
        self.info.fingerprint.as_str() < fp
    }

    /// Housekeeping: schedule and start due dials, let graces expire
    fn maintain(&mut self) {
        let now = Instant::now();
        let wanted: Vec<String> = self
            .peers
            .keys()
            .filter(|fp| self.wants_link(fp) && !self.dials.contains_key(*fp))
            .cloned()
            .collect();
        for fp in wanted {
            let defer = if self.dials_first(&fp) {
                Duration::ZERO
            } else {
                DIAL_DEFER
            };
            self.dials.insert(
                fp,
                Dial {
                    due: now + defer,
                    backoff: RETRY_MIN,
                    in_flight: false,
                    attempt: 0,
                },
            );
        }

        let due: Vec<String> = self
            .dials
            .iter()
            .filter(|(_, dial)| !dial.in_flight && dial.due <= now)
            .map(|(fp, _)| fp.clone())
            .collect();
        for fp in due {
            match self.peers.get(&fp) {
                Some(peer) if self.wants_link(&fp) => {
                    self.next_attempt += 1;
                    self.spawn_dial(peer.clone(), self.next_attempt);
                    if let Some(dial) = self.dials.get_mut(&fp) {
                        dial.in_flight = true;
                        dial.attempt = self.next_attempt;
                    }
                }
                _ => {
                    self.dials.remove(&fp);
                }
            }
        }

        let expired: Vec<String> = self
            .announced
            .iter()
            .filter(|(_, since)| since.is_some_and(|t| now >= t + DUPLICATE_GRACE))
            .map(|(fp, _)| fp.clone())
            .collect();
        for fp in expired {
            self.announced.remove(&fp);
            let name = self.name_of(&fp);
            self.emit(EngineEvent::Offline {
                fingerprint: fp,
                name,
            });
        }
    }

    /// Dial a member in the background; the result comes back to the inbox
    fn spawn_dial(&self, peer: Peer, attempt: u64) {
        let transport = Arc::clone(&self.transport);
        let info = self.info.clone();
        let inbox = self.wiring.inbox.clone();
        tokio::spawn(async move {
            let msg = match transport.connect(&peer, &info, Purpose::Member).await {
                Ok(link) => Msg::LinkUp { link, dialed: true },
                Err(error) => Msg::DialFailed {
                    fingerprint: peer.info.fingerprint,
                    attempt,
                    error,
                },
            };
            let _ = inbox.send(msg);
        });
    }

    /// Back off after a failed dial; a member saying this device was removed
    /// ends its membership
    fn on_dial_failed(&mut self, fp: &str, attempt: u64, error: &TransportError) {
        let removed =
            matches!(error, TransportError::Rejected(code) if code == reason_code::REMOVED);
        // Only a current member is believed: the dial pinned its certificate
        if removed && self.doc.as_ref().is_some_and(|doc| doc.is_member(fp)) {
            tracing::info!(
                "{} says this device was removed from the group",
                self.name_of(fp)
            );
            self.drop_group(true);
            return;
        }
        tracing::debug!(member = %self.name_of(fp), "dial failed: {error}");
        if let Some(dial) = self.dials.get_mut(fp).filter(|d| d.attempt == attempt) {
            dial.in_flight = false;
            dial.due = Instant::now() + dial.backoff;
            dial.backoff = (dial.backoff * 2).min(RETRY_MAX);
        }
    }

    /// Register a member link, unless it duplicates a better one
    fn on_link_up(&mut self, link: Link, dialed: bool) {
        let info = link.remote().clone();
        let fp = info.fingerprint.clone();
        self.dials.remove(&fp);
        let member = self.doc.as_ref().is_some_and(|doc| doc.is_member(&fp));
        if !member || fp == self.info.fingerprint {
            // Membership changed while the handshake ran
            link.close();
            return;
        }
        if let Some(existing) = self.links.get(&fp) {
            let raced = existing.since.elapsed() < RACE_WINDOW;
            let keep_new = !raced || existing.dialed == dialed || dialed == self.dials_first(&fp);
            if !keep_new {
                link.connection()
                    .close(close_code::DUPLICATE, b"duplicate link");
                return;
            }
            existing
                .conn
                .close(close_code::DUPLICATE, b"duplicate link");
            self.links.remove(&fp);
        }
        self.register(link, info, dialed);
    }

    /// Start the reader and writer of a link and send it our document
    fn register(&mut self, link: Link, info: PeerInfo, dialed: bool) {
        let id = self.next_id;
        self.next_id += 1;
        let (_, conn, send, recv) = link.into_parts();
        let (out, out_rx) = mpsc::unbounded_channel();
        tokio::spawn(write_link(send, conn.clone(), out_rx));
        tokio::spawn(read_link(id, recv, conn.clone(), self.wiring.inbox.clone()));
        tokio::spawn(read_datagrams(
            info.fingerprint.clone(),
            conn.clone(),
            self.wiring.input.clone(),
        ));
        if let Some(doc) = &self.doc {
            let _ = out.send(Control::Group { doc: doc.clone() });
        }
        let fp = info.fingerprint.clone();
        if self.announced.insert(fp.clone(), None).is_none() {
            self.emit(EngineEvent::Online(info.clone()));
        }
        self.links.insert(
            fp,
            LinkEntry {
                id,
                info,
                dialed,
                since: Instant::now(),
                out,
                conn,
            },
        );
        self.publish_links();
        self.publish_world();
    }

    /// The member behind a link ID, if that link is still the registered one
    fn member_of(&self, id: u64) -> Option<String> {
        self.links
            .iter()
            .find(|(_, entry)| entry.id == id)
            .map(|(fp, _)| fp.clone())
    }

    /// Handle a control message from a member
    fn on_received(&mut self, id: u64, msg: Control) {
        let Some(fp) = self.member_of(id) else {
            return;
        };
        match msg {
            Control::Group { doc } => self.on_doc(&fp, &doc),
            Control::Identify => self.emit(EngineEvent::Identify),
            Control::Ping { seq, sent_us } => {
                if let Some(entry) = self.links.get(&fp) {
                    let _ = entry.out.send(Control::Pong { seq, sent_us });
                }
            }
            msg @ (Control::Enter { .. }
            | Control::PointerLocked { .. }
            | Control::Request { .. }
            | Control::Leave
            | Control::Key { .. }
            | Control::Button { .. }
            | Control::Wheel { .. }
            | Control::Released { .. }
            | Control::Pong { .. }) => {
                let _ = self.wiring.input.send(InputMsg::Control { from: fp, msg });
            }
            other => {
                tracing::debug!(kind = other.kind(), from = %self.name_of(&fp), "ignoring control message");
            }
        }
    }

    /// Forget a lost link and dial again
    fn on_link_down(&mut self, id: u64, duplicate: bool) {
        let Some(fp) = self.member_of(id) else {
            return;
        };
        let Some(entry) = self.links.remove(&fp) else {
            return;
        };
        if duplicate {
            self.announced.insert(fp, Some(Instant::now()));
        } else {
            let _ = self.wiring.input.send(InputMsg::LinkDown(fp.clone()));
            self.announced.remove(&fp);
            self.emit(EngineEvent::Offline {
                fingerprint: fp,
                name: entry.info.name,
            });
        }
        self.publish_links();
        self.publish_world();
        self.maintain();
    }

    /// Merge a member's document; learning of this device's removal ends
    /// its membership
    fn on_doc(&mut self, from: &str, theirs: &GroupDoc) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        if theirs.id != doc.id {
            tracing::warn!(from = %from, "ignoring the document of another group");
            return;
        }
        if !doc.merge(theirs) {
            return;
        }
        if !doc.is_member(&self.info.fingerprint) {
            tracing::info!("removed from the group by another member");
            self.drop_group(true);
            return;
        }
        self.commit();
    }

    /// Let a verified joiner in (founding a group if there is none) and
    /// give it a place right of everyone, level with this device
    fn admit(&mut self, joiner: &PeerInfo) {
        let own = self.info.fingerprint.clone();
        if self.doc.is_none() {
            self.doc = Some(GroupDoc::new(&self.info));
            // Place ourselves first, so the joiner lands beside us
            self.commit();
        }
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.admit(joiner);
        let fp = &joiner.fingerprint;
        if doc.devices.get(fp).is_some_and(|r| r.placement.is_none()) {
            let at = layout::newcomer_spot(doc, &own);
            doc.place(fp, at, &own);
        }
        self.commit();
    }

    /// Move a member on the canvas, refusing spots that overlap others
    fn place(&mut self, fp: &str, spot: Spot) -> Result<(), EngineError> {
        let doc = self.doc.as_mut().ok_or(EngineError::NoGroup)?;
        let at = match spot {
            Spot::At(at) => at,
            Spot::Beside {
                side,
                anchor,
                offset,
            } => layout::spot_beside(doc, fp, side, &anchor, offset)?,
        };
        layout::check(doc, fp, at)?;
        doc.place(fp, at, &self.info.fingerprint);
        self.commit();
        Ok(())
    }

    /// Set the edge between members `a` and `b`, for the whole group
    fn set_edge(&mut self, a: &str, b: &str, settings: EdgeSettings) -> Result<(), EngineError> {
        let doc = self.doc.as_mut().ok_or(EngineError::NoGroup)?;
        if let Some(stranger) = [a, b].into_iter().find(|fp| !doc.is_member(fp)) {
            return Err(EngineError::NotAMember(stranger.to_string()));
        }
        if a == b || !settings.valid() {
            return Err(EngineError::InvalidSettings);
        }
        doc.set_edge(a, b, settings, &self.info.fingerprint);
        self.commit();
        Ok(())
    }

    /// This device's profile as it stands: settings and, until they are
    /// first read, displays as the document holds them
    fn own_profile(&self) -> Profile {
        let known = self
            .doc
            .as_ref()
            .and_then(|doc| doc.devices.get(&self.info.fingerprint))
            .map(|record| record.profile.clone());
        let mut profile = known.unwrap_or_else(|| Profile::new(&self.info));
        profile.device_id.clone_from(&self.info.device_id);
        profile.name.clone_from(&self.info.name);
        profile.platform.clone_from(&self.info.platform);
        if let Some((displays, scale)) = &self.screens {
            profile.displays.clone_from(displays);
            profile.scale = *scale;
        }
        if let Some(on) = self.swap_cmd_ctrl {
            profile.swap_cmd_ctrl = on;
        }
        if let Some(speed) = self.pointer_speed {
            profile.pointer_speed = speed;
        }
        profile
    }

    /// Set this device's pointer speed, for the group
    fn set_pointer_speed(&mut self, speed: u32) -> Result<(), EngineError> {
        if self.doc.is_none() {
            return Err(EngineError::NoGroup);
        }
        if !valid_pointer_speed(speed) {
            return Err(EngineError::InvalidSettings);
        }
        self.pointer_speed = Some(speed);
        self.commit();
        Ok(())
    }

    /// Take a sponsor's document after joining
    fn adopt(&mut self, doc: GroupDoc) -> Result<(), EngineError> {
        if self.doc.is_some() {
            return Err(EngineError::Grouped);
        }
        if !doc.is_member(&self.info.fingerprint) {
            return Err(EngineError::NotAMember(self.info.name.clone()));
        }
        self.doc = Some(doc);
        self.commit();
        Ok(())
    }

    /// Remove a member (oneself: leave)
    fn kick(&mut self, fp: &str) -> Result<(), EngineError> {
        if fp == self.info.fingerprint {
            return self.leave();
        }
        let doc = self.doc.as_mut().ok_or(EngineError::NoGroup)?;
        if !doc.remove(fp) {
            return Err(EngineError::NotAMember(fp.to_string()));
        }
        // Sends the kicked member the news, then closes its link
        self.commit();
        Ok(())
    }

    /// Leave the group, telling the members linked right now
    fn leave(&mut self) -> Result<(), EngineError> {
        let doc = self.doc.as_mut().ok_or(EngineError::NoGroup)?;
        doc.remove(&self.info.fingerprint);
        let doc = doc.clone();
        for entry in self.links.values() {
            let _ = entry.out.send(Control::Group { doc: doc.clone() });
        }
        self.drop_group(false);
        Ok(())
    }

    /// Our document changed: refresh our profile, save, publish, send it to
    /// every member, drop links to devices no longer in the group, dial new
    /// members
    fn commit(&mut self) {
        let profile = self.own_profile();
        let own = self.info.fingerprint.as_str();
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.update_profile(own, profile);
        if doc.devices.get(own).is_some_and(|r| r.placement.is_none()) {
            let at = layout::newcomer_spot(doc, own);
            doc.place(own, at, own);
        }
        let doc = doc.clone();
        self.save();
        let shared = Arc::new(doc.clone());
        self.wiring
            .published
            .send_replace(Some(Arc::clone(&shared)));
        self.advertise(Some(&doc.id));
        for entry in self.links.values() {
            let _ = entry.out.send(Control::Group { doc: doc.clone() });
        }
        let gone: Vec<String> = self
            .links
            .keys()
            .filter(|fp| !doc.is_member(fp))
            .cloned()
            .collect();
        for fp in gone {
            // Dropping the entry closes the link after the document is out
            if let Some(entry) = self.links.remove(&fp) {
                let _ = self.wiring.input.send(InputMsg::LinkDown(fp.clone()));
                self.announced.remove(&fp);
                self.emit(EngineEvent::Offline {
                    fingerprint: fp,
                    name: entry.info.name,
                });
            }
        }
        self.publish_links();
        self.publish_world();
        self.emit(EngineEvent::Group(Some(shared)));
        self.maintain();
    }

    /// Leave the group state behind: no document, no links
    fn drop_group(&mut self, kicked: bool) {
        self.doc = None;
        self.save();
        self.wiring.published.send_replace(None);
        self.advertise(None);
        let links: Vec<(String, LinkEntry)> = self.links.drain().collect();
        for (fp, entry) in links {
            let _ = self.wiring.input.send(InputMsg::LinkDown(fp.clone()));
            self.emit(EngineEvent::Offline {
                fingerprint: fp,
                name: entry.info.name,
            });
        }
        self.publish_links();
        self.publish_world();
        self.announced.clear();
        self.dials.clear();
        self.emit(EngineEvent::Group(None));
        if kicked {
            self.emit(EngineEvent::Kicked);
        }
    }

    /// Save the document; a failure is logged, the group carries on in
    /// memory
    fn save(&self) {
        if let Err(e) = self.store.save(self.doc.as_ref()) {
            tracing::warn!("cannot save the group document: {e}");
        }
    }

    /// Advertise the group ID if it changed
    fn advertise(&mut self, group: Option<&str>) {
        if self.advertised.as_deref() != group {
            self.advertised = group.map(str::to_string);
            (self.wiring.advertise)(&self.info, group);
        }
    }

    /// Go by a new name: in discovery, in handshakes, and in the group
    fn rename(&mut self, name: String) {
        self.info.name = name;
        self.wiring.local.send_replace(self.info.clone());
        (self.wiring.advertise)(&self.info, self.advertised.as_deref());
        if self.doc.is_some() {
            self.commit();
        }
    }

    /// Hand the current links to the input actor
    fn publish_links(&self) {
        let links = self
            .links
            .iter()
            .map(|(fp, entry)| {
                let handle = LinkHandle {
                    out: entry.out.clone(),
                    conn: entry.conn.clone(),
                };
                (fp.clone(), handle)
            })
            .collect();
        self.wiring.links.send_replace(Arc::new(links));
    }

    /// Hand the input actor the layout of the devices online right now (this
    /// one and those linked), which of them get Command and Control swapped,
    /// their pointer speeds, and the members' names
    fn publish_world(&self) {
        let own = self.info.fingerprint.as_str();
        let (world, swapped, speeds, names, numbered, edges) = match &self.doc {
            Some(doc) => {
                let online = |fp: &str| fp == own || self.links.contains_key(fp);
                let placed = layout::world(doc);
                let numbered = layout::numbered(doc);
                let devices = placed
                    .devices()
                    .iter()
                    .filter(|d| online(&d.key))
                    .cloned()
                    .collect::<Vec<_>>();
                let swapped = doc
                    .members()
                    .filter(|(fp, record)| {
                        online(fp)
                            && *fp != own
                            && record.profile.swap_cmd_ctrl
                            && record.profile.platform != self.info.platform
                    })
                    .map(|(fp, _)| fp.to_string())
                    .collect();
                // Another version may send anything: keep it in range
                let speeds = doc
                    .members()
                    .filter(|(fp, _)| online(fp) && *fp != own)
                    .map(|(fp, record)| {
                        let speed = record.profile.pointer_speed;
                        (fp.to_string(), speed.clamp(MIN_SPEED, MAX_POINTER_SPEED))
                    })
                    .collect();
                let names = doc
                    .members()
                    .map(|(fp, record)| (fp.to_string(), record.profile.name.clone()))
                    .collect();
                let edges = doc.edge_settings().collect();
                (World::new(devices), swapped, speeds, names, numbered, edges)
            }
            None => (
                World::default(),
                HashSet::new(),
                HashMap::new(),
                HashMap::new(),
                Vec::new(),
                HashMap::new(),
            ),
        };
        let _ = self.wiring.input.send(InputMsg::World {
            world,
            swapped,
            speeds,
            names,
            numbered,
            edges,
        });
    }

    /// A member's name for messages
    fn name_of(&self, fp: &str) -> String {
        self.doc
            .as_ref()
            .and_then(|doc| doc.devices.get(fp))
            .map(|record| record.profile.name.clone())
            .or_else(|| self.peers.get(fp).map(|peer| peer.info.name.clone()))
            .unwrap_or_else(|| fp.chars().take(8).collect())
    }

    /// Hand an event to the user
    fn emit(&self, event: EngineEvent) {
        let _ = self.wiring.events.send(event);
    }
}

/// Read a link's control stream into the mesh's inbox until it ends
///
/// A frame that does not decode is skipped: it was read whole, so the stream
/// stays in step, and it is most likely a message from a newer minor
/// version that this one may ignore.
async fn read_link(
    id: u64,
    mut recv: quinn::RecvStream,
    conn: quinn::Connection,
    inbox: mpsc::UnboundedSender<Msg>,
) {
    loop {
        match FRAMING.read::<_, Control>(&mut recv).await {
            Ok(msg) => {
                if inbox.send(Msg::Received { id, msg }).is_err() {
                    return;
                }
            }
            Err(FrameError::Codec(e)) => {
                tracing::debug!("skipping a control message this version cannot read: {e}");
            }
            Err(_) => break,
        }
    }
    let duplicate = matches!(
        conn.close_reason(),
        Some(quinn::ConnectionError::ApplicationClosed(close))
            if close.error_code == close_code::DUPLICATE
    );
    let _ = inbox.send(Msg::LinkDown { id, duplicate });
}

/// Hand a link's datagrams to the input actor, answering diagnostic pings
/// on the spot
async fn read_datagrams(
    from: String,
    conn: quinn::Connection,
    input: mpsc::UnboundedSender<InputMsg>,
) {
    while let Ok(bytes) = conn.read_datagram().await {
        match Datagram::decode(&bytes) {
            Some(Datagram::Ping { seq, sent_us }) => {
                let _ = conn.send_datagram(Datagram::Pong { seq, sent_us }.encode());
            }
            Some(datagram) => {
                let msg = InputMsg::Datagram {
                    from: from.clone(),
                    datagram,
                };
                if input.send(msg).is_err() {
                    return;
                }
            }
            None => {}
        }
    }
}

/// Write a link's outgoing messages; once the mesh lets go of the link,
/// deliver what is left and close it
async fn write_link(
    mut send: quinn::SendStream,
    conn: quinn::Connection,
    mut out: mpsc::UnboundedReceiver<Control>,
) {
    while let Some(msg) = out.recv().await {
        if FRAMING.write(&mut send, &msg).await.is_err() {
            return;
        }
    }
    if send.finish().is_ok() {
        let _ = tokio::time::timeout(LINGER, send.stopped()).await;
    }
    conn.close(close_code::NORMAL, b"bye");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TestNode;

    /// An unknown message is skipped and the link keeps working
    #[tokio::test]
    async fn unknown_messages_are_skipped() {
        let (_a, _b, client, server) = TestNode::link_pair().await;
        let (_, conn, _send, recv) = server.into_parts();
        let (inbox, mut received) = mpsc::unbounded_channel();
        tokio::spawn(read_link(7, recv, conn, inbox));

        let (_, _conn, mut send, _recv) = client.into_parts();
        let unknown = serde_json::json!({ "type": "from_the_future", "x": 1 });
        FRAMING.write(&mut send, &unknown).await.unwrap();
        FRAMING
            .write(&mut send, &Control::Ping { seq: 1, sent_us: 2 })
            .await
            .unwrap();
        let next = tokio::time::timeout(Duration::from_secs(5), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            next,
            Msg::Received {
                id: 7,
                msg: Control::Ping { seq: 1, .. }
            }
        ));
    }
}
