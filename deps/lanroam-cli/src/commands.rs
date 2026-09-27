//! Subcommand implementations.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use lanroam_core::group::GroupStore;
use lanroam_core::lan_kit::discovery::DiscoveryOptions;
use lanroam_core::lan_kit::{DeviceIdentity, DiscoveryService, Peer, PeerEvent};
use lanroam_core::node::{Node, NodeConfig};
use lanroam_core::protocol::Purpose;
use lanroam_core::transport::Link;
use lanroam_core::{PROFILE, diag};
use tokio::sync::mpsc;

use crate::CommonArgs;
use crate::output::{describe, describe_peer, print_stats};

/// How long to wait for the last datagram answers
const DATAGRAM_GRACE: Duration = Duration::from_secs(2);

/// `id`: show the local identity
pub(crate) fn cmd_id(common: &CommonArgs) -> Result<()> {
    let identity = DeviceIdentity::load_or_create(&common.data_dir, &PROFILE)
        .context("failed to load the device identity")?;
    let info = identity.peer_info();
    println!("name         {}", info.name);
    println!("device id    {}", info.device_id);
    println!("fingerprint  {}", info.fingerprint);
    println!(
        "platform     {} ({})",
        info.platform,
        info.os_version.as_deref().unwrap_or("unknown")
    );
    match GroupStore::new(&common.data_dir).load() {
        Ok(Some(doc)) => println!(
            "group        {} ({} members)",
            doc.id,
            doc.members().count()
        ),
        Ok(None) => println!("group        none"),
        Err(e) => println!("group        unreadable: {e}"),
    }
    println!("data dir     {}", common.data_dir.display());
    Ok(())
}

/// `scan`: listen passively and list the nodes seen
pub(crate) async fn cmd_scan(common: &CommonArgs, wait_secs: u64) -> Result<()> {
    let identity = DeviceIdentity::load_or_create(&common.data_dir, &PROFILE)
        .context("failed to load the device identity")?;
    let mut options = DiscoveryOptions::new(0, common.discovery_port);
    options.passive = true;
    let (discovery, _events) = DiscoveryService::start(&PROFILE, identity.peer_info(), options)
        .await
        .context("failed to start discovery")?;
    println!("scanning for {wait_secs}s ...");
    tokio::time::sleep(Duration::from_secs(wait_secs)).await;

    let mut peers = discovery.peers();
    peers.sort_by(|a, b| a.info.name.cmp(&b.info.name));
    if peers.is_empty() {
        println!("no Lanroam nodes found");
    } else {
        println!("{} node(s):", peers.len());
        for peer in &peers {
            println!("  {}", describe_peer(peer));
        }
    }
    discovery.shutdown().await;
    Ok(())
}

/// `ping`: connect to a node and measure both paths
pub(crate) async fn cmd_ping(
    common: &CommonArgs,
    target: &str,
    count: u32,
    interval_ms: u64,
    wait_secs: u64,
) -> Result<()> {
    let mut config = NodeConfig::new(common.data_dir.clone());
    config.port = 0;
    config.discovery_port = common.discovery_port;
    config.passive = true;
    let (node, mut events) = Node::start(config)
        .await
        .context("failed to start the node")?;
    let mut link = connect_target(
        &node,
        &mut events,
        target,
        Duration::from_secs(wait_secs),
        Purpose::Diag,
    )
    .await?;

    let stream = diag::ping_stream(&mut link, count).await?;
    print_stats("control stream", &stream);
    let datagrams = diag::ping_datagrams(
        &link,
        count,
        Duration::from_millis(interval_ms.max(1)),
        DATAGRAM_GRACE,
    )
    .await?;
    print_stats("datagrams     ", &datagrams);

    link.close();
    node.shutdown().await;
    Ok(())
}

/// Connect to `target` for `purpose`: an `ip:port` directly, printing the
/// fingerprint to verify by hand; anything else through discovery,
/// fingerprint pinned
async fn connect_target(
    node: &Node,
    events: &mut mpsc::Receiver<PeerEvent>,
    target: &str,
    wait: Duration,
    purpose: Purpose,
) -> Result<Link> {
    if let Ok(addr) = target.parse::<SocketAddr>() {
        let started = Instant::now();
        let link = node
            .transport()
            .connect_direct(addr, node.info(), purpose)
            .await
            .with_context(|| format!("failed to connect to {addr}"))?;
        report_link(&link, started.elapsed());
        println!(
            "direct connection: nothing was pinned, verify this fingerprint yourself:\n  {}",
            link.remote().fingerprint
        );
        return Ok(link);
    }
    let peer = find_target(node, events, target, wait).await?;
    let started = Instant::now();
    let link = node
        .connect(&peer, purpose)
        .await
        .with_context(|| format!("failed to connect to {}", peer.info.name))?;
    report_link(&link, started.elapsed());
    Ok(link)
}

/// Print who we are linked to and how long the handshake took
fn report_link(link: &Link, took: Duration) {
    println!(
        "linked to {} at {} in {:.1}ms",
        describe(link.remote()),
        link.remote_address(),
        took.as_secs_f64() * 1000.0
    );
}

/// Wait until discovery shows exactly one node matching `target`
async fn find_target(
    node: &Node,
    events: &mut mpsc::Receiver<PeerEvent>,
    target: &str,
    wait: Duration,
) -> Result<Peer> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let peers = node.discovery().peers();
        match match_target(&peers, target) {
            Ok(Some(peer)) => return Ok(peer.clone()),
            Ok(None) => {}
            Err(candidates) => bail!("{target:?} is ambiguous:\n{candidates}"),
        }
        // Any discovery event may bring the target; the snapshot is re-read
        if tokio::time::timeout_at(deadline, events.recv())
            .await
            .is_err()
        {
            let seen: Vec<String> = peers
                .iter()
                .map(|p| format!("  {}", describe_peer(p)))
                .collect();
            if seen.is_empty() {
                bail!(
                    "{target:?} not found within {}s, and no node was seen",
                    wait.as_secs()
                );
            }
            bail!(
                "{target:?} not found within {}s; nodes seen:\n{}",
                wait.as_secs(),
                seen.join("\n")
            );
        }
    }
}

/// Resolve a target among discovered nodes (see [`pick`]); `Err` lists the
/// candidates of an ambiguous target
pub(crate) fn match_target<'a>(
    peers: &'a [Peer],
    target: &str,
) -> Result<Option<&'a Peer>, String> {
    pick(peers, target, |p| {
        (&p.info.device_id, &p.info.name, &p.info.fingerprint)
    })
    .map_err(|many| {
        many.iter()
            .map(|p| format!("  {}", describe_peer(p)))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Resolve a target among devices, given each one's (device id, name,
/// fingerprint)
///
/// An exact device id or name (case-insensitive) wins; otherwise a
/// fingerprint prefix of at least 4 hex digits. `Ok(None)` means no match,
/// `Err` holds the candidates of an ambiguous target.
pub(crate) fn pick<'a, T>(
    items: &'a [T],
    target: &str,
    key: impl Fn(&T) -> (&str, &str, &str),
) -> Result<Option<&'a T>, Vec<&'a T>> {
    let exact: Vec<&T> = items
        .iter()
        .filter(|item| {
            let (id, name, _) = key(item);
            id == target || name.eq_ignore_ascii_case(target)
        })
        .collect();
    let candidates =
        if exact.is_empty() && target.len() >= 4 && target.bytes().all(|b| b.is_ascii_hexdigit()) {
            let prefix = target.to_ascii_lowercase();
            items
                .iter()
                .filter(|item| key(item).2.starts_with(&prefix))
                .collect()
        } else {
            exact
        };
    match candidates.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        _ => Err(candidates),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr};

    use lanroam_core::lan_kit::PeerInfo;

    use super::*;

    /// A discovered node for the matching tests
    fn peer(name: &str, fingerprint: &str) -> Peer {
        Peer {
            info: PeerInfo {
                device_id: format!("id-{name}"),
                name: name.to_string(),
                fingerprint: fingerprint.to_string(),
                platform: "macos".to_string(),
                os_version: None,
                props: BTreeMap::new(),
            },
            addrs: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))],
            port: 42624,
        }
    }

    /// Names (any case), device ids and fingerprint prefixes all resolve
    #[test]
    fn resolves_each_form() {
        let peers = [peer("Desk-PC", "abcd1234"), peer("MacBook", "ef561234")];
        let name_of = |t: &str| {
            match_target(&peers, t)
                .unwrap()
                .map(|p| p.info.name.clone())
        };
        assert_eq!(name_of("desk-pc").as_deref(), Some("Desk-PC"));
        assert_eq!(name_of("id-MacBook").as_deref(), Some("MacBook"));
        assert_eq!(name_of("EF56").as_deref(), Some("MacBook"));
        assert_eq!(name_of("nobody"), None);
    }

    /// Short prefixes are not taken as fingerprints, and an ambiguous target
    /// lists its candidates
    #[test]
    fn rejects_short_and_ambiguous() {
        let peers = [peer("A", "abcd1111"), peer("B", "abcd2222")];
        assert!(matches!(match_target(&peers, "abc"), Ok(None)));
        let err = match_target(&peers, "abcd").unwrap_err();
        assert!(err.contains('A') && err.contains('B'));
    }
}
