//! Terminal formatting helpers.

use std::time::Duration;

use lanroam_core::diag::RttStats;
use lanroam_core::lan_kit::{Peer, PeerInfo};
use lanroam_core::protocol::PROP_PROTOCOL;

/// Leading part of a fingerprint, enough to tell devices apart by eye
pub(crate) fn short_fp(fingerprint: &str) -> &str {
    fingerprint.get(..12).unwrap_or(fingerprint)
}

/// One-line description of a device
pub(crate) fn describe(info: &PeerInfo) -> String {
    let os = info.os_version.as_deref().unwrap_or(&info.platform);
    let protocol = info
        .props
        .get(PROP_PROTOCOL)
        .map(|v| format!(" protocol {v}"))
        .unwrap_or_default();
    format!(
        "{} [{}] ({os}){protocol}",
        info.name,
        short_fp(&info.fingerprint)
    )
}

/// One-line description of a discovered node, with its addresses
pub(crate) fn describe_peer(peer: &Peer) -> String {
    let addrs: Vec<String> = peer.socket_addrs().map(|a| a.to_string()).collect();
    format!("{}  {}", describe(&peer.info), addrs.join(", "))
}

/// Milliseconds with two decimals
fn ms(d: Duration) -> String {
    format!("{:.2}ms", d.as_secs_f64() * 1000.0)
}

/// Print one path's round-trip statistics
pub(crate) fn print_stats(label: &str, stats: &RttStats) {
    let received = stats.samples.len();
    let loss = stats.loss() * 100.0;
    let (Some(min), Some(mean), Some(p50), Some(p99), Some(max)) = (
        stats.min(),
        stats.mean(),
        stats.percentile(50.0),
        stats.percentile(99.0),
        stats.max(),
    ) else {
        println!(
            "{label}: {received}/{} answered ({loss:.1}% loss)",
            stats.sent
        );
        return;
    };
    println!(
        "{label}: {received}/{} answered ({loss:.1}% loss)  min {}  avg {}  p50 {}  p99 {}  max {}",
        stats.sent,
        ms(min),
        ms(mean),
        ms(p50),
        ms(p99),
        ms(max),
    );
}
