//! Node registry: merges the mDNS and UDP sources, tracks online state and
//! dispatches events.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::{Peer, PeerEvent};

/// Which channel a sighting came from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PeerSource {
    /// mDNS browsing: event driven, no repeated events while the service
    /// stays up
    Mdns,
    /// UDP multicast: heartbeat driven, refreshed every heartbeat period
    Udp,
}

/// Node state inside the registry
///
/// Liveness is tracked per channel. mDNS can only be declared dead by
/// ServiceRemoved (goodbye or TTL expiry), never by elapsed time, because it
/// has no periodic heartbeat; UDP is dead once no heartbeat arrives within
/// the timeout. A node goes offline only when both channels are dead,
/// otherwise it would be dropped after 15s on every network that blocks
/// multicast but lets mDNS through.
struct PeerState {
    /// Node info
    peer: Peer,
    /// Time of the last UDP heartbeat (None if never seen over UDP)
    last_udp: Option<Instant>,
    /// mDNS channel alive (set by ServiceResolved, cleared by
    /// ServiceRemoved)
    mdns_alive: bool,
    /// When the last liveness probe started (throttling; coming online
    /// counts as just probed)
    last_probe: Option<Instant>,
}

/// Registry of online nodes
pub(super) struct Registry {
    /// Our own fingerprint (filters out self-sightings)
    self_fingerprint: String,
    /// Minimum gap between two liveness probes of the same node
    probe_interval: Duration,
    /// Online nodes, keyed by certificate fingerprint
    peers: Mutex<HashMap<String, PeerState>>,
    /// Event sender (drops when full; consumers can resync from a snapshot)
    events: mpsc::Sender<PeerEvent>,
}

impl Registry {
    /// Create an empty registry
    pub(super) fn new(
        self_fingerprint: String,
        probe_interval: Duration,
        events: mpsc::Sender<PeerEvent>,
    ) -> Self {
        Self {
            self_fingerprint,
            probe_interval,
            peers: Mutex::new(HashMap::new()),
            events,
        }
    }

    /// Take the lock; a poisoned lock simply recovers the inner data (the map
    /// holds no cross-thread invariant)
    fn lock_peers(&self) -> MutexGuard<'_, HashMap<String, PeerState>> {
        self.peers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Insert or refresh a node; Up is emitted only when the info changed, a
    /// heartbeat merely refreshes the timestamp
    ///
    /// Address merging: mDNS (many addresses) and UDP (the single source
    /// address) complement each other; the union is deduped and normalized
    /// (IPv4 first, stable so alternating channels do not make the order
    /// jitter). An update left with no usable address is dropped until a
    /// later event brings one. Stale addresses are not pruned one by one; the
    /// list is rebuilt after the node times out as a whole.
    pub(super) fn upsert(&self, mut peer: Peer, source: PeerSource) {
        if peer.info.fingerprint == self.self_fingerprint {
            return;
        }
        let mut peers = self.lock_peers();
        let fingerprint = peer.info.fingerprint.clone();
        // A new sighting only adds liveness; it never clears the other
        // channel's flag
        let (changed, mut last_udp, mut mdns_alive, last_probe) = match peers.get(&fingerprint) {
            Some(state) => {
                let mut merged = state.peer.addrs.clone();
                for addr in &peer.addrs {
                    if !merged.contains(addr) {
                        merged.push(*addr);
                    }
                }
                peer.addrs = normalize_addrs(merged);
                (
                    state.peer != peer,
                    state.last_udp,
                    state.mdns_alive,
                    state.last_probe,
                )
            }
            None => {
                peer.addrs = normalize_addrs(peer.addrs);
                // Coming online is proof of life, so the first probe waits a
                // full interval
                (true, None, false, Some(Instant::now()))
            }
        };
        if peer.addrs.is_empty() {
            return;
        }
        match source {
            PeerSource::Udp => last_udp = Some(Instant::now()),
            PeerSource::Mdns => mdns_alive = true,
        }
        peers.insert(
            fingerprint,
            PeerState {
                peer: peer.clone(),
                last_udp,
                mdns_alive,
                last_probe,
            },
        );
        drop(peers);
        if changed {
            self.emit(PeerEvent::Up(peer));
        }
    }

    /// Remove a node by fingerprint and emit Down
    pub(super) fn remove(&self, fingerprint: &str) {
        let existed = self.lock_peers().remove(fingerprint).is_some();
        if existed {
            self.emit(PeerEvent::Down(fingerprint.to_string()));
        }
    }

    /// The mDNS service disappeared (goodbye or TTL expiry): clear the mDNS
    /// liveness flag
    ///
    /// The node stays while its UDP heartbeat is fresh (degrading to one
    /// channel must not flicker it offline), otherwise it goes at once.
    /// ServiceRemoved only names the instance, i.e. the device ID.
    pub(super) fn mdns_removed(&self, device_id: &str, udp_timeout: Duration) {
        let gone = {
            let mut peers = self.lock_peers();
            let Some((fp, state)) = peers
                .iter_mut()
                .find(|(_, s)| s.peer.info.device_id == device_id)
            else {
                return;
            };
            state.mdns_alive = false;
            let udp_dead = state.last_udp.is_none_or(|t| t.elapsed() > udp_timeout);
            let fp = fp.clone();
            // Under the same lock: a heartbeat arriving in between must not
            // be wiped out by the verdict from before it
            udp_dead.then(|| {
                peers.remove(&fp);
                fp
            })
        };
        if let Some(fp) = gone {
            self.emit(PeerEvent::Down(fp));
        }
    }

    /// Drop dead nodes and return the suspicious ones that are due for a
    /// liveness probe
    ///
    /// - Cleanup: mDNS dead and the UDP heartbeat timed out (or never came)
    ///   → removed; a node alive over mDNS is never dropped on elapsed time,
    ///   it goes offline through ServiceRemoved
    /// - Probing: a node alive only over mDNS while UDP is silent cannot be
    ///   judged by time (a crashed node would linger for the ~2min SRV TTL),
    ///   so it is handed back for a probe. The probe timestamp is stamped
    ///   here, throttled by the probe interval, so being returned means "due"
    pub(super) fn sweep(&self, timeout: Duration) -> Vec<Peer> {
        let now = Instant::now();
        let mut probes = Vec::new();
        let expired: Vec<String> = {
            let mut peers = self.lock_peers();
            for state in peers.values_mut() {
                let udp_silent = state.last_udp.is_none_or(|t| t.elapsed() > timeout);
                let probe_due = state
                    .last_probe
                    .is_none_or(|t| t.elapsed() > self.probe_interval);
                if state.mdns_alive && udp_silent && probe_due {
                    state.last_probe = Some(now);
                    probes.push(state.peer.clone());
                }
            }
            let expired: Vec<String> = peers
                .iter()
                .filter(|(_, s)| !s.mdns_alive && s.last_udp.is_none_or(|t| t.elapsed() > timeout))
                .map(|(fp, _)| fp.clone())
                .collect();
            // Removed under the lock they were judged under, like in
            // `mdns_removed`
            for fp in &expired {
                peers.remove(fp);
            }
            expired
        };
        for fp in expired {
            tracing::debug!(fingerprint = %fp, "heartbeat timed out, node is offline");
            self.emit(PeerEvent::Down(fp));
        }
        probes
    }

    /// Whether the node's UDP heartbeat is still inside the window (fresher
    /// evidence than a failed probe)
    pub(super) fn udp_fresh(&self, fingerprint: &str, timeout: Duration) -> bool {
        self.lock_peers()
            .get(fingerprint)
            .is_some_and(|s| s.last_udp.is_some_and(|t| t.elapsed() <= timeout))
    }

    /// Apply a failed probe directly: remove the node
    ///
    /// Only the fallback for when no mDNS daemon is available; the regular
    /// path leaves the verdict to mDNS cache verification (see
    /// `probe::probe_peer`). A UDP heartbeat that came back during the probe
    /// is newer evidence and keeps the node: a probe can only refute liveness
    /// that rests on mDNS alone.
    pub(super) fn probe_failed(&self, fingerprint: &str, udp_timeout: Duration) {
        {
            let mut peers = self.lock_peers();
            let Some(state) = peers.get(fingerprint) else {
                return;
            };
            if state.last_udp.is_some_and(|t| t.elapsed() <= udp_timeout) {
                return;
            }
            // Under the lock the heartbeat was checked under, like in
            // `mdns_removed`
            peers.remove(fingerprint);
        }
        tracing::info!(fingerprint = %fingerprint, "identity probe failed, node is gone");
        self.emit(PeerEvent::Down(fingerprint.to_string()));
    }

    /// Snapshot of the online nodes
    pub(super) fn snapshot(&self) -> Vec<Peer> {
        self.lock_peers().values().map(|s| s.peer.clone()).collect()
    }

    /// One online node by fingerprint
    pub(super) fn get(&self, fingerprint: &str) -> Option<Peer> {
        self.lock_peers().get(fingerprint).map(|s| s.peer.clone())
    }

    /// Emit an event; dropped when the channel is full (consumers can resync
    /// from a snapshot at any time)
    fn emit(&self, event: PeerEvent) {
        if let Err(e) = self.events.try_send(event) {
            tracing::debug!("peer event channel is full, dropping an event: {e}");
        }
    }

    /// Test hook: forget when a node was last probed, making it due now
    #[cfg(test)]
    pub(super) fn clear_last_probe(&self, fingerprint: &str) {
        if let Some(state) = self.lock_peers().get_mut(fingerprint) {
            state.last_probe = None;
        }
    }
}

/// Normalize candidate addresses: drop IPv6 link-local addresses (not
/// dialable without a scope id) and put non-loopback IPv4 first
///
/// The result may be empty (a first mDNS event carrying only AAAA records,
/// say); the caller then drops the update, because dead addresses must never
/// enter the list.
pub(super) fn normalize_addrs(all: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut addrs: Vec<IpAddr> = all.into_iter().filter(|ip| !is_link_local_v6(ip)).collect();
    // Stable sort: equal priority keeps first-seen order
    addrs.sort_by_key(|ip| (ip.is_loopback(), !ip.is_ipv4()));
    addrs
}

/// IPv6 link-local address (fe80::/10)
fn is_link_local_v6(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
        IpAddr::V4(_) => false,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::BTreeMap;
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;
    use crate::PeerInfo;

    /// Default timeout used by the tests
    pub(crate) const TIMEOUT: Duration = Duration::from_secs(15);

    /// A registry plus its event receiver
    pub(crate) fn test_registry(
        self_fp: &str,
    ) -> (std::sync::Arc<Registry>, mpsc::Receiver<PeerEvent>) {
        let (tx, rx) = mpsc::channel(64);
        (
            std::sync::Arc::new(Registry::new(
                self_fp.to_string(),
                Duration::from_secs(30),
                tx,
            )),
            rx,
        )
    }

    /// A test node at 192.168.1.2
    pub(crate) fn test_peer(fp: &str, name: &str) -> Peer {
        Peer {
            info: PeerInfo {
                device_id: format!("dev-{fp}"),
                name: name.to_string(),
                fingerprint: fp.to_string(),
                platform: "macos".to_string(),
                os_version: None,
                props: BTreeMap::new(),
            },
            addrs: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))],
            port: 42624,
        }
    }

    /// A repeated heartbeat does not emit Up again; changed info (a rename)
    /// does
    #[test]
    fn upsert_emits_only_on_change() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "old"), PeerSource::Udp);
        reg.upsert(test_peer("aaa", "old"), PeerSource::Udp); // heartbeat
        reg.upsert(test_peer("aaa", "new"), PeerSource::Udp); // rename
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.info.name == "old"));
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.info.name == "new"));
        assert!(rx.try_recv().is_err());
    }

    /// A props change is an info change too
    #[test]
    fn upsert_emits_on_props_change() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "n"), PeerSource::Udp);
        let mut peer = test_peer("aaa", "n");
        peer.info.props.insert("pv".into(), "2.0".into());
        reg.upsert(peer, PeerSource::Udp);
        let _ = rx.try_recv();
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.info.props["pv"] == "2.0"));
    }

    /// Our own packets are filtered out
    #[test]
    fn self_is_filtered() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("self", "me"), PeerSource::Udp);
        assert!(rx.try_recv().is_err());
        assert!(reg.snapshot().is_empty());
    }

    /// Cross-channel address merging: a new address emits one Up, a
    /// single-address heartbeat no longer causes jitter, the order is stable
    #[test]
    fn upsert_merges_addrs_stably() {
        let (reg, mut rx) = test_registry("self");
        let addr_a = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));
        let addr_b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

        reg.upsert(test_peer("aaa", "n"), PeerSource::Udp);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.addrs == vec![addr_a]));

        // mDNS reports [B, A]: merged onto the existing order as [A, B]
        let mut peer = test_peer("aaa", "n");
        peer.addrs = vec![addr_b, addr_a];
        reg.upsert(peer, PeerSource::Mdns);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.addrs == vec![addr_a, addr_b]));

        // A UDP heartbeat reports A again: nothing changed, no event
        reg.upsert(test_peer("aaa", "n"), PeerSource::Udp);
        assert!(rx.try_recv().is_err());
    }

    /// remove emits Down only for a node that exists
    #[test]
    fn remove_emits_down_once() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("bbb", "b"), PeerSource::Udp);
        let _ = rx.try_recv();
        reg.remove("bbb");
        reg.remove("bbb");
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "bbb"));
        assert!(rx.try_recv().is_err());
    }

    /// Regression: with only mDNS getting through (multicast blocked), a node
    /// must not be dropped by the heartbeat timeout
    #[test]
    fn mdns_peer_survives_sweep() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "n"), PeerSource::Mdns);
        let _ = rx.try_recv();
        reg.sweep(Duration::ZERO);
        assert!(rx.try_recv().is_err());
        assert_eq!(reg.snapshot().len(), 1);
    }

    /// A UDP-only node is cleaned up once its heartbeat times out
    #[test]
    fn udp_peer_swept_after_timeout() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "n"), PeerSource::Udp);
        let _ = rx.try_recv();
        reg.sweep(Duration::ZERO);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "aaa"));
        assert!(reg.snapshot().is_empty());
    }

    /// mDNS gone and no UDP heartbeat: offline at once
    #[test]
    fn mdns_removed_downs_peer_without_udp() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "n"), PeerSource::Mdns);
        let _ = rx.try_recv();
        reg.mdns_removed("dev-aaa", TIMEOUT);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "aaa"));
        assert!(reg.snapshot().is_empty());
    }

    /// mDNS gone but the UDP heartbeat fresh: kept until UDP times out too
    #[test]
    fn mdns_removed_keeps_peer_with_live_udp() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "n"), PeerSource::Mdns);
        reg.upsert(test_peer("aaa", "n"), PeerSource::Udp);
        let _ = rx.try_recv();
        reg.mdns_removed("dev-aaa", TIMEOUT);
        assert!(rx.try_recv().is_err());
        assert_eq!(reg.snapshot().len(), 1);
        reg.sweep(Duration::ZERO);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "aaa"));
    }

    /// Normalization drops fe80:: and puts non-loopback IPv4 first; only
    /// link-local left means empty
    #[test]
    fn normalize_addrs_filters_and_sorts() {
        let v4 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));
        let lo4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let ll6 = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
        let lo6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert_eq!(normalize_addrs(vec![ll6, lo6, lo4, v4]), vec![v4, lo4, lo6]);
        assert_eq!(normalize_addrs(vec![ll6]), Vec::<IpAddr>::new());
    }

    /// A node whose first mDNS event carries only fe80:: is not reported; it
    /// is once IPv4 arrives, and fe80:: never enters the list
    #[test]
    fn link_local_only_peer_is_deferred() {
        let (reg, mut rx) = test_registry("self");
        let ll6 = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
        let v4 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));

        let mut peer = test_peer("aaa", "n");
        peer.addrs = vec![ll6];
        reg.upsert(peer, PeerSource::Mdns);
        assert!(rx.try_recv().is_err());
        assert!(reg.snapshot().is_empty());

        let mut peer = test_peer("aaa", "n");
        peer.addrs = vec![ll6, v4];
        reg.upsert(peer, PeerSource::Mdns);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Up(p)) if p.addrs == vec![v4]));
    }

    /// sweep hands back only nodes that are mDNS-only alive, UDP silent and
    /// past the throttle interval, and stamps them
    #[test]
    fn sweep_flags_stale_mdns_only_peers_for_probe() {
        let (reg, _rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "a"), PeerSource::Mdns);
        reg.upsert(test_peer("bbb", "b"), PeerSource::Udp);
        reg.clear_last_probe("aaa");

        let probes = reg.sweep(TIMEOUT);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].info.fingerprint, "aaa");
        assert!(reg.sweep(TIMEOUT).is_empty());
        assert_eq!(reg.snapshot().len(), 2);
    }

    /// A failed probe: a fresh UDP heartbeat keeps the node, an mDNS-only
    /// node is removed
    #[test]
    fn probe_failed_respects_fresh_udp() {
        let (reg, mut rx) = test_registry("self");
        reg.upsert(test_peer("aaa", "a"), PeerSource::Mdns);
        reg.upsert(test_peer("aaa", "a"), PeerSource::Udp);
        let _ = rx.try_recv();
        reg.probe_failed("aaa", TIMEOUT);
        assert_eq!(reg.snapshot().len(), 1);
        assert!(rx.try_recv().is_err());

        reg.upsert(test_peer("ccc", "c"), PeerSource::Mdns);
        let _ = rx.try_recv();
        reg.probe_failed("ccc", TIMEOUT);
        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "ccc"));
        assert_eq!(reg.snapshot().len(), 1);
    }
}
