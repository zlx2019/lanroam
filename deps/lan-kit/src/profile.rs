//! Per-application constants.
//!
//! Sibling apps built on lan-kit share one LAN; the profile keeps their
//! discovery traffic and certificates apart. Every field is a compile-time
//! constant of the app, so a profile is normally declared as a `const`.

use std::net::Ipv4Addr;

/// Constants that identify one application on the LAN
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppProfile {
    /// Product name, used as the display name when the hostname is unavailable
    pub product: &'static str,
    /// Name written into certificates and passed as the TLS server name. It is
    /// never verified (trust rests on fingerprints) but must be a valid DNS
    /// name
    pub server_name: &'static str,
    /// DNS-SD service type, e.g. `_lanroam._udp.local.`; the protocol label
    /// should match the transport peers dial (`_tcp` or `_udp`)
    pub mdns_service_type: &'static str,
    /// Multicast group of the UDP fallback discovery channel. Give every app
    /// its own group inside 224.0.0.0/24, the range routers handle best
    pub multicast_group: Ipv4Addr,
}
