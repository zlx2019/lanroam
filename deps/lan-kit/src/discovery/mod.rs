//! Discovery layer: lets the nodes of one app on the LAN see each other.
//!
//! Two channels:
//! - Primary: mDNS/DNS-SD registration and browsing of the app's service
//!   type
//! - Fallback: periodic UDP multicast announcements (some enterprise routers
//!   block mDNS)
//!
//! Either channel alone is enough; only a failure of both is an error.
//! Node lifecycle: heartbeat every 5s → offline after 15s of silence →
//! goodbye packet on exit. A node that stays visible on mDNS while silent on
//! UDP is settled by the app's [`IdentityProbe`].
//!
//! Advertised info is small by design: the fixed identity fields plus a few
//! app props (see [`validate_props`]).

mod mdns;
mod probe;
mod registry;
mod udp;

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub use self::probe::{IdentityProbe, ProbeFuture};
use self::probe::{ProbeTiming, probe_peer};
use self::registry::Registry;
use self::udp::{MulticastPulse, SharedPackets, UdpChannel, UdpPackets, read_packets};
use crate::PeerInfo;
use crate::profile::AppProfile;

/// Longest prop key in bytes
pub const MAX_PROP_KEY_LEN: usize = 32;
/// Longest `key=value` entry in bytes (a DNS TXT string holds 255 bytes)
pub const MAX_PROP_ENTRY_LEN: usize = 255;
/// Total size budget of all `key=value` entries in bytes
pub const MAX_PROPS_LEN: usize = 1024;

/// Discovery layer errors
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// Neither the mDNS nor the UDP multicast channel could start
    #[error("discovery unavailable: both mDNS and UDP multicast failed to start")]
    AllChannelsFailed,
    /// UDP socket operation failed
    #[error("UDP multicast channel error: {0}")]
    Io(#[from] std::io::Error),
    /// mDNS daemon error
    #[error("mDNS channel error: {0}")]
    Mdns(#[from] mdns_sd::Error),
    /// An app prop breaks the advertising rules
    #[error("invalid discovery prop {key:?}: {reason}")]
    InvalidProp {
        /// Offending key
        key: String,
        /// Which rule it breaks
        reason: &'static str,
    },
    /// An update tried to change the fingerprint, the root of the identity
    #[error("the advertised fingerprint cannot change while discovery runs")]
    FingerprintChanged,
}

/// An online node on the LAN
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// Device info
    pub info: PeerInfo,
    /// Candidate addresses (several with several NICs; non-loopback IPv4
    /// first, tried in order when dialing)
    pub addrs: Vec<IpAddr>,
    /// Service port peers dial
    pub port: u16,
}

impl Peer {
    /// Candidate socket addresses, in dialing order
    pub fn socket_addrs(&self) -> impl Iterator<Item = SocketAddr> + '_ {
        self.addrs.iter().map(|ip| SocketAddr::new(*ip, self.port))
    }
}

/// Node up / down event
#[derive(Debug, Clone)]
pub enum PeerEvent {
    /// A node came online, or its info changed
    Up(Peer),
    /// A node went offline (payload: its certificate fingerprint)
    Down(String),
}

/// Discovery timing; the defaults suit a LAN
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryConfig {
    /// Period of the UDP multicast announcement
    pub heartbeat_interval: Duration,
    /// No heartbeat for this long means offline (tolerates 2 missed beats)
    pub peer_timeout: Duration,
    /// Minimum gap between identity probes of the same mDNS-only node
    pub probe_interval: Duration,
    /// Budget per address for one identity probe (connect + handshake)
    pub probe_timeout: Duration,
    /// Multicast silence after which the membership is rebuilt; must be far
    /// longer than the heartbeat to ride out the odd lost packet
    pub multicast_silence_timeout: Duration,
    /// Capacity of the peer event channel; events are dropped when it is
    /// full, and consumers can resync from [`DiscoveryService::peers`]
    pub event_capacity: usize,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(5),
            peer_timeout: Duration::from_secs(15),
            probe_interval: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(2),
            multicast_silence_timeout: Duration::from_secs(60),
            event_capacity: 64,
        }
    }
}

/// How to run discovery
pub struct DiscoveryOptions {
    /// Port peers dial to reach this device's service
    pub service_port: u16,
    /// UDP port of the multicast fallback channel (shared by all instances of
    /// the app)
    pub discovery_port: u16,
    /// Passive ("incognito") mode: listen only, never advertise
    pub passive: bool,
    /// Identity probe used to settle mDNS-only nodes; without one they are
    /// handed straight to mDNS cache verification
    pub prober: Option<Arc<dyn IdentityProbe>>,
    /// Timing
    pub config: DiscoveryConfig,
}

impl DiscoveryOptions {
    /// Active discovery with default timing and no identity probe
    pub fn new(service_port: u16, discovery_port: u16) -> Self {
        Self {
            service_port,
            discovery_port,
            passive: false,
            prober: None,
            config: DiscoveryConfig::default(),
        }
    }
}

/// Check app props against the advertising rules
///
/// Props end up in DNS TXT records and UDP announcements, hence the rules:
/// keys are 1..=[`MAX_PROP_KEY_LEN`] bytes of printable ASCII without `=`
/// and are not one of the reserved keys (`id`, `name`, `fp`, `platform`,
/// `osv`, any case); each `key=value` fits in [`MAX_PROP_ENTRY_LEN`] bytes,
/// and all of them together in [`MAX_PROPS_LEN`] bytes.
pub fn validate_props(props: &BTreeMap<String, String>) -> Result<(), DiscoveryError> {
    let invalid = |key: &str, reason| DiscoveryError::InvalidProp {
        key: key.to_string(),
        reason,
    };
    let mut total = 0;
    for (key, value) in props {
        if key.is_empty() || key.len() > MAX_PROP_KEY_LEN {
            return Err(invalid(key, "key must be 1 to 32 bytes"));
        }
        if !key.bytes().all(|b| b.is_ascii_graphic() && b != b'=') {
            return Err(invalid(key, "key must be printable ASCII without '='"));
        }
        if mdns::is_reserved(key) {
            return Err(invalid(key, "key is reserved by lan-kit"));
        }
        let entry = key.len() + 1 + value.len();
        if entry > MAX_PROP_ENTRY_LEN {
            return Err(invalid(key, "key=value exceeds 255 bytes"));
        }
        total += entry;
    }
    if total > MAX_PROPS_LEN {
        return Err(DiscoveryError::InvalidProp {
            key: String::new(),
            reason: "props exceed 1024 bytes in total",
        });
    }
    Ok(())
}

/// Mutable broadcast state (incognito toggling and identity updates share
/// it; one lock keeps them consistent)
struct BroadcastState {
    /// Identity currently advertised
    info: PeerInfo,
    /// Incognito: receive only, never send
    passive: bool,
    /// Full name of our mDNS service (Some while registered, for
    /// unregistering)
    mdns_fullname: Option<String>,
}

/// Discovery service: advertises this device, listens to the network and
/// reports node changes over an event channel
pub struct DiscoveryService {
    /// App constants (service type, multicast group)
    profile: AppProfile,
    /// Node registry
    registry: Arc<Registry>,
    /// mDNS daemon (None if it failed to start, degrading to UDP only)
    mdns: Option<mdns_sd::ServiceDaemon>,
    /// Multicast socket (None if it failed to start, degrading to mDNS only)
    udp: Option<Arc<UdpSocket>>,
    /// Multicast destination
    udp_target: (Ipv4Addr, u16),
    /// Pre-encoded UDP packets
    packets: SharedPackets,
    /// Advertised service port (fixed after start)
    service_port: u16,
    /// Advertised identity, incognito switch, registered mDNS name
    state: Mutex<BroadcastState>,
    /// Background tasks, aborted on shutdown
    tasks: Vec<JoinHandle<()>>,
}

impl DiscoveryService {
    /// Start discovery: advertise `info`, begin listening, and return the
    /// service handle plus the node event stream
    pub async fn start(
        profile: &AppProfile,
        info: PeerInfo,
        options: DiscoveryOptions,
    ) -> Result<(Self, mpsc::Receiver<PeerEvent>), DiscoveryError> {
        validate_props(&info.props)?;
        let DiscoveryOptions {
            service_port,
            discovery_port,
            passive,
            prober,
            config,
        } = options;
        let (events_tx, events_rx) = mpsc::channel(config.event_capacity);
        let registry = Arc::new(Registry::new(
            info.fingerprint.clone(),
            config.probe_interval,
            events_tx,
        ));
        let mut tasks = Vec::new();

        // Channel one: mDNS registration + browsing
        let (mdns, mdns_fullname) = match mdns::start_mdns(
            profile.mdns_service_type,
            &info,
            service_port,
            passive,
            &registry,
            config.peer_timeout,
            &mut tasks,
        ) {
            Ok((daemon, fullname)) => (Some(daemon), fullname),
            Err(e) => {
                tracing::warn!("mDNS failed to start, running on UDP multicast only: {e}");
                (None, None)
            }
        };

        // Channel two: UDP multicast
        let packets: SharedPackets = Arc::new(std::sync::RwLock::new(UdpPackets::encode(
            &info,
            service_port,
            passive,
        )));
        let pulse: MulticastPulse = Arc::new(Mutex::new(None));
        let udp_target = (profile.multicast_group, discovery_port);
        let channel = UdpChannel {
            group: profile.multicast_group,
            port: discovery_port,
            self_fingerprint: info.fingerprint.clone(),
            heartbeat: config.heartbeat_interval,
        };
        let udp = match udp::start_udp(
            channel,
            Arc::clone(&packets),
            Arc::clone(&pulse),
            &registry,
            &mut tasks,
        )
        .await
        {
            Ok(socket) => Some(socket),
            Err(e) => {
                tracing::warn!("UDP multicast failed to start, running on mDNS only: {e}");
                None
            }
        };

        if mdns.is_none() && udp.is_none() {
            for task in &tasks {
                task.abort();
            }
            return Err(DiscoveryError::AllChannelsFailed);
        }

        // Timeout sweep + identity probes. Probes run as their own tasks so a
        // slow one never stalls the sweep cadence; the daemon handle is a
        // cheap command-channel clone
        let sweeper = Arc::clone(&registry);
        let sweeper_mdns = mdns.clone();
        let service_type = profile.mdns_service_type;
        let timing = ProbeTiming {
            per_addr: config.probe_timeout,
            peer_timeout: config.peer_timeout,
        };
        tasks.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(config.heartbeat_interval);
            loop {
                tick.tick().await;
                for peer in sweeper.sweep(config.peer_timeout) {
                    tokio::spawn(probe_peer(
                        Arc::clone(&sweeper),
                        sweeper_mdns.clone(),
                        prober.clone(),
                        peer,
                        service_type,
                        timing,
                    ));
                }
            }
        }));

        // Multicast membership watchdog (only with a UDP channel)
        if let Some(socket) = &udp {
            tasks.push(tokio::spawn(udp::membership_watchdog(
                Arc::clone(socket),
                udp_target,
                Arc::clone(&packets),
                pulse,
                config.multicast_silence_timeout,
            )));
        }

        Ok((
            Self {
                profile: *profile,
                registry,
                mdns,
                udp,
                udp_target,
                packets,
                service_port,
                state: Mutex::new(BroadcastState {
                    info,
                    passive,
                    mdns_fullname,
                }),
                tasks,
            },
            events_rx,
        ))
    }

    /// Lock the broadcast state (a poisoned lock recovers the inner data)
    fn lock_state(&self) -> std::sync::MutexGuard<'_, BroadcastState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Send one packet right away without waiting for the heartbeat
    ///
    /// Synchronous on purpose: callers may sit on a thread without a tokio
    /// runtime (a synchronous Tauri command, say), where spawning panics. A
    /// single `try_send_to` completes instantly, and the heartbeat covers the
    /// occasional failure.
    fn send_now(&self, packet: &[u8]) {
        if let Some(udp) = &self.udp
            && !packet.is_empty()
            && let Err(e) = udp.try_send_to(packet, self.udp_target.into())
        {
            tracing::debug!("immediate UDP send failed, the heartbeat will cover it: {e}");
        }
    }

    /// Register (or re-register) our mDNS service with the given info
    ///
    /// Registering the same name again overwrites the record: peers see the
    /// new TXT at once and never see us go offline.
    fn register_mdns(&self, state: &mut BroadcastState) {
        let Some(daemon) = &self.mdns else {
            return;
        };
        match mdns::build_service(
            self.profile.mdns_service_type,
            &state.info,
            self.service_port,
        ) {
            Ok(service) => {
                let fullname = service.get_fullname().to_string();
                match daemon.register(service) {
                    // Remember the latest name so a later unregister matches
                    Ok(()) => state.mdns_fullname = Some(fullname),
                    Err(e) => tracing::warn!("mDNS registration failed: {e}"),
                }
            }
            Err(e) => tracing::warn!("failed to build the mDNS service record: {e}"),
        }
    }

    /// Toggle incognito mode live; a no-op when already in that state
    ///
    /// On: send goodbye first so peers drop us immediately, then empty the
    /// packet set (silencing the heartbeat) and unregister mDNS. Off:
    /// re-encode the packets, register mDNS again and announce right away.
    /// Synchronous and non-blocking, like [`Self::update_info`].
    pub fn set_passive(&self, passive: bool) {
        let mut state = self.lock_state();
        if state.passive == passive {
            return;
        }
        state.passive = passive;
        if passive {
            // Grab the goodbye before emptying the set; UDP is unreliable,
            // so it goes out twice (peers fall back to the timeout anyway)
            let goodbye = read_packets(&self.packets).goodbye.clone();
            self.send_now(&goodbye);
            self.send_now(&goodbye);
            *self.packets.write().unwrap_or_else(PoisonError::into_inner) =
                UdpPackets::encode(&state.info, self.service_port, true);
            if let Some(daemon) = &self.mdns
                && let Some(fullname) = state.mdns_fullname.take()
                && let Err(e) = daemon.unregister(&fullname)
            {
                tracing::warn!("mDNS unregistration failed, peers will wait for the TTL: {e}");
            }
            tracing::info!("incognito mode on (listening only)");
        } else {
            *self.packets.write().unwrap_or_else(PoisonError::into_inner) =
                UdpPackets::encode(&state.info, self.service_port, false);
            self.register_mdns(&mut state);
            let announce = read_packets(&self.packets).announce.clone();
            self.send_now(&announce);
            tracing::info!("incognito mode off, advertising again");
        }
    }

    /// Update the advertised info live (a rename, changed props, ...)
    ///
    /// The fingerprint is the root of the identity and cannot change; the
    /// port is fixed at start. Both channels pick the change up at once: the
    /// UDP packets are re-encoded and announced immediately, and mDNS is
    /// re-registered under the same name. The broadcast actions stay under
    /// the state lock, mutually exclusive with [`Self::set_passive`]:
    /// otherwise a concurrent switch into incognito could finish
    /// unregistering and then be undone by this registration.
    pub fn update_info(&self, info: &PeerInfo) -> Result<(), DiscoveryError> {
        validate_props(&info.props)?;
        let mut state = self.lock_state();
        if info.fingerprint != state.info.fingerprint {
            return Err(DiscoveryError::FingerprintChanged);
        }
        state.info = info.clone();
        *self.packets.write().unwrap_or_else(PoisonError::into_inner) =
            UdpPackets::encode(info, self.service_port, state.passive);
        if state.passive {
            return Ok(());
        }
        let announce = read_packets(&self.packets).announce.clone();
        self.send_now(&announce);
        self.register_mdns(&mut state);
        Ok(())
    }

    /// Snapshot of the online nodes (the fallback beside the event stream)
    pub fn peers(&self) -> Vec<Peer> {
        self.registry.snapshot()
    }

    /// One online node by certificate fingerprint
    pub fn peer_by_fingerprint(&self, fingerprint: &str) -> Option<Peer> {
        self.registry.get(fingerprint)
    }

    /// Graceful shutdown: send goodbye, unregister mDNS, stop the background
    /// tasks (idempotent)
    pub async fn shutdown(&self) {
        if let Some(udp) = &self.udp {
            // Twice, since UDP is unreliable (empty in passive mode)
            let goodbye = read_packets(&self.packets).goodbye.clone();
            if !goodbye.is_empty() {
                for _ in 0..2 {
                    let _ = udp.send_to(&goodbye, self.udp_target).await;
                }
            }
        }
        if let Some(mdns) = &self.mdns {
            // In incognito the name is already gone, but browsing still has
            // to stop
            let fullname = self.lock_state().mdns_fullname.take();
            if let Some(fullname) = fullname {
                let _ = mdns.unregister(&fullname);
            }
            let _ = mdns.shutdown();
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Drop for DiscoveryService {
    /// Without an explicit shutdown the background tasks and the mDNS daemon
    /// thread would outlive the handle
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        if let Some(mdns) = &self.mdns {
            // Already shut down if `shutdown` ran; the error is expected then
            let _ = mdns.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Props builder for the validation tests
    fn props(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Ordinary props pass
    #[test]
    fn valid_props_pass() {
        assert!(validate_props(&props(&[("pv", "1.0"), ("group", "desk-7")])).is_ok());
        assert!(validate_props(&BTreeMap::new()).is_ok());
    }

    /// Every rule is enforced
    #[test]
    fn invalid_props_are_rejected() {
        let long_value = "v".repeat(MAX_PROP_ENTRY_LEN);
        let cases = [
            props(&[("", "x")]),
            props(&[(&"k".repeat(MAX_PROP_KEY_LEN + 1), "x")]),
            props(&[("a=b", "x")]),
            props(&[("with space", "x")]),
            props(&[("组", "x")]),
            props(&[("fp", "spoof")]),
            props(&[("NAME", "spoof")]),
            props(&[("k", &long_value)]),
        ];
        for case in &cases {
            assert!(
                matches!(
                    validate_props(case),
                    Err(DiscoveryError::InvalidProp { .. })
                ),
                "accepted {case:?}"
            );
        }
    }

    /// The total budget is enforced even when every entry is fine
    #[test]
    fn props_total_budget() {
        let value = "v".repeat(200);
        let many: BTreeMap<String, String> =
            (0..6).map(|i| (format!("k{i}"), value.clone())).collect();
        assert!(matches!(
            validate_props(&many),
            Err(DiscoveryError::InvalidProp { .. })
        ));
    }

    /// Candidate socket addresses follow the address order
    #[test]
    fn socket_addrs_follow_order() {
        let mut peer = registry::tests::test_peer("aaa", "n");
        peer.addrs.push(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));
        let addrs: Vec<SocketAddr> = peer.socket_addrs().collect();
        assert_eq!(
            addrs,
            vec![
                SocketAddr::from(([192, 168, 1, 2], 42624)),
                SocketAddr::from(([10, 0, 0, 2], 42624)),
            ]
        );
    }
}
