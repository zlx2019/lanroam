//! lanroam-core: the LAN keyboard and mouse sharing engine.
//!
//! A pure library with no UI, shared by the CLI (protocol debugging and
//! integration tests) and the desktop app. Layering, see `docs/design.md`:
//!
//! ```text
//! ┌─ node      ─ wiring: identity + transport + discovery        (M0)
//! ├─ transport ─ QUIC per peer: fingerprint-pinned mutual TLS 1.3,
//! │              the Hello gate, identity probes for discovery  (M0)
//! ├─ protocol  ─ control messages and datagram codec            (M0)
//! ├─ diag      ─ round-trip measurement on both paths            (M0)
//! ├─ session   ─ input sessions: source forwards, target replays  (M1)
//! │              control arbitration                             (M2)
//! ├─ group     ─ desk group: signed membership, join / kick       (M2)
//! ├─ layout    ─ monitor arrangement shared by the group         (M2)
//! ├─ clipboard ─ clipboard hand-off on entering a device          (M4)
//! └─ dnd       ─ cross-device file drag and drop                  (M5)
//! ```
//!
//! Discovery, device identity, mutual TLS and framing come from [`lan_kit`],
//! the foundation shared with the sibling apps; keyboard and mouse capture,
//! injection and edge switching from [`lanroam_input`].

use std::net::Ipv4Addr;

use lan_kit::AppProfile;

pub mod diag;
pub mod node;
pub mod protocol;
pub mod session;
pub mod transport;

#[cfg(test)]
mod test_util;

pub use lan_kit;
pub use lanroam_input;

/// Lanroam's constants on the LAN
pub const PROFILE: AppProfile = AppProfile {
    product: "Lanroam",
    server_name: "lanroam",
    // QUIC runs over UDP, hence `_udp`
    mdns_service_type: "_lanroam._udp.local.",
    // Deskmate uses .168 and Lanecho .169
    multicast_group: Ipv4Addr::new(224, 0, 0, 170),
};

/// Default QUIC port (UDP)
pub const DEFAULT_PORT: u16 = 42624;

/// Default UDP multicast discovery port
pub const DEFAULT_DISCOVERY_PORT: u16 = 42625;

#[cfg(test)]
mod tests {
    use super::*;

    /// The TLS server name must parse as a DNS name, or every dial fails
    #[test]
    fn server_name_is_valid() {
        assert!(rustls_pki_types::ServerName::try_from(PROFILE.server_name).is_ok());
    }

    /// The service type follows DNS-SD naming and matches the transport
    #[test]
    fn service_type_is_well_formed() {
        let ty = PROFILE.mdns_service_type;
        assert!(ty.starts_with('_') && ty.ends_with("._udp.local."));
    }

    /// Ports must not collide
    #[test]
    fn default_ports_distinct() {
        assert_ne!(DEFAULT_PORT, DEFAULT_DISCOVERY_PORT);
    }
}
