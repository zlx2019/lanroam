//! Input sessions over a link (M1 prototype).
//!
//! - **Source** ([`run_source`]): the machine whose keyboard and mouse are
//!   in use. Its capture thread decides every event with a [`Switch`]; what
//!   the switch emits for the target is sent from here, pointer motion as
//!   datagrams and everything else on the control stream.
//! - **Target** ([`run_target`]): reports its displays, then replays what
//!   arrives through an [`Injector`] running on a thread of its own, and
//!   releases whatever is still held when the source leaves or the link
//!   drops.
//!
//! The layout is one fixed edge for now; desk groups, layout sync and
//! control arbitration arrive with M2.

use std::future::Future;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lanroam_input::inject::{InjectStats, Injector, RemoteInput};
use lanroam_input::switch::{self, Emit, Switch};
use lanroam_input::{Desktop, MouseButton, Point, Rect};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::diag::RttStats;
use crate::protocol::{Control, Datagram, FRAMING};
use crate::transport::{Link, TransportError, close_code};

/// How long the source waits for the target's displays
pub const SCREENS_TIMEOUT: Duration = Duration::from_secs(5);

/// Heartbeat period while the target is being controlled
const HEARTBEAT: Duration = Duration::from_secs(1);

/// Control comes back when the target has not answered a heartbeat for
/// this long: a live connection with a stuck target must not keep the
/// user's input either
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(3);

/// Budget for telling the target goodbye at the end of a session
const GOODBYE_TIMEOUT: Duration = Duration::from_secs(1);

/// Motion send times kept for matching receipts (about a second of motion
/// at the fastest mouse report rates)
const MOTION_WINDOW: usize = 1024;

/// Session errors
#[derive(Debug, Error)]
pub enum SessionError {
    /// The link failed
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The target never reported its displays
    #[error(
        "the target did not report its displays within {}s (does it run a build with input support?)",
        SCREENS_TIMEOUT.as_secs()
    )]
    NoScreens,
    /// The injection thread could not start
    #[error("cannot start the injection thread: {0}")]
    Thread(#[from] std::io::Error),
}

/// Something the user of the source should know about
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceNotice {
    /// Control moved to the target
    Entered,
    /// Control came back
    Left,
    /// The target stopped answering; control comes back at the next input
    Unresponsive,
}

/// Something the user of the target should know about
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetNotice {
    /// The source took control
    Entered,
    /// The source gave control back
    Left,
}

/// Why a source session ended
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SessionEnd {
    /// Asked to stop
    #[default]
    Shutdown,
    /// The link failed
    LinkLost(String),
    /// The capture stopped delivering events
    CaptureStopped,
}

/// Source-side account of a session
#[derive(Debug, Clone, Default)]
pub struct SourceReport {
    /// Times control moved to the target
    pub crossings: u32,
    /// Motion datagrams sent, and round trips of the acknowledged ones
    /// (sent here, applied by the target's session, receipt back here)
    pub motion: RttStats,
    /// Key presses, autorepeats and releases sent
    pub keys: u32,
    /// Button presses and releases sent
    pub buttons: u32,
    /// Scroll steps sent
    pub wheels: u32,
    /// Why the session ended
    pub end: SessionEnd,
}

/// Wait for the target's displays, which open every input session
pub async fn recv_screens(link: &mut Link) -> Result<Desktop, SessionError> {
    let wait = async {
        loop {
            match link.recv().await {
                Ok(Control::Screens { displays }) => return Ok(Desktop::new(displays)),
                Ok(other) => {
                    tracing::debug!(kind = other.kind(), "ignoring message before the screens")
                }
                Err(e) => return Err(SessionError::from(e)),
            }
        }
    };
    tokio::time::timeout(SCREENS_TIMEOUT, wait)
        .await
        .map_err(|_| SessionError::NoScreens)?
}

/// Run the source side until `shutdown` completes, the link drops or the
/// capture stops
///
/// `emits` carries what `switch` produced on the capture thread.
pub async fn run_source(
    link: Link,
    switch: Arc<Mutex<Switch>>,
    mut emits: mpsc::UnboundedReceiver<Emit>,
    mut notify: impl FnMut(SourceNotice),
    shutdown: impl Future<Output = ()>,
) -> SourceReport {
    let (_remote, conn, send, recv) = link.into_parts();
    let (mut control, reader) = spawn_control_reader(recv);
    let mut forwarder = Forwarder {
        conn: conn.clone(),
        send,
        clock: MotionClock::new(),
        next_seq: 0,
        last_pong: Instant::now(),
        report: SourceReport::default(),
    };
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ping_seq = 0u32;
    tokio::pin!(shutdown);

    let end = loop {
        tokio::select! {
            emit = emits.recv() => {
                let Some(first) = emit else { break SessionEnd::CaptureStopped };
                let mut batch = vec![first];
                while let Ok(more) = emits.try_recv() {
                    batch.push(more);
                }
                if let Err(e) = forwarder.forward(batch, &mut notify).await {
                    break SessionEnd::LinkLost(e.to_string());
                }
            }
            datagram = conn.read_datagram() => match datagram {
                Ok(bytes) => {
                    if let Some(Datagram::MotionAck { seq }) = Datagram::decode(&bytes) {
                        forwarder.acked(seq);
                    }
                }
                Err(e) => break SessionEnd::LinkLost(e.to_string()),
            },
            message = control.recv() => match message {
                Some(Control::Screens { displays }) => {
                    switch::lock(&switch).set_target(Desktop::new(displays));
                }
                Some(Control::Pong { .. }) => forwarder.last_pong = Instant::now(),
                Some(other) => tracing::debug!(kind = other.kind(), "ignoring control message"),
                None => break SessionEnd::LinkLost("the control stream closed".into()),
            },
            _ = heartbeat.tick() => {
                if !switch::lock(&switch).is_remote() {
                    continue;
                }
                if forwarder.last_pong.elapsed() > HEARTBEAT_TIMEOUT {
                    switch::lock(&switch).request_release();
                    notify(SourceNotice::Unresponsive);
                    // One notice per silence
                    forwarder.last_pong = Instant::now();
                } else {
                    let ping = Control::Ping { seq: ping_seq, sent_us: 0 };
                    ping_seq = ping_seq.wrapping_add(1);
                    if let Err(e) = FRAMING.write(&mut forwarder.send, &ping).await {
                        break SessionEnd::LinkLost(e.to_string());
                    }
                }
            }
            () = &mut shutdown => break SessionEnd::Shutdown,
        }
    };

    // Whatever the target still holds for us must be released there, and the
    // local cursor must come back here at the next event
    switch::lock(&switch).request_release();
    let goodbye = FRAMING.write(&mut forwarder.send, &Control::Leave);
    let _ = tokio::time::timeout(GOODBYE_TIMEOUT, goodbye).await;
    conn.close(close_code::NORMAL, b"bye");
    reader.abort();
    let mut report = forwarder.report;
    report.end = end;
    report
}

/// Read the control stream on its own task: reads are not cancel-safe, so
/// they must not be raced in `select!`
fn spawn_control_reader(
    mut recv: quinn::RecvStream,
) -> (mpsc::Receiver<Control>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(32);
    let task = tokio::spawn(async move {
        while let Ok(message) = FRAMING.read::<_, Control>(&mut recv).await {
            if tx.send(message).await.is_err() {
                break;
            }
        }
    });
    (rx, task)
}

/// Sends the switch's messages to the target and keeps the statistics
struct Forwarder {
    /// The connection, for datagrams
    conn: quinn::Connection,
    /// Sending half of the control stream
    send: quinn::SendStream,
    /// Send times of recent motions
    clock: MotionClock,
    /// Sequence number of the next motion
    next_seq: u32,
    /// When the target last answered a heartbeat
    last_pong: Instant,
    /// Statistics so far
    report: SourceReport,
}

impl Forwarder {
    /// Send a batch; a motion immediately followed by another one is
    /// superseded and skipped (only the newest position matters)
    async fn forward(
        &mut self,
        batch: Vec<Emit>,
        notify: &mut impl FnMut(SourceNotice),
    ) -> Result<(), TransportError> {
        let mut batch = batch.into_iter().peekable();
        while let Some(emit) = batch.next() {
            if matches!(emit, Emit::Motion(_)) && matches!(batch.peek(), Some(Emit::Motion(_))) {
                continue;
            }
            self.send(emit, notify).await?;
        }
        Ok(())
    }

    /// Send one message
    async fn send(
        &mut self,
        emit: Emit,
        notify: &mut impl FnMut(SourceNotice),
    ) -> Result<(), TransportError> {
        let message = match emit {
            Emit::Motion(at) => {
                let seq = self.next_seq;
                self.next_seq = seq.wrapping_add(1);
                self.clock.sent(seq);
                self.report.motion.sent += 1;
                let datagram = Datagram::Motion {
                    seq,
                    x: at.x,
                    y: at.y,
                };
                self.conn.send_datagram(datagram.encode())?;
                return Ok(());
            }
            Emit::Enter(at) => {
                self.report.crossings += 1;
                // Silence while nobody was controlled must not count against
                // the target
                self.last_pong = Instant::now();
                notify(SourceNotice::Entered);
                Control::Enter { x: at.x, y: at.y }
            }
            Emit::Leave => {
                notify(SourceNotice::Left);
                Control::Leave
            }
            Emit::Key { usage, down } => {
                self.report.keys += 1;
                Control::Key { usage, down }
            }
            Emit::Button { button, down, at } => {
                self.report.buttons += 1;
                Control::Button {
                    button,
                    down,
                    x: at.x,
                    y: at.y,
                }
            }
            Emit::Wheel { dx, dy } => {
                self.report.wheels += 1;
                Control::Wheel { dx, dy }
            }
        };
        Ok(FRAMING.write(&mut self.send, &message).await?)
    }

    /// The target received motion `seq`
    fn acked(&mut self, seq: u32) {
        if let Some(rtt) = self.clock.acked(seq) {
            self.report.motion.samples.push(rtt);
        }
    }
}

/// Send times of the most recent motions, indexed by sequence number
struct MotionClock {
    /// `(seq, sent at)` in slot `seq % MOTION_WINDOW`
    slots: Vec<Option<(u32, Instant)>>,
}

impl MotionClock {
    /// An empty clock
    fn new() -> Self {
        Self {
            slots: vec![None; MOTION_WINDOW],
        }
    }

    /// Record that motion `seq` is being sent now
    fn sent(&mut self, seq: u32) {
        self.slots[seq as usize % MOTION_WINDOW] = Some((seq, Instant::now()));
    }

    /// Round trip of motion `seq`, once; `None` for unknown or old receipts
    fn acked(&mut self, seq: u32) -> Option<Duration> {
        let slot = &mut self.slots[seq as usize % MOTION_WINDOW];
        match *slot {
            Some((sent_seq, at)) if sent_seq == seq => {
                *slot = None;
                Some(at.elapsed())
            }
            _ => None,
        }
    }
}

/// An instruction for the replay thread
enum Op {
    /// The source took control, cursor at this position
    Enter(Point),
    /// The source gave control back
    Leave,
    /// Cursor position with its sequence number
    Motion(u32, Point),
    /// Key press or release
    Key(u16, bool),
    /// Button press or release at a position
    Button(MouseButton, bool, Point),
    /// Scrolling
    Wheel(i32, i32),
}

/// Run the target side until the link closes: report `displays`, then
/// replay the source's input through `injector`
///
/// Returns what was injected. Everything the source still holds is released
/// before returning.
pub async fn run_target(
    link: Link,
    displays: Vec<Rect>,
    injector: Box<dyn Injector>,
    mut notify: impl FnMut(TargetNotice),
) -> Result<InjectStats, SessionError> {
    let (_remote, conn, mut send, mut recv) = link.into_parts();
    FRAMING
        .write(&mut send, &Control::Screens { displays })
        .await
        .map_err(TransportError::from)?;
    let (ops, replayer) = spawn_replayer(injector)?;
    let datagrams = tokio::spawn(serve_datagrams(conn, ops.clone()));

    while let Ok(message) = FRAMING.read::<_, Control>(&mut recv).await {
        let op = match message {
            Control::Enter { x, y } => {
                notify(TargetNotice::Entered);
                Op::Enter(Point::new(x, y))
            }
            Control::Leave => {
                notify(TargetNotice::Left);
                Op::Leave
            }
            Control::Key { usage, down } => Op::Key(usage, down),
            Control::Button { button, down, x, y } => Op::Button(button, down, Point::new(x, y)),
            Control::Wheel { dx, dy } => Op::Wheel(dx, dy),
            Control::Ping { seq, sent_us } => {
                if FRAMING
                    .write(&mut send, &Control::Pong { seq, sent_us })
                    .await
                    .is_err()
                {
                    break;
                }
                continue;
            }
            other => {
                tracing::debug!(kind = other.kind(), "ignoring control message");
                continue;
            }
        };
        if ops.send(op).is_err() {
            break;
        }
    }

    // Every sender must be gone for the replay thread to finish
    datagrams.abort();
    let _ = datagrams.await;
    drop(ops);
    let stats = tokio::task::spawn_blocking(move || replayer.join())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    Ok(stats)
}

/// Start the replay thread; it ends, releasing everything, once every
/// sender is dropped
fn spawn_replayer(
    injector: Box<dyn Injector>,
) -> std::io::Result<(std_mpsc::Sender<Op>, JoinHandle<InjectStats>)> {
    let (tx, rx) = std_mpsc::channel::<Op>();
    let thread = std::thread::Builder::new()
        .name("lanroam-inject".into())
        .spawn(move || {
            let mut input = RemoteInput::new(injector);
            for op in rx {
                match op {
                    Op::Enter(at) => input.enter(at),
                    Op::Leave => input.release_all(),
                    Op::Motion(seq, at) => input.motion(seq, at),
                    Op::Key(usage, down) => input.key(usage, down),
                    Op::Button(button, down, at) => input.button(button, down, at),
                    Op::Wheel(dx, dy) => input.wheel(dx, dy),
                }
            }
            input.release_all();
            input.stats().clone()
        })?;
    Ok((tx, thread))
}

/// Hand motion datagrams to the replay thread and acknowledge them; answer
/// ping datagrams too
async fn serve_datagrams(conn: quinn::Connection, ops: std_mpsc::Sender<Op>) {
    while let Ok(bytes) = conn.read_datagram().await {
        match Datagram::decode(&bytes) {
            Some(Datagram::Motion { seq, x, y }) => {
                if ops.send(Op::Motion(seq, Point::new(x, y))).is_err() {
                    break;
                }
                // A lost receipt only shows up as loss in the source's stats
                let _ = conn.send_datagram(Datagram::MotionAck { seq }.encode());
            }
            Some(Datagram::Ping { seq, sent_us }) => {
                let _ = conn.send_datagram(Datagram::Pong { seq, sent_us }.encode());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use lanroam_input::switch::Verdict;
    use lanroam_input::{Edge, InputError, InputEvent};

    use super::*;
    use crate::test_util::TestNode;

    /// What the fake injector was asked to do
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Injected {
        Move(Point),
        Button(MouseButton, bool),
        Key(u16, bool),
    }

    /// Records every injection
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
        fn wheel(&mut self, _dx: i32, _dy: i32) -> Result<(), InputError> {
            Ok(())
        }
        fn key(&mut self, usage: u16, down: bool) -> Result<(), InputError> {
            self.0.lock().unwrap().push(Injected::Key(usage, down));
            Ok(())
        }
    }

    /// Wait until the recorder holds `op`
    async fn wait_for(log: &Arc<Mutex<Vec<Injected>>>, op: &Injected) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !log.lock().unwrap().contains(op) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{op:?} never injected: {:?}", log.lock().unwrap()));
    }

    /// Captured input crosses over a real loopback link and is replayed on
    /// the target; a key still held when the session ends is released there
    #[tokio::test]
    async fn source_drives_target_over_a_link() {
        let (_a, _b, mut client, server) = TestNode::link_pair().await;
        let log = Arc::new(Mutex::new(Vec::new()));
        let target = tokio::spawn(run_target(
            server,
            vec![Rect::new(0, 0, 1920, 1080)],
            Box::new(Recorder(Arc::clone(&log))),
            |_| {},
        ));

        let screens = recv_screens(&mut client).await.unwrap();
        let switch = Arc::new(Mutex::new(Switch::new(
            Desktop::new([Rect::new(0, 0, 1000, 800)]),
            screens,
            Edge::Right,
        )));
        let (emit_tx, emit_rx) = mpsc::unbounded_channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let source = tokio::spawn(run_source(
            client,
            Arc::clone(&switch),
            emit_rx,
            |_| {},
            async {
                let _ = stop_rx.await;
            },
        ));

        // Stand in for the capture thread
        let feed = |event| {
            let mut out = Vec::new();
            let decision = switch::lock(&switch).handle(event, &mut out);
            for emit in out {
                emit_tx.send(emit).unwrap();
            }
            decision.verdict
        };
        let motion = |x, dx| InputEvent::Motion {
            at: Point::new(x, 400),
            dx,
            dy: 0.0,
        };
        assert_eq!(feed(motion(999, 5.0)), Verdict::Swallow);
        feed(motion(999, 10.0));
        feed(InputEvent::Key {
            usage: 0x04,
            down: true,
        });
        feed(InputEvent::Key {
            usage: 0x04,
            down: false,
        });
        feed(InputEvent::Button {
            button: MouseButton::Left,
            down: true,
        });
        feed(InputEvent::Button {
            button: MouseButton::Left,
            down: false,
        });
        feed(InputEvent::Key {
            usage: 0x05,
            down: true,
        });

        wait_for(&log, &Injected::Move(Point::new(1, 540))).await;
        wait_for(&log, &Injected::Move(Point::new(11, 540))).await;
        wait_for(&log, &Injected::Button(MouseButton::Left, false)).await;
        wait_for(&log, &Injected::Key(0x05, true)).await;

        stop_tx.send(()).unwrap();
        let report = source.await.unwrap();
        assert_eq!(report.end, SessionEnd::Shutdown);
        assert_eq!(report.crossings, 1);
        assert_eq!((report.keys, report.buttons), (3, 2));
        assert_eq!(report.motion.sent, 1);

        let stats = tokio::time::timeout(Duration::from_secs(5), target)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        // Only what the source sent counts; the release below is the target's
        assert_eq!((stats.keys, stats.buttons), (3, 2));
        let log = log.lock().unwrap();
        let a_down = log.iter().position(|op| *op == Injected::Key(0x04, true));
        let a_up = log.iter().position(|op| *op == Injected::Key(0x04, false));
        assert!(a_down < a_up, "{log:?}");
        // The key still held when the session ended was released there
        assert_eq!(log.last(), Some(&Injected::Key(0x05, false)));
    }

    /// Receipts match the motions they acknowledge, once
    #[test]
    fn motion_clock_matches_receipts() {
        let mut clock = MotionClock::new();
        clock.sent(3);
        assert!(clock.acked(3).is_some());
        assert!(clock.acked(3).is_none());
        clock.sent(5);
        // Same slot, newer motion: the old receipt no longer matches
        clock.sent(5 + MOTION_WINDOW as u32);
        assert!(clock.acked(5).is_none());
        assert!(clock.acked(7).is_none());
    }
}
