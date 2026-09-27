//! mDNS channel: registration and browsing of the app's DNS-SD service.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use super::registry::{PeerSource, Registry, normalize_addrs};
use super::{DiscoveryError, Peer};
use crate::PeerInfo;

/// TXT keys owned by lan-kit. App props may not use them; DNS-SD compares
/// keys case-insensitively, and so does [`is_reserved`]
pub(super) const RESERVED_KEYS: [&str; 5] = ["id", "name", "fp", "platform", "osv"];

/// Whether a TXT key belongs to lan-kit
pub(super) fn is_reserved(key: &str) -> bool {
    RESERVED_KEYS.iter().any(|r| r.eq_ignore_ascii_case(key))
}

/// Build this device's service record (first registration and every update)
///
/// The instance name is the device ID, so it stays unique and does not change
/// with the display name; the host record reuses it to avoid clashing with
/// the machine's real hostname record.
pub(super) fn build_service(
    service_type: &str,
    info: &PeerInfo,
    port: u16,
) -> Result<mdns_sd::ServiceInfo, mdns_sd::Error> {
    // Props were validated against the reserved keys, so the fixed fields
    // below cannot be overwritten by them
    let mut props: HashMap<String, String> = info
        .props
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    props.insert("id".to_string(), info.device_id.clone());
    props.insert("name".to_string(), info.name.clone());
    props.insert("fp".to_string(), info.fingerprint.clone());
    props.insert("platform".to_string(), info.platform.clone());
    if let Some(osv) = &info.os_version {
        props.insert("osv".to_string(), osv.clone());
    }
    Ok(mdns_sd::ServiceInfo::new(
        service_type,
        &info.device_id,
        &format!("{}.local.", info.device_id),
        "",
        port,
        props,
    )?
    .enable_addr_auto())
}

/// Start the mDNS daemon: register our service (skipped when passive) and
/// spawn the browsing task
///
/// Returns the daemon and the registered fullname (None when passive).
pub(super) fn start_mdns(
    service_type: &'static str,
    info: &PeerInfo,
    port: u16,
    passive: bool,
    registry: &Arc<Registry>,
    peer_timeout: Duration,
    tasks: &mut Vec<JoinHandle<()>>,
) -> Result<(mdns_sd::ServiceDaemon, Option<String>), DiscoveryError> {
    let daemon = mdns_sd::ServiceDaemon::new()?;

    let fullname = if passive {
        None
    } else {
        let service = build_service(service_type, info, port)?;
        let name = service.get_fullname().to_string();
        daemon.register(service)?;
        Some(name)
    };

    let receiver = daemon.browse(service_type)?;
    let reg = Arc::clone(registry);
    tasks.push(tokio::spawn(async move {
        while let Ok(event) = receiver.recv_async().await {
            match event {
                mdns_sd::ServiceEvent::ServiceResolved(svc) => {
                    if let Some(peer) = peer_from_resolved(&svc) {
                        reg.upsert(peer, PeerSource::Mdns);
                    }
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_ty, fullname) => {
                    if let Some(device_id) = instance_of(&fullname, service_type) {
                        reg.mdns_removed(device_id, peer_timeout);
                    }
                }
                _ => {}
            }
        }
    }));

    Ok((daemon, fullname))
}

/// Build a Peer from a resolved service; None when fields are missing (not
/// one of ours) or no usable address is left
fn peer_from_resolved(svc: &mdns_sd::ResolvedService) -> Option<Peer> {
    let info = info_from_txt(&svc.txt_properties)?;
    // Several NICs yield several addresses; ScopedIp carries a scope id, but
    // only the bare address is kept
    let addrs = normalize_addrs(svc.addresses.iter().map(|ip| ip.to_ip_addr()).collect());
    if addrs.is_empty() {
        return None;
    }
    Some(Peer {
        info,
        addrs,
        port: svc.port,
    })
}

/// Read peer info from TXT properties: the fixed fields by their reserved
/// keys, everything else as app props
fn info_from_txt(txt: &mdns_sd::TxtProperties) -> Option<PeerInfo> {
    let props = txt
        .iter()
        .filter(|p| !is_reserved(p.key()))
        .map(|p| (p.key().to_string(), p.val_str().to_string()))
        .collect();
    Some(PeerInfo {
        device_id: txt.get_property_val_str("id")?.to_string(),
        name: txt.get_property_val_str("name")?.to_string(),
        fingerprint: txt.get_property_val_str("fp")?.to_string(),
        platform: txt.get_property_val_str("platform")?.to_string(),
        os_version: txt.get_property_val_str("osv").map(str::to_string),
        props,
    })
}

/// Instance name (the device ID) of an mDNS fullname: strip the service type
/// and the separating dot
pub(super) fn instance_of<'a>(fullname: &'a str, service_type: &str) -> Option<&'a str> {
    fullname
        .strip_suffix(service_type)
        .map(|s| s.trim_end_matches('.'))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Service type used by the tests
    const SERVICE: &str = "_lan-kit-test._udp.local.";

    /// Peer info with props, as an app would advertise it
    fn sample_info() -> PeerInfo {
        PeerInfo {
            device_id: "0f5c3a2e-1b7d-4c1e-9a3f-2d6b8e4f7a10".into(),
            name: "Desk Mac 书桌".into(),
            fingerprint: "ab".repeat(32),
            platform: "macos".into(),
            os_version: Some("macOS 15.3".into()),
            props: BTreeMap::from([
                ("pv".to_string(), "1.0".to_string()),
                ("group".to_string(), "desk-7".to_string()),
            ]),
        }
    }

    /// Info written into a service record reads back unchanged, props
    /// included
    #[test]
    fn txt_roundtrip() {
        let info = sample_info();
        let service = build_service(SERVICE, &info, 42624).unwrap();
        assert_eq!(info_from_txt(service.get_properties()), Some(info));
    }

    /// A record without our fixed fields is not one of ours
    #[test]
    fn foreign_record_is_ignored() {
        let service = mdns_sd::ServiceInfo::new(
            SERVICE,
            "printer",
            "printer.local.",
            "",
            631,
            HashMap::from([("rp".to_string(), "queue".to_string())]),
        )
        .unwrap();
        assert_eq!(info_from_txt(service.get_properties()), None);
    }

    /// Reserved keys are matched case-insensitively
    #[test]
    fn reserved_keys_ignore_case() {
        assert!(is_reserved("fp"));
        assert!(is_reserved("FP"));
        assert!(is_reserved("Name"));
        assert!(!is_reserved("pv"));
    }

    /// The instance (device ID) is extracted from a fullname
    #[test]
    fn instance_extraction() {
        assert_eq!(
            instance_of("uuid-1234._lan-kit-test._udp.local.", SERVICE),
            Some("uuid-1234")
        );
        assert_eq!(instance_of("x._other._udp.local.", SERVICE), None);
    }
}
