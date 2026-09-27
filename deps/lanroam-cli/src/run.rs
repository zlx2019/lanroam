//! `run`: this device in its desk group, with a console for the group
//! commands.
//!
//! Group changes happen inside the running node rather than in separate
//! invocations, which would fight it over the port and the group document.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use lanroam_core::engine::{Engine, EngineEvent, Status};
use lanroam_core::group::GroupDoc;
use lanroam_core::group::join::normalize_pin;
use lanroam_core::lan_kit::{Peer, PeerInfo};
use lanroam_core::node::NodeConfig;
use lanroam_core::protocol::PROP_GROUP;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::CommonArgs;
use crate::commands::{match_target, pick};
use crate::output::{describe, describe_peer, short_fp};

/// Console commands
const HELP: &str = "commands:
  nearby           devices on the LAN
  group            members of this device's group
  join <device>    join the group of a nearby device (it shows a PIN)
  kick <member>    remove a member from the group
  leave            leave the group
  help             this list
  quit             stop (Ctrl-C and Ctrl-D work too)";

/// A running join asks for a PIN: the sponsor's name, the attempts left,
/// where to send the PIN (`None` cancels)
type PinRequest = (String, u32, oneshot::Sender<Option<String>>);

/// Whether the console keeps going
#[derive(PartialEq, Eq)]
enum Flow {
    /// Read the next command
    Continue,
    /// Stop the node
    Quit,
}

/// `run`: run this device in its desk group until Ctrl-C or `quit`
pub(crate) async fn cmd_run(common: &CommonArgs, port: u16, name: Option<String>) -> Result<()> {
    let mut config = NodeConfig::new(common.data_dir.clone());
    config.port = port;
    config.discovery_port = common.discovery_port;
    config.display_name = name;
    let (engine, mut events) = Engine::start(config)
        .await
        .context("failed to start the node")?;
    println!(
        "running as {} on udp/{}  (lanroam-cli {})",
        describe(engine.info()),
        engine.local_port(),
        crate::VERSION
    );
    print_group(&engine.status().await?, engine.info());
    println!("type `help` for commands");

    let mut lines = stdin_lines();
    let (asks_tx, mut asks) = mpsc::unbounded_channel::<PinRequest>();
    let mut pin_reply: Option<oneshot::Sender<Option<String>>> = None;
    let mut joining: Option<JoinHandle<()>> = None;
    loop {
        tokio::select! {
            line = lines.recv() => {
                // End of input (Ctrl-D) quits like `quit`
                let Some(line) = line else { break };
                if let Some(reply) = pin_reply.take() {
                    pin_reply = answer_pin(&line, reply);
                } else if handle(&line, &engine, &asks_tx, &mut joining).await == Flow::Quit {
                    break;
                }
            }
            Some((sponsor, left, reply)) = asks.recv() => {
                println!("enter the PIN {sponsor} shows ({left} attempts left, empty line cancels):");
                pin_reply = Some(reply);
            }
            Some(event) = events.recv() => print_event(&event),
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    engine.shutdown().await;
    Ok(())
}

/// Hand a typed PIN to the waiting join; keeps waiting (returns the reply
/// channel) when the line is not a PIN
fn answer_pin(
    line: &str,
    reply: oneshot::Sender<Option<String>>,
) -> Option<oneshot::Sender<Option<String>>> {
    if line.trim().is_empty() {
        let _ = reply.send(None);
        return None;
    }
    match normalize_pin(line) {
        Some(pin) => {
            let _ = reply.send(Some(pin));
            None
        }
        None => {
            println!("a PIN has 6 digits; try again (empty line cancels):");
            Some(reply)
        }
    }
}

/// Run one console command
async fn handle(
    line: &str,
    engine: &Engine,
    asks: &mpsc::UnboundedSender<PinRequest>,
    joining: &mut Option<JoinHandle<()>>,
) -> Flow {
    let mut words = line.split_whitespace();
    let Some(command) = words.next() else {
        return Flow::Continue;
    };
    let arg = words.collect::<Vec<_>>().join(" ");
    let result = match (command, arg.is_empty()) {
        ("help" | "?", _) => {
            println!("{HELP}");
            Ok(())
        }
        ("quit" | "exit", _) => return Flow::Quit,
        ("nearby", _) => {
            print_nearby(engine);
            Ok(())
        }
        ("group", _) => engine
            .status()
            .await
            .map(|status| print_group(&status, engine.info()))
            .map_err(anyhow::Error::from),
        ("join", false) => start_join(engine, &arg, asks, joining),
        ("kick", false) => kick(engine, &arg).await,
        ("leave", _) => engine
            .leave()
            .await
            .map(|()| println!("group    left the group"))
            .map_err(anyhow::Error::from),
        ("join" | "kick", true) => Err(anyhow!("`{command}` needs a device; see `help`")),
        _ => Err(anyhow!("unknown command {command:?}; see `help`")),
    };
    if let Err(e) = result {
        println!("error    {e:#}");
    }
    Flow::Continue
}

/// Start joining the group of the nearby device `target` in the background
fn start_join(
    engine: &Engine,
    target: &str,
    asks: &mpsc::UnboundedSender<PinRequest>,
    joining: &mut Option<JoinHandle<()>>,
) -> Result<()> {
    if joining.as_ref().is_some_and(|task| !task.is_finished()) {
        bail!("a join is in progress already");
    }
    if engine.group().is_some() {
        bail!("this device is in a group already; `leave` it first");
    }
    let peers = engine.nearby();
    let peer = match match_target(&peers, target) {
        Ok(Some(peer)) => peer.clone(),
        Ok(None) => bail!("no nearby device matches {target:?}; see `nearby`"),
        Err(candidates) => bail!("{target:?} is ambiguous:\n{candidates}"),
    };
    let engine = engine.clone();
    let asks = asks.clone();
    *joining = Some(tokio::spawn(async move {
        let name = peer.info.name.clone();
        println!("join     asking {name}; it will show a PIN");
        match join(&engine, &peer, &asks).await {
            Ok(doc) => println!(
                "join     joined the group of {name} ({} members)",
                doc.members().count()
            ),
            Err(e) => println!("join     failed: {e:#}"),
        }
    }));
    Ok(())
}

/// Join through `sponsor`, asking the console for each PIN
async fn join(
    engine: &Engine,
    sponsor: &Peer,
    asks: &mpsc::UnboundedSender<PinRequest>,
) -> Result<Arc<GroupDoc>> {
    let mut joining = engine.join(sponsor).await?;
    loop {
        let (reply, pin) = oneshot::channel();
        asks.send((sponsor.info.name.clone(), joining.attempts_left(), reply))
            .map_err(|_| anyhow!("the console is gone"))?;
        let Some(pin) = pin.await.ok().flatten() else {
            bail!("cancelled");
        };
        match joining.answer(&pin).await? {
            Some(doc) => return Ok(doc),
            None => println!("join     wrong PIN"),
        }
    }
}

/// Remove the member `target` from the group
async fn kick(engine: &Engine, target: &str) -> Result<()> {
    let doc = engine.group().context("this device is in no group")?;
    let members: Vec<(&str, &str, &str)> = doc
        .members()
        .map(|(fp, record)| {
            (
                record.profile.device_id.as_str(),
                record.profile.name.as_str(),
                fp,
            )
        })
        .collect();
    let (_, name, fp) = match pick(&members, target, |m| *m) {
        Ok(Some(member)) => *member,
        Ok(None) => bail!("no member matches {target:?}; see `group`"),
        Err(many) => {
            let names: Vec<&str> = many.iter().map(|m| m.1).collect();
            bail!("{target:?} is ambiguous: {}", names.join(", "));
        }
    };
    if fp == engine.info().fingerprint {
        bail!("that is this device; use `leave`");
    }
    engine.kick(fp).await?;
    println!("group    removed {name}");
    Ok(())
}

/// Print the devices discovery sees and how they relate to our group
fn print_nearby(engine: &Engine) {
    let doc = engine.group();
    let mut peers = engine.nearby();
    peers.sort_by(|a, b| a.info.name.cmp(&b.info.name));
    if peers.is_empty() {
        println!("nearby   none seen yet");
    }
    for peer in &peers {
        let fp = &peer.info.fingerprint;
        let relation = match (&doc, peer.info.props.get(PROP_GROUP)) {
            (Some(doc), _) if doc.is_member(fp) => "member".to_string(),
            (_, Some(group)) => format!("in group {}", short_id(group)),
            (_, None) => "no group".to_string(),
        };
        println!("nearby   {}  · {relation}", describe_peer(peer));
    }
}

/// Print the group and which members are linked
fn print_group(status: &Status, own: &PeerInfo) {
    let Some(doc) = &status.doc else {
        println!(
            "group    none: `join <device>` joins a nearby device's group, and others can join this one"
        );
        return;
    };
    println!(
        "group    {} · {} members",
        short_id(&doc.id),
        doc.members().count()
    );
    for (fp, record) in doc.members() {
        let state = if fp == own.fingerprint {
            "this device"
        } else if status.online.iter().any(|p| p.fingerprint == fp) {
            "online"
        } else {
            "offline"
        };
        println!(
            "  {:<20} {:<8} [{}]  {state}",
            record.profile.name,
            record.profile.platform,
            short_fp(fp)
        );
    }
}

/// Print an engine event
fn print_event(event: &EngineEvent) {
    match event {
        EngineEvent::Online(info) => println!("online   {}", describe(info)),
        EngineEvent::Offline { name, .. } => println!("offline  {name}"),
        EngineEvent::Group(Some(doc)) => {
            let names: Vec<&str> = doc
                .members()
                .map(|(_, record)| record.profile.name.as_str())
                .collect();
            println!("group    {} members: {}", names.len(), names.join(", "));
        }
        EngineEvent::Group(None) => println!("group    this device is in no group now"),
        EngineEvent::Kicked => {
            println!("group    another member removed this device from the group")
        }
        EngineEvent::JoinPin { joiner, pin } => {
            let (head, tail) = pin.split_at(pin.len() / 2);
            println!(
                "join     {} wants to join; enter this PIN there:  {head} {tail}",
                describe(joiner)
            );
        }
        EngineEvent::JoinEnded { joiner, admitted } => {
            let outcome = if *admitted { "joined" } else { "did not join" };
            println!("join     {} {outcome}", joiner.name);
        }
    }
}

/// First block of a group ID, enough to tell groups apart
fn short_id(id: &str) -> &str {
    id.split('-').next().unwrap_or(id)
}

/// Console lines, read on a plain thread: a blocked read there cannot hold
/// up the runtime's shutdown
fn stdin_lines() -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    let reader = std::thread::Builder::new()
        .name("console".into())
        .spawn(move || {
            for line in std::io::stdin().lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    if let Err(e) = reader {
        // The receiver closes at once, which reads as end of input
        println!("error    cannot read the console: {e}");
    }
    rx
}
