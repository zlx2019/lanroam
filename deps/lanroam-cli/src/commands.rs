//! Subcommand implementations.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use lanroam_core::lan_kit::discovery::DiscoveryOptions;
use lanroam_core::lan_kit::{DeviceIdentity, DiscoveryService, Peer, PeerEvent, PeerInfo};
use lanroam_core::lanroam_input::inject::Injector;
use lanroam_core::lanroam_input::switch::Switch;
use lanroam_core::lanroam_input::{Desktop, Edge, platform};
use lanroam_core::node::{Node, NodeConfig};
use lanroam_core::session::{self, SourceNotice, TargetNotice};
use lanroam_core::transport::{Link, Transport};
use lanroam_core::{PROFILE, diag};
use tokio::sync::mpsc;

use crate::CommonArgs;
use crate::dryrun::PrintInjector;
use crate::output::{
    describe, describe_peer, describe_screens, print_inject_stats, print_source_report, print_stats,
};

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

/// What `listen` does with a link
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replay {
    /// Inject the source's input into this session
    Inject,
    /// Print the source's input instead of injecting it
    DryRun,
    /// No injection on this platform: only answer pings
    PingOnly,
}

/// `listen`: run a node until Ctrl-C, replaying input sessions and
/// answering pings on every link
pub(crate) async fn cmd_listen(
    common: &CommonArgs,
    port: u16,
    name: Option<String>,
    dry_run: bool,
) -> Result<()> {
    let replay = if dry_run {
        Replay::DryRun
    } else {
        match platform::injector() {
            Ok(_) => Replay::Inject,
            Err(e) => {
                println!("{e}: links will only answer pings (try --dry-run)");
                Replay::PingOnly
            }
        }
    };
    let mut config = NodeConfig::new(common.data_dir.clone());
    config.port = port;
    config.discovery_port = common.discovery_port;
    config.display_name = name;
    let (node, mut events) = Node::start(config)
        .await
        .context("failed to start the node")?;
    println!(
        "listening as {} on udp/{}  (Ctrl-C to quit)",
        describe(node.info()),
        node.transport().local_port()
    );

    let accept = tokio::spawn(accept_loop(
        Arc::clone(node.transport()),
        node.info().clone(),
        replay,
    ));
    // Down events only carry a fingerprint; remember names to print them
    let mut names: HashMap<String, String> = HashMap::new();
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(PeerEvent::Up(peer)) => {
                    let verb = if names.insert(peer.info.fingerprint.clone(), peer.info.name.clone()).is_some() {
                        "updated"
                    } else {
                        "online "
                    };
                    println!("{verb}  {}", describe_peer(&peer));
                }
                Some(PeerEvent::Down(fingerprint)) => {
                    let name = names.remove(&fingerprint).unwrap_or_else(|| fingerprint.clone());
                    println!("offline  {name}");
                }
                None => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    accept.abort();
    node.shutdown().await;
    Ok(())
}

/// Accept connections forever, each handshake and session in its own task
async fn accept_loop(transport: Arc<Transport>, local: PeerInfo, replay: Replay) {
    while let Some(incoming) = transport.accept().await {
        let local = local.clone();
        tokio::spawn(async move {
            let from = incoming.remote_address();
            match incoming.handshake(&local).await {
                Ok(link) => {
                    let who = describe(link.remote());
                    println!("linked   {who} from {from}");
                    serve_link(link, replay).await;
                    println!("unlinked {who}");
                }
                // Other nodes probing our identity for discovery
                Err(e) if e.is_probe() => {}
                Err(e) => println!("refused  {from}: {e}"),
            }
        });
    }
}

/// Serve one link: an input session (which answers pings too), or pings
/// only where input cannot be replayed
async fn serve_link(link: Link, replay: Replay) {
    let name = link.remote().name.clone();
    let setup = match replay {
        Replay::PingOnly => None,
        Replay::DryRun => Some(
            platform::displays().map(|d| (d, Box::new(PrintInjector::new()) as Box<dyn Injector>)),
        ),
        Replay::Inject => Some(platform::displays().and_then(|d| Ok((d, platform::injector()?)))),
    };
    let (displays, injector) = match setup {
        None => return diag::respond(link).await,
        Some(Ok(setup)) => setup,
        Some(Err(e)) => {
            println!("cannot replay input from {name} ({e}); answering pings only");
            return diag::respond(link).await;
        }
    };
    let notice = |notice| match notice {
        TargetNotice::Entered => println!("control  {name} took over"),
        TargetNotice::Left => println!("release  {name} gave control back"),
    };
    match session::run_target(link, displays, injector, notice).await {
        Ok(stats) => print_inject_stats(&stats),
        Err(e) => println!("session failed: {e}"),
    }
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
    let mut link =
        connect_target(&node, &mut events, target, Duration::from_secs(wait_secs)).await?;

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

/// `share`: capture this machine's keyboard and mouse and control `target`,
/// which sits on `edge` of the local desktop, until Ctrl-C or `duration`
pub(crate) async fn cmd_share(
    common: &CommonArgs,
    target: &str,
    edge: Edge,
    wait_secs: u64,
    duration: Option<u64>,
) -> Result<()> {
    let local = Desktop::new(platform::displays().context("cannot read the local displays")?);
    let mut config = NodeConfig::new(common.data_dir.clone());
    config.port = 0;
    config.discovery_port = common.discovery_port;
    config.passive = true;
    let (node, mut events) = Node::start(config)
        .await
        .context("failed to start the node")?;
    let mut link =
        connect_target(&node, &mut events, target, Duration::from_secs(wait_secs)).await?;
    let screens = session::recv_screens(&mut link).await?;
    let name = link.remote().name.clone();
    println!("local    {}", describe_screens(&local));
    println!("target   {}", describe_screens(&screens));

    let switch = Arc::new(Mutex::new(Switch::new(local, screens, edge)));
    let (emit_tx, emit_rx) = mpsc::unbounded_channel();
    let sink = Box::new(move |emit| {
        // Only fails once the session is over, when nothing listens anymore
        let _ = emit_tx.send(emit);
    });
    let capture =
        platform::start_capture(Arc::clone(&switch), sink).context("cannot capture input")?;
    println!(
        "sharing: push the pointer through the {edge} edge to control {name}\n  \
         Ctrl+Alt+Esc (Ctrl+Option+Esc on a Mac) takes control back at once, Ctrl-C quits"
    );

    let notice = |notice| match notice {
        SourceNotice::Entered => println!("control  now controlling {name}"),
        SourceNotice::Left => println!("release  back on this machine"),
        SourceNotice::Unresponsive => {
            println!("warning  {name} stopped answering; control comes back at the next input")
        }
    };
    let shutdown = async {
        match duration {
            Some(secs) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    () = tokio::time::sleep(Duration::from_secs(secs)) => println!("time is up"),
                }
            }
            None => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    };
    let report = session::run_source(link, switch, emit_rx, notice, shutdown).await;
    // Stops the capture and frees the local cursor
    drop(capture);
    print_source_report(&report);
    node.shutdown().await;
    Ok(())
}

/// Connect to `target`: an `ip:port` directly, printing the fingerprint to
/// verify by hand; anything else through discovery, fingerprint pinned
async fn connect_target(
    node: &Node,
    events: &mut mpsc::Receiver<PeerEvent>,
    target: &str,
    wait: Duration,
) -> Result<Link> {
    if let Ok(addr) = target.parse::<SocketAddr>() {
        let started = Instant::now();
        let link = node
            .transport()
            .connect_direct(addr, node.info())
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
        .connect(&peer)
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

/// Resolve a target among discovered nodes
///
/// An exact device id or name (case-insensitive) wins; otherwise a
/// fingerprint prefix of at least 4 hex digits. `Ok(None)` means no match
/// yet, `Err` lists the candidates of an ambiguous target.
fn match_target<'a>(peers: &'a [Peer], target: &str) -> Result<Option<&'a Peer>, String> {
    let exact: Vec<&Peer> = peers
        .iter()
        .filter(|p| p.info.device_id == target || p.info.name.eq_ignore_ascii_case(target))
        .collect();
    let candidates =
        if exact.is_empty() && target.len() >= 4 && target.bytes().all(|b| b.is_ascii_hexdigit()) {
            let prefix = target.to_ascii_lowercase();
            peers
                .iter()
                .filter(|p| p.info.fingerprint.starts_with(&prefix))
                .collect()
        } else {
            exact
        };
    match candidates.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        many => Err(many
            .iter()
            .map(|p| format!("  {}", describe_peer(p)))
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr};

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
