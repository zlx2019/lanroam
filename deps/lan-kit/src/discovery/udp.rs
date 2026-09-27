//! UDP multicast channel: periodic announcements, unicast responses to
//! newcomers, and goodbyes; plus the watchdog that repairs silently lost
//! multicast membership.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use super::registry::{PeerSource, Registry};
use super::{DiscoveryError, Peer};
use crate::PeerInfo;

/// Receive buffer size; announcements stay far below it because props are
/// capped (see `validate_props`)
const RECV_BUF: usize = 4096;

/// Packet kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AnnounceKind {
    /// Periodic multicast: I am online
    Announce,
    /// Unicast reply to an announcement, so a newcomer sees existing nodes
    /// right away
    Response,
    /// Graceful goodbye
    Goodbye,
}

/// Wire packet; the sender's IP is taken from the UDP source address
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct AnnouncePacket {
    /// Packet kind
    pub(super) kind: AnnounceKind,
    /// Device info
    pub(super) info: PeerInfo,
    /// Service port peers dial. Serialized as `tcp_port`, the name the
    /// sibling apps' existing releases use, whatever the transport
    #[serde(rename = "tcp_port")]
    pub(super) port: u16,
}

/// Pre-serialized packet set, replaced as a whole when the identity changes
pub(super) struct UdpPackets {
    /// Periodic multicast
    pub(super) announce: Vec<u8>,
    /// Unicast reply to an announcement
    pub(super) response: Vec<u8>,
    /// Graceful goodbye
    pub(super) goodbye: Vec<u8>,
}

impl UdpPackets {
    /// Encode the set; passive mode only listens, so every packet is empty
    pub(super) fn encode(info: &PeerInfo, port: u16, passive: bool) -> Self {
        if passive {
            return Self {
                announce: Vec::new(),
                response: Vec::new(),
                goodbye: Vec::new(),
            };
        }
        let encode = |kind| {
            serde_json::to_vec(&AnnouncePacket {
                kind,
                info: info.clone(),
                port,
            })
            // Every field is a plain string or number; this cannot fail
            .unwrap_or_default()
        };
        Self {
            announce: encode(AnnounceKind::Announce),
            response: encode(AnnounceKind::Response),
            goodbye: encode(AnnounceKind::Goodbye),
        }
    }
}

/// Shared handle to the packet set
pub(super) type SharedPackets = Arc<RwLock<UdpPackets>>;

/// Read the packet set (a poisoned lock simply recovers the inner data)
pub(super) fn read_packets(packets: &SharedPackets) -> RwLockReadGuard<'_, UdpPackets> {
    packets.read().unwrap_or_else(PoisonError::into_inner)
}

/// When a packet last arrived through multicast (proof that the membership
/// still works)
pub(super) type MulticastPulse = Arc<Mutex<Option<Instant>>>;

/// Record a multicast arrival
fn mark_pulse(pulse: &MulticastPulse) {
    *pulse.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
}

/// Time of the last multicast arrival
fn read_pulse(pulse: &MulticastPulse) -> Option<Instant> {
    *pulse.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Build the multicast socket: enable address reuse, then bind the discovery
/// port
///
/// SO_REUSEADDR (plus SO_REUSEPORT on unix) lets several instances on one
/// machine all receive multicast and lets a restart skip TIME_WAIT; under
/// multicast semantics every socket bound with reuse gets its own copy of
/// each packet, so none steals from another.
fn bind_multicast_socket(discovery_port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, discovery_port)).into())?;
    UdpSocket::from_std(sock.into())
}

/// Inputs of the UDP channel
pub(super) struct UdpChannel {
    /// Multicast group
    pub(super) group: Ipv4Addr,
    /// Discovery port
    pub(super) port: u16,
    /// Our fingerprint (our own packets come back through loopback)
    pub(super) self_fingerprint: String,
    /// Heartbeat period
    pub(super) heartbeat: Duration,
}

/// Join the group and spawn the heartbeat + receive task
///
/// Packets are read from the shared set on every send, so an identity update
/// takes effect at once; in passive mode the set is empty, which silences
/// heartbeats and replies (receive only).
pub(super) async fn start_udp(
    channel: UdpChannel,
    packets: SharedPackets,
    pulse: MulticastPulse,
    registry: &Arc<Registry>,
    tasks: &mut Vec<JoinHandle<()>>,
) -> Result<Arc<UdpSocket>, DiscoveryError> {
    let socket = bind_multicast_socket(channel.port)?;
    socket.join_multicast_v4(channel.group, Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_loop_v4(true)?;
    let socket = Arc::new(socket);

    let sock = Arc::clone(&socket);
    let reg = Arc::clone(registry);
    tasks.push(tokio::spawn(async move {
        let target = (channel.group, channel.port);
        let mut buf = vec![0u8; RECV_BUF];
        let mut heartbeat = tokio::time::interval(channel.heartbeat);
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    let announce = read_packets(&packets).announce.clone();
                    if announce.is_empty() {
                        continue;
                    }
                    if let Err(e) = sock.send_to(&announce, target).await {
                        tracing::debug!("failed to send a UDP announcement: {e}");
                    }
                }
                recv = sock.recv_from(&mut buf) => {
                    let Ok((n, src)) = recv else { continue };
                    let Ok(packet) = serde_json::from_slice::<AnnouncePacket>(&buf[..n]) else {
                        continue;
                    };
                    // Announcements and goodbyes only travel by multicast, so
                    // one arriving proves the membership is alive (unicast
                    // responses do not count). Our own announcement loops
                    // back every heartbeat in active mode, so record it
                    // before filtering ourselves out
                    if packet.kind != AnnounceKind::Response {
                        mark_pulse(&pulse);
                    }
                    if packet.info.fingerprint == channel.self_fingerprint {
                        continue;
                    }
                    match packet.kind {
                        AnnounceKind::Goodbye => reg.remove(&packet.info.fingerprint),
                        kind => {
                            reg.upsert(
                                Peer {
                                    info: packet.info,
                                    addrs: vec![src.ip()],
                                    port: packet.port,
                                },
                                PeerSource::Udp,
                            );
                            // Answer an announcement by unicast so the
                            // newcomer sees us right away
                            if kind == AnnounceKind::Announce {
                                let response = read_packets(&packets).response.clone();
                                if !response.is_empty() {
                                    let _ = sock.send_to(&response, src).await;
                                }
                            }
                        }
                    }
                }
            }
        }
    }));

    Ok(socket)
}

/// Whether multicast reception has gone silent: it worked before and nothing
/// arrived within `threshold`
///
/// Never having received anything is not silence: on an empty network in
/// passive mode there is simply nothing to hear.
fn multicast_silent(last_seen: Option<Instant>, threshold: Duration) -> bool {
    last_seen.is_some_and(|t| t.elapsed() >= threshold)
}

/// Rebuild the multicast membership and announce at once (unless passive)
///
/// Leave before joining, since joining over stale socket state can fail; a
/// failing leave is expected when the membership vanished together with the
/// interface. The announcement makes us visible sooner, and its loopback
/// refreshes the pulse, confirming the repair before the next check.
async fn rejoin_multicast(udp: &UdpSocket, target: (Ipv4Addr, u16), packets: &SharedPackets) {
    let _ = udp.leave_multicast_v4(target.0, Ipv4Addr::UNSPECIFIED);
    if let Err(e) = udp.join_multicast_v4(target.0, Ipv4Addr::UNSPECIFIED) {
        tracing::warn!("failed to rejoin the multicast group, retrying next cycle: {e}");
        return;
    }
    let announce = read_packets(packets).announce.clone();
    if !announce.is_empty() {
        let _ = udp.send_to(&announce, target).await;
    }
}

/// Multicast membership watchdog: repairs two kinds of silent breakage
///
/// Systems can drop the IGMP membership without any error, after which no
/// multicast arrives at all, not even our own loopback:
///
/// 1. **Sleep**: the monotonic clock stalls while the wall clock keeps
///    running; the difference is the stall, and a large one means we woke
///    up (NTP corrections are far smaller). Windows counts sleep in its
///    monotonic clock, so the second path covers it there.
/// 2. **Network reconnection**: an interface going down and up can clear the
///    membership without any clock stall. If multicast worked before and has
///    been silent past `silence_timeout`, the receive path is rebuilt every
///    cycle until traffic returns; one attempt is not enough while the
///    interface is still down.
///
/// The mDNS daemon watches interfaces and recovers by itself.
pub(super) async fn membership_watchdog(
    udp: Arc<UdpSocket>,
    target: (Ipv4Addr, u16),
    packets: SharedPackets,
    pulse: MulticastPulse,
    silence_timeout: Duration,
) {
    /// Check period
    const TICK: Duration = Duration::from_secs(30);
    /// A clock stall beyond this means we resumed from sleep
    const STALL_JUMP: Duration = Duration::from_secs(60);
    let mut wall = std::time::SystemTime::now();
    let mut mono = Instant::now();
    let mut tick = tokio::time::interval(TICK);
    // Log the first silent cycle and the recovery at info, retries at debug:
    // in passive mode on an empty network recovery can never be proven, and
    // that must not flood the log
    let mut silence_logged = false;
    loop {
        tick.tick().await;
        let wall_gap = std::time::SystemTime::now()
            .duration_since(wall)
            .unwrap_or_default();
        let mono_gap = mono.elapsed();
        wall = std::time::SystemTime::now();
        mono = Instant::now();
        let stalled = wall_gap.saturating_sub(mono_gap);
        let silent = multicast_silent(read_pulse(&pulse), silence_timeout);
        if stalled >= STALL_JUMP {
            tracing::info!(
                stalled_secs = stalled.as_secs(),
                "resumed from sleep, rebuilding the multicast membership"
            );
        } else if silent && !silence_logged {
            silence_logged = true;
            tracing::info!("multicast has gone silent, rebuilding the membership");
        } else if silent {
            tracing::debug!("multicast still silent, rebuilding the membership again");
        } else {
            if silence_logged {
                silence_logged = false;
                tracing::info!("multicast reception recovered");
            }
            continue;
        }
        rejoin_multicast(&udp, target, &packets).await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Peer info for the packet tests
    fn info() -> PeerInfo {
        PeerInfo {
            device_id: "d".into(),
            name: "n".into(),
            fingerprint: "c".repeat(64),
            platform: "windows".into(),
            os_version: None,
            props: BTreeMap::from([("pv".to_string(), "1.0".to_string())]),
        }
    }

    /// Packets roundtrip, and the port travels under the sibling apps'
    /// `tcp_port` key
    #[test]
    fn packet_roundtrip_keeps_wire_name() {
        let packets = UdpPackets::encode(&info(), 42624, false);
        let json = String::from_utf8(packets.announce.clone()).unwrap();
        assert!(json.contains(r#""tcp_port":42624"#));
        let back: AnnouncePacket = serde_json::from_slice(&packets.announce).unwrap();
        assert_eq!(back.kind, AnnounceKind::Announce);
        assert_eq!(back.port, 42624);
        assert_eq!(back.info, info());
        let bye: AnnouncePacket = serde_json::from_slice(&packets.goodbye).unwrap();
        assert_eq!(bye.kind, AnnounceKind::Goodbye);
    }

    /// A packet from an older sibling release (no props, no os_version)
    /// still parses
    #[test]
    fn legacy_packet_parses() {
        let legacy = r#"{"kind":"announce","info":{"device_id":"d","name":"n","fingerprint":"f","platform":"macos"},"tcp_port":42524}"#;
        let packet: AnnouncePacket = serde_json::from_str(legacy).unwrap();
        assert_eq!(packet.port, 42524);
        assert!(packet.info.props.is_empty());
    }

    /// Passive mode encodes nothing to send
    #[test]
    fn passive_packets_are_empty() {
        let packets = UdpPackets::encode(&info(), 42624, true);
        assert!(packets.announce.is_empty());
        assert!(packets.response.is_empty());
        assert!(packets.goodbye.is_empty());
    }

    /// Never having heard anything is not silence
    #[test]
    fn multicast_silent_requires_prior_reception() {
        assert!(!multicast_silent(None, Duration::ZERO));
    }

    /// A fresh pulse is not silent; an old one is
    #[test]
    fn multicast_silent_after_threshold() {
        assert!(!multicast_silent(
            Some(Instant::now()),
            Duration::from_secs(3600)
        ));
        assert!(multicast_silent(Some(Instant::now()), Duration::ZERO));
    }

    /// The pulse records arrivals
    #[test]
    fn pulse_mark_roundtrip() {
        let pulse: MulticastPulse = Arc::new(Mutex::new(None));
        assert!(read_pulse(&pulse).is_none());
        mark_pulse(&pulse);
        assert!(read_pulse(&pulse).is_some());
    }
}
