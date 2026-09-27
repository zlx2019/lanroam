//! QUIC transport: one endpoint per node serving both directions, mutual
//! TLS 1.3 pinned to certificate fingerprints, and the Hello gate on every
//! connection.
//!
//! Trust at this layer is purely cryptographic: a [`Link`] proves that the
//! remote holds the private key of the fingerprint it declares. Whether that
//! fingerprint may connect for the [`Purpose`] it declares is the desk
//! group's decision, which the acceptor makes through the admission check
//! it passes to [`Incoming::handshake`].

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use lan_kit::discovery::{IdentityProbe, ProbeFuture};
use lan_kit::{DeviceIdentity, Peer, PeerInfo};
use quinn::crypto::rustls::{NoInitialCipherSuite, QuicClientConfig, QuicServerConfig};
use rustls_pki_types::CertificateDer;
use thiserror::Error;

use crate::PROFILE;
use crate::protocol::{
    ALPN, Control, FRAMING, PROTOCOL_VERSION, Purpose, reason_code, version_compatible,
};

/// Budget per candidate address for the QUIC + TLS handshake
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Budget for the Hello gate after the QUIC handshake; blocks connections
/// that squat without ever saying Hello
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Keep-alive period: keeps the path warm and lets an idle link notice a
/// vanished peer
const KEEP_ALIVE: Duration = Duration::from_secs(2);

/// A link with no traffic for this long is dead (five missed keep-alives).
/// Short on purpose: input must never be sent into a void for long
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a rejection may take to reach the dialer before the connection
/// is closed anyway
const REJECT_LINGER: Duration = Duration::from_secs(1);

/// Application close codes
pub mod close_code {
    use quinn::VarInt;

    /// Normal shutdown
    pub const NORMAL: VarInt = VarInt::from_u32(0);
    /// The peer broke the protocol
    pub const PROTOCOL: VarInt = VarInt::from_u32(1);
    /// The Hello gate refused the peer
    pub const REJECTED: VarInt = VarInt::from_u32(2);
    /// An identity probe is done (no session was ever intended)
    pub const PROBE: VarInt = VarInt::from_u32(3);
    /// Another link to the same peer is kept instead of this one
    pub const DUPLICATE: VarInt = VarInt::from_u32(4);
}

/// Transport errors
#[derive(Debug, Error)]
pub enum TransportError {
    /// Building the TLS config failed
    #[error("TLS config: {0}")]
    Tls(#[from] lan_kit::tls::TlsError),
    /// The TLS config is unusable for QUIC
    #[error("QUIC crypto config: {0}")]
    Crypto(#[from] NoInitialCipherSuite),
    /// Binding the UDP socket failed (port taken, ...)
    #[error("socket error: {0}")]
    Io(#[from] std::io::Error),
    /// No candidate address completed a handshake
    #[error("peer unreachable at every candidate address")]
    Unreachable,
    /// The QUIC connection failed or was closed
    #[error("connection error: {0}")]
    Connection(#[from] quinn::ConnectionError),
    /// Reading or writing the control stream failed
    #[error("control stream error: {0}")]
    Frame(#[from] lan_kit::frame::FrameError),
    /// A datagram could not be sent (unsupported by the peer, too large,
    /// connection lost)
    #[error("datagram error: {0}")]
    Datagram(#[from] quinn::SendDatagramError),
    /// The peer did not complete a step in time
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    /// The peer refused the connection (a [`reason_code`])
    #[error("rejected by peer: {0}")]
    Rejected(String),
    /// This side refused the peer in the admission check (a
    /// [`reason_code`])
    #[error("refused the peer: {0}")]
    Refused(&'static str),
    /// The protocol major versions differ
    #[error("incompatible protocol version: peer {peer}, local {PROTOCOL_VERSION}")]
    Version {
        /// Peer's version
        peer: String,
    },
    /// The declared identity does not match the certificate, or no client
    /// certificate arrived
    #[error("declared identity does not match the certificate")]
    IdentityMismatch,
    /// A message arrived out of order
    #[error("unexpected message: expected {expected}, got {got}")]
    Unexpected {
        /// Expected message type
        expected: &'static str,
        /// Received message type
        got: &'static str,
    },
}

impl TransportError {
    /// Whether the remote hung up because it was only probing our identity
    /// (discovery liveness check), which is routine rather than a failure
    pub fn is_probe(&self) -> bool {
        matches!(
            self,
            Self::Connection(quinn::ConnectionError::ApplicationClosed(close))
                if close.error_code == close_code::PROBE
        )
    }
}

/// Transport parameters shared by both directions
fn transport_config() -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    config.keep_alive_interval(Some(KEEP_ALIVE));
    // Infallible: 10s is far below QUIC's idle timeout limit (2^62 ms)
    config.max_idle_timeout(quinn::IdleTimeout::try_from(IDLE_TIMEOUT).ok());
    Arc::new(config)
}

/// A node's QUIC endpoint: accepts peers and dials them over one UDP socket
pub struct Transport {
    /// The endpoint (server config set, so it accepts as well as dials)
    endpoint: quinn::Endpoint,
    /// This device's identity
    identity: Arc<DeviceIdentity>,
    /// Shared transport parameters
    transport_config: Arc<quinn::TransportConfig>,
    /// Client config that accepts any certificate, for identity probes and
    /// direct connections; built once since probes recur
    unpinned: quinn::ClientConfig,
}

impl Transport {
    /// Bind the endpoint on `port` (0 picks a free one) on all IPv4
    /// interfaces
    pub fn bind(identity: Arc<DeviceIdentity>, port: u16) -> Result<Self, TransportError> {
        let transport_config = transport_config();
        let mut tls = lan_kit::tls::server_config(&identity)?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let mut server =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
        server.transport_config(Arc::clone(&transport_config));
        let endpoint =
            quinn::Endpoint::server(server, SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))?;
        let unpinned = client_config(&identity, None, &transport_config)?;
        Ok(Self {
            endpoint,
            identity,
            transport_config,
            unpinned,
        })
    }

    /// Port the endpoint is bound to
    pub fn local_port(&self) -> u16 {
        self.endpoint
            .local_addr()
            .map(|addr| addr.port())
            .unwrap_or_default()
    }

    /// This device's identity
    pub fn identity(&self) -> &Arc<DeviceIdentity> {
        &self.identity
    }

    /// Dial a discovered peer for `purpose`: try each candidate address with
    /// TLS pinned to the peer's fingerprint, then pass the Hello gate
    pub async fn connect(
        &self,
        peer: &Peer,
        local: &PeerInfo,
        purpose: Purpose,
    ) -> Result<Link, TransportError> {
        let config = client_config(
            &self.identity,
            Some(peer.info.fingerprint.clone()),
            &self.transport_config,
        )?;
        let conn = self
            .dial_any(&config, peer.socket_addrs())
            .await
            .ok_or(TransportError::Unreachable)?;
        hello_out(conn, local, purpose, Some(&peer.info.fingerprint)).await
    }

    /// Dial an address directly, accepting whatever certificate it presents
    ///
    /// For debugging on networks where discovery is blocked: nothing is
    /// pinned, so the caller must show [`Link::remote`]'s fingerprint to the
    /// user before trusting it.
    pub async fn connect_direct(
        &self,
        addr: SocketAddr,
        local: &PeerInfo,
        purpose: Purpose,
    ) -> Result<Link, TransportError> {
        let conn = self
            .dial_any(&self.unpinned, std::iter::once(addr))
            .await
            .ok_or(TransportError::Unreachable)?;
        hello_out(conn, local, purpose, None).await
    }

    /// Try addresses in order; the first completed QUIC handshake wins
    async fn dial_any(
        &self,
        config: &quinn::ClientConfig,
        addrs: impl Iterator<Item = SocketAddr>,
    ) -> Option<quinn::Connection> {
        for addr in addrs {
            // The endpoint is IPv4-only, so IPv6 candidates are refused here
            let connecting =
                match self
                    .endpoint
                    .connect_with(config.clone(), addr, PROFILE.server_name)
                {
                    Ok(connecting) => connecting,
                    Err(e) => {
                        tracing::debug!(%addr, "cannot dial this address: {e}");
                        continue;
                    }
                };
            match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
                Ok(Ok(conn)) => return Some(conn),
                Ok(Err(e)) => {
                    tracing::debug!(%addr, "handshake failed, trying the next address: {e}")
                }
                Err(_) => tracing::debug!(%addr, "handshake timed out, trying the next address"),
            }
        }
        None
    }

    /// Wait for the next incoming connection; None once the endpoint is
    /// closed
    ///
    /// Run the handshake of each [`Incoming`] in its own task, so one slow
    /// peer cannot hold up the others.
    pub async fn accept(&self) -> Option<Incoming> {
        self.endpoint.accept().await.map(|inner| Incoming { inner })
    }

    /// Close every connection and stop accepting
    pub fn close(&self) {
        self.endpoint.close(close_code::NORMAL, b"shutdown");
    }

    /// Wait until closed connections have told their peers
    pub async fn wait_idle(&self) {
        self.endpoint.wait_idle().await;
    }
}

impl IdentityProbe for Transport {
    /// Complete a QUIC + TLS handshake with `addr` and report the
    /// certificate fingerprint presented there, then hang up without a
    /// session
    ///
    /// Bounded by [`CONNECT_TIMEOUT`] on its own: QUIC ignores ICMP port
    /// unreachable, so a closed port would otherwise hang until the idle
    /// timeout.
    fn probe(&self, addr: SocketAddr) -> ProbeFuture<'_> {
        Box::pin(async move {
            let connecting = self
                .endpoint
                .connect_with(self.unpinned.clone(), addr, PROFILE.server_name)
                .ok()?;
            let conn = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
                .await
                .ok()?
                .ok()?;
            let fingerprint = cert_fingerprint(&conn);
            conn.close(close_code::PROBE, b"probe");
            fingerprint
        })
    }
}

/// Build a QUIC client config (`None` accepts any certificate)
fn client_config(
    identity: &DeviceIdentity,
    expected_fingerprint: Option<String>,
    transport: &Arc<quinn::TransportConfig>,
) -> Result<quinn::ClientConfig, TransportError> {
    let mut tls = lan_kit::tls::client_config(identity, expected_fingerprint)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls)?));
    config.transport_config(Arc::clone(transport));
    Ok(config)
}

/// Fingerprint of the certificate the remote presented in the TLS handshake
fn cert_fingerprint(conn: &quinn::Connection) -> Option<String> {
    let certs = conn
        .peer_identity()?
        .downcast::<Vec<CertificateDer<'static>>>()
        .ok()?;
    lan_kit::tls::peer_fingerprint(Some(&certs))
}

/// Dialer side of the Hello gate: open the control stream, say Hello, check
/// the HelloAck
///
/// With `expected` set, TLS already pinned the certificate; the check here
/// additionally ties the declared identity to it. Without it (direct
/// connections), the declaration must match whatever certificate was shown.
async fn hello_out(
    conn: quinn::Connection,
    local: &PeerInfo,
    purpose: Purpose,
    expected: Option<&str>,
) -> Result<Link, TransportError> {
    let gate = async {
        let cert_fp = cert_fingerprint(&conn).ok_or(TransportError::IdentityMismatch)?;
        let (mut send, mut recv) = conn.open_bi().await?;
        FRAMING
            .write(
                &mut send,
                &Control::Hello {
                    version: PROTOCOL_VERSION.to_string(),
                    info: local.clone(),
                    purpose,
                },
            )
            .await?;
        let (version, info) = match FRAMING.read(&mut recv).await? {
            Control::HelloAck { version, info } => (version, info),
            Control::Rejected { reason_code } => return Err(TransportError::Rejected(reason_code)),
            other => {
                return Err(TransportError::Unexpected {
                    expected: "hello_ack",
                    got: other.kind(),
                });
            }
        };
        if !version_compatible(&version) {
            return Err(TransportError::Version { peer: version });
        }
        if info.fingerprint != cert_fp || expected.is_some_and(|fp| fp != cert_fp) {
            return Err(TransportError::IdentityMismatch);
        }
        Ok((send, recv, info))
    };
    match tokio::time::timeout(HELLO_TIMEOUT, gate).await {
        Ok(Ok((send, recv, remote))) => Ok(Link {
            remote,
            purpose,
            conn,
            control_tx: send,
            control_rx: recv,
        }),
        Ok(Err(e)) => {
            conn.close(close_code::PROTOCOL, b"hello failed");
            Err(e)
        }
        Err(_) => {
            conn.close(close_code::PROTOCOL, b"hello timed out");
            Err(TransportError::Timeout("hello_ack"))
        }
    }
}

/// An inbound connection that has not passed the handshake yet
pub struct Incoming {
    /// The pending QUIC connection
    inner: quinn::Incoming,
}

impl Incoming {
    /// Where the connection comes from
    pub fn remote_address(&self) -> SocketAddr {
        self.inner.remote_address()
    }

    /// Complete TLS and the acceptor side of the Hello gate, answering with
    /// `local`
    ///
    /// The declared identity must match the client certificate, which is
    /// what blocks impersonation: a node can only claim the fingerprint whose
    /// private key it holds. `admit` then decides whether that verified
    /// identity may connect for the purpose it declares; a refusal is a
    /// [`reason_code`] the dialer gets to see.
    pub async fn handshake(
        self,
        local: &PeerInfo,
        admit: impl FnOnce(&PeerInfo, Purpose) -> Result<(), &'static str>,
    ) -> Result<Link, TransportError> {
        let gate = async {
            let conn = self.inner.await?;
            let result = hello_in(&conn, local, admit).await;
            if result.is_err() {
                conn.close(close_code::PROTOCOL, b"hello failed");
            }
            result.map(|(send, recv, remote, purpose)| Link {
                remote,
                purpose,
                conn,
                control_tx: send,
                control_rx: recv,
            })
        };
        tokio::time::timeout(HELLO_TIMEOUT, gate)
            .await
            .map_err(|_| TransportError::Timeout("hello"))?
    }
}

/// Acceptor side of the Hello gate on an established connection
async fn hello_in(
    conn: &quinn::Connection,
    local: &PeerInfo,
    admit: impl FnOnce(&PeerInfo, Purpose) -> Result<(), &'static str>,
) -> Result<(quinn::SendStream, quinn::RecvStream, PeerInfo, Purpose), TransportError> {
    let cert_fp = cert_fingerprint(conn).ok_or(TransportError::IdentityMismatch)?;
    let (mut send, mut recv) = conn.accept_bi().await?;
    let (version, info, purpose) = match FRAMING.read(&mut recv).await? {
        Control::Hello {
            version,
            info,
            purpose,
        } => (version, info, purpose),
        other => {
            reject(conn, &mut send, reason_code::PROTOCOL_VIOLATION).await;
            return Err(TransportError::Unexpected {
                expected: "hello",
                got: other.kind(),
            });
        }
    };
    if !version_compatible(&version) {
        reject(conn, &mut send, reason_code::UNSUPPORTED_VERSION).await;
        return Err(TransportError::Version { peer: version });
    }
    if info.fingerprint != cert_fp {
        reject(conn, &mut send, reason_code::IDENTITY_MISMATCH).await;
        return Err(TransportError::IdentityMismatch);
    }
    if let Err(code) = admit(&info, purpose) {
        reject(conn, &mut send, code).await;
        return Err(TransportError::Refused(code));
    }
    FRAMING
        .write(
            &mut send,
            &Control::HelloAck {
                version: PROTOCOL_VERSION.to_string(),
                info: local.clone(),
            },
        )
        .await?;
    Ok((send, recv, info, purpose))
}

/// Tell the dialer why it is refused, give the message a moment to arrive,
/// then close
///
/// Closing at once could discard the unsent frame and leave the dialer with
/// a bare connection error instead of the reason.
async fn reject(conn: &quinn::Connection, send: &mut quinn::SendStream, code: &str) {
    let refusal = Control::Rejected {
        reason_code: code.to_string(),
    };
    if FRAMING.write(send, &refusal).await.is_ok() && send.finish().is_ok() {
        let _ = tokio::time::timeout(REJECT_LINGER, send.stopped()).await;
    }
    conn.close(close_code::REJECTED, code.as_bytes());
}

/// An authenticated connection to one peer (Hello gate passed)
pub struct Link {
    /// The remote's device info, as declared in the Hello gate and bound to
    /// its certificate
    remote: PeerInfo,
    /// What the connection is for
    purpose: Purpose,
    /// The QUIC connection
    conn: quinn::Connection,
    /// Sending half of the control stream
    control_tx: quinn::SendStream,
    /// Receiving half of the control stream
    control_rx: quinn::RecvStream,
}

impl Link {
    /// The remote's device info (its fingerprint is verified)
    pub fn remote(&self) -> &PeerInfo {
        &self.remote
    }

    /// What the connection is for, as the dialer declared it
    pub fn purpose(&self) -> Purpose {
        self.purpose
    }

    /// The remote's current address
    pub fn remote_address(&self) -> SocketAddr {
        self.conn.remote_address()
    }

    /// QUIC's smoothed round-trip estimate
    pub fn rtt(&self) -> Duration {
        self.conn.rtt()
    }

    /// The underlying connection, for datagrams and extra streams (a cheap
    /// handle to clone)
    pub fn connection(&self) -> &quinn::Connection {
        &self.conn
    }

    /// Send one control message
    pub async fn send(&mut self, msg: &Control) -> Result<(), TransportError> {
        Ok(FRAMING.write(&mut self.control_tx, msg).await?)
    }

    /// Receive the next control message
    ///
    /// Not cancel-safe: a frame half read when the future is dropped is lost
    /// and the stream desynchronizes. Read the control stream from a single
    /// task (see [`Self::into_parts`]) rather than racing it in `select!`.
    pub async fn recv(&mut self) -> Result<Control, TransportError> {
        Ok(FRAMING.read(&mut self.control_rx).await?)
    }

    /// Close the connection normally
    pub fn close(&self) {
        self.conn.close(close_code::NORMAL, b"bye");
    }

    /// Close once the messages already sent have reached the peer (bounded
    /// wait), so a final message is not discarded by the close
    pub async fn close_after_flush(mut self) {
        if self.control_tx.finish().is_ok() {
            let _ = tokio::time::timeout(REJECT_LINGER, self.control_tx.stopped()).await;
        }
        self.close();
    }

    /// Split into the remote info, the connection and the two control stream
    /// halves, so reading and writing can live in different tasks
    pub fn into_parts(
        self,
    ) -> (
        PeerInfo,
        quinn::Connection,
        quinn::SendStream,
        quinn::RecvStream,
    ) {
        (self.remote, self.conn, self.control_tx, self.control_rx)
    }
}

#[cfg(test)]
mod tests {
    use lan_kit::discovery::IdentityProbe;

    use super::*;
    use crate::test_util::TestNode;

    /// Both sides of a pinned connection learn each other's verified info,
    /// and the control stream carries messages both ways
    #[tokio::test]
    async fn pinned_handshake_and_control() {
        let (a, b) = (TestNode::new(), TestNode::new());
        let server = b.accept_one();
        let mut client_link = a
            .transport
            .connect(&b.as_peer(), &a.info, Purpose::Member)
            .await
            .unwrap();
        let mut server_link = server.await.unwrap().unwrap();

        assert_eq!(client_link.remote().fingerprint, b.info.fingerprint);
        assert_eq!(server_link.remote().fingerprint, a.info.fingerprint);

        client_link
            .send(&Control::Ping { seq: 1, sent_us: 9 })
            .await
            .unwrap();
        assert_eq!(
            server_link.recv().await.unwrap(),
            Control::Ping { seq: 1, sent_us: 9 }
        );
        server_link
            .send(&Control::Pong { seq: 1, sent_us: 9 })
            .await
            .unwrap();
        assert_eq!(
            client_link.recv().await.unwrap(),
            Control::Pong { seq: 1, sent_us: 9 }
        );
    }

    /// Datagrams flow both ways on an established link
    #[tokio::test]
    async fn datagrams_flow() {
        let (a, b) = (TestNode::new(), TestNode::new());
        let server = b.accept_one();
        let client_link = a
            .transport
            .connect(&b.as_peer(), &a.info, Purpose::Member)
            .await
            .unwrap();
        let server_link = server.await.unwrap().unwrap();

        let payload = crate::protocol::Datagram::Ping { seq: 5, sent_us: 6 }.encode();
        client_link
            .connection()
            .send_datagram(payload.clone())
            .unwrap();
        let got = tokio::time::timeout(
            Duration::from_secs(5),
            server_link.connection().read_datagram(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(got, payload);
    }

    /// A peer that answers with a different certificate than discovery
    /// promised is unreachable (the pin fails the TLS handshake)
    #[tokio::test]
    async fn wrong_pin_is_unreachable() {
        let (a, b, c) = (TestNode::new(), TestNode::new(), TestNode::new());
        // Serve b's handshake, but claim c's fingerprint for b's address
        let _server = b.accept_one();
        let mut peer = b.as_peer();
        peer.info.fingerprint = c.info.fingerprint.clone();
        assert!(matches!(
            a.transport.connect(&peer, &a.info, Purpose::Member).await,
            Err(TransportError::Unreachable)
        ));
    }

    /// Declaring someone else's fingerprint in Hello is refused with a reason
    /// the dialer gets to see
    #[tokio::test]
    async fn impersonation_is_rejected() {
        let (a, b, c) = (TestNode::new(), TestNode::new(), TestNode::new());
        let server = b.accept_one();
        // a holds its own key but claims to be c
        let mut forged = a.info.clone();
        forged.fingerprint = c.info.fingerprint.clone();
        let result = a
            .transport
            .connect(&b.as_peer(), &forged, Purpose::Member)
            .await;
        assert!(
            matches!(&result, Err(TransportError::Rejected(code)) if code == reason_code::IDENTITY_MISMATCH),
            "got {:?}",
            result.err()
        );
        assert!(matches!(
            server.await.unwrap(),
            Err(TransportError::IdentityMismatch)
        ));
    }

    /// A direct connection accepts any certificate and reports the verified
    /// fingerprint
    #[tokio::test]
    async fn direct_connection_reports_fingerprint() {
        let (a, b) = (TestNode::new(), TestNode::new());
        let server = b.accept_one();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, b.transport.local_port()));
        let link = a
            .transport
            .connect_direct(addr, &a.info, Purpose::Diag)
            .await
            .unwrap();
        assert_eq!(link.remote().fingerprint, b.info.fingerprint);
        server.await.unwrap().unwrap();
    }

    /// The identity probe reports the fingerprint of whoever listens at an
    /// address, and nothing for a closed port
    #[tokio::test]
    async fn probe_reports_identity() {
        let (a, b) = (TestNode::new(), TestNode::new());
        // The probed side must be accepting for the QUIC handshake to finish
        let _server = b.accept_one();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, b.transport.local_port()));
        assert_eq!(
            a.transport.probe(addr).await,
            Some(b.info.fingerprint.clone())
        );

        let closed = {
            let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            socket.local_addr().unwrap()
        };
        assert_eq!(a.transport.probe(closed).await, None);
    }
}
