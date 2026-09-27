//! Engine tests: several engines on loopback, with discovery fed by hand.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use lan_kit::DeviceIdentity;
use lanroam_input::inject::Injector;
use lanroam_input::platform::EmitSink;
use lanroam_input::switch::{self, Decision, Switch};
use lanroam_input::{InputError, InputEvent, MouseButton};

use super::*;
use crate::PROFILE;
use crate::test_util::TempDir;

/// How long to wait for an expected outcome
const WAIT: Duration = Duration::from_secs(10);

/// What the fake injector was asked to do
#[derive(Debug, Clone, PartialEq, Eq)]
enum Injected {
    /// Cursor moved
    Move(Point),
    /// Key pressed or released
    Key(u16, bool),
    /// Button pressed or released
    Button(MouseButton, bool),
    /// Scrolled
    Wheel(i32, i32),
}

/// Records injections
struct Recorder(Arc<Mutex<Vec<Injected>>>);

impl Injector for Recorder {
    fn move_to(&mut self, at: Point) -> Result<(), InputError> {
        self.0.lock().unwrap().push(Injected::Move(at));
        Ok(())
    }
    fn button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
        self.0.lock().unwrap().push(Injected::Button(button, down));
        Ok(())
    }
    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        self.0.lock().unwrap().push(Injected::Wheel(dx, dy));
        Ok(())
    }
    fn key(&mut self, usage: u16, down: bool) -> Result<(), InputError> {
        self.0.lock().unwrap().push(Injected::Key(usage, down));
        Ok(())
    }
}

/// A keyboard and mouse driven by the test
#[derive(Default)]
struct FakeInput {
    /// What the engine handed to the capture
    capture: Mutex<Option<(Arc<Mutex<Switch>>, EmitSink)>>,
    /// Injections so far
    injected: Arc<Mutex<Vec<Injected>>>,
}

impl InputBackend for FakeInput {
    fn screens(&self) -> Result<(Vec<lanroam_input::Rect>, u32), InputError> {
        // The tests report screens themselves, at once
        Err(InputError::Unsupported("fake screens"))
    }
    fn capture(
        &self,
        switch: Arc<Mutex<Switch>>,
        sink: EmitSink,
    ) -> Result<Box<dyn std::any::Any + Send>, InputError> {
        *self.capture.lock().unwrap() = Some((switch, sink));
        Ok(Box::new(()))
    }
    fn injector(&self) -> Result<Box<dyn Injector>, InputError> {
        Ok(Box::new(Recorder(Arc::clone(&self.injected))))
    }
}

impl FakeInput {
    /// One local event through the switch, as the capture thread would
    fn feed(&self, event: InputEvent) -> Decision {
        let mut capture = self.capture.lock().unwrap();
        let (switch, sink) = capture.as_mut().unwrap();
        let mut out = Vec::new();
        let decision = switch::lock(switch).handle(event, &mut out);
        for emit in out {
            sink(emit);
        }
        decision
    }

    /// Push the pointer from `at` by (`dx`, `dy`)
    fn push(&self, at: (i32, i32), dx: f64, dy: f64) -> Decision {
        self.feed(InputEvent::Motion {
            at: Point::new(at.0, at.1),
            dx,
            dy,
        })
    }

    /// Press or release a key
    fn key(&self, usage: u16, down: bool) -> Decision {
        self.feed(InputEvent::Key { usage, down })
    }

    /// Devices the switch knows of
    fn devices(&self) -> usize {
        let capture = self.capture.lock().unwrap();
        let (switch, _) = capture.as_ref().unwrap();
        switch::lock(switch).world().devices().len()
    }
}

/// An engine with its event stream, data directory and fake input
struct TestEngine {
    /// The engine
    engine: Engine,
    /// Its events
    events: mpsc::UnboundedReceiver<EngineEvent>,
    /// Its data directory
    dir: TempDir,
    /// Its keyboard and mouse
    input: Arc<FakeInput>,
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
        let input = Arc::new(FakeInput::default());
        let backend = Arc::clone(&input) as Arc<dyn InputBackend>;
        let (engine, events) = Engine::launch(
            GroupStore::new(&dir.0),
            transport,
            info,
            None,
            None,
            backend,
        )
        .unwrap();
        Self {
            engine,
            events,
            dir,
            input,
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

impl TestEngine {
    /// Report one display of `width`x`height` at `scale` percent
    fn screens(&self, width: i32, height: i32, scale: u32) {
        let displays = vec![lanroam_input::Rect::new(0, 0, width, height)];
        let msg = Msg::Screens { displays, scale };
        self.engine.inner.inbox.send(msg).unwrap();
    }
}

/// Displays and placements spread through the group: the joiner lands
/// right of its sponsor, and moving it shows up everywhere
#[tokio::test]
async fn layout_syncs() {
    let (mut a, b) = (TestEngine::start(), TestEngine::start());
    a.sees(&[&b]);
    b.sees(&[&a]);
    a.screens(2560, 1440, 100);
    b.screens(2880, 1620, 150);
    join(&b, &mut a).await;
    let (fa, fb) = (a.fp(), b.fp());
    let placed = |status: &Status, fp: &str, at: Point| {
        status.doc.as_ref().is_some_and(|doc| {
            doc.devices[fp].placement.as_ref().map(|p| p.at) == Some(at)
                && !doc.devices[fp].profile.displays.is_empty()
        })
    };
    for engine in [&a, &b] {
        engine
            .until("both placed side by side", |s| {
                placed(s, &fa, Point::new(0, 0)) && placed(s, &fb, Point::new(2560, 0))
            })
            .await;
    }
    let edges = crate::layout::world(&a.engine.group().unwrap()).shared_edges();
    assert_eq!(edges.len(), 1);
    assert_eq!((edges[0].from, edges[0].to), (0.0, 1080.0));

    // b moves itself below a; a sees it
    let below = Spot::Beside {
        side: Edge::Bottom,
        anchor: fa.clone(),
        offset: 100,
    };
    b.engine.place(&fb, below).await.unwrap();
    a.until("b below a", |s| placed(s, &fb, Point::new(100, 1440)))
        .await;

    // Overlapping spots are refused
    let refused = a.engine.place(&fb, Spot::At(Point::new(10, 10))).await;
    assert!(
        matches!(refused, Err(EngineError::Layout(LayoutError::Overlap(..)))),
        "{refused:?}"
    );
}

impl TestEngine {
    /// Wait for a control event
    async fn expect_control(&mut self, want: ControlEvent) {
        let what = format!("{want:?}");
        self.expect(&what, |event| match event {
            EngineEvent::Control(got) if *got == want => Some(()),
            _ => None,
        })
        .await;
    }

    /// Wait until the injector recorded `want` (and return everything so
    /// far)
    async fn injected(&self, want: &Injected) -> Vec<Injected> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let injected = self.input.injected.lock().unwrap().clone();
            if injected.contains(want) {
                return injected;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{want:?} not injected within {WAIT:?}: {injected:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Wait until the switch knows `n` devices
    async fn until_devices(&self, n: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while self.input.devices() != n {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{n} devices within {WAIT:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// This device's name
    fn name(&self) -> String {
        self.engine.info().name.clone()
    }
}

/// a | b | c, each one 1000x1000 display, all linked, every switch aware of
/// all three
async fn row_of_three() -> (TestEngine, TestEngine, TestEngine) {
    let (mut a, mut b, c) = (
        TestEngine::start(),
        TestEngine::start(),
        TestEngine::start(),
    );
    for (engine, others) in [(&a, [&b, &c]), (&b, [&a, &c]), (&c, [&a, &b])] {
        engine.sees(&others);
        engine.screens(1000, 1000, 100);
    }
    join(&b, &mut a).await;
    join(&c, &mut b).await;
    for engine in [&a, &b, &c] {
        engine.until_devices(3).await;
    }
    (a, b, c)
}

/// The pointer walks a → b → c and back; each device gets its input, and
/// a hop is not reported as coming home
#[tokio::test]
async fn control_walks_across_devices() {
    let (mut a, b, c) = row_of_three().await;
    let (b_name, c_name) = (b.name(), c.name());

    a.input.push((999, 500), 5.0, 0.0);
    b.injected(&Injected::Move(Point::new(1, 500))).await;
    a.expect_control(ControlEvent::Controlling {
        name: b_name.clone(),
    })
    .await;
    a.input.key(0x04, true);
    a.input.key(0x04, false);
    b.injected(&Injected::Key(0x04, false)).await;

    a.input.push((0, 0), 1000.0, 0.0);
    c.injected(&Injected::Move(Point::new(1, 500))).await;
    a.expect_control(ControlEvent::Controlling { name: c_name })
        .await;
    a.input.push((0, 0), -5.0, 0.0);
    b.injected(&Injected::Move(Point::new(998, 500))).await;

    let decision = a.input.push((0, 0), -1000.0, 0.0);
    assert!(decision.cursor.is_some(), "back home: {decision:?}");
    let mut hops = Vec::new();
    a.expect("home", |event| match event {
        EngineEvent::Control(ControlEvent::Home) => Some(()),
        EngineEvent::Control(other) => {
            hops.push(other.clone());
            None
        }
        _ => None,
    })
    .await;
    assert_eq!(hops, [ControlEvent::Controlling { name: b_name }]);
}

/// A second controller preempts the first, which gets its keys released
/// and control back; local input then takes the device back from the
/// second
#[tokio::test]
async fn preemption_and_takeover() {
    let (mut a, mut b, mut c) = row_of_three().await;
    let (a_name, b_name, c_name) = (a.name(), b.name(), c.name());

    a.input.push((999, 500), 5.0, 0.0);
    a.input.key(0x04, true);
    b.injected(&Injected::Key(0x04, true)).await;
    b.expect_control(ControlEvent::ControlledBy { name: a_name })
        .await;

    // c comes in from the right
    c.input.push((0, 500), -5.0, 0.0);
    let injected = b.injected(&Injected::Move(Point::new(998, 500))).await;
    assert!(
        injected.contains(&Injected::Key(0x04, false)),
        "{injected:?}"
    );
    a.expect_control(ControlEvent::LetGo {
        name: b_name.clone(),
        reason: crate::protocol::released::PREEMPTED.into(),
    })
    .await;
    let decision = a.input.key(0x05, true);
    assert!(decision.cursor.is_some(), "a comes home at its next input");

    // Someone at b touches its keyboard
    b.input.key(0x06, true);
    b.expect_control(ControlEvent::TookBack { name: c_name })
        .await;
    c.expect_control(ControlEvent::LetGo {
        name: b_name,
        reason: crate::protocol::released::LOCAL_INPUT.into(),
    })
    .await;
}

/// A controller that vanishes gets what it held released
#[tokio::test]
async fn lost_controller_is_released() {
    let (a, mut b, _c) = row_of_three().await;
    let a_name = a.name();
    a.input.push((999, 500), 5.0, 0.0);
    a.input.key(0x04, true);
    b.injected(&Injected::Key(0x04, true)).await;
    a.engine.shutdown().await;
    b.injected(&Injected::Key(0x04, false)).await;
    b.expect_control(ControlEvent::Freed { name: a_name }).await;
}
