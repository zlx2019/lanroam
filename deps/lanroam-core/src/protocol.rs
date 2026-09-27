//! Wire protocol between Lanroam nodes.
//!
//! One QUIC connection per pair of nodes, carrying:
//! - **Control stream**: the first bidirectional stream, opened by the
//!   dialer. Length-prefixed JSON [`Control`] messages, reliable and
//!   ordered. It opens with the Hello gate:
//!   `Hello → HelloAck | Rejected`
//! - **Datagrams**: unreliable and unordered, for high-frequency data where
//!   only the latest value matters (pointer motion, from M1). A compact
//!   binary [`Datagram`] codec; the first byte is the kind
//!
//! The protocol version is `major.minor`; a different major refuses the
//! connection, a newer minor only adds messages an older peer may ignore.

use bytes::{BufMut, Bytes, BytesMut};
use lan_kit::PeerInfo;
use lan_kit::frame::Framing;
use serde::{Deserialize, Serialize};

/// Protocol version (major.minor), checked by the Hello gate
pub const PROTOCOL_VERSION: &str = "1.0";

/// ALPN of the QUIC connections; a client speaking anything else is refused
/// during the TLS handshake
pub const ALPN: &[u8] = b"lanroam/1";

/// Frame codec of the control stream (control messages are small; 64 KiB is
/// ample)
pub const FRAMING: Framing = Framing::new(64 * 1024);

/// Discovery prop advertising the protocol version, so a scan can flag an
/// incompatible node before dialing it
pub const PROP_PROTOCOL: &str = "pv";

/// Structured rejection codes: the dialer renders them in its own language
pub mod reason_code {
    /// The identity declared in Hello does not match the TLS certificate
    pub const IDENTITY_MISMATCH: &str = "identity_mismatch";
    /// The protocol major versions differ
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    /// The first message was not a Hello
    pub const PROTOCOL_VIOLATION: &str = "protocol_violation";
}

/// Control stream message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    /// Opens the control stream (dialer → acceptor)
    Hello {
        /// Protocol version (major.minor)
        version: String,
        /// Dialer's device info
        info: PeerInfo,
    },
    /// Accepts the Hello (acceptor → dialer)
    HelloAck {
        /// Protocol version (major.minor)
        version: String,
        /// Acceptor's device info
        info: PeerInfo,
    },
    /// Refuses the connection; the acceptor closes it right after
    Rejected {
        /// Structured reason (see [`reason_code`])
        reason_code: String,
    },
    /// Round-trip probe on the reliable path
    Ping {
        /// Sequence number
        seq: u32,
        /// Sender's clock in microseconds, echoed back untouched
        sent_us: u64,
    },
    /// Answer to [`Control::Ping`]
    Pong {
        /// Echoed sequence number
        seq: u32,
        /// Echoed sender clock
        sent_us: u64,
    },
}

impl Control {
    /// Short name of the message type (logs and errors)
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::HelloAck { .. } => "hello_ack",
            Self::Rejected { .. } => "rejected",
            Self::Ping { .. } => "ping",
            Self::Pong { .. } => "pong",
        }
    }
}

/// Whether a peer's protocol version is compatible (same major)
pub fn version_compatible(peer_version: &str) -> bool {
    major_of(peer_version) == major_of(PROTOCOL_VERSION)
}

/// Major segment of a version string
fn major_of(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

/// Datagram message
///
/// Layout: `kind: u8` followed by the kind's fields in big-endian. Unknown
/// kinds decode to `None` and are dropped, so a newer minor version can add
/// kinds without breaking older peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Datagram {
    /// Round-trip probe on the unreliable path
    Ping {
        /// Sequence number
        seq: u32,
        /// Sender's clock in microseconds, echoed back untouched
        sent_us: u64,
    },
    /// Answer to [`Datagram::Ping`]
    Pong {
        /// Echoed sequence number
        seq: u32,
        /// Echoed sender clock
        sent_us: u64,
    },
}

/// Datagram kind tags
mod kind {
    /// [`super::Datagram::Ping`]
    pub(super) const PING: u8 = 1;
    /// [`super::Datagram::Pong`]
    pub(super) const PONG: u8 = 2;
}

impl Datagram {
    /// Encode into a datagram payload
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(13);
        match *self {
            Self::Ping { seq, sent_us } => {
                buf.put_u8(kind::PING);
                buf.put_u32(seq);
                buf.put_u64(sent_us);
            }
            Self::Pong { seq, sent_us } => {
                buf.put_u8(kind::PONG);
                buf.put_u32(seq);
                buf.put_u64(sent_us);
            }
        }
        buf.freeze()
    }

    /// Decode a datagram payload; `None` for unknown kinds or bad lengths
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let (&kind, rest) = buf.split_first()?;
        match kind {
            kind::PING | kind::PONG => {
                let rest: &[u8; 12] = rest.try_into().ok()?;
                let (seq, sent_us) = rest.split_at(4);
                let seq = u32::from_be_bytes(seq.try_into().ok()?);
                let sent_us = u64::from_be_bytes(sent_us.try_into().ok()?);
                Some(if kind == kind::PING {
                    Self::Ping { seq, sent_us }
                } else {
                    Self::Pong { seq, sent_us }
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Device info for the tests
    fn info() -> PeerInfo {
        PeerInfo {
            device_id: "d1".into(),
            name: "Desk PC".into(),
            fingerprint: "f".repeat(64),
            platform: "windows".into(),
            os_version: Some("Windows 11".into()),
            props: BTreeMap::from([(PROP_PROTOCOL.to_string(), PROTOCOL_VERSION.to_string())]),
        }
    }

    /// Every control message survives the frame codec
    #[tokio::test]
    async fn control_roundtrip() {
        let samples = [
            Control::Hello {
                version: PROTOCOL_VERSION.into(),
                info: info(),
            },
            Control::HelloAck {
                version: PROTOCOL_VERSION.into(),
                info: info(),
            },
            Control::Rejected {
                reason_code: reason_code::IDENTITY_MISMATCH.into(),
            },
            Control::Ping {
                seq: 7,
                sent_us: 123_456,
            },
            Control::Pong {
                seq: 7,
                sent_us: 123_456,
            },
        ];
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        for msg in &samples {
            FRAMING.write(&mut a, msg).await.unwrap();
            let got: Control = FRAMING.read(&mut b).await.unwrap();
            assert_eq!(&got, msg);
        }
    }

    /// Same major is compatible, a different one is not
    #[test]
    fn version_compat() {
        assert!(version_compatible("1.0"));
        assert!(version_compatible("1.9"));
        assert!(!version_compatible("2.0"));
        assert!(!version_compatible("garbage"));
    }

    /// Datagrams roundtrip; truncated, padded and unknown payloads are
    /// dropped
    #[test]
    fn datagram_codec() {
        for dg in [
            Datagram::Ping {
                seq: 1,
                sent_us: u64::MAX,
            },
            Datagram::Pong {
                seq: u32::MAX,
                sent_us: 0,
            },
        ] {
            let bytes = dg.encode();
            assert_eq!(bytes.len(), 13);
            assert_eq!(Datagram::decode(&bytes), Some(dg));
        }
        let ping = Datagram::Ping { seq: 1, sent_us: 2 }.encode();
        assert_eq!(Datagram::decode(&ping[..12]), None);
        let mut padded = ping.to_vec();
        padded.push(0);
        assert_eq!(Datagram::decode(&padded), None);
        assert_eq!(Datagram::decode(&[0xff, 0, 0]), None);
        assert_eq!(Datagram::decode(&[]), None);
    }
}
