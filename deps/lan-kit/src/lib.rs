//! lan-kit: the LAN foundation shared by Lanroam and its sibling apps
//! (Deskmate, Lanecho).
//!
//! Everything here is product-agnostic. An app plugs in its own
//! [`AppProfile`], which keeps sibling apps on the same LAN from ever seeing
//! each other's discovery traffic. Layering:
//!
//! ```text
//! ┌─ profile   ─ per-app constants: service type, multicast group, names
//! ├─ identity  ─ device identity: UUID + BLAKE3 fingerprint of a self-signed cert
//! ├─ tls       ─ mutual TLS 1.3: fingerprint pinning, no CA chain
//! ├─ discovery ─ peer discovery: mDNS primary, UDP multicast fallback, and a
//! │              pluggable identity probe that settles whether a peer crashed
//! └─ frame     ─ length-prefixed JSON frames over any async byte stream
//! ```
//!
//! The transport itself is left to the app: the TLS configs work with
//! tokio-rustls over TCP as well as with QUIC.

pub mod discovery;
pub mod frame;
pub mod identity;
pub mod profile;
pub mod tls;

pub use discovery::{DiscoveryService, Peer, PeerEvent};
pub use identity::{DeviceIdentity, PeerInfo};
pub use profile::AppProfile;
