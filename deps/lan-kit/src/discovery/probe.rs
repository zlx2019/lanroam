//! Liveness probes for nodes that stay visible on mDNS but fall silent on
//! UDP (typically a crashed process: its mDNS record lingers in caches for
//! the ~2min SRV TTL).
//!
//! lan-kit does not know how an app dials its peers (TCP + TLS, QUIC, ...),
//! so the handshake itself is supplied by the app through [`IdentityProbe`].
//! The verdict logic stays here.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::Peer;
use super::registry::Registry;

/// Future returned by [`IdentityProbe::probe`]
pub type ProbeFuture<'a> = Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>>;

/// Proves who is listening at an address
///
/// Implementations complete an authenticated handshake (mutual TLS 1.3 over
/// the app's own transport) and return the certificate fingerprint the other
/// side presented, or `None` when nothing answers or the handshake fails. The
/// connection should be closed right afterwards; no application data is
/// needed. Timeouts are applied by the caller.
pub trait IdentityProbe: Send + Sync {
    /// Handshake with `addr` and report the fingerprint presented there
    fn probe(&self, addr: SocketAddr) -> ProbeFuture<'_>;
}

/// Timing inputs of one probe run
#[derive(Debug, Clone, Copy)]
pub(super) struct ProbeTiming {
    /// Budget per candidate address (connect + handshake)
    pub(super) per_addr: Duration,
    /// UDP heartbeat freshness window
    pub(super) peer_timeout: Duration,
}

/// Probe a suspicious node: handshake against each candidate address in turn
/// and compare the certificate fingerprint
///
/// **Liveness must identify the peer; a bare connect proves nothing.** The
/// address list only grows, so a stale address may have been reassigned by
/// DHCP to another device running the same app. The handshake would succeed
/// there, and a node that is long gone would stay online forever in
/// everyone's list while every real connection (fingerprint pinned) fails.
/// So a node is alive only when a handshake succeeds **and** presents the
/// node's own fingerprint; reaching someone else is not proof, and the next
/// address is tried.
///
/// When the node is not found it is **not removed directly**; mDNS cache
/// verification (RFC 6762 §10.4) is triggered instead: the daemon queries the
/// instance and, with no answer within 10s, flushes the cache and emits
/// ServiceRemoved, which removes the node through the regular path. Registry
/// and daemon cache are then cleaned together, so a later reconnect is a
/// fresh discovery with a guaranteed Up. Removing directly breaks that: a
/// peer reconnecting before the cached record expires only refreshes an
/// unchanged record, emits no event, and stays invisible. A wrong verdict is
/// harmless: a live peer answers the query and the next probe succeeds.
pub(super) async fn probe_peer(
    registry: Arc<Registry>,
    mdns: Option<mdns_sd::ServiceDaemon>,
    prober: Option<Arc<dyn IdentityProbe>>,
    peer: Peer,
    service_type: &'static str,
    timing: ProbeTiming,
) {
    if let Some(prober) = &prober {
        for addr in peer.socket_addrs() {
            match tokio::time::timeout(timing.per_addr, prober.probe(addr)).await {
                Ok(Some(fp)) if fp == peer.info.fingerprint => return,
                Ok(Some(_)) => {
                    tracing::debug!(
                        %addr, name = %peer.info.name,
                        "probe reached a different device (stale address), not proof of life"
                    );
                }
                // No answer / handshake failed / timed out: next address
                _ => {}
            }
        }
    }
    // The UDP heartbeat came back meanwhile: newer evidence wins
    if registry.udp_fresh(&peer.info.fingerprint, timing.peer_timeout) {
        return;
    }
    match &mdns {
        Some(daemon) => {
            let fullname = format!("{}.{service_type}", peer.info.device_id);
            tracing::info!(name = %peer.info.name, "identity probe failed, verifying the mDNS cache");
            if let Err(e) = daemon.verify(fullname, mdns_sd::VERIFY_TIMEOUT_DEFAULT) {
                tracing::warn!("mDNS cache verification failed to start, removing directly: {e}");
                registry.probe_failed(&peer.info.fingerprint, timing.peer_timeout);
            }
        }
        // Without mDNS no node can be mDNS-only alive; purely defensive
        None => registry.probe_failed(&peer.info.fingerprint, timing.peer_timeout),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;
    use crate::discovery::PeerEvent;
    use crate::discovery::registry::PeerSource;
    use crate::discovery::registry::tests::{TIMEOUT, test_peer, test_registry};

    /// Scripted prober: answers with a fixed fingerprint per address, nothing
    /// for unknown addresses (a closed port)
    struct FakeProbe(HashMap<SocketAddr, String>);

    impl IdentityProbe for FakeProbe {
        fn probe(&self, addr: SocketAddr) -> ProbeFuture<'_> {
            let answer = self.0.get(&addr).cloned();
            Box::pin(async move { answer })
        }
    }

    /// Probe timing for tests
    const TIMING: ProbeTiming = ProbeTiming {
        per_addr: Duration::from_secs(2),
        peer_timeout: TIMEOUT,
    };

    /// Service type the tests pretend to browse
    const SERVICE: &str = "_lan-kit-test._udp.local.";

    /// A node answering with its own fingerprint survives; one that answers
    /// nowhere is removed (no daemon: the direct-removal fallback)
    #[tokio::test]
    async fn probe_keeps_live_and_removes_dead() {
        let (reg, mut rx) = test_registry("self");
        let live = test_peer("live-fp", "live");
        let mut dead = test_peer("dead-fp", "dead");
        dead.addrs = vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 3))];
        reg.upsert(live.clone(), PeerSource::Mdns);
        reg.upsert(dead.clone(), PeerSource::Mdns);
        while rx.try_recv().is_ok() {}

        let prober: Arc<dyn IdentityProbe> = Arc::new(FakeProbe(HashMap::from([(
            live.socket_addrs().next().unwrap(),
            "live-fp".to_string(),
        )])));
        probe_peer(
            Arc::clone(&reg),
            None,
            Some(Arc::clone(&prober)),
            live,
            SERVICE,
            TIMING,
        )
        .await;
        probe_peer(Arc::clone(&reg), None, Some(prober), dead, SERVICE, TIMING).await;

        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "dead-fp"));
        let snapshot = reg.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].info.fingerprint, "live-fp");
    }

    /// Regression: a stale address now held by another device (handshake
    /// succeeds, fingerprint differs) must not count as alive
    #[tokio::test]
    async fn probe_rejects_imposter() {
        let (reg, mut rx) = test_registry("self");
        let victim = test_peer("victim-fp", "victim");
        reg.upsert(victim.clone(), PeerSource::Mdns);
        while rx.try_recv().is_ok() {}

        let prober: Arc<dyn IdentityProbe> = Arc::new(FakeProbe(HashMap::from([(
            victim.socket_addrs().next().unwrap(),
            "imposter-fp".to_string(),
        )])));
        probe_peer(
            Arc::clone(&reg),
            None,
            Some(prober),
            victim,
            SERVICE,
            TIMING,
        )
        .await;

        assert!(matches!(rx.try_recv(), Ok(PeerEvent::Down(fp)) if fp == "victim-fp"));
        assert!(reg.snapshot().is_empty());
    }

    /// The real node may sit behind a later address: an imposter on the
    /// first one does not end the search
    #[tokio::test]
    async fn probe_tries_every_address() {
        let (reg, mut rx) = test_registry("self");
        let mut peer = test_peer("real-fp", "real");
        let first = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));
        let second = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        peer.addrs = vec![first, second];
        reg.upsert(peer.clone(), PeerSource::Mdns);
        while rx.try_recv().is_ok() {}

        let prober: Arc<dyn IdentityProbe> = Arc::new(FakeProbe(HashMap::from([
            (SocketAddr::new(first, peer.port), "imposter-fp".to_string()),
            (SocketAddr::new(second, peer.port), "real-fp".to_string()),
        ])));
        probe_peer(Arc::clone(&reg), None, Some(prober), peer, SERVICE, TIMING).await;

        assert!(rx.try_recv().is_err());
        assert_eq!(reg.snapshot().len(), 1);
    }

    /// With a daemon available a failed probe must not remove the node: the
    /// verdict is left to the ServiceRemoved that verification triggers
    #[tokio::test]
    async fn probe_failure_with_daemon_defers_to_verify() {
        let (reg, mut rx) = test_registry("self");
        let Ok(daemon) = mdns_sd::ServiceDaemon::new() else {
            eprintln!("skipped: cannot create an mDNS daemon in this environment");
            return;
        };
        let dead = test_peer("dead-fp", "dead");
        reg.upsert(dead.clone(), PeerSource::Mdns);
        while rx.try_recv().is_ok() {}

        let prober: Arc<dyn IdentityProbe> = Arc::new(FakeProbe(HashMap::new()));
        probe_peer(
            Arc::clone(&reg),
            Some(daemon.clone()),
            Some(prober),
            dead,
            SERVICE,
            TIMING,
        )
        .await;

        assert!(rx.try_recv().is_err());
        assert_eq!(reg.snapshot().len(), 1);
        let _ = daemon.shutdown();
    }
}
