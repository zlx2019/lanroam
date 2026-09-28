//! Engine tests: several engines on loopback, with discovery fed by hand.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use lan_kit::DeviceIdentity;
use lanroam_clipboard::{Content, Image, MemoryClipboard};
use lanroam_input::inject::Injector;
use lanroam_input::keymap::usage;
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

/// What the fake drag and drop was asked to do
#[derive(Debug, Clone, PartialEq, Eq)]
enum DragCall {
    /// A press here
    Pressed,
    /// Probe with this id
    Probe(u64),
    /// Probe over
    Unprobe,
    /// Armed with this id, at, with these files, all there or not
    Arm(u64, Point, Vec<std::path::PathBuf>, bool),
    /// What this id carries: how many files and folders
    Listed(u64, usize),
    /// Cancel this id
    Cancel(u64),
    /// The files of this id are there, or not coming
    Deliver(u64, bool),
}

/// Drag and drop driven by the test: probes find its files (set with
/// [`FakeDrag::holds`]), cancelling succeeds at once, and so does arming
/// unless the test holds it back ([`FakeDrag::arm_later`])
#[derive(Default)]
struct FakeDrag(Arc<FakeDragState>);

/// What [`FakeDrag`] and its running side share
#[derive(Default)]
struct FakeDragState {
    /// The files a probe finds
    files: Mutex<Vec<std::path::PathBuf>>,
    /// Calls so far
    calls: Mutex<Vec<DragCall>>,
    /// Arming waits for [`FakeDrag::arm_now`]: the id waiting, if any
    later: Mutex<Option<Option<u64>>>,
    /// Where events go, once started
    sink: Mutex<Option<Arc<DragSink>>>,
    /// A drag armed before its files are there drops as soon as the button
    /// goes up (it promises them, as on macOS and Windows)
    early: std::sync::atomic::AtomicBool,
}

impl FakeDrag {
    /// Let probes find `files` from now on
    fn holds(&self, files: &[std::path::PathBuf]) {
        *self.0.files.lock().unwrap() = files.to_vec();
    }

    /// Calls so far
    fn calls(&self) -> Vec<DragCall> {
        self.0.calls.lock().unwrap().clone()
    }

    /// Drop as soon as the button goes up from now on, files there or not
    fn drop_early(&self) {
        self.0
            .early
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Hold arming back until [`Self::arm_now`]
    fn arm_later(&self) {
        *self.0.later.lock().unwrap() = Some(None);
    }

    /// Report the drag held back as armed, and arm at once from now on
    fn arm_now(&self) {
        let waiting = self.0.later.lock().unwrap().take().flatten();
        let sink = self.0.sink.lock().unwrap().clone();
        if let (Some(id), Some(sink)) = (waiting, sink) {
            sink(DragEvent::Armed { id });
        }
    }
}

impl DragBackend for FakeDrag {
    fn start(&self, sink: DragSink) -> Result<Box<dyn Dragging>, String> {
        *self.0.sink.lock().unwrap() = Some(Arc::new(sink));
        Ok(Box::new(FakeDragging(Arc::clone(&self.0))))
    }
}

/// The running side of [`FakeDrag`]
struct FakeDragging(Arc<FakeDragState>);

impl FakeDragging {
    /// Note a call
    fn note(&self, call: DragCall) {
        self.0.calls.lock().unwrap().push(call);
    }

    /// Report `event`
    fn report(&self, event: DragEvent) {
        let sink = self.0.sink.lock().unwrap().clone();
        if let Some(sink) = sink {
            sink(event);
        }
    }
}

impl Dragging for FakeDragging {
    fn pressed(&self) {
        self.note(DragCall::Pressed);
    }
    fn probe(&self, id: u64) {
        self.note(DragCall::Probe(id));
        let files = self.0.files.lock().unwrap().clone();
        self.report(DragEvent::Probed { id, files });
    }
    fn unprobe(&self) {
        self.note(DragCall::Unprobe);
    }
    fn arm(&self, id: u64, at: Point, paths: Vec<std::path::PathBuf>, ready: bool) {
        self.note(DragCall::Arm(id, at, paths, ready));
        let mut later = self.0.later.lock().unwrap();
        if let Some(waiting) = later.as_mut() {
            *waiting = Some(id);
            return;
        }
        drop(later);
        self.report(DragEvent::Armed { id });
    }
    fn listed(&self, id: u64, entries: Vec<lanroam_dnd::Listed>) {
        self.note(DragCall::Listed(id, entries.len()));
    }
    fn cancel(&self, id: u64) {
        self.note(DragCall::Cancel(id));
        self.report(DragEvent::Cancelling { id });
    }
    fn drops_early(&self) -> bool {
        self.0.early.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn deliver(&self, id: u64, ok: bool) {
        self.note(DragCall::Deliver(id, ok));
    }
}

/// A keyboard and mouse driven by the test
#[derive(Default)]
struct FakeInput {
    /// The OS refuses the capture (a missing permission)
    denied: std::sync::atomic::AtomicBool,
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
        if self.denied.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(InputError::PermissionDenied("grant it".into()));
        }
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

    /// Press or release the left button
    fn left(&self, down: bool) -> Decision {
        self.feed(InputEvent::Button {
            button: MouseButton::Left,
            down,
        })
    }

    /// Whether the switch holds the pointer for a waiting drop
    fn awaiting_drop(&self) -> bool {
        let capture = self.capture.lock().unwrap();
        let (switch, _) = capture.as_ref().unwrap();
        switch::lock(switch).is_awaiting_drop()
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
    /// Its clipboard
    clipboard: Arc<MemoryClipboard>,
    /// Its drag and drop
    drag: Arc<FakeDrag>,
}

impl TestEngine {
    /// An engine in a fresh data directory
    fn start() -> Self {
        Self::start_in(TempDir::new())
    }

    /// An engine in `dir`, keeping the identity and group found there
    fn start_in(dir: TempDir) -> Self {
        Self::start_with(dir, FakeInput::default())
    }

    /// An engine in `dir` on the keyboard and mouse `input`
    fn start_with(dir: TempDir, input: FakeInput) -> Self {
        let identity = Arc::new(DeviceIdentity::load_or_create(&dir.0, &PROFILE).unwrap());
        let info = identity.peer_info();
        let transport = Arc::new(Transport::bind(identity, 0).unwrap());
        let input = Arc::new(input);
        let backend = Arc::clone(&input) as Arc<dyn InputBackend>;
        let clipboard = Arc::new(MemoryClipboard::new());
        let drag = Arc::new(FakeDrag::default());
        let (engine, events) = Engine::launch(
            GroupStore::new(&dir.0),
            Some(dir.0.clone()),
            transport,
            info,
            None,
            None,
            backend,
            Arc::clone(&clipboard) as Arc<dyn Clipboard>,
            Arc::clone(&drag) as Arc<dyn DragBackend>,
        )
        .unwrap();
        Self {
            engine,
            events,
            dir,
            input,
            clipboard,
            drag,
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
        .connect(&a.as_peer(), &c.engine.info(), Purpose::Member)
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
        .connect(&a.as_peer(), &stranger.engine.info(), Purpose::Member)
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
    assert_eq!(
        (edges[0].first_span, edges[0].second_span),
        ((0.0, 1440.0), (0.0, 1080.0))
    );

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
        fingerprint: b.fp(),
    })
    .await;
    a.input.key(0x04, true);
    a.input.key(0x04, false);
    b.injected(&Injected::Key(0x04, false)).await;

    a.input.push((0, 0), 1000.0, 0.0);
    c.injected(&Injected::Move(Point::new(1, 500))).await;
    a.expect_control(ControlEvent::Controlling {
        name: c_name,
        fingerprint: c.fp(),
    })
    .await;
    a.input.push((0, 0), -5.0, 0.0);
    b.injected(&Injected::Move(Point::new(998, 500))).await;

    let decision = a.input.push((0, 0), -1000.0, 0.0);
    let Some(switch::CursorAction::Release(back)) = decision.cursor else {
        panic!("not back home: {decision:?}");
    };
    let mut hops = Vec::new();
    let at = a
        .expect("home", |event| match event {
            EngineEvent::Control(ControlEvent::Home { at, .. }) => Some(*at),
            EngineEvent::Control(other) => {
                hops.push(other.clone());
                None
            }
            _ => None,
        })
        .await;
    // The pointer comes back where the switch put it
    assert_eq!(at, back);
    assert_eq!(
        hops,
        [ControlEvent::Controlling {
            name: b_name,
            fingerprint: b.fp(),
        }]
    );
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
    b.expect_control(ControlEvent::ControlledBy {
        name: a_name,
        fingerprint: a.fp(),
        at: Point::new(1, 500),
    })
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
        fingerprint: b.fp(),
        reason: crate::protocol::released::PREEMPTED.into(),
    })
    .await;
    let decision = a.input.key(0x05, true);
    assert!(decision.cursor.is_some(), "a comes home at its next input");

    // Someone at b touches its keyboard
    b.input.key(0x06, true);
    b.expect_control(ControlEvent::TookBack {
        name: c_name,
        fingerprint: c.fp(),
    })
    .await;
    c.expect_control(ControlEvent::LetGo {
        name: b_name,
        fingerprint: b.fp(),
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
    b.expect_control(ControlEvent::Freed {
        name: a_name,
        fingerprint: a.fp(),
    })
    .await;
}

/// Hotkeys act on the whole group: number 3 jumps straight to c (mid
/// display), and pausing is reported
#[tokio::test]
async fn hotkeys_jump_across_the_group() {
    use lanroam_input::keymap::usage;

    let (mut a, _b, mut c) = row_of_three().await;
    let (a_name, c_name) = (a.name(), c.name());
    let chord = |k: u16| {
        a.input.key(usage::LEFT_CTRL, true);
        a.input.key(usage::LEFT_ALT, true);
        a.input.key(k, true);
        a.input.key(k, false);
        a.input.key(usage::LEFT_ALT, false);
        a.input.key(usage::LEFT_CTRL, false);
    };
    chord(usage::DIGIT_1 + 2);
    c.injected(&Injected::Move(Point::new(500, 500))).await;
    chord(usage::ESCAPE);
    a.expect_control(ControlEvent::Controlling {
        name: c_name,
        fingerprint: c.fp(),
    })
    .await;
    a.expect_control(ControlEvent::Paused { on: true }).await;
    // c saw the modifiers (pressed while it was controlled) come and go,
    // never the hotkey's own key
    c.expect_control(ControlEvent::Freed {
        name: a_name,
        fingerprint: a.fp(),
    })
    .await;
    let injected = c.injected(&Injected::Key(usage::LEFT_CTRL, false)).await;
    assert!(
        !injected.contains(&Injected::Key(usage::ESCAPE, true)),
        "{injected:?}"
    );
}

/// The sponsor's user turns a join down: the joiner hears why while it is
/// still typing the PIN
#[tokio::test]
async fn rejected_join() {
    let (mut a, b) = (TestEngine::start(), TestEngine::start());
    let joining = b.engine.join(&a.as_peer()).await.unwrap();
    a.expect("PIN", |event| {
        matches!(event, EngineEvent::JoinPin { .. }).then_some(())
    })
    .await;
    a.engine.reject_join();
    let reason = tokio::time::timeout(WAIT, joining.ended()).await.unwrap();
    assert_eq!(reason.as_deref(), Some(join_denied::REJECTED));
    let admitted = a
        .expect("the end of the join", |event| match event {
            EngineEvent::JoinEnded { admitted, .. } => Some(*admitted),
            _ => None,
        })
        .await;
    assert!(!admitted);
    assert!(b.engine.group().is_none());
}

/// Every attempt shows the PIN again with the attempts left, so the
/// sponsor can count down
#[tokio::test]
async fn attempts_count_down_on_the_sponsor() {
    use crate::group::join::PIN_ATTEMPTS;

    let (mut a, b) = (TestEngine::start(), TestEngine::start());
    let mut joining = b.engine.join(&a.as_peer()).await.unwrap();
    let shown = |event: &EngineEvent| match event {
        EngineEvent::JoinPin {
            pin, attempts_left, ..
        } => Some((pin.clone(), *attempts_left)),
        _ => None,
    };
    let (pin, left) = a.expect("PIN", shown).await;
    assert_eq!(left, PIN_ATTEMPTS);
    let wrong = if pin == "000000" { "111111" } else { "000000" };
    assert!(joining.answer(wrong).await.unwrap().is_none());
    assert_eq!(
        a.expect("PIN again", shown).await,
        (pin.clone(), PIN_ATTEMPTS - 1)
    );
    assert!(joining.answer(&pin).await.unwrap().is_some());
}

/// A new name is saved with the identity and reaches the other members
#[tokio::test]
async fn rename_reaches_the_group() {
    let (mut a, b) = (TestEngine::start(), TestEngine::start());
    a.sees(&[&b]);
    b.sees(&[&a]);
    join(&b, &mut a).await;
    b.until_online(&[a.fp()]).await;

    a.engine.rename("  Studio  ").await.unwrap();
    assert_eq!(a.engine.info().name, "Studio");
    let fp = a.fp();
    b.until("the new name in the group", |status| {
        status
            .doc
            .as_ref()
            .and_then(|doc| doc.devices.get(&fp))
            .is_some_and(|record| record.profile.name == "Studio")
    })
    .await;
    let saved = DeviceIdentity::load_or_create(&a.dir.0, &PROFILE).unwrap();
    assert_eq!(saved.peer_info().name, "Studio");

    for bad in ["   ", &"x".repeat(MAX_NAME_CHARS + 1)] {
        assert!(matches!(
            a.engine.rename(bad).await,
            Err(EngineError::InvalidName)
        ));
    }
}

/// Requests from the tray reach the switch and act at the next local
/// event, like their hotkeys
#[tokio::test]
async fn requests_from_the_tray() {
    let (mut a, _b, c) = row_of_three().await;
    let c_name = c.name();
    a.engine.request(Request::Jump(c.fp())).unwrap();
    // Answered after the request: it is in the switch by then
    a.engine.input_status().await.unwrap();
    a.input.push((500, 500), 1.0, 0.0);
    c.injected(&Injected::Move(Point::new(500, 500))).await;
    a.expect_control(ControlEvent::Controlling {
        name: c_name,
        fingerprint: c.fp(),
    })
    .await;

    a.engine.request(Request::Pause).unwrap();
    a.engine.input_status().await.unwrap();
    a.input.push((500, 500), 1.0, 0.0);
    a.expect_control(ControlEvent::Paused { on: true }).await;
}

/// A capture the OS refused starts once the permission is granted
#[tokio::test]
async fn input_restarts_after_a_permission() {
    use std::sync::atomic::Ordering;

    let input = FakeInput::default();
    input.denied.store(true, Ordering::SeqCst);
    let a = TestEngine::start_with(TempDir::new(), input);
    let status = a.engine.input_status().await.unwrap();
    assert!(status.capture.is_err());
    assert_eq!(status.injection, Ok(()));
    assert!(a.engine.restart_input().await.unwrap().capture.is_err());

    a.input.denied.store(false, Ordering::SeqCst);
    let status = a.engine.restart_input().await.unwrap();
    assert_eq!((status.capture, status.injection), (Ok(()), Ok(())));
    assert!(a.input.capture.lock().unwrap().is_some());
}

/// Identify reaches every online member, and the asker too
#[tokio::test]
async fn identify_reaches_every_member() {
    let (mut a, mut b, mut c) = group_of_three().await;
    a.engine.identify().unwrap();
    for engine in [&mut a, &mut b, &mut c] {
        engine
            .expect("identify", |event| {
                matches!(event, EngineEvent::Identify).then_some(())
            })
            .await;
    }
}

/// Locking the pointer while controlling another device tells that device,
/// which shows it where the user looks
#[tokio::test]
async fn a_lock_reaches_the_controlled_device() {
    use lanroam_input::keymap::usage;

    let (mut a, mut b, _c) = row_of_three().await;
    let a_name = a.name();
    a.input.push((999, 500), 5.0, 0.0);
    b.injected(&Injected::Move(Point::new(1, 500))).await;
    a.input.key(usage::SCROLL_LOCK, true);
    a.input.key(usage::SCROLL_LOCK, false);
    a.expect_control(ControlEvent::Locked { on: true }).await;
    b.expect_control(ControlEvent::LockedHere {
        name: a_name,
        fingerprint: a.fp(),
        on: true,
    })
    .await;
}

/// Wait for an event `pick` accepts, feeding still motions to `input`
/// meanwhile: a request only runs at the next local input
async fn expect_nudging<T>(
    events: &mut mpsc::UnboundedReceiver<EngineEvent>,
    input: &FakeInput,
    what: &str,
    mut pick: impl FnMut(&EngineEvent) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        input.push((500, 500), 0.0, 0.0);
        if let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(50), events.recv()).await
            && let Some(found) = pick(&event)
        {
            return found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no {what} within {WAIT:?}"
        );
    }
}

/// Locking or pausing from the tray of a controlled device acts on its
/// controller: the user works that device with the controller's keyboard
/// and mouse
#[tokio::test]
async fn requests_reach_the_controller() {
    let (mut a, mut b, _c) = row_of_three().await;
    let a_name = a.name();
    a.input.push((999, 500), 5.0, 0.0);
    b.injected(&Injected::Move(Point::new(1, 500))).await;

    b.engine.request(Request::Lock).unwrap();
    expect_nudging(&mut a.events, &a.input, "a lock", |event| {
        matches!(
            event,
            EngineEvent::Control(ControlEvent::Locked { on: true })
        )
        .then_some(())
    })
    .await;
    b.expect_control(ControlEvent::LockedHere {
        name: a_name.clone(),
        fingerprint: a.fp(),
        on: true,
    })
    .await;

    b.engine.request(Request::Pause).unwrap();
    expect_nudging(&mut a.events, &a.input, "a pause", |event| {
        matches!(
            event,
            EngineEvent::Control(ControlEvent::Paused { on: true })
        )
        .then_some(())
    })
    .await;
    b.expect_control(ControlEvent::Freed {
        name: a_name,
        fingerprint: a.fp(),
    })
    .await;
}

/// The link to the device controlled dropping is reported
#[tokio::test]
async fn a_lost_link_is_reported() {
    let (mut a, b, _c) = row_of_three().await;
    let b_name = b.name();
    a.input.push((999, 500), 5.0, 0.0);
    b.injected(&Injected::Move(Point::new(1, 500))).await;
    b.engine.shutdown().await;
    a.expect_control(ControlEvent::Lost {
        name: b_name,
        fingerprint: b.fp(),
    })
    .await;
}

/// New input settings are saved and take effect: another pause hotkey
/// pauses, the old one types; unusable settings are refused
#[tokio::test]
async fn input_settings_take_effect() {
    use lanroam_input::config::{Chord, Hotkeys, Mods};
    use lanroam_input::keymap::usage;

    const KEY_P: u16 = 0x13;
    let mut a = TestEngine::start();
    let ctrl_shift = Mods {
        ctrl: true,
        shift: true,
        ..Mods::default()
    };
    let settings = InputSettings {
        hotkeys: Hotkeys {
            pause: Chord::new(ctrl_shift, KEY_P),
            ..Hotkeys::default()
        },
        ..InputSettings::default()
    };
    a.engine.set_input_settings(settings.clone()).await.unwrap();
    assert_eq!(a.engine.input_settings(), settings);
    assert_eq!(InputSettings::load(&a.dir.0), settings);

    a.engine.input_status().await.unwrap();
    a.input.key(usage::LEFT_CTRL, true);
    a.input.key(usage::LEFT_SHIFT, true);
    a.input.key(KEY_P, true);
    a.expect_control(ControlEvent::Paused { on: true }).await;

    let shift_only = InputSettings {
        hotkeys: Hotkeys {
            lock: Chord::new(
                Mods {
                    shift: true,
                    ..Mods::default()
                },
                usage::KEY_L,
            ),
            ..Hotkeys::default()
        },
        ..InputSettings::default()
    };
    assert!(matches!(
        a.engine.set_input_settings(shift_only).await,
        Err(EngineError::InvalidSettings)
    ));
    assert_eq!(a.engine.input_settings(), settings);
}

/// A closed edge reaches the whole group, and the pointer no longer
/// crosses it
#[tokio::test]
async fn edge_settings_reach_the_group() {
    use lanroam_input::config::EdgeSettings;

    let (a, mut b, _c) = row_of_three().await;
    let closed = EdgeSettings {
        crossable: false,
        ..EdgeSettings::default()
    };
    a.engine.set_edge(&a.fp(), &b.fp(), closed).await.unwrap();
    // Handled by a's input actor once this answers
    a.engine.input_status().await.unwrap();
    let decision = a.input.push((999, 500), 5.0, 0.0);
    assert_eq!(decision.cursor, None, "crossed a closed edge");
    let (a_fp, b_fp) = (a.fp(), b.fp());
    b.expect("the edge", |event| match event {
        EngineEvent::Group(Some(doc)) => (doc.edge(&a_fp, &b_fp) == closed).then_some(()),
        _ => None,
    })
    .await;
    assert!(matches!(
        a.engine.set_edge(&a.fp(), &a.fp(), closed).await,
        Err(EngineError::InvalidSettings)
    ));
}

/// Recording takes the next combination from this device's keyboard, or
/// from the controlling device's while controlled
#[tokio::test]
async fn recording_takes_either_keyboard() {
    use lanroam_input::config::{Chord, Mods};
    use lanroam_input::keymap::usage;

    const SPACE: u16 = 0x2C;
    let meta_space = Chord::new(
        Mods {
            meta: true,
            ..Mods::default()
        },
        SPACE,
    );
    let (mut a, mut b, _c) = row_of_three().await;
    let recorded = |event: &EngineEvent| match event {
        EngineEvent::Recorded(chord) => Some(*chord),
        _ => None,
    };

    a.engine.record(true).unwrap();
    a.engine.input_status().await.unwrap();
    a.input.key(usage::LEFT_META, true);
    a.input.key(SPACE, true);
    assert_eq!(a.expect("a recording", recorded).await, Some(meta_space));
    a.input.key(SPACE, false);
    a.input.key(usage::LEFT_META, false);

    a.input.push((999, 500), 5.0, 0.0);
    b.injected(&Injected::Move(Point::new(1, 500))).await;
    b.engine.record(true).unwrap();
    b.engine.input_status().await.unwrap();
    a.input.key(usage::LEFT_META, true);
    a.input.key(SPACE, true);
    assert_eq!(b.expect("b recording", recorded).await, Some(meta_space));
}

/// A combination kept local, pressed while controlling another device,
/// is pressed on this one: the other gets its modifier back up
#[tokio::test]
async fn kept_combinations_stay_here() {
    use lanroam_input::config::{Chord, Mods};
    use lanroam_input::keymap::usage;

    const SPACE: u16 = 0x2C;
    let (a, b, _c) = row_of_three().await;
    let meta = Mods {
        meta: true,
        ..Mods::default()
    };
    let settings = InputSettings {
        keep_local: vec![Chord::new(meta, SPACE)],
        ..InputSettings::default()
    };
    a.engine.set_input_settings(settings).await.unwrap();
    a.input.push((999, 500), 5.0, 0.0);
    b.injected(&Injected::Move(Point::new(1, 500))).await;
    a.input.key(usage::LEFT_META, true);
    b.injected(&Injected::Key(usage::LEFT_META, true)).await;
    a.input.key(SPACE, true);
    b.injected(&Injected::Key(usage::LEFT_META, false)).await;
    let here = a.injected(&Injected::Key(SPACE, true)).await;
    assert!(
        here.contains(&Injected::Key(usage::LEFT_META, true)),
        "{here:?}"
    );
    let there = b.input.injected.lock().unwrap().clone();
    assert!(!there.contains(&Injected::Key(SPACE, true)), "{there:?}");
}

/// A device's pointer speed reaches its controller, which moves the
/// pointer there faster; scrolling is scaled and turned around where it is
/// replayed; media keys kept here work this machine
#[tokio::test]
async fn pointer_wheel_and_media_settings() {
    use lanroam_input::config::{MediaKeys, Scrolling};
    use lanroam_input::keymap::usage;
    use lanroam_input::switch::Verdict;

    let (mut a, b, _c) = row_of_three().await;
    b.engine.set_pointer_speed(200).await.unwrap();
    let b_fp = b.fp();
    a.expect("b's pointer speed", |event| match event {
        EngineEvent::Group(Some(doc)) => doc
            .devices
            .get(&b_fp)
            .is_some_and(|record| record.profile.pointer_speed == 200)
            .then_some(()),
        _ => None,
    })
    .await;
    // The layout reached a's input actor before the group event
    a.engine.input_status().await.unwrap();
    let scrolling = InputSettings {
        scrolling: Scrolling {
            speed: 50,
            reverse: true,
        },
        ..InputSettings::default()
    };
    b.engine.set_input_settings(scrolling).await.unwrap();

    a.input.push((999, 500), 5.0, 0.0);
    b.injected(&Injected::Move(Point::new(1, 500))).await;
    a.input.push((0, 0), 10.0, 0.0);
    b.injected(&Injected::Move(Point::new(21, 500))).await;
    a.input.feed(InputEvent::Wheel { dx: 0, dy: -120 });
    b.injected(&Injected::Wheel(0, 60)).await;

    let media = InputSettings {
        media_keys: MediaKeys::Local,
        ..InputSettings::default()
    };
    a.engine.set_input_settings(media).await.unwrap();
    a.engine.input_status().await.unwrap();
    assert_eq!(a.input.key(usage::VOLUME_UP, true).verdict, Verdict::Pass);
    assert_eq!(a.input.key(0x04, true).verdict, Verdict::Swallow);

    assert!(matches!(
        b.engine.set_pointer_speed(10).await,
        Err(EngineError::InvalidSettings)
    ));
}

/// Long enough for a hand-over that should not happen to have happened
const SETTLE: Duration = Duration::from_millis(300);

impl TestEngine {
    /// Wait until the clipboard holds `want`
    async fn until_clipboard(&self, want: &Content) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let held = self.clipboard.content();
            if held.as_ref() == Some(want) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{want:?} not on the clipboard within {WAIT:?}: {held:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Wait until the group knows what `fp` shares of its clipboard
    async fn until_share(&self, fp: &str, share: ClipboardShare) {
        self.until("the clipboard settings", |status| {
            status
                .doc
                .as_ref()
                .is_some_and(|doc| doc.devices[fp].profile.clipboard == share)
        })
        .await;
    }
}

/// Controlling b from a: the pointer enters b from the left
async fn enter_b(a: &TestEngine, b: &TestEngine) {
    a.input.push((999, 500), 5.0, 0.0);
    b.injected(&Injected::Move(Point::new(1, 500))).await;
}

/// The pointer on b comes back to a
async fn back_to_a(a: &mut TestEngine) {
    a.input.push((0, 0), -1000.0, 0.0);
    a.expect("home", |event| {
        matches!(event, EngineEvent::Control(ControlEvent::Home { .. })).then_some(())
    })
    .await;
}

/// Text of the tests
fn text(text: &str) -> Content {
    Content::Text(text.into())
}

/// What a device copies on its way in arrives; what is copied there comes
/// back on the way out
#[tokio::test]
async fn clipboard_follows_the_pointer() {
    let (mut a, b, _c) = row_of_three().await;
    a.clipboard.copy(text("copied on a"));
    enter_b(&a, &b).await;
    b.until_clipboard(&text("copied on a")).await;

    b.clipboard.copy(text("copied on b"));
    back_to_a(&mut a).await;
    a.until_clipboard(&text("copied on b")).await;
}

/// Images go over as well, pixel for pixel
#[tokio::test]
async fn images_follow_the_pointer() {
    let (a, b, _c) = row_of_three().await;
    let rgba = (0..64 * 48 * 4).map(|i| (i % 251) as u8).collect();
    let image = Content::Image(Image {
        width: 64,
        height: 48,
        rgba,
    });
    a.clipboard.copy(image.clone());
    enter_b(&a, &b).await;
    b.until_clipboard(&image).await;
}

/// What b held before the pointer came is not the user's to carry: only a
/// copy made during the visit goes back
#[tokio::test]
async fn only_a_copy_made_there_comes_back() {
    let (mut a, b, _c) = row_of_three().await;
    b.clipboard.copy(text("on b all along"));
    enter_b(&a, &b).await;
    back_to_a(&mut a).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(a.clipboard.content(), None);
}

/// A password manager's copy never leaves; the next plain copy does
#[tokio::test]
async fn concealed_content_stays_home() {
    let (mut a, b, _c) = row_of_three().await;
    a.clipboard.copy_concealed(text("hunter2"));
    enter_b(&a, &b).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(b.clipboard.content(), None);

    back_to_a(&mut a).await;
    a.clipboard.copy(text("plain"));
    enter_b(&a, &b).await;
    b.until_clipboard(&text("plain")).await;
}

/// A device taking no images gets none (text still comes), and one that
/// shares nothing hands nothing over
#[tokio::test]
async fn clipboard_settings_are_kept() {
    let (mut a, b, _c) = row_of_three().await;
    let no_images = ClipboardShare {
        image: false,
        ..ClipboardShare::default()
    };
    b.engine.set_clipboard(no_images).await.unwrap();
    a.until_share(&b.fp(), no_images).await;
    let image = Content::Image(Image {
        width: 1,
        height: 1,
        rgba: vec![1, 2, 3, 4],
    });
    a.clipboard.copy(image);
    enter_b(&a, &b).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(b.clipboard.content(), None);
    back_to_a(&mut a).await;

    a.clipboard.copy(text("text is fine"));
    enter_b(&a, &b).await;
    b.until_clipboard(&text("text is fine")).await;
    back_to_a(&mut a).await;

    let off = ClipboardShare {
        on: false,
        ..ClipboardShare::default()
    };
    b.engine.set_clipboard(off).await.unwrap();
    a.until_share(&b.fp(), off).await;
    enter_b(&a, &b).await;
    b.clipboard.copy(text("stays on b"));
    back_to_a(&mut a).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(a.clipboard.content(), Some(text("text is fine")));
}

/// The pointer goes a → b → c: what was copied on b reaches c, through a
#[tokio::test]
async fn a_copy_is_passed_on() {
    let (a, b, c) = row_of_three().await;
    enter_b(&a, &b).await;
    b.clipboard.copy(text("copied on b"));
    a.input.push((0, 0), 1000.0, 0.0);
    c.until_clipboard(&text("copied on b")).await;
    a.until_clipboard(&text("copied on b")).await;
}

/// Files copied on a are fetched to b as the pointer comes, b's clipboard
/// holds them; files copied on b while the pointer is there come back
#[tokio::test]
async fn copied_files_come_along() {
    let (mut a, b, _c) = row_of_three().await;
    let photos = a.dir.0.join("photos");
    std::fs::create_dir_all(photos.join("inner")).unwrap();
    std::fs::write(photos.join("inner/b.jpg"), b"45").unwrap();
    let report = a.file_to_drag("report.pdf");
    a.clipboard.copy(Content::Files(vec![report, photos]));
    enter_b(&a, &b).await;
    let paths = b.until_files(&["photos", "report.pdf"]).await;
    assert_eq!(std::fs::read(&paths[1]).unwrap(), b"12345");
    assert_eq!(std::fs::read(paths[0].join("inner/b.jpg")).unwrap(), b"45");

    b.clipboard
        .copy(Content::Files(vec![b.file_to_drag("notes.txt")]));
    back_to_a(&mut a).await;
    let paths = a.until_files(&["notes.txt"]).await;
    assert_eq!(std::fs::read(&paths[0]).unwrap(), b"12345");
}

/// Files larger than a device fetches ahead stay where they are, and it
/// says so
#[tokio::test]
async fn copied_files_beyond_the_limit_stay() {
    let (a, mut b, _c) = row_of_three().await;
    let big = a.dir.0.join("big.bin");
    let size = (32 << 20) + 1;
    std::fs::File::create(&big).unwrap().set_len(size).unwrap();
    a.clipboard.copy(Content::Files(vec![big]));
    enter_b(&a, &b).await;
    let told = b
        .expect("files too large", |event| match event {
            EngineEvent::CopiedFiles(CopiedFiles::TooLarge {
                name, bytes, limit, ..
            }) => Some((name.clone(), *bytes, *limit)),
            _ => None,
        })
        .await;
    assert_eq!(told, ("big.bin".to_string(), size, 32 << 20));
    assert_eq!(b.clipboard.content(), None);
}

/// A device taking no files gets none
#[tokio::test]
async fn copied_files_stay_off_devices_without_them() {
    let (a, b, _c) = row_of_three().await;
    let no_files = ClipboardShare {
        files: false,
        ..ClipboardShare::default()
    };
    b.engine.set_clipboard(no_files).await.unwrap();
    a.until_share(&b.fp(), no_files).await;
    a.clipboard
        .copy(Content::Files(vec![a.file_to_drag("report.pdf")]));
    enter_b(&a, &b).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(b.clipboard.content(), None);
}

impl TestEngine {
    /// Wait until the clipboard holds files with these names; where they
    /// are
    async fn until_files(&self, want: &[&str]) -> Vec<std::path::PathBuf> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(Content::Files(paths)) = self.clipboard.content()
                && names(&paths) == want
            {
                return paths;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{want:?} not on the clipboard within {WAIT:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A file of this engine's to drag
    fn file_to_drag(&self, name: &str) -> std::path::PathBuf {
        let path = self.dir.0.join(name);
        std::fs::write(&path, b"12345").unwrap();
        path
    }

    /// Wait until the drag and drop was armed; where, with what
    async fn armed(&self) -> (Point, Vec<std::path::PathBuf>) {
        let calls = self.dragged(|call| matches!(call, DragCall::Arm(..))).await;
        calls
            .into_iter()
            .find_map(|call| match call {
                DragCall::Arm(_, at, paths, _) => Some((at, paths)),
                _ => None,
            })
            .unwrap()
    }

    /// Wait until the drag and drop was asked for `want` (and return
    /// everything so far)
    async fn dragged(&self, want: impl Fn(&DragCall) -> bool) -> Vec<DragCall> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let calls = self.drag.calls();
            if calls.iter().any(&want) {
                return calls;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "drag call not made within {WAIT:?}: {calls:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Push the pointer by (`dx`, 0) until `crossed` holds, as a hand
    /// pushing against an edge would while the drag is asked about
    async fn push_until(&self, dx: f64, crossed: impl Fn(&Decision) -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if crossed(&self.input.push((999, 500), dx, 0.0)) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no crossing within {WAIT:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// The file names of `paths`
fn names(paths: &[std::path::PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

/// Whether a decision moved control elsewhere
fn parked(decision: &Decision) -> bool {
    decision.cursor == Some(switch::CursorAction::Park)
}

/// Whether a decision brought control home
fn released(decision: &Decision) -> bool {
    matches!(decision.cursor, Some(switch::CursorAction::Release(_)))
}

/// A drag of files held here goes along to b: b drags stand-ins with the
/// same names from where the pointer entered; small, it gives the files a
/// moment to arrive first, and drags them as they are
#[tokio::test]
async fn files_dragged_from_here_go_along() {
    let (a, b, _c) = row_of_three().await;
    a.drag.holds(&[a.file_to_drag("report.pdf")]);
    a.input.left(true);
    a.dragged(|call| *call == DragCall::Pressed).await;
    a.push_until(5.0, parked).await;
    let (at, paths) = b.armed().await;
    assert_eq!(at, Point::new(1, 500));
    assert_eq!(names(&paths), ["report.pdf"]);
    let ready = b
        .drag
        .calls()
        .into_iter()
        .any(|call| matches!(call, DragCall::Arm(.., true)));
    assert!(ready);
    b.injected(&Injected::Button(MouseButton::Left, true)).await;

    let decision = a.input.left(false);
    assert_eq!(decision.verdict, switch::Verdict::Pass);
    b.injected(&Injected::Button(MouseButton::Left, false))
        .await;
    // What lands is the file itself
    assert_eq!(std::fs::read(&paths[0]).unwrap(), b"12345");
    a.dragged(|call| *call == DragCall::Unprobe).await;
}

/// A drag that holds no files stays where it is
#[tokio::test]
async fn other_drags_stay() {
    let (mut a, _b, _c) = row_of_three().await;
    a.input.left(true);
    a.input.push((999, 500), 5.0, 0.0);
    a.dragged(|call| matches!(call, DragCall::Probe(_))).await;
    tokio::time::sleep(SETTLE).await;
    assert!(!parked(&a.input.push((999, 500), 5.0, 0.0)));
    a.input.left(false);
    a.input.push((999, 500), 5.0, 0.0);
    a.expect("control on b", |event| {
        matches!(
            event,
            EngineEvent::Control(ControlEvent::Controlling { .. })
        )
        .then_some(())
    })
    .await;
}

/// A drag of files stays at the edge of a device that takes no drags,
/// without asking what it holds
#[tokio::test]
async fn drags_stay_off_devices_without_them() {
    let (a, b, _c) = row_of_three().await;
    let off = FileShare { drag: false };
    b.engine.set_files(off).await.unwrap();
    let fp = b.fp();
    a.until("b's file settings", |status| {
        status
            .doc
            .as_ref()
            .is_some_and(|doc| doc.devices[&fp].profile.files == off)
    })
    .await;
    tokio::time::sleep(SETTLE).await;
    a.drag.holds(&[a.file_to_drag("report.pdf")]);
    a.input.left(true);
    a.dragged(|call| *call == DragCall::Pressed).await;
    for _ in 0..3 {
        assert!(!parked(&a.input.push((999, 500), 5.0, 0.0)));
        tokio::time::sleep(SETTLE).await;
    }
    let probed = a
        .drag
        .calls()
        .into_iter()
        .any(|call| matches!(call, DragCall::Probe(_)));
    assert!(!probed);
}

/// Files dragged on b come home with the pointer: b's drag ends there, and
/// this device drags them on
#[tokio::test]
async fn files_dragged_there_come_home() {
    let (a, b, _c) = row_of_three().await;
    enter_b(&a, &b).await;
    b.drag.holds(&[b.file_to_drag("photos.zip")]);
    a.input.left(true);
    b.dragged(|call| *call == DragCall::Pressed).await;
    a.push_until(-5.0, released).await;
    // b lets go of its drag (on its catcher)
    b.injected(&Injected::Button(MouseButton::Left, false))
        .await;
    let (_, paths) = a.armed().await;
    assert_eq!(names(&paths), ["photos.zip"]);
    a.injected(&Injected::Button(MouseButton::Left, true)).await;
    a.input.left(false);
    a.injected(&Injected::Button(MouseButton::Left, false))
        .await;
    assert_eq!(std::fs::read(&paths[0]).unwrap(), b"12345");
}

/// Esc cancels a drag carried to b: it refuses the drop, then the button
/// goes up there
#[tokio::test]
async fn esc_cancels_a_carried_drag() {
    let (a, b, _c) = row_of_three().await;
    a.drag.holds(&[a.file_to_drag("draft.txt")]);
    a.input.left(true);
    a.push_until(5.0, parked).await;
    b.injected(&Injected::Button(MouseButton::Left, true)).await;
    a.input.key(usage::ESCAPE, true);
    a.input.key(usage::ESCAPE, false);
    b.dragged(|call| matches!(call, DragCall::Cancel(1))).await;
    b.injected(&Injected::Button(MouseButton::Left, false))
        .await;
    // Nothing of Esc reaches b
    let injected = b.input.injected.lock().unwrap().clone();
    assert!(!injected.contains(&Injected::Key(usage::ESCAPE, true)));
    // The release ends the drag held here
    assert_eq!(a.input.left(false).verdict, switch::Verdict::Pass);
}

/// Esc cancels a drag carried home too: the button goes up where the
/// pointer is, which the injector here cannot know by itself
#[tokio::test]
async fn esc_cancels_a_drag_carried_home() {
    let (a, b, _c) = row_of_three().await;
    enter_b(&a, &b).await;
    b.drag.holds(&[b.file_to_drag("photos.zip")]);
    a.input.left(true);
    a.push_until(-5.0, released).await;
    a.injected(&Injected::Button(MouseButton::Left, true)).await;
    a.input.push((990, 480), -8.0, -20.0);
    a.input.key(usage::ESCAPE, true);
    a.dragged(|call| matches!(call, DragCall::Cancel(1))).await;
    let injected = a
        .injected(&Injected::Button(MouseButton::Left, false))
        .await;
    let up = injected.len() - 1;
    assert_eq!(injected[up - 1], Injected::Move(Point::new(990, 480)));
    // The release then goes nowhere
    assert_eq!(a.input.left(false).verdict, switch::Verdict::Swallow);
}

/// A folder goes over whole, empty folders included; released before it
/// is all there, its drop waits (pressed first, then let go), and b says
/// how far it is meanwhile
#[tokio::test]
async fn a_folder_goes_over_whole() {
    let (a, mut b, _c) = row_of_three().await;
    let folder = a.dir.0.join("shots");
    std::fs::create_dir_all(folder.join("2026/empty")).unwrap();
    let big: Vec<u8> = (0..24u32 << 20).map(|i| (i % 251) as u8).collect();
    std::fs::write(folder.join("2026/big.bin"), &big).unwrap();
    std::fs::write(folder.join("note.txt"), b"hi").unwrap();
    a.drag.holds(std::slice::from_ref(&folder));
    a.input.left(true);
    a.push_until(5.0, parked).await;
    // Let go at once: nothing is there yet
    a.input.left(false);
    let (_, paths) = b.armed().await;
    assert_eq!(names(&paths), ["shots"]);

    b.expect("the drop", |event| match event {
        EngineEvent::Receiving(Some(receiving)) => {
            assert_eq!((receiving.name.as_str(), receiving.count), ("shots", 1));
            assert_eq!(receiving.total, (24 << 20) + 2);
            None
        }
        EngineEvent::Receiving(None) => Some(()),
        _ => None,
    })
    .await;
    let injected = b
        .injected(&Injected::Button(MouseButton::Left, false))
        .await;
    let press = injected
        .iter()
        .position(|i| *i == Injected::Button(MouseButton::Left, true));
    assert!(press.is_some(), "{injected:?}");
    let landed = &paths[0];
    assert_eq!(std::fs::read(landed.join("2026/big.bin")).unwrap(), big);
    assert_eq!(std::fs::read(landed.join("note.txt")).unwrap(), b"hi");
    assert!(landed.join("2026/empty").is_dir());
}

/// Where the native side promises the files of a drag armed before they
/// are there (macOS, Windows), its release drops at once: the pointer is
/// free, and the files are delivered to the app they landed on once they
/// are all there; it learns what is coming first
#[tokio::test]
async fn an_early_drop_frees_the_pointer() {
    let (a, mut b, _c) = row_of_three().await;
    b.drag.drop_early();
    let folder = a.dir.0.join("shots");
    std::fs::create_dir_all(&folder).unwrap();
    let big: Vec<u8> = (0..24u32 << 20).map(|i| (i % 251) as u8).collect();
    std::fs::write(folder.join("big.bin"), &big).unwrap();
    a.drag.holds(std::slice::from_ref(&folder));
    a.input.left(true);
    a.push_until(5.0, parked).await;
    // Let go at once: nothing is there yet
    a.input.left(false);
    let (_, paths) = b.armed().await;

    // Dropped: the card says how far the files are, with no Esc to cancel
    b.expect("the early drop", |event| match event {
        EngineEvent::Receiving(Some(receiving)) if !receiving.cancel => Some(()),
        _ => None,
    })
    .await;
    b.injected(&Injected::Button(MouseButton::Left, false))
        .await;
    tokio::time::sleep(SETTLE).await;
    assert!(!a.input.awaiting_drop());

    b.expect("the files delivered", |event| {
        matches!(event, EngineEvent::Receiving(None)).then_some(())
    })
    .await;
    let calls = b.drag.calls();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, DragCall::Arm(.., false)))
    );
    // The folder and its file
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, DragCall::Listed(_, 2)))
    );
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, DragCall::Deliver(_, true)))
    );
    assert_eq!(std::fs::read(paths[0].join("big.bin")).unwrap(), big);
}

/// Files gone since they were dragged do not arrive: b says so, and the
/// release drops nothing
#[tokio::test]
async fn files_gone_meanwhile_fail() {
    let (a, mut b, _c) = row_of_three().await;
    let file = a.file_to_drag("gone.txt");
    a.drag.holds(std::slice::from_ref(&file));
    a.input.left(true);
    a.input.push((999, 500), 5.0, 0.0);
    a.dragged(|call| matches!(call, DragCall::Probe(_))).await;
    // Offered by now: the probe is answered at once
    tokio::time::sleep(SETTLE).await;
    std::fs::remove_file(&file).unwrap();
    a.push_until(5.0, parked).await;
    let failure = b
        .expect("the failure", |event| match event {
            EngineEvent::DragFailed { reason, name } => Some((reason.clone(), name.clone())),
            _ => None,
        })
        .await;
    assert_eq!(
        failure,
        (drag_failed::TRANSFER.to_string(), "gone.txt".into())
    );
    a.input.left(false);
    b.dragged(|call| matches!(call, DragCall::Cancel(1))).await;
    tokio::time::sleep(SETTLE).await;
    // Given up before its press, or cancelled after: nothing stays pressed
    let injected = b.input.injected.lock().unwrap().clone();
    let count = |down| {
        let button = Injected::Button(MouseButton::Left, down);
        injected.iter().filter(|i| **i == button).count()
    };
    assert_eq!(count(true), count(false), "{injected:?}");
}

/// The files of a cancelled drag go
#[tokio::test]
async fn a_cancelled_drag_leaves_nothing() {
    let (a, b, _c) = row_of_three().await;
    a.drag.holds(&[a.file_to_drag("draft.txt")]);
    a.input.left(true);
    a.push_until(5.0, parked).await;
    let (_, paths) = b.armed().await;
    b.injected(&Injected::Button(MouseButton::Left, true)).await;
    a.input.key(usage::ESCAPE, true);
    b.injected(&Injected::Button(MouseButton::Left, false))
        .await;
    let folder = paths[0].parent().unwrap().to_path_buf();
    let deadline = tokio::time::Instant::now() + WAIT;
    while folder.exists() {
        assert!(tokio::time::Instant::now() < deadline, "{folder:?} stays");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A drop waiting on b holds the pointer on a: it neither moves nor clicks
/// there, nor goes back to a (which would cancel the drop); once dropped,
/// it moves again
#[tokio::test]
async fn a_waiting_drop_holds_the_pointer() {
    let (a, b, _c) = row_of_three().await;
    b.drag.arm_later();
    a.drag.holds(&[a.file_to_drag("hold.txt")]);
    a.input.left(true);
    a.push_until(5.0, parked).await;
    // Let go before b is ready: the drop waits
    a.input.left(false);
    b.armed().await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while !a.input.awaiting_drop() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the pointer is not held"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!released(&a.input.push((0, 0), -5000.0, 0.0)));
    a.input.left(true);
    a.input.left(false);

    b.drag.arm_now();
    let injected = b
        .injected(&Injected::Button(MouseButton::Left, false))
        .await;
    let presses = injected
        .iter()
        .filter(|i| **i == Injected::Button(MouseButton::Left, true))
        .count();
    assert_eq!(presses, 1, "{injected:?}");
    let deadline = tokio::time::Instant::now() + WAIT;
    while a.input.awaiting_drop() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the pointer stays held"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(released(&a.input.push((0, 0), -5000.0, 0.0)));
}
