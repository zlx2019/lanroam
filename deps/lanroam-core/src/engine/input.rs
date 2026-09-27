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

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use lanroam_input::inject::{Injector, RemoteInput};
use lanroam_input::platform::{self, EmitSink};
use lanroam_input::switch::{self, Emit, Switch};
use lanroam_input::world::World;
use lanroam_input::{InputError, MouseButton, Point, Rect};
use tokio::sync::{mpsc, oneshot, watch};

use super::EngineEvent;
use crate::protocol::{Control, Datagram, released};

/// Heartbeat period while another device is controlled
const HEARTBEAT: Duration = Duration::from_secs(1);

/// Control comes back when the controlled device has not answered a
/// heartbeat for this long: a live connection to a stuck device must not
/// keep the user's input either
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(3);

/// Access to the keyboard, mouse and displays (the platform's, or a test's)
pub trait InputBackend: Send + Sync {
    /// The displays (device coordinates) and the scale (device units per
    /// logical pixel, in percent)
    fn screens(&self) -> Result<(Vec<Rect>, u32), InputError>;
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
    fn screens(&self) -> Result<(Vec<Rect>, u32), InputError> {
        Ok((platform::displays()?, platform::scale()?))
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

/// What changed about who controls what, for the user
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    /// This device's keyboard and mouse now drive `name`
    Controlling {
        /// The device
        name: String,
    },
    /// They drive this device again
    Home,
    /// `name` took control of this device
    ControlledBy {
        /// The device
        name: String,
    },
    /// `name` gave control of this device back, or its link dropped
    Freed {
        /// The device
        name: String,
    },
    /// Local input took this device back from `name`
    TookBack {
        /// The device
        name: String,
    },
    /// The device being controlled let go (a [`released`] reason); control
    /// comes back at the next input
    LetGo {
        /// The device
        name: String,
        /// Why
        reason: String,
    },
    /// The device being controlled stopped answering; control comes back at
    /// the next input
    Unresponsive {
        /// The device
        name: String,
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
        /// Member names by fingerprint
        names: HashMap<String, String>,
    },
    /// Give everything back and stop
    Shutdown(oneshot::Sender<()>),
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

/// The actor
pub(super) struct Input {
    /// Decides local events (shared with the capture thread)
    switch: Arc<Mutex<Switch>>,
    /// The capture, while it runs
    capture: Option<Box<dyn Any + Send>>,
    /// The injection thread, if this device can be controlled
    replay: Option<Replay>,
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
}

impl Input {
    /// Start capture and injection as far as the platform allows; problems
    /// are reported as [`ControlEvent::Unavailable`]
    pub(super) fn new(
        local: &str,
        backend: &dyn InputBackend,
        links: watch::Receiver<Links>,
        events: mpsc::UnboundedSender<EngineEvent>,
        inbox: mpsc::UnboundedSender<InputMsg>,
    ) -> Self {
        let unavailable = |what, reason: String| {
            tracing::warn!("{what} is unavailable: {reason}");
            let _ = events.send(EngineEvent::Control(ControlEvent::Unavailable {
                what,
                reason,
            }));
        };
        let switch = Arc::new(Mutex::new(Switch::new(local)));
        let sink: EmitSink = Box::new(move |emit| {
            // Fails only once the engine is gone
            let _ = inbox.send(InputMsg::Emit(emit));
        });
        let capture = backend
            .capture(Arc::clone(&switch), sink)
            .map_err(|e| unavailable("input capture", e.to_string()))
            .ok();
        let replay = backend
            .injector()
            .map_err(|e| e.to_string())
            .and_then(|injector| Replay::spawn(injector).map_err(|e| e.to_string()))
            .map_err(|e| unavailable("input injection", e))
            .ok();
        Self {
            switch,
            capture,
            replay,
            links,
            events,
            names: HashMap::new(),
            target: None,
            seq: 0,
            heard: Instant::now(),
            unresponsive: false,
            home_pending: false,
            controller: None,
        }
    }

    /// Run until shut down
    pub(super) async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<InputMsg>) {
        let mut heartbeat = tokio::time::interval(HEARTBEAT);
        loop {
            tokio::select! {
                msg = inbox.recv() => {
                    let mut next = msg;
                    // Everything already queued in one go, so a hop (leave
                    // one device, enter the next) is not reported as home
                    while let Some(msg) = next.take() {
                        if let InputMsg::Shutdown(reply) = msg {
                            self.stop().await;
                            let _ = reply.send(());
                            return;
                        }
                        self.handle(msg);
                        next = inbox.try_recv().ok();
                    }
                    if std::mem::take(&mut self.home_pending) && self.target.is_none() {
                        self.notify(ControlEvent::Home);
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
                }
            }
            InputMsg::World {
                world,
                swapped,
                names,
            } => {
                self.names = names;
                switch::lock(&self.switch).set_world(world, swapped);
            }
            // Handled by the run loop
            InputMsg::Shutdown(_) => {}
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
                let name = self.name(&device);
                self.notify(ControlEvent::Controlling { name });
            }
            Emit::Leave { device } => {
                self.send(&device, Control::Leave);
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
            Emit::Takeover => {
                if let Some(controller) = self.controller.take() {
                    self.replay(Op::ReleaseAll);
                    self.send(
                        &controller,
                        Control::Released {
                            reason_code: released::LOCAL_INPUT.into(),
                        },
                    );
                    let name = self.name(&controller);
                    self.notify(ControlEvent::TookBack { name });
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
            Control::Key { usage, down } if controlled => self.replay(Op::Key(usage, down)),
            Control::Button { button, down, x, y } if controlled => {
                self.replay(Op::Button(button, down, Point::new(x, y)));
            }
            Control::Wheel { dx, dy } if controlled => self.replay(Op::Wheel(dx, dy)),
            Control::Released { reason_code } if self.target.as_deref() == Some(from) => {
                switch::lock(&self.switch).request_release(Some(from));
                let name = self.name(from);
                self.notify(ControlEvent::LetGo {
                    name,
                    reason: reason_code,
                });
            }
            Control::Pong { .. } if self.target.as_deref() == Some(from) => {
                self.heard = Instant::now();
                self.unresponsive = false;
            }
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
            self.replay(Op::ReleaseAll);
            let reason_code = released::PREEMPTED.into();
            self.send(&previous, Control::Released { reason_code });
        }
        self.controller = Some(from.to_string());
        self.replay(Op::Enter(at));
        switch::lock(&self.switch).set_controlled(true);
        if fresh {
            let name = self.name(from);
            self.notify(ControlEvent::ControlledBy { name });
        }
    }

    /// The controller let go (or its link dropped): release what it held
    fn free(&mut self, from: &str) {
        self.controller = None;
        self.replay(Op::ReleaseAll);
        switch::lock(&self.switch).set_controlled(false);
        let name = self.name(from);
        self.notify(ControlEvent::Freed { name });
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
            self.notify(ControlEvent::Unresponsive { name });
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
