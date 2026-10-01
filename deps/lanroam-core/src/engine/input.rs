//! Input: this device's keyboard and mouse, and the devices it controls or
//! is controlled by.
//!
//! One actor owns both roles:
//! - **Source**: the capture thread decides every local event with the
//!   [`Switch`]; what it emits comes here and leaves on the link of the
//!   device concerned, pointer motion as datagrams and the rest on the
//!   control stream. A heartbeat checks that the controlled device answers.
//! - **Target**: input from other members is replayed through the injector,
//!   on a thread of its own. One member controls this device at a time: a
//!   newer one preempts the older, and local input takes it back. Whatever
//!   a controller holds is released when it leaves, is preempted, or its
//!   link drops.
//!
//! Where the pointer goes, the clipboard actor hears too. Drags of files
//! go along with it (see [`carry`]).

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use lanroam_input::config::EdgeSettings;
use lanroam_input::inject::{Injector, RemoteInput, WheelScale};
use lanroam_input::platform::{self, EmitSink};
use lanroam_input::switch::{self, Emit, Request, Switch};
use lanroam_input::world::World;
use lanroam_input::{InputError, MouseButton, Point, Rect};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::MissedTickBehavior;

use super::EngineEvent;
use super::clipboard::ClipMsg;
use super::drag::{DragBackend, DragEvent};
use super::files::Offers;
use crate::protocol::{Control, Datagram, DragItem, released};

mod carry;
use crate::settings::InputSettings;

/// Heartbeat period while another device is controlled
const HEARTBEAT: Duration = Duration::from_secs(1);

/// Control comes back when the controlled device has not answered a
/// heartbeat for this long: a live connection to a stuck device must not
/// keep the user's input either
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(3);

/// Access to the keyboard, mouse and displays (the platform's, or a test's)
pub trait InputBackend: Send + Sync {
    /// The displays (device coordinates) and each one's scale (device units
    /// per logical pixel, in percent), in the same order
    fn screens(&self) -> Result<(Vec<Rect>, Vec<u32>), InputError>;
    /// Start capturing, deciding each event with `switch`; the capture runs
    /// until the returned guard is dropped
    fn capture(
        &self,
        switch: Arc<Mutex<Switch>>,
        sink: EmitSink,
    ) -> Result<Box<dyn Any + Send>, InputError>;
    /// An injector for this session
    fn injector(&self) -> Result<Box<dyn Injector>, InputError>;
}

/// This machine's keyboard, mouse and displays
pub struct PlatformInput;

impl InputBackend for PlatformInput {
    fn screens(&self) -> Result<(Vec<Rect>, Vec<u32>), InputError> {
        platform::screens()
    }

    fn capture(
        &self,
        switch: Arc<Mutex<Switch>>,
        sink: EmitSink,
    ) -> Result<Box<dyn Any + Send>, InputError> {
        Ok(Box::new(platform::start_capture(switch, sink)?))
    }

    fn injector(&self) -> Result<Box<dyn Injector>, InputError> {
        platform::injector()
    }
}

/// Whether capture and injection run on this device
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputStatus {
    /// Capturing local input, to control other devices; why it does not run
    pub capture: Result<(), String>,
    /// Injecting input from other devices; why it does not run
    pub injection: Result<(), String>,
}

/// What changed about who controls what, for the user
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    /// This device's keyboard and mouse now drive `name`
    Controlling {
        /// The device
        name: String,
        /// Its fingerprint
        fingerprint: String,
    },
    /// They drive this device again
    Home {
        /// Where the pointer came back, in this device's coordinates: on an
        /// outer edge after crossing it, mid-display otherwise
        at: Point,
        /// It came back with a jump (hotkey or request)
        jumped: bool,
    },
    /// `name` took control of this device
    ControlledBy {
        /// The device
        name: String,
        /// Its fingerprint
        fingerprint: String,
        /// Where the pointer came in, in this device's coordinates: on an
        /// outer edge after crossing it, mid-display after a jump
        at: Point,
    },
    /// `name` gave control of this device back, or its link dropped
    Freed {
        /// The device
        name: String,
        /// Its fingerprint
        fingerprint: String,
    },
    /// Local input took this device back from `name`
    TookBack {
        /// The device
        name: String,
        /// Its fingerprint
        fingerprint: String,
    },
    /// The device being controlled let go (a [`released`] reason); control
    /// comes back at the next input
    LetGo {
        /// The device
        name: String,
        /// Its fingerprint
        fingerprint: String,
        /// Why
        reason: String,
    },
    /// The device being controlled stopped answering; control comes back at
    /// the next input
    Unresponsive {
        /// The device
        name: String,
        /// Its fingerprint
        fingerprint: String,
    },
    /// The link to the device being controlled dropped; control comes back
    /// at the next input
    Lost {
        /// The device
        name: String,
        /// Its fingerprint
        fingerprint: String,
    },
    /// `name`, controlling this device, locked the pointer to it (or
    /// unlocked it)
    LockedHere {
        /// The device
        name: String,
        /// Its fingerprint
        fingerprint: String,
        /// Locked
        on: bool,
    },
    /// Crossing edges was paused (Ctrl+Alt+Esc) or resumed
    Paused {
        /// Paused
        on: bool,
    },
    /// The pointer was locked to its device (Ctrl+Alt+L, Scroll Lock) or
    /// unlocked
    Locked {
        /// Locked
        on: bool,
    },
    /// Capture or injection is not available on this device
    Unavailable {
        /// Which one
        what: &'static str,
        /// Why
        reason: String,
    },
}

/// A member link as the input side uses it
#[derive(Clone)]
pub(super) struct LinkHandle {
    /// Control messages to send
    pub(super) out: mpsc::UnboundedSender<Control>,
    /// The connection, for datagrams
    pub(super) conn: quinn::Connection,
}

/// Member links by fingerprint
pub(super) type Links = Arc<HashMap<String, LinkHandle>>;

/// The input actor's inbox
pub(super) enum InputMsg {
    /// The switch emitted something (from the capture thread)
    Emit(Emit),
    /// An input message from a member
    Control {
        /// The member
        from: String,
        /// The message
        msg: Control,
    },
    /// A datagram from a member
    Datagram {
        /// The member
        from: String,
        /// The datagram
        datagram: Datagram,
    },
    /// A member's link is gone
    LinkDown(String),
    /// The online part of the layout changed
    World {
        /// Online devices, this one included
        world: World,
        /// Devices that get Command and Control swapped
        swapped: HashSet<String>,
        /// Pointer speeds of the other devices, in percent
        speeds: HashMap<String, u32>,
        /// Member names by fingerprint
        names: HashMap<String, String>,
        /// Every placed member, online or not, in reading order (the
        /// number hotkeys)
        numbered: Vec<String>,
        /// Settings of edges between members
        edges: HashMap<(String, String), EdgeSettings>,
        /// Members files are not dragged to or from
        dragless: HashSet<String>,
    },
    /// Carry out the user's request at the next local event
    Request(Request),
    /// The user's input settings changed
    Settings(InputSettings),
    /// Start or stop recording a key combination
    Record(bool),
    /// Report whether capture and injection run
    Status(oneshot::Sender<InputStatus>),
    /// Start whichever of capture and injection does not run (the user
    /// granted a permission since), then report
    Restart(oneshot::Sender<InputStatus>),
    /// Give everything back and stop
    Shutdown(oneshot::Sender<()>),
    /// The native drag and drop reported something
    Drag(DragEvent),
    /// What the files a probe found are (read off the runtime), and the
    /// token they are offered under, for whoever asked (`None`: this
    /// device's switch)
    Described {
        /// The probe
        id: u64,
        /// Who asked
        asker: Option<String>,
        /// The files
        files: Vec<DragItem>,
        /// Their token
        token: String,
    },
    /// Stand-ins for the files of a drag carried here are staged, or could
    /// not be
    Staged {
        /// The drag
        id: u64,
        /// Their folder and paths, or why not
        staged: Result<(PathBuf, Vec<PathBuf>), String>,
    },
    /// Everything a drag carried here carries, listed before the files
    /// come
    DragListed {
        /// The drag
        id: u64,
        /// What it carries
        entries: Vec<lanroam_dnd::Listed>,
    },
    /// A small drag carried here gave its files a moment to arrive
    DragSettled(u64),
    /// The user cancelled a drop whose files are still coming
    CancelDrop(u64),
    /// Some of the files of a drag carried here are there
    DragProgress {
        /// The drag
        id: u64,
        /// Bytes there so far
        done: u64,
        /// Bytes in all
        total: u64,
    },
    /// The files of a drag carried here are all there
    DragReady(u64),
    /// The files of a drag carried here will not arrive
    DragFailed {
        /// The drag
        id: u64,
        /// Why, for the user (a `drag_failed` code)
        reason: &'static str,
        /// Why, for the log
        detail: String,
    },
    /// A drag carried here took too long to get ready, or to refuse its
    /// drop
    DragTimeout(u64),
}

/// One step of replayed input
enum Op {
    /// A controller took over, cursor here
    Enter(Point),
    /// Cursor position with its sequence number
    Motion(u32, Point),
    /// Key press or release
    Key(u16, bool),
    /// Button press or release at a position
    Button(MouseButton, bool, Point),
    /// Button release where the cursor is
    Release(MouseButton),
    /// Scrolling
    Wheel(i32, i32),
    /// Release everything held
    ReleaseAll,
}

/// The injection thread: OS injection calls are synchronous, so they stay
/// off the runtime
struct Replay {
    /// Steps for the thread; dropped first to stop it
    ops: Option<std_mpsc::Sender<Op>>,
    /// The thread
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Replay {
    /// Start the thread around `injector`
    fn spawn(injector: Box<dyn Injector>) -> std::io::Result<Self> {
        let (ops, rx) = std_mpsc::channel::<Op>();
        let thread = std::thread::Builder::new()
            .name("lanroam-inject".into())
            .spawn(move || {
                let mut input = RemoteInput::new(injector);
                for op in rx {
                    match op {
                        Op::Enter(at) => input.enter(at),
                        Op::Motion(seq, at) => input.motion(seq, at),
                        Op::Key(usage, down) => input.key(usage, down),
                        Op::Button(button, down, at) => input.button(button, down, at),
                        Op::Release(button) => input.release(button),
                        Op::Wheel(dx, dy) => input.wheel(dx, dy),
                        Op::ReleaseAll => input.release_all(),
                    }
                }
                // Dropping `input` releases whatever is still held
            })?;
        Ok(Self {
            ops: Some(ops),
            thread: Some(thread),
        })
    }

    /// Queue one step
    fn send(&self, op: Op) {
        if let Some(ops) = &self.ops {
            let _ = ops.send(op);
        }
    }
}

impl Drop for Replay {
    /// Stop the thread once it has released everything
    fn drop(&mut self) {
        self.ops = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A sink handing what the switch emits to the actor
fn emit_sink(inbox: &mpsc::UnboundedSender<InputMsg>) -> EmitSink {
    let inbox = inbox.clone();
    Box::new(move |emit| {
        // Fails only once the engine is gone
        let _ = inbox.send(InputMsg::Emit(emit));
    })
}

/// The platform's injector on a thread of its own
fn start_injection(backend: &dyn InputBackend) -> Result<Replay, String> {
    backend
        .injector()
        .map_err(|e| e.to_string())
        .and_then(|injector| Replay::spawn(injector).map_err(|e| e.to_string()))
}

/// The actor
pub(super) struct Input {
    /// Decides local events (shared with the capture thread)
    switch: Arc<Mutex<Switch>>,
    /// The platform's keyboard, mouse and displays
    backend: Arc<dyn InputBackend>,
    /// The actor's own inbox, for the capture to emit into
    inbox: mpsc::UnboundedSender<InputMsg>,
    /// The capture, while it runs
    capture: Option<Box<dyn Any + Send>>,
    /// Why the capture does not run
    capture_error: Option<String>,
    /// The injection thread, if this device can be controlled
    replay: Option<Replay>,
    /// Why injection does not run
    injection_error: Option<String>,
    /// Member links
    links: watch::Receiver<Links>,
    /// Events for the user
    events: mpsc::UnboundedSender<EngineEvent>,
    /// Member names by fingerprint
    names: HashMap<String, String>,
    /// The device this one controls
    target: Option<String>,
    /// Sequence number of the last motion sent
    seq: u32,
    /// When the controlled device last answered a heartbeat
    heard: Instant,
    /// The controlled device was reported unresponsive
    unresponsive: bool,
    /// Control came home; told the user unless it moves on at once
    home_pending: bool,
    /// The device controlling this one
    controller: Option<String>,
    /// Scrolling from the controller, as the settings want it here
    wheel: WheelScale,
    /// The clipboard actor's inbox
    clip: mpsc::UnboundedSender<ClipMsg>,
    /// This device's fingerprint
    local: String,
    /// Drags of files through this device
    drags: carry::Drags,
}

impl Input {
    /// Start capture, injection and native drag and drop as far as the
    /// platform allows; problems are reported as
    /// [`ControlEvent::Unavailable`]
    #[allow(clippy::too_many_arguments)] // one call site
    pub(super) fn new(
        local: &str,
        backend: Arc<dyn InputBackend>,
        links: watch::Receiver<Links>,
        events: mpsc::UnboundedSender<EngineEvent>,
        inbox: mpsc::UnboundedSender<InputMsg>,
        clip: mpsc::UnboundedSender<ClipMsg>,
        drag: &dyn DragBackend,
        offers: Offers,
    ) -> Self {
        let switch = Arc::new(Mutex::new(Switch::new(local)));
        let capture = backend.capture(Arc::clone(&switch), emit_sink(&inbox));
        let injection = start_injection(backend.as_ref());
        let drags = carry::Drags::start(drag, &inbox, offers);
        let mut input = Self {
            switch,
            backend,
            inbox,
            capture: None,
            capture_error: None,
            replay: None,
            injection_error: None,
            links,
            events,
            names: HashMap::new(),
            target: None,
            seq: 0,
            heard: Instant::now(),
            unresponsive: false,
            home_pending: false,
            controller: None,
            wheel: WheelScale::default(),
            clip,
            local: local.to_string(),
            drags,
        };
        input.took_capture(capture);
        input.took_injection(injection);
        input
    }

    /// Keep a capture that started, or report why it did not
    fn took_capture(&mut self, started: Result<Box<dyn Any + Send>, InputError>) {
        match started {
            Ok(capture) => {
                self.capture = Some(capture);
                self.capture_error = None;
            }
            Err(e) => {
                let reason = e.to_string();
                self.unavailable("input capture", &reason);
                self.capture_error = Some(reason);
            }
        }
    }

    /// Keep an injection thread that started, or report why it did not
    fn took_injection(&mut self, started: Result<Replay, String>) {
        match started {
            Ok(replay) => {
                self.replay = Some(replay);
                self.injection_error = None;
            }
            Err(reason) => {
                self.unavailable("input injection", &reason);
                self.injection_error = Some(reason);
            }
        }
    }

    /// Tell the user that capture or injection does not run
    fn unavailable(&self, what: &'static str, reason: &str) {
        tracing::warn!("{what} is unavailable: {reason}");
        self.notify(ControlEvent::Unavailable {
            what,
            reason: reason.to_string(),
        });
    }

    /// Whether capture and injection run
    fn status(&self) -> InputStatus {
        let state = |error: &Option<String>| error.clone().map_or(Ok(()), Err);
        InputStatus {
            capture: state(&self.capture_error),
            injection: state(&self.injection_error),
        }
    }

    /// Start whichever of capture and injection does not run; both wait for
    /// a thread to come up, so off the runtime
    async fn restart(&mut self) {
        if self.capture.is_none() {
            let backend = Arc::clone(&self.backend);
            let switch = Arc::clone(&self.switch);
            let sink = emit_sink(&self.inbox);
            let started = tokio::task::spawn_blocking(move || backend.capture(switch, sink))
                .await
                .unwrap_or_else(|e| Err(InputError::Os(e.to_string())));
            self.took_capture(started);
        }
        if self.replay.is_none() {
            let backend = Arc::clone(&self.backend);
            let started = tokio::task::spawn_blocking(move || start_injection(backend.as_ref()))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            self.took_injection(started);
        }
    }

    /// Run until shut down
    pub(super) async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<InputMsg>) {
        let mut heartbeat = tokio::time::interval(HEARTBEAT);
        // After a sleep or a stall, one heartbeat rather than a burst
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                msg = inbox.recv() => {
                    let mut next = msg;
                    // Everything already queued in one go, so a hop (leave
                    // one device, enter the next) is not reported as home
                    while let Some(msg) = next.take() {
                        match msg {
                            InputMsg::Shutdown(reply) => {
                                self.stop().await;
                                let _ = reply.send(());
                                return;
                            }
                            InputMsg::Restart(reply) => {
                                self.restart().await;
                                let _ = reply.send(self.status());
                            }
                            msg => self.handle(msg),
                        }
                        next = inbox.try_recv().ok();
                    }
                    if std::mem::take(&mut self.home_pending) && self.target.is_none() {
                        let back = switch::lock(&self.switch).homecoming();
                        self.notify(ControlEvent::Home {
                            at: back.at,
                            jumped: back.jumped,
                        });
                    }
                }
                _ = heartbeat.tick() => self.heartbeat(),
            }
        }
    }

    /// Dispatch one message
    fn handle(&mut self, msg: InputMsg) {
        match msg {
            InputMsg::Emit(emit) => self.on_emit(emit),
            InputMsg::Control { from, msg } => self.on_control(&from, msg),
            InputMsg::Datagram { from, datagram } => {
                if let Datagram::Motion { seq, x, y } = datagram
                    && self.controlled_by(&from)
                    && !self.hold_motion(&from, seq, Point::new(x, y))
                {
                    self.replay(Op::Motion(seq, Point::new(x, y)));
                }
            }
            InputMsg::LinkDown(fp) => {
                if self.controlled_by(&fp) {
                    self.free(&fp);
                }
                if self.target.as_deref() == Some(fp.as_str()) {
                    switch::lock(&self.switch).request_release(Some(&fp));
                    // Said once: not again as unresponsive
                    if !std::mem::replace(&mut self.unresponsive, true) {
                        let name = self.name(&fp);
                        tracing::info!(device = %name, "lost the link to the device controlled");
                        self.notify(ControlEvent::Lost {
                            name,
                            fingerprint: fp,
                        });
                    }
                }
            }
            InputMsg::World {
                world,
                swapped,
                speeds,
                names,
                numbered,
                edges,
                dragless,
            } => {
                self.names = names;
                let mut switch = switch::lock(&self.switch);
                switch.set_world(world, swapped);
                switch.set_pointer_speeds(speeds);
                switch.set_numbering(numbered);
                switch.set_edges(edges);
                switch.set_dragless(dragless);
            }
            InputMsg::Settings(settings) => {
                self.wheel.set(settings.scrolling);
                let mut switch = switch::lock(&self.switch);
                switch.set_hotkeys(settings.hotkeys);
                switch.set_keep_local(settings.keep_local);
                switch.set_switching(settings.switching);
                switch.set_media_keys(settings.media_keys);
            }
            InputMsg::Record(on) => switch::lock(&self.switch).record(on),
            InputMsg::Request(request) => match self.controller.clone() {
                // The user works this device with its controller's keyboard
                // and mouse: that is where pausing, locking and jumping act
                Some(controller) => self.send(
                    &controller,
                    Control::Request {
                        request: request.into(),
                    },
                ),
                None => switch::lock(&self.switch).request(request),
            },
            InputMsg::Status(reply) => {
                let _ = reply.send(self.status());
            }
            InputMsg::Drag(event) => self.on_drag_event(event),
            InputMsg::Described {
                id,
                asker,
                files,
                token,
            } => self.described(id, asker, files, token),
            InputMsg::Staged { id, staged } => self.staged(id, staged),
            InputMsg::DragListed { id, entries } => self.listed(id, entries),
            InputMsg::DragSettled(id) => self.settled(id),
            InputMsg::CancelDrop(id) => self.cancel_drop(id),
            InputMsg::DragProgress { id, done, total } => self.progress(id, done, total),
            InputMsg::DragReady(id) => self.ready(id),
            InputMsg::DragFailed { id, reason, detail } => {
                self.transfer_failed(id, reason, &detail);
            }
            InputMsg::DragTimeout(id) => self.timed_out(id),
            // Handled by the run loop
            InputMsg::Restart(_) | InputMsg::Shutdown(_) => {}
        }
    }

    /// Send what the switch emitted to the device it concerns
    fn on_emit(&mut self, emit: Emit) {
        match emit {
            Emit::Enter { device, at } => {
                self.target = Some(device.clone());
                self.heard = Instant::now();
                self.unresponsive = false;
                self.seq = 0;
                self.send(&device, Control::Enter { x: at.x, y: at.y });
                let _ = self.clip.send(ClipMsg::Entered(device.clone()));
                let name = self.name(&device);
                self.notify(ControlEvent::Controlling {
                    name,
                    fingerprint: device,
                });
            }
            Emit::Leave { device } => {
                self.send(&device, Control::Leave);
                let _ = self.clip.send(ClipMsg::Left(device.clone()));
                if self.target.as_deref() == Some(device.as_str()) {
                    self.target = None;
                    self.home_pending = true;
                }
            }
            Emit::Motion { device, at } => {
                if let Some(link) = self.links.borrow().get(&device) {
                    self.seq = self.seq.wrapping_add(1);
                    let motion = Datagram::Motion {
                        seq: self.seq,
                        x: at.x,
                        y: at.y,
                    };
                    // A lost motion is superseded by the next one anyway
                    let _ = link.conn.send_datagram(motion.encode());
                }
            }
            Emit::Key {
                device,
                usage,
                down,
            } => self.send(&device, Control::Key { usage, down }),
            Emit::Button {
                device,
                button,
                down,
                at,
            } => self.send(
                &device,
                Control::Button {
                    button,
                    down,
                    x: at.x,
                    y: at.y,
                },
            ),
            Emit::Wheel { device, dx, dy } => self.send(&device, Control::Wheel { dx, dy }),
            // A combination kept local: pressed here in the real one's place
            Emit::Local { usage, down } => self.replay(Op::Key(usage, down)),
            Emit::Recorded(chord) => {
                let _ = self.events.send(EngineEvent::Recorded(chord));
            }
            Emit::Paused(on) => self.notify(ControlEvent::Paused { on }),
            Emit::Locked(on) => {
                // The pointer is on the controlled device: tell it too, so
                // that it can say so where the user looks
                if let Some(target) = self.target.clone() {
                    self.send(&target, Control::PointerLocked { on });
                }
                self.notify(ControlEvent::Locked { on });
            }
            emit @ (Emit::LocalPress { .. }
            | Emit::DragAtEdge { .. }
            | Emit::Carry { .. }
            | Emit::Drop { .. }
            | Emit::DragCancel { .. }) => self.on_drag_emit(emit),
            Emit::Takeover => {
                if let Some(controller) = self.controller.take() {
                    let _ = self.clip.send(ClipMsg::TookBack);
                    if !self.controller_gone(&controller) {
                        self.replay(Op::ReleaseAll);
                    }
                    self.send(
                        &controller,
                        Control::Released {
                            reason_code: released::LOCAL_INPUT.into(),
                        },
                    );
                    let name = self.name(&controller);
                    self.notify(ControlEvent::TookBack {
                        name,
                        fingerprint: controller,
                    });
                }
            }
        }
    }

    /// Handle an input message from a member
    fn on_control(&mut self, from: &str, msg: Control) {
        let controlled = self.controlled_by(from);
        match msg {
            Control::Enter { x, y } => self.on_enter(from, Point::new(x, y)),
            Control::Leave if controlled => self.free(from),
            Control::Key { usage, down } if controlled => {
                // Esc cancels a drop waiting here
                if self.on_controller_escape(from, usage, down) {
                    return;
                }
                // Recording here with the controller's keyboard: the keys
                // make the combination instead of typing
                let mut switch = switch::lock(&self.switch);
                if switch.is_recording() {
                    if let Some(chord) = switch.record_key(usage, down) {
                        let _ = self.events.send(EngineEvent::Recorded(chord));
                    }
                    return;
                }
                drop(switch);
                self.replay(Op::Key(usage, down));
            }
            Control::Button { button, down, x, y } if controlled => {
                let at = Point::new(x, y);
                if button == MouseButton::Left && self.on_controller_left(from, down, at) {
                    return;
                }
                self.replay(Op::Button(button, down, at));
            }
            Control::Wheel { dx, dy } if controlled => {
                let (dx, dy) = self.wheel.apply(dx, dy);
                if (dx, dy) != (0, 0) {
                    self.replay(Op::Wheel(dx, dy));
                }
            }
            Control::PointerLocked { on } if controlled => {
                self.notify(ControlEvent::LockedHere {
                    name: self.name(from),
                    fingerprint: from.to_string(),
                    on,
                });
            }
            Control::Released { reason_code } if self.target.as_deref() == Some(from) => {
                switch::lock(&self.switch).request_release(Some(from));
                let name = self.name(from);
                tracing::info!(device = %name, reason = %reason_code, "the device controlled let go");
                self.notify(ControlEvent::LetGo {
                    name,
                    fingerprint: from.to_string(),
                    reason: reason_code,
                });
            }
            Control::Request { request } if self.target.as_deref() == Some(from) => {
                switch::lock(&self.switch).request(request.into());
            }
            Control::Pong { .. } if self.target.as_deref() == Some(from) => {
                self.heard = Instant::now();
                self.unresponsive = false;
            }
            msg @ (Control::DragProbe { .. }
            | Control::DragFiles { .. }
            | Control::DragEnter { .. }
            | Control::DragCancel { .. }
            | Control::DropWaiting { .. }) => self.on_drag_control(from, msg),
            // From a device that is not in control (any more), or not input
            _ => {}
        }
    }

    /// A member takes control of this device; an earlier controller is
    /// preempted
    fn on_enter(&mut self, from: &str, at: Point) {
        if self.replay.is_none() {
            let reason_code = released::UNAVAILABLE.into();
            self.send(from, Control::Released { reason_code });
            return;
        }
        let fresh = !self.controlled_by(from);
        if let Some(previous) = self.controller.take_if(|c| c != from) {
            if !self.controller_gone(&previous) {
                self.replay(Op::ReleaseAll);
            }
            let reason_code = released::PREEMPTED.into();
            self.send(&previous, Control::Released { reason_code });
        }
        self.controller = Some(from.to_string());
        self.replay(Op::Enter(at));
        switch::lock(&self.switch).set_controlled(true);
        if fresh {
            let _ = self.clip.send(ClipMsg::ControlledBy(from.to_string()));
            let name = self.name(from);
            self.notify(ControlEvent::ControlledBy {
                name,
                fingerprint: from.to_string(),
                at,
            });
        }
    }

    /// The controller let go (or its link dropped): release what it held,
    /// once a drag it carried here refuses its drop
    fn free(&mut self, from: &str) {
        self.controller = None;
        let _ = self.clip.send(ClipMsg::Freed(from.to_string()));
        if !self.controller_gone(from) {
            self.replay(Op::ReleaseAll);
        }
        switch::lock(&self.switch).set_controlled(false);
        let name = self.name(from);
        self.notify(ControlEvent::Freed {
            name,
            fingerprint: from.to_string(),
        });
    }

    /// Ping the controlled device; take control back if it went quiet
    fn heartbeat(&mut self) {
        let Some(target) = self.target.clone() else {
            return;
        };
        self.send(&target, Control::Ping { seq: 0, sent_us: 0 });
        if !self.unresponsive && self.heard.elapsed() > HEARTBEAT_TIMEOUT {
            self.unresponsive = true;
            switch::lock(&self.switch).request_release(Some(&target));
            let name = self.name(&target);
            tracing::info!(device = %name, "the device controlled stopped answering");
            self.notify(ControlEvent::Unresponsive {
                name,
                fingerprint: target,
            });
        }
    }

    /// Give everything back: leave the controlled device, release what a
    /// controller holds, stop capturing (which frees the local cursor)
    async fn stop(&mut self) {
        if let Some(target) = self.target.take() {
            self.send(&target, Control::Leave);
        }
        let (capture, replay) = (self.capture.take(), self.replay.take());
        // Both joins wait for a thread; keep them off the runtime
        let _ = tokio::task::spawn_blocking(move || {
            drop(capture);
            drop(replay);
        })
        .await;
    }

    /// Whether `fp` controls this device
    fn controlled_by(&self, fp: &str) -> bool {
        self.controller.as_deref() == Some(fp)
    }

    /// Send a control message to a member, if linked
    fn send(&self, fp: &str, msg: Control) {
        if let Some(link) = self.links.borrow().get(fp) {
            let _ = link.out.send(msg);
        }
    }

    /// Queue a step for the injection thread
    fn replay(&self, op: Op) {
        if let Some(replay) = &self.replay {
            replay.send(op);
        }
    }

    /// A member's name for the user
    fn name(&self, fp: &str) -> String {
        self.names
            .get(fp)
            .cloned()
            .unwrap_or_else(|| fp.chars().take(12).collect())
    }

    /// Tell the user
    fn notify(&self, event: ControlEvent) {
        let _ = self.events.send(EngineEvent::Control(event));
    }
}
