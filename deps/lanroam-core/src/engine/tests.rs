//! Engine tests: several engines on loopback, with discovery fed by hand.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use lan_kit::DeviceIdentity;

use super::*;
use crate::PROFILE;
use crate::test_util::TempDir;

/// How long to wait for an expected outcome
const WAIT: Duration = Duration::from_secs(10);

/// An engine with its event stream and data directory
struct TestEngine {
    /// The engine
    engine: Engine,
    /// Its events
    events: mpsc::UnboundedReceiver<EngineEvent>,
    /// Its data directory
    dir: TempDir,
}

impl TestEngine {
    /// An engine in a fresh data directory
    fn start() -> Self {
        Self::start_in(TempDir::new())
    }

    /// An engine in `dir`, keeping the identity and group found there
    fn start_in(dir: TempDir) -> Self {
        let identity = Arc::new(DeviceIdentity::load_or_create(&dir.0, &PROFILE).unwrap());
        let info = identity.peer_info();
        let transport = Arc::new(Transport::bind(identity, 0).unwrap());
        let (engine, events) =
            Engine::launch(GroupStore::new(&dir.0), transport, info, None, None).unwrap();
        Self {
            engine,
            events,
            dir,
        }
    }

    /// Fingerprint
    fn fp(&self) -> String {
        self.engine.info().fingerprint.clone()
    }

    /// This engine as a discovered peer
    fn as_peer(&self) -> Peer {
        Peer {
            info: self.engine.info().clone(),
            addrs: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            port: self.engine.local_port(),
        }
    }

    /// Let this engine discover `others`
    fn sees(&self, others: &[&TestEngine]) {
        for other in others {
            let event = PeerEvent::Up(other.as_peer());
            self.engine.inner.inbox.send(Msg::Peer(event)).unwrap();
        }
    }

    /// Wait for an event `pick` accepts, skipping the others
    async fn expect<T>(
        &mut self,
        what: &str,
        mut pick: impl FnMut(&EngineEvent) -> Option<T>,
    ) -> T {
        let events = &mut self.events;
        let found = tokio::time::timeout(WAIT, async {
            loop {
                let event = events.recv().await.expect("the engine stopped");
                if let Some(found) = pick(&event) {
                    return found;
                }
            }
        });
        found
            .await
            .unwrap_or_else(|_| panic!("no {what} within {WAIT:?}"))
    }

    /// Wait until the status satisfies `holds`
    async fn until(&self, what: &str, holds: impl Fn(&Status) -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if holds(&self.engine.status().await.unwrap()) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{what} within {WAIT:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Wait until exactly the members `fps` are linked
    async fn until_online(&self, fps: &[String]) {
        let name = &self.engine.info().name;
        self.until(
            &format!("{name} linked to {} members", fps.len()),
            |status| {
                let mut online: Vec<&str> = status
                    .online
                    .iter()
                    .map(|p| p.fingerprint.as_str())
                    .collect();
                online.sort_unstable();
                let mut want: Vec<&str> = fps.iter().map(String::as_str).collect();
                want.sort_unstable();
                online == want
            },
        )
        .await;
    }
}

/// `joiner` joins through `sponsor`, typing the PIN the sponsor shows
async fn join(joiner: &TestEngine, sponsor: &mut TestEngine) -> Arc<GroupDoc> {
    let mut joining = joiner.engine.join(&sponsor.as_peer()).await.unwrap();
    let pin = sponsor
        .expect("PIN", |event| match event {
            EngineEvent::JoinPin { pin, .. } => Some(pin.clone()),
            _ => None,
        })
        .await;
    joining.answer(&pin).await.unwrap().expect("joined")
}

/// Three engines that discover each other, grouped: b joined a, c joined b
async fn group_of_three() -> (TestEngine, TestEngine, TestEngine) {
    let (mut a, mut b, c) = (
        TestEngine::start(),
        TestEngine::start(),
        TestEngine::start(),
    );
    a.sees(&[&b, &c]);
    b.sees(&[&a, &c]);
    c.sees(&[&a, &b]);
    join(&b, &mut a).await;
    join(&c, &mut b).await;
    a.until_online(&[b.fp(), c.fp()]).await;
    b.until_online(&[a.fp(), c.fp()]).await;
    c.until_online(&[a.fp(), b.fp()]).await;
    (a, b, c)
}

/// Joining founds a group, a second join extends it, and every member
/// links to every other one with the same document
#[tokio::test]
async fn three_devices_form_a_group() {
    let (a, b, c) = group_of_three().await;
    let doc = a.engine.group().unwrap();
    assert_eq!(doc.members().count(), 3);
    // Gossip settles on one document everywhere
    for other in [&b, &c] {
        other
            .until("the same document", |status| {
                status.doc.as_deref() == Some(&*doc)
            })
            .await;
    }
}

/// A kicked device learns it at once and is refused afterwards; joining
/// again brings it back
#[tokio::test]
async fn kick_and_rejoin() {
    let (mut a, b, mut c) = group_of_three().await;
    a.engine.kick(&c.fp()).await.unwrap();
    c.expect("the kick", |event| {
        matches!(event, EngineEvent::Kicked).then_some(())
    })
    .await;
    assert!(c.engine.group().is_none());
    b.until_online(&[a.fp()]).await;
    assert!(!b.engine.group().unwrap().is_member(&c.fp()));

    // Its old membership no longer opens a member link
    let refused = c
        .engine
        .inner
        .transport
        .connect(&a.as_peer(), c.engine.info(), Purpose::Member)
        .await;
    assert!(
        matches!(&refused, Err(TransportError::Rejected(code)) if code == reason_code::REMOVED),
        "{:?}",
        refused.err()
    );

    join(&c, &mut a).await;
    for engine in [&a, &b, &c] {
        let others: Vec<String> = [&a, &b, &c]
            .iter()
            .map(|e| e.fp())
            .filter(|fp| *fp != engine.fp())
            .collect();
        engine.until_online(&others).await;
    }
}

/// A device that leaves is gone from everyone's document
#[tokio::test]
async fn leaving() {
    let (a, b, c) = group_of_three().await;
    b.engine.leave().await.unwrap();
    assert!(b.engine.group().is_none());
    for other in [&a, &c] {
        other
            .until("b removed", |status| {
                status
                    .doc
                    .as_ref()
                    .is_some_and(|doc| doc.standing(&b.fp()) == Standing::Removed)
            })
            .await;
    }
    a.until_online(&[c.fp()]).await;
    assert!(matches!(b.engine.leave().await, Err(EngineError::NoGroup)));
}

/// A restarted device is still in its group and links up again
#[tokio::test]
async fn restart_keeps_the_group() {
    let (mut a, b) = (TestEngine::start(), TestEngine::start());
    a.sees(&[&b]);
    b.sees(&[&a]);
    let doc = join(&b, &mut a).await;
    a.until_online(&[b.fp()]).await;

    b.engine.shutdown().await;
    let b = TestEngine::start_in(b.dir);
    assert_eq!(b.engine.group().map(|d| d.id.clone()), Some(doc.id.clone()));
    // It comes back on a new port
    a.sees(&[&b]);
    b.sees(&[&a]);
    a.until_online(&[b.fp()]).await;
    b.until_online(&[a.fp()]).await;
}

/// Strangers may ask to join but not link up as members, and a device in a
/// group cannot join another
#[tokio::test]
async fn strangers_and_grouped_devices() {
    let (mut a, b, stranger) = (
        TestEngine::start(),
        TestEngine::start(),
        TestEngine::start(),
    );
    join(&b, &mut a).await;
    let refused = stranger
        .engine
        .inner
        .transport
        .connect(&a.as_peer(), stranger.engine.info(), Purpose::Member)
        .await;
    assert!(
        matches!(&refused, Err(TransportError::Rejected(code)) if code == reason_code::NOT_A_MEMBER),
        "{:?}",
        refused.err()
    );
    assert!(matches!(
        b.engine.join(&stranger.as_peer()).await,
        Err(EngineError::Grouped)
    ));
}
