//! `run`: this device in its desk group, sharing its keyboard and mouse,
//! with a console for the group commands.
//!
//! Group changes happen inside the running node rather than in separate
//! invocations, which would fight it over the port and the group document.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use lanroam_core::engine::{
    ControlEvent, Engine, EngineEvent, InputBackend, NoDrag, PlatformInput, Spot, Status,
};
use lanroam_core::group::GroupDoc;
use lanroam_core::group::join::{PIN_ATTEMPTS, normalize_pin};
use lanroam_core::lan_kit::{Peer, PeerInfo};
use lanroam_core::lanroam_clipboard::{Clipboard, MemoryClipboard, SystemClipboard};
use lanroam_core::lanroam_input::Edge;
use lanroam_core::layout;
use lanroam_core::node::NodeConfig;
use lanroam_core::protocol::{PROP_GROUP, released};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::CommonArgs;
use crate::commands::{match_target, pick};
use crate::dryrun::DryRunInput;
use crate::output::{describe, describe_peer, short_fp};

/// Console commands
const HELP: &str = "commands:
  nearby           devices on the LAN
  group            members of this device's group
  join <device>    join the group of a nearby device (it shows a PIN)
  kick <member>    remove a member from the group
  leave            leave the group
  swap on|off      swap Command and Control on input into this device from
                   a device of the other platform (on by default)
  layout           where the members' screens sit, and the edges they share
  place <member> <left-of|right-of|above|below> <member> [offset]
                   move a member beside another, shifted `offset` logical
                   pixels along the edge (down or right)
  help             this list
  quit             stop (Ctrl-C and Ctrl-D work too)";

/// How to move between devices
const HOTKEYS: &str = "Push the pointer off an edge shared with another member to control it.
hotkeys (Option for Alt on a Mac; the digits, arrows and L need the left Alt):
  Ctrl+Alt+1..9          jump to device n (numbers as `layout` shows them)
  Ctrl+Alt+arrow         jump to the neighbour that way
  Ctrl+Alt+L, ScrollLock lock the pointer to its device, or unlock
  Ctrl+Alt+Esc           back to this device and pause crossing, or resume";

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
pub(crate) async fn cmd_run(
    common: &CommonArgs,
    port: u16,
    name: Option<String>,
    dry_run: bool,
) -> Result<()> {
    let mut config = NodeConfig::new(common.data_dir.clone());
    config.port = port;
    config.discovery_port = common.discovery_port;
    config.display_name = name;
    // A dry run keeps its hands off this machine's clipboard too
    let (input, clipboard): (Arc<dyn InputBackend>, Arc<dyn Clipboard>) = if dry_run {
        (Arc::new(DryRunInput), Arc::new(MemoryClipboard::new()))
    } else {
        (Arc::new(PlatformInput), Arc::new(SystemClipboard))
    };
    // No desktop to drag files on: a drag stays where it is
    let (engine, mut events) = Engine::start(config, input, clipboard, Arc::new(NoDrag))
        .await
        .context("failed to start the node")?;
    println!(
        "running as {} on udp/{}  (lanroam-cli {})",
        describe(&engine.info()),
        engine.local_port(),
        crate::VERSION
    );
    print_group(&engine.status().await?, &engine.info());
    println!("type `help` for commands. {HOTKEYS}");

    let mut lines = stdin_lines();
    let (asks_tx, mut asks) = mpsc::unbounded_channel::<PinRequest>();
    let mut pin_reply: Option<oneshot::Sender<Option<String>>> = None;
    let mut joining: Option<JoinHandle<()>> = None;
    let mut seen = engine.group();
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
            Some(event) = events.recv() => print_event(&event, &mut seen),
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
            println!("{HELP}\n{HOTKEYS}");
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
            .map(|status| print_group(&status, &engine.info()))
            .map_err(anyhow::Error::from),
        ("join", false) => start_join(engine, &arg, asks, joining),
        ("kick", false) => kick(engine, &arg).await,
        ("layout", _) => {
            print_layout(engine);
            Ok(())
        }
        ("place", false) => place(engine, &arg).await,
        ("swap", _) => swap(engine, &arg).await,
        ("leave", _) => engine
            .leave()
            .await
            .map(|()| println!("group    left the group"))
            .map_err(anyhow::Error::from),
        ("join" | "kick" | "place", true) => Err(anyhow!("`{command}` needs a device; see `help`")),
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

/// Resolve a member by name, device id or fingerprint prefix: (fingerprint,
/// name)
fn find_member(doc: &GroupDoc, target: &str) -> Result<(String, String)> {
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
    match pick(&members, target, |m| *m) {
        Ok(Some((_, name, fp))) => Ok((fp.to_string(), name.to_string())),
        Ok(None) => bail!("no member matches {target:?}; see `group`"),
        Err(many) => {
            let names: Vec<&str> = many.iter().map(|m| m.1).collect();
            bail!("{target:?} is ambiguous: {}", names.join(", "));
        }
    }
}

/// Remove the member `target` from the group
async fn kick(engine: &Engine, target: &str) -> Result<()> {
    let doc = engine.group().context("this device is in no group")?;
    let (fp, name) = find_member(&doc, target)?;
    if fp == engine.info().fingerprint {
        bail!("that is this device; use `leave`");
    }
    engine.kick(&fp).await?;
    println!("group    removed {name}");
    Ok(())
}

/// `swap on|off`
async fn swap(engine: &Engine, arg: &str) -> Result<()> {
    let on = match arg {
        "on" => true,
        "off" => false,
        _ => bail!("usage: swap on|off"),
    };
    engine.set_swap(on).await?;
    let state = if on { "swapped" } else { "left as they are" };
    println!("keys     Command and Control from the other platform are {state} now");
    Ok(())
}

/// `place <member> <left-of|right-of|above|below> <member> [offset]`
async fn place(engine: &Engine, args: &str) -> Result<()> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let (target, relation, anchor, offset) = match words.as_slice() {
        [target, relation, anchor] => (*target, *relation, *anchor, 0),
        [target, relation, anchor, offset] => {
            let offset = offset
                .parse()
                .with_context(|| format!("{offset:?} is not an offset in pixels"))?;
            (*target, *relation, *anchor, offset)
        }
        _ => bail!("usage: place <member> <left-of|right-of|above|below> <member> [offset]"),
    };
    let side = match relation {
        "left-of" => Edge::Left,
        "right-of" => Edge::Right,
        "above" => Edge::Top,
        "below" => Edge::Bottom,
        _ => bail!("{relation:?} is not one of left-of, right-of, above, below"),
    };
    let doc = engine.group().context("this device is in no group")?;
    let (fp, name) = find_member(&doc, target)?;
    let (anchor, anchor_name) = find_member(&doc, anchor)?;
    if fp == anchor {
        bail!("a device cannot sit beside itself");
    }
    let spot = Spot::Beside {
        side,
        anchor,
        offset,
    };
    engine.place(&fp, spot).await?;
    println!("layout   {name} is {relation} {anchor_name} now");
    Ok(())
}

/// Print the layout: each member's place and size, the shared edges, and
/// problems
fn print_layout(engine: &Engine) {
    let Some(doc) = engine.group() else {
        println!("layout   this device is in no group");
        return;
    };
    let world = layout::world(&doc);
    let name = |fp: &str| {
        doc.devices
            .get(fp)
            .map_or_else(|| short_fp(fp).to_string(), |r| r.profile.name.clone())
    };
    println!("layout   canvas in logical pixels; numbers are for the Ctrl+Alt+N hotkeys");
    for (n, device) in world.ordered().iter().enumerate() {
        let (Some(record), Some(area)) = (doc.devices.get(&device.key), device.bounds()) else {
            continue;
        };
        let scale = match record.profile.scale {
            100 => String::new(),
            pct => format!(" at {pct}%"),
        };
        println!(
            "  {}. {:<20} at ({:.0}, {:.0})  {:.0}x{:.0}  {} display(s){scale}",
            n + 1,
            record.profile.name,
            area.left,
            area.top,
            area.right - area.left,
            area.bottom - area.top,
            device.desktop.displays().len(),
        );
    }
    for (fp, record) in doc.members() {
        if world.device(fp).is_none() {
            let why = if record.profile.displays.is_empty() {
                "displays not reported yet"
            } else {
                "not placed yet"
            };
            println!("  -  {:<20} {why}", record.profile.name);
        }
    }
    let edges = world.shared_edges();
    if edges.is_empty() && world.devices().len() > 1 {
        println!("edges    none: no two devices touch; move one with `place`");
    }
    for edge in &edges {
        let (from_side, to_side, axis) = match edge.edge {
            Edge::Right => ("right", "left", "y"),
            _ => ("bottom", "top", "x"),
        };
        let ((a_from, a_to), (b_from, b_to)) = (edge.first_span, edge.second_span);
        println!(
            "edge     {} {from_side} ({axis} {a_from:.0}..{a_to:.0}) <-> {} {to_side} ({axis} {b_from:.0}..{b_to:.0})",
            name(&edge.first),
            name(&edge.second),
        );
    }
    if let Some((a, b)) = world.overlapping() {
        println!(
            "warning  {} and {} overlap; move one with `place`",
            name(a),
            name(b)
        );
    }
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
///
/// Group documents are compared with the last one `seen`, to say what
/// changed rather than repeat the whole group.
fn print_event(event: &EngineEvent, seen: &mut Option<Arc<GroupDoc>>) {
    match event {
        EngineEvent::Online(info) => println!("online   {}", describe(info)),
        EngineEvent::Offline { name, .. } => println!("offline  {name}"),
        // The app shows the number on the screens; the layout lists them
        EngineEvent::Identify => {
            println!("identify a member asked every device to show its number")
        }
        // Only the app records key combinations (its settings), and drags
        // files (it has a desktop to drag on)
        EngineEvent::Recorded(_)
        | EngineEvent::Receiving(_)
        | EngineEvent::DragFailed { .. }
        | EngineEvent::CopiedFiles(_) => {}
        EngineEvent::Group(Some(doc)) => {
            let names = |doc: &GroupDoc| -> Vec<String> {
                doc.members()
                    .map(|(_, record)| record.profile.name.clone())
                    .collect()
            };
            let world = |doc: &GroupDoc| layout::world(doc);
            let before = seen.as_deref();
            if before.map(names) != Some(names(doc)) {
                let names = names(doc);
                println!("group    {} members: {}", names.len(), names.join(", "));
            } else if before.map(world) != Some(world(doc)) {
                println!("layout   changed; `layout` shows it");
            }
            *seen = Some(Arc::clone(doc));
        }
        EngineEvent::Group(None) => {
            *seen = None;
            println!("group    this device is in no group now");
        }
        EngineEvent::Control(event) => print_control(event),
        EngineEvent::Kicked => {
            println!("group    another member removed this device from the group")
        }
        EngineEvent::JoinPin {
            joiner,
            pin,
            attempts_left,
        } => {
            if *attempts_left == PIN_ATTEMPTS {
                let (head, tail) = pin.split_at(pin.len() / 2);
                println!(
                    "join     {} wants to join; enter this PIN there:  {head} {tail}",
                    describe(joiner)
                );
            } else {
                println!(
                    "join     {} typed a wrong PIN; {attempts_left} attempts left",
                    joiner.name
                );
            }
        }
        EngineEvent::JoinEnded { joiner, admitted } => {
            let outcome = if *admitted { "joined" } else { "did not join" };
            println!("join     {} {outcome}", joiner.name);
        }
    }
}

/// Print a change of who controls what
fn print_control(event: &ControlEvent) {
    match event {
        ControlEvent::Controlling { name, .. } => println!("control  now controlling {name}"),
        ControlEvent::Home { .. } => println!("control  back on this device"),
        ControlEvent::Lost { name, .. } => {
            println!("warning  lost the link to {name}; back here at the next input");
        }
        ControlEvent::ControlledBy { name, .. } => println!("control  {name} controls this device"),
        ControlEvent::Freed { name, .. } => println!("control  {name} gave this device back"),
        ControlEvent::TookBack { name, .. } => {
            println!("control  took this device back from {name} (local input)");
        }
        ControlEvent::LetGo { name, reason, .. } => {
            let why = match reason.as_str() {
                released::PREEMPTED => "another device took it over",
                released::LOCAL_INPUT => "someone used it",
                released::UNAVAILABLE => "it cannot inject input",
                other => other,
            };
            println!("control  {name} let go ({why}); back here at the next input");
        }
        ControlEvent::Unresponsive { name, .. } => {
            println!("warning  {name} stopped answering; back here at the next input");
        }
        ControlEvent::Paused { on: true } => {
            println!("control  crossing paused; Ctrl+Alt+Esc resumes it");
        }
        ControlEvent::Paused { on: false } => println!("control  crossing resumed"),
        ControlEvent::Locked { on: true } => {
            println!("control  pointer locked to its device; Ctrl+Alt+L unlocks it");
        }
        ControlEvent::Locked { on: false } => println!("control  pointer unlocked"),
        ControlEvent::LockedHere { name, on: true, .. } => {
            println!("control  {name} locked the pointer to this device");
        }
        ControlEvent::LockedHere {
            name, on: false, ..
        } => {
            println!("control  {name} unlocked the pointer");
        }
        ControlEvent::Unavailable { what, reason } => {
            println!("warning  {what} is unavailable: {reason}");
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
