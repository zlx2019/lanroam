//! Source side: event by event, decides whether local input stays on this
//! machine or goes to another device of the layout, and moves the virtual
//! cursor across the devices.
//!
//! The cursor is always on one device, in that device's own coordinates:
//! inside a device it moves across the displays like the real one would,
//! and pushed against an outer edge it goes wherever the [`World`] says the
//! canvas continues (another device, back home, or nowhere: a wall).
//!
//! Every key and button remembers which device its press went to, and its
//! release follows it there. That single rule keeps keys from getting stuck
//! when control changes hands in the middle of a press.
//!
//! A held button keeps the pointer on its device, with one exception: a
//! drag of files. When the left button alone pushes against an edge that
//! would let the pointer through, the engine is asked whether it drags
//! files ([`Emit::DragAtEdge`]); if so ([`Switch::allow_carry`]), the drag
//! goes along ([`Emit::Carry`]) and the release drops the files where the
//! pointer is then. Esc, or taking the drag back where it came from,
//! cancels it.
//!
//! Hotkeys are recognised here too, from the physical keys, and never
//! reach any device. By default (Mac: Option for Alt; see
//! [`Hotkeys`] for changing them):
//!
//! | keys | action |
//! |---|---|
//! | Ctrl+Alt+1..9 | jump to the n-th device of the layout |
//! | Ctrl+Alt+arrow | jump to the neighbour in that direction |
//! | Ctrl+Alt+L, Scroll Lock | lock the pointer to its device (toggle) |
//! | Ctrl+Alt+Esc | come home and pause crossing; again to resume |
//!
//! Crossing an edge follows the user's [`Switching`] and the settings of
//! that edge ([`EdgeSettings`]): it may be closed, guarded near corners,
//! or need a modifier held or a dwell against it. Key combinations the
//! user keeps local ([`Switch::set_keep_local`]) are pressed on this machine
//! even while another device is controlled, and so are media keys when the
//! user keeps them here ([`MediaKeys::Local`]).
//!
//! The pointer moves the same physical distance on every device, times the
//! pointer speed each device asks for ([`Switch::set_pointer_speeds`]).
//!
//! The switch runs inside the capture callback, synchronously: it must
//! decide before the OS delivers the event, and never blocks.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::config::{
    Chord, EdgeSettings, Hotkeys, MediaKeys, Mods, NORMAL_SPEED, SwitchMode, Switching, edge_key,
    is_modifier,
};
use crate::event::{InputEvent, MouseButton};
use crate::geometry::{Edge, Point, Rect, Step};
use crate::keymap::{self, usage};
use crate::world::World;

/// How far inside the edge the pointer lands after a crossing, so a
/// one-pixel jitter does not bounce it straight back
const LANDING_INSET: i32 = 1;

/// Local pointer travel (device units) that counts as someone using this
/// machine while another device controls it; smaller twitches are ignored
const TAKEOVER_TRAVEL: f64 = 4.0;

/// What the capture backend does with the event it just reported
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Deliver it locally as usual
    Pass,
    /// Drop it; it was forwarded to another device, or belongs to nobody
    Swallow,
}

/// Change of the local cursor the capture backend must apply
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorAction {
    /// Control moved to another device: freeze the local cursor where it is
    Park,
    /// Control came back: unfreeze the local cursor and put it here
    Release(Point),
    /// A drop of files here waits: keep the local cursor here, still shown,
    /// until [`CursorAction::Release`]
    Freeze(Point),
}

/// Outcome of one event
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// What to do with the event
    pub verdict: Verdict,
    /// Cursor change to apply, if control changed hands
    pub cursor: Option<CursorAction>,
}

/// Message for another device (or the engine), produced by
/// [`Switch::handle`]
#[derive(Debug, Clone, PartialEq)]
pub enum Emit {
    /// Control moves to `device`; its cursor goes to `at`
    Enter {
        /// The device
        device: String,
        /// Cursor position (the device's coordinates)
        at: Point,
    },
    /// Control leaves `device`, which releases whatever is still held
    Leave {
        /// The device
        device: String,
    },
    /// The virtual cursor moved to `at` on `device`
    Motion {
        /// The device
        device: String,
        /// Cursor position (the device's coordinates)
        at: Point,
    },
    /// A key press, autorepeat or release
    Key {
        /// The device
        device: String,
        /// USB HID usage, as the device should see it (Cmd / Ctrl swapped
        /// if configured)
        usage: u16,
        /// Pressed (true) or released
        down: bool,
    },
    /// A mouse button, with the cursor position it happened at
    Button {
        /// The device
        device: String,
        /// Which button
        button: MouseButton,
        /// Pressed (true) or released
        down: bool,
        /// Virtual cursor position (the device's coordinates)
        at: Point,
    },
    /// Scrolling (see [`InputEvent::Wheel`])
    Wheel {
        /// The device
        device: String,
        /// Horizontal amount
        dx: i32,
        /// Vertical amount
        dy: i32,
    },
    /// Someone used this machine's own mouse or keyboard while another
    /// device controlled it: that device must let go
    Takeover,
    /// Crossing edges was paused (true) or resumed
    Paused(bool),
    /// The pointer was locked to its device (true) or unlocked
    Locked(bool),
    /// Press or release a key on this machine itself: a combination kept
    /// local, whose real event the capture swallowed
    Local {
        /// USB HID usage
        usage: u16,
        /// Pressed (true) or released
        down: bool,
    },
    /// A key combination was recorded ([`Switch::record`]); `None` when
    /// the user gave up (Esc alone)
    Recorded(Option<Chord>),
    /// The left button went down or up on this machine itself (its press
    /// stayed here): a drag of files may start in between
    LocalPress {
        /// Pressed (true) or released
        down: bool,
    },
    /// A drag held with the left button on `device` (this one or another)
    /// pushes against an edge that would let the pointer through: whether
    /// it drags files decides whether it goes along (answered with
    /// [`Switch::allow_carry`]). Asked once per press
    DragAtEdge {
        /// The device the drag is held on
        device: String,
        /// The press, as the answer names it
        press: u64,
    },
    /// A drag of files went along with the pointer, from `origin` to `to`
    /// (either may be this device); follows the `Enter` or `Leave` of the
    /// move
    Carry {
        /// The device the files are dragged from
        origin: String,
        /// The device the pointer took them to
        to: String,
        /// Where the pointer entered `to`
        at: Point,
        /// The press, as [`Emit::DragAtEdge`] named it
        press: u64,
    },
    /// The left button carrying files to this machine was released: they
    /// land here
    Drop {
        /// Where the local cursor is
        at: Point,
    },
    /// The drag of files carried to `device` ends without dropping
    /// anything (Esc, or the pointer took it back)
    DragCancel {
        /// The device dragging the files
        device: String,
        /// Where the pointer is on it
        at: Point,
    },
}

/// A hotkey's action
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hotkey {
    /// Come home and pause crossing, or resume
    Pause,
    /// Lock the pointer to its device, or unlock
    Lock,
    /// Jump to the device with this index in the layout's reading order
    Number(usize),
    /// Jump to the neighbour in this direction
    Toward(Edge),
}

/// What the user asks for from outside the keyboard (the tray, the app
/// window); each does what its hotkey does
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Come home and pause crossing, or resume (Ctrl+Alt+Esc)
    Pause,
    /// Lock the pointer to its device, or unlock (Ctrl+Alt+L)
    Lock,
    /// Move control to this device, this machine included (Ctrl+Alt+n);
    /// ignored when it is not online
    Jump(String),
}

/// How control last came back to this machine
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Homecoming {
    /// Where the local cursor reappeared: on the edge it crossed, or
    /// mid-display after a jump, a pause or a release
    pub at: Point,
    /// It came back with a jump (a number or arrow hotkey, a request)
    pub jumped: bool,
}

/// Where the local cursor reappears when control comes back
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Landing {
    /// Where the canvas continues from the device the cursor leaves: the
    /// pointer simply carries on
    At(Point),
    /// In the middle of the display the cursor left from: after a hotkey or
    /// a lost device the user wants to stay here, and a pointer left at the
    /// edge would slip straight back at the slightest motion
    Centre,
}

/// Where a key or button press went
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    /// Delivered locally
    Local,
    /// Forwarded to this device
    Remote(String),
    /// Consumed by Lanroam itself (a hotkey)
    Dropped,
    /// Pressed on this machine by Lanroam in its place (a combination kept
    /// local); the release follows the same way
    Replayed,
    /// The left button carrying a drag of files (see [`Carry`]): its
    /// release drops them
    Carried,
}

/// A drag of files carried from one device to another: the left button
/// held since the press that started it, the pointer on the device that
/// drags the files now
#[derive(Debug, Clone, PartialEq, Eq)]
struct Carry {
    /// The device the files are dragged from
    origin: String,
    /// The device dragging them now, where the pointer is
    to: String,
}

/// What is known about the drag of the left button held now
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Probe {
    /// Nothing asked yet
    #[default]
    None,
    /// Asked whether it drags files ([`Emit::DragAtEdge`]), no answer yet
    Asked,
    /// It drags files: it may go along with the pointer
    Files,
}

/// What the buttons held allow at an edge
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// Nothing held on the device the pointer is on: it crosses freely
    Nothing,
    /// The left button alone, maybe dragging files
    Drag,
    /// A drag of files carried here: only back where it came from
    Carrying,
    /// Other buttons: the pointer stays
    Other,
}

/// Where the time comes from; tests move it by hand
#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> Instant + Send + Sync>);

impl Clock {
    /// A clock reading `now`
    pub fn new(now: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    /// The time
    fn now(&self) -> Instant {
        (self.0)()
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new(Instant::now)
    }
}

impl fmt::Debug for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Clock")
    }
}

/// The pointer pushing against an edge that crosses after a dwell
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dwell {
    /// The device it is on
    from: String,
    /// The edge
    edge: Edge,
    /// Since when
    since: Instant,
}

/// The virtual cursor on another device
#[derive(Debug, Clone, PartialEq)]
struct Remote {
    /// The device
    device: String,
    /// Horizontal position (the device's coordinates)
    x: f64,
    /// Vertical position
    y: f64,
}

impl Remote {
    /// The pixel the cursor is on
    fn pixel(&self) -> Point {
        Point::floor(self.x, self.y)
    }
}

/// The source-side state machine
#[derive(Debug)]
pub struct Switch {
    /// Every device on the canvas, this one included
    world: World,
    /// This device's key in the world
    local: String,
    /// The virtual cursor while another device is controlled
    remote: Option<Remote>,
    /// Where the local cursor left this machine, for coming back to it
    departed: Point,
    /// How control last came back
    homecoming: Homecoming,
    /// Where the local cursor is, as last seen
    local_at: Point,
    /// Where the cursor last was on each device left
    last: HashMap<String, Point>,
    /// Every placed device, online or not, in reading order: the numbers
    /// of the Ctrl+Alt+n hotkeys
    numbered: Vec<String>,
    /// Crossing edges is paused
    paused: bool,
    /// The pointer stays on its device
    locked: bool,
    /// Keys currently pressed (by physical usage): where each press went,
    /// and the usage it was sent as
    keys: HashMap<u16, (Owner, u16)>,
    /// Buttons currently pressed, and where each press went
    buttons: [Option<Owner>; MouseButton::COUNT],
    /// Presses of the left button so far: a probe names the press it asks
    /// about
    press: u64,
    /// What is known about the drag of the left button held now
    probe: Probe,
    /// The drag of files carried to another device, while its button is
    /// held
    carry: Option<Carry>,
    /// A drop of files waits for them to arrive where the pointer is, here
    /// or on the device controlled (set by the engine): the pointer holds
    /// still, clicks go nowhere, Esc cancels it
    awaiting_drop: bool,
    /// The local cursor is frozen for a drop waiting here
    frozen: bool,
    /// Hand control back at the next event (set from outside the capture)
    release_requested: bool,
    /// Requests to carry out at the next event (set from outside the
    /// capture)
    requests: Vec<Request>,
    /// Devices that get Command and Control swapped
    swapped: HashSet<String>,
    /// Pointer speed of each device, in percent; normal for the others
    speeds: HashMap<String, u32>,
    /// Where media keys go while another device is controlled
    media_keys: MediaKeys,
    /// Another device controls this machine right now
    controlled: bool,
    /// Local pointer travel since control was taken
    local_travel: f64,
    /// The hotkeys
    hotkeys: Hotkeys,
    /// Key combinations that stay on this machine while another device is
    /// controlled
    keep_local: Vec<Chord>,
    /// How the pointer crosses edges
    switching: Switching,
    /// Settings of single edges, by [`edge_key`]
    edges: HashMap<(String, String), EdgeSettings>,
    /// The pointer pushing against an edge in dwell mode
    dwell: Option<Dwell>,
    /// The time
    clock: Clock,
    /// Recording a key combination: the modifiers held meanwhile
    recorder: Option<HashSet<u16>>,
}

impl Switch {
    /// A switch for the device `local`, with nothing to switch to until
    /// [`Self::set_world`]
    pub fn new(local: impl Into<String>) -> Self {
        Self {
            world: World::default(),
            local: local.into(),
            remote: None,
            departed: Point::default(),
            homecoming: Homecoming::default(),
            local_at: Point::default(),
            last: HashMap::new(),
            numbered: Vec::new(),
            paused: false,
            locked: false,
            keys: HashMap::new(),
            buttons: std::array::from_fn(|_| None),
            press: 0,
            probe: Probe::None,
            carry: None,
            awaiting_drop: false,
            frozen: false,
            release_requested: false,
            requests: Vec::new(),
            swapped: HashSet::new(),
            speeds: HashMap::new(),
            media_keys: MediaKeys::default(),
            controlled: false,
            local_travel: 0.0,
            hotkeys: Hotkeys::default(),
            keep_local: Vec::new(),
            switching: Switching::default(),
            edges: HashMap::new(),
            dwell: None,
            clock: Clock::default(),
            recorder: None,
        }
    }

    /// The layout the switch works with
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The device being controlled, if any
    pub fn target(&self) -> Option<&str> {
        self.remote.as_ref().map(|r| r.device.as_str())
    }

    /// Whether input currently goes to another device
    pub fn is_remote(&self) -> bool {
        self.remote.is_some()
    }

    /// How control last came back to this machine
    pub fn homecoming(&self) -> Homecoming {
        self.homecoming
    }

    /// Update the layout, and the devices that get Command and Control
    /// swapped; control comes back if the device being controlled left it
    pub fn set_world(&mut self, world: World, swapped: HashSet<String>) {
        if let Some(remote) = &mut self.remote {
            match world.device(&remote.device) {
                Some(device) => {
                    let p = device.desktop.clamp(remote.pixel());
                    (remote.x, remote.y) = (f64::from(p.x), f64::from(p.y));
                }
                None => self.release_requested = true,
            }
        }
        self.world = world;
        self.swapped = swapped;
    }

    /// Set how fast the pointer goes on each device, in percent (normal for
    /// devices left out)
    pub fn set_pointer_speeds(&mut self, speeds: HashMap<String, u32>) {
        self.speeds = speeds;
    }

    /// Set where media keys go while another device is controlled
    pub fn set_media_keys(&mut self, media_keys: MediaKeys) {
        self.media_keys = media_keys;
    }

    /// Set the devices the number hotkeys go to, in order
    pub fn set_numbering(&mut self, numbered: Vec<String>) {
        self.numbered = numbered;
    }

    /// Set the hotkeys
    pub fn set_hotkeys(&mut self, hotkeys: Hotkeys) {
        self.hotkeys = hotkeys;
    }

    /// Set the key combinations that stay on this machine while another
    /// device is controlled
    pub fn set_keep_local(&mut self, chords: Vec<Chord>) {
        self.keep_local = chords;
    }

    /// Set how the pointer crosses edges
    pub fn set_switching(&mut self, switching: Switching) {
        self.switching = switching;
        self.dwell = None;
    }

    /// Set the settings of single edges, by [`edge_key`]
    pub fn set_edges(&mut self, edges: HashMap<(String, String), EdgeSettings>) {
        self.edges = edges;
    }

    /// Replace the clock (tests)
    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
    }

    /// Start recording a key combination, or stop: the next key pressed
    /// with the modifiers held is reported ([`Emit::Recorded`]) instead of
    /// going anywhere, and hotkeys wait meanwhile
    pub fn record(&mut self, on: bool) {
        self.recorder = on.then(|| {
            self.keys
                .keys()
                .copied()
                .filter(|key| is_modifier(*key))
                .collect()
        });
    }

    /// Whether a key combination is being recorded
    pub fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }

    /// Record with a key that did not come through the capture (from the
    /// device controlling this one); the outcome once a combination is
    /// complete, as [`Emit::Recorded`] carries it
    pub fn record_key(&mut self, key: u16, down: bool) -> Option<Option<Chord>> {
        let held = self.recorder.as_mut()?;
        if is_modifier(key) {
            if down {
                held.insert(key);
            } else {
                held.remove(&key);
            }
            return None;
        }
        if !down {
            return None;
        }
        let mods = Mods::held(held.iter().copied());
        self.recorder = None;
        if key == usage::ESCAPE && mods == Mods::default() {
            return Some(None);
        }
        Some(Some(Chord::new(mods, key)))
    }

    /// Hand control back to this machine at the next event, if `device`
    /// (any device for `None`) is being controlled: it stopped answering,
    /// its link dropped, someone else took it over
    ///
    /// Deferred to the next event because only the capture callback may
    /// move the local cursor; the user touching the mouse or keyboard is
    /// exactly when it matters.
    pub fn request_release(&mut self, device: Option<&str>) {
        if let Some(remote) = &self.remote
            && device.is_none_or(|d| d == remote.device)
        {
            self.release_requested = true;
        }
    }

    /// Carry out `request` at the next event, for the same reason as
    /// [`Self::request_release`]: whoever clicked the tray or the window
    /// moves the mouse right after
    pub fn request(&mut self, request: Request) {
        self.requests.push(request);
    }

    /// Whether another device controls this machine; while it does, local
    /// input emits [`Emit::Takeover`] once
    pub fn set_controlled(&mut self, controlled: bool) {
        self.controlled = controlled;
        self.local_travel = 0.0;
    }

    /// Whether a drop of files waits for them to arrive where the pointer
    /// is (this machine, or the device controlled): meanwhile the pointer
    /// holds still (from the next event on here), clicks and scrolling go
    /// nowhere, and Esc cancels it ([`Emit::DragCancel`] for this device;
    /// the device controlled hears Esc itself). Over once the pointer goes
    /// to another device
    pub fn set_awaiting_drop(&mut self, waiting: bool) {
        self.awaiting_drop = waiting;
    }

    /// Whether a drop of files waits where the pointer is
    pub fn is_awaiting_drop(&self) -> bool {
        self.awaiting_drop
    }

    /// The drag of the press `press` ([`Emit::DragAtEdge`]) drags files: it
    /// goes along with the pointer from the next push on. Ignored once that
    /// press is over
    pub fn allow_carry(&mut self, press: u64) {
        if press == self.press && self.probe == Probe::Asked {
            self.probe = Probe::Files;
        }
    }

    /// Decide what happens to one captured event; messages for other
    /// devices are appended to `out`
    pub fn handle(&mut self, event: InputEvent, out: &mut Vec<Emit>) -> Decision {
        let mut cursor = None;
        if std::mem::take(&mut self.release_requested) && self.remote.is_some() {
            tracing::debug!("control comes back on request");
            cursor = Some(self.come_home(Landing::Centre, out));
        }
        for request in std::mem::take(&mut self.requests) {
            self.run_request(request, out, &mut cursor);
        }
        self.freeze_for_drop(&mut cursor);
        if self.controlled && self.remote.is_none() && self.used_locally(&event) {
            self.controlled = false;
            out.push(Emit::Takeover);
        }
        let verdict = match event {
            InputEvent::Motion { at, dx, dy } => self.motion(at, dx, dy, out, &mut cursor),
            InputEvent::Button { button, down } => self.button(button, down, out),
            InputEvent::Wheel { dx, dy } => self.wheel(dx, dy, out),
            InputEvent::Key { usage, down } => self.key(usage, down, out, &mut cursor),
        };
        Decision { verdict, cursor }
    }

    /// Freeze the local cursor while a drop here waits, and let it go after
    /// (only the capture may move it, hence at an event)
    fn freeze_for_drop(&mut self, cursor: &mut Option<CursorAction>) {
        if cursor.is_some() || self.remote.is_some() {
            return;
        }
        if self.awaiting_drop && !self.frozen {
            self.frozen = true;
            *cursor = Some(CursorAction::Freeze(self.local_at));
        } else if !self.awaiting_drop && self.frozen {
            self.frozen = false;
            *cursor = Some(CursorAction::Release(self.local_at));
        }
    }

    /// Whether `event` shows someone using this machine's own devices
    fn used_locally(&mut self, event: &InputEvent) -> bool {
        match *event {
            InputEvent::Motion { dx, dy, .. } => {
                self.local_travel += dx.abs() + dy.abs();
                self.local_travel > TAKEOVER_TRAVEL
            }
            _ => true,
        }
    }

    /// Pointer motion: cross to another device, move the virtual cursor, or
    /// come back
    fn motion(
        &mut self,
        at: Point,
        dx: f64,
        dy: f64,
        out: &mut Vec<Emit>,
        cursor: &mut Option<CursorAction>,
    ) -> Verdict {
        let Some(remote) = self.remote.clone() else {
            // Held where a drop waits
            if self.frozen {
                return Verdict::Swallow;
            }
            self.local_at = at;
            // No crossing right after a forced release, or a motion that
            // arrives with it would take control straight back
            if cursor.is_none()
                && let Some((device, entry, departed, held)) = self.crossing(at, dx, dy, out)
            {
                self.departed = departed;
                self.enter(device, entry, out);
                if held == Held::Drag {
                    let origin = self.local.clone();
                    self.carry_on(origin, entry, out);
                }
                *cursor = Some(CursorAction::Park);
                return Verdict::Swallow;
            }
            return Verdict::Pass;
        };
        let Some(target) = self.world.device(&remote.device) else {
            // Gone from the layout; the release is already requested
            return Verdict::Swallow;
        };
        // Held where a drop waits there
        if self.awaiting_drop {
            return Verdict::Swallow;
        }
        // Same physical travel on every device (source units to logical
        // pixels to the target's units), at the target's pointer speed
        let local_scale = self.world.device(&self.local).map_or(1.0, |d| d.scale);
        let k = target.scale / local_scale * self.speed_of(&remote.device);
        let (x, y) = match target.desktop.step((remote.x, remote.y), dx * k, dy * k) {
            Step::Inside(x, y) => {
                self.keep_dwell(&remote.device, Point::floor(x, y));
                (x, y)
            }
            Step::Blocked { x, y, .. } if self.locked => (x, y),
            // A held button keeps the pointer on the device, unless it drags
            // files (see `lets_through`)
            Step::Blocked { x, y, .. } if self.held() == Held::Other => (x, y),
            Step::Blocked { x, y, stop, edges } => {
                let held = self.held();
                let candidates = edges
                    .into_iter()
                    .filter_map(|edge| {
                        self.world
                            .cross(&remote.device, stop, edge, LANDING_INSET)
                            .map(|(device, entry)| (edge, device.key.clone(), entry))
                    })
                    .collect();
                let passed = self
                    .pass(&remote.device, stop, candidates)
                    .filter(|(device, _)| self.lets_through(held, &remote.device, device, out));
                // The drag held on the device left goes along
                let carried = (held == Held::Drag).then(|| remote.device.clone());
                match passed {
                    Some((device, entry)) if device == self.local => {
                        *cursor = Some(self.come_home(Landing::At(entry), out));
                        if let Some(origin) = carried {
                            self.carry_on(origin, entry, out);
                        }
                        return Verdict::Swallow;
                    }
                    Some((device, entry)) => {
                        self.remote = Some(Remote { x, y, ..remote });
                        self.leave_remote(out);
                        self.enter(device, entry, out);
                        if let Some(origin) = carried {
                            self.carry_on(origin, entry, out);
                        }
                        return Verdict::Swallow;
                    }
                    // A wall, or an edge that does not let it through yet:
                    // slide along it
                    None => (x, y),
                }
            }
        };
        let before = remote.pixel();
        let moved = Remote { x, y, ..remote };
        if moved.pixel() != before {
            out.push(Emit::Motion {
                device: moved.device.clone(),
                at: moved.pixel(),
            });
        }
        self.remote = Some(moved);
        Verdict::Swallow
    }

    /// Where this motion takes the local pointer when it pushes out of this
    /// machine: (device, entry point, departure point, buttons held)
    fn crossing(
        &mut self,
        at: Point,
        dx: f64,
        dy: f64,
        out: &mut Vec<Emit>,
    ) -> Option<(String, Point, Point, Held)> {
        let held = self.held();
        if self.paused || self.locked || held == Held::Other {
            self.dwell = None;
            return None;
        }
        let local = self.world.device(&self.local)?;
        let at = local.desktop.clamp(at);
        let candidates = Edge::ALL
            .into_iter()
            .filter(|edge| edge.pushed_by(dx, dy) && local.desktop.on_edge(at, *edge))
            .filter_map(|edge| {
                self.world
                    .cross(&self.local, at, edge, LANDING_INSET)
                    .map(|(device, entry)| (edge, device.key.clone(), entry))
            })
            .collect();
        let from = self.local.clone();
        self.keep_dwell(&from, at);
        let (device, entry) = self.pass(&from, at, candidates)?;
        self.lets_through(held, &from, &device, out)
            .then_some((device, entry, at, held))
    }

    /// What the buttons held allow at an edge: those pressed on the device
    /// the pointer is on count (a press elsewhere was released there)
    fn held(&self) -> Held {
        if self.carry.is_some() {
            return Held::Carrying;
        }
        let remote = self.remote.is_some();
        let here = |button: &MouseButton| match &self.buttons[button.index()] {
            Some(Owner::Local) => !remote,
            Some(Owner::Remote(_)) => remote,
            _ => false,
        };
        let mut held = MouseButton::ALL.into_iter().filter(here);
        match (held.next(), held.next()) {
            (None, _) => Held::Nothing,
            (Some(MouseButton::Left), None) => Held::Drag,
            _ => Held::Other,
        }
    }

    /// Whether the buttons held (`held`) let the pointer on `on` over to
    /// `to`: a drag goes along once it is known to drag files, which is
    /// asked the first time; a drag carried here only goes back where it
    /// came from, which cancels it
    fn lets_through(&mut self, held: Held, on: &str, to: &str, out: &mut Vec<Emit>) -> bool {
        match held {
            Held::Nothing => true,
            Held::Other => false,
            Held::Carrying => self.carry.as_ref().is_some_and(|carry| carry.origin == to),
            Held::Drag => match self.probe {
                Probe::Files => true,
                Probe::Asked => false,
                Probe::None => {
                    self.probe = Probe::Asked;
                    out.push(Emit::DragAtEdge {
                        device: on.to_string(),
                        press: self.press,
                    });
                    false
                }
            },
        }
    }

    /// The drag held on `origin` went along to the device the pointer is on
    /// now, entered at `at`: the left button carries it
    fn carry_on(&mut self, origin: String, at: Point, out: &mut Vec<Emit>) {
        let to = self.target().unwrap_or(&self.local).to_string();
        self.buttons[MouseButton::Left.index()] = Some(Owner::Carried);
        out.push(Emit::Carry {
            origin: origin.clone(),
            to: to.clone(),
            at,
            press: self.press,
        });
        self.carry = Some(Carry { origin, to });
    }

    /// The left button carrying files was released: they land where the
    /// pointer is. A press made here is released here too, where the drag
    /// held under the parked cursor ends (the engine's catcher refuses it)
    fn drop_carry(&mut self, out: &mut Vec<Emit>) -> Verdict {
        self.probe = Probe::None;
        let Some(carry) = self.carry.take() else {
            return Verdict::Swallow;
        };
        match &self.remote {
            Some(remote) if remote.device == carry.to => out.push(Emit::Button {
                device: carry.to,
                button: MouseButton::Left,
                down: false,
                at: remote.pixel(),
            }),
            None if carry.to == self.local => out.push(Emit::Drop { at: self.local_at }),
            _ => {}
        }
        if carry.origin == self.local {
            out.push(Emit::LocalPress { down: false });
            return Verdict::Pass;
        }
        Verdict::Swallow
    }

    /// The drag of files carried to another device ends without dropping
    /// anything: that device lets go of it. The button still held stays
    /// with a drag held on this machine, which goes on here; a drag held
    /// elsewhere was let go of already
    fn cancel_carry(&mut self, out: &mut Vec<Emit>) {
        let Some(carry) = self.carry.take() else {
            return;
        };
        let at = match &self.remote {
            Some(remote) if remote.device == carry.to => remote.pixel(),
            _ => self.local_at,
        };
        out.push(Emit::DragCancel {
            device: carry.to,
            at,
        });
        let owner = if carry.origin == self.local {
            Owner::Local
        } else {
            Owner::Dropped
        };
        self.buttons[MouseButton::Left.index()] = Some(owner);
    }

    /// End a dwell once the pointer, now at `at` on device `from`, is off
    /// its edge. Motions in between that do not push outwards keep it: a
    /// hand pushing against an edge slides along it too, and fast mice
    /// report many motions with nothing outwards
    fn keep_dwell(&mut self, from: &str, at: Point) {
        let stays = self.dwell.as_ref().is_some_and(|dwell| {
            dwell.from == from
                && self
                    .world
                    .device(from)
                    .is_some_and(|device| device.desktop.on_edge(at, dwell.edge))
        });
        if !stays {
            self.dwell = None;
        }
    }

    /// The first of the crossings the pointer pushed at `at` on device
    /// `from` could take, (edge, device, entry), that its edge lets through
    /// now (see [`Self::may_cross`])
    fn pass(
        &mut self,
        from: &str,
        at: Point,
        candidates: Vec<(Edge, String, Point)>,
    ) -> Option<(String, Point)> {
        for (edge, device, entry) in candidates {
            if self.may_cross(from, &device, at, edge) {
                self.dwell = None;
                return Some((device, entry));
            }
        }
        None
    }

    /// Whether the edge between `from` and `to` lets the pointer through at
    /// `at` now: it is open, `at` is clear of its corner guard, and its
    /// switching mode agrees (a modifier held, or a dwell long enough,
    /// which the first push starts)
    fn may_cross(&mut self, from: &str, to: &str, at: Point, edge: Edge) -> bool {
        let settings = self
            .edges
            .get(&edge_key(from, to))
            .copied()
            .unwrap_or_default();
        if !settings.crossable {
            return false;
        }
        let corner = f64::from(settings.corner_px.unwrap_or(self.switching.corner_px));
        if self
            .world
            .corner_distance(from, at, edge)
            .is_some_and(|distance| distance < corner)
        {
            return false;
        }
        match settings.mode.unwrap_or(self.switching.mode) {
            SwitchMode::Direct => true,
            SwitchMode::Modifier => self.switching.hold.held_in(self.mods()),
            SwitchMode::Dwell => self.dwelt(from, edge),
        }
    }

    /// Whether the pointer has pushed against `edge` of `from` for the
    /// dwell time; the first push starts counting
    fn dwelt(&mut self, from: &str, edge: Edge) -> bool {
        let now = self.clock.now();
        match &self.dwell {
            Some(dwell) if dwell.from == from && dwell.edge == edge => {
                let wait = Duration::from_millis(u64::from(self.switching.dwell_ms));
                now.duration_since(dwell.since) >= wait
            }
            _ => {
                self.dwell = Some(Dwell {
                    from: from.to_string(),
                    edge,
                    since: now,
                });
                false
            }
        }
    }

    /// Take control of `device` with its cursor at `at`
    fn enter(&mut self, device: String, at: Point, out: &mut Vec<Emit>) {
        // A drag carried to this machine does not follow, nor a drop
        // waiting here
        self.cancel_carry(out);
        self.awaiting_drop = false;
        self.frozen = false;
        tracing::debug!(%device, "control moves to another device");
        out.push(Emit::Enter {
            device: device.clone(),
            at,
        });
        self.remote = Some(Remote {
            device,
            x: f64::from(at.x),
            y: f64::from(at.y),
        });
    }

    /// Give control back to this machine: tell the device, and work out
    /// where the local cursor reappears
    fn come_home(&mut self, landing: Landing, out: &mut Vec<Emit>) -> CursorAction {
        self.leave_remote(out);
        let local = self.world.device(&self.local).map(|d| &d.desktop);
        let back = match landing {
            Landing::At(entry) => entry,
            Landing::Centre => local
                .and_then(|desktop| desktop.display_at(self.departed))
                .map_or(self.departed, Rect::centre),
        };
        self.homecoming = Homecoming {
            at: back,
            jumped: false,
        };
        // Where a drop lands should the button go up before any motion
        self.local_at = back;
        CursorAction::Release(back)
    }

    /// A mouse button: the release goes where the press went
    fn button(&mut self, button: MouseButton, down: bool, out: &mut Vec<Emit>) -> Verdict {
        if !down && self.buttons[button.index()] == Some(Owner::Carried) {
            self.buttons[button.index()] = None;
            return self.drop_carry(out);
        }
        // A drop waits where the pointer is: a click would move it
        if down && self.awaiting_drop {
            self.buttons[button.index()] = Some(Owner::Dropped);
            return Verdict::Swallow;
        }
        let side = self.side();
        let slot = &mut self.buttons[button.index()];
        let owner = if down {
            *slot = Some(side.clone());
            Some(side)
        } else {
            slot.take()
        };
        if button == MouseButton::Left {
            // Each press of the left button starts afresh; the engine hears
            // of those made here, where a drag of files may start
            if down {
                self.press += 1;
            }
            self.probe = Probe::None;
            if owner == Some(Owner::Local) {
                out.push(Emit::LocalPress { down });
            }
        }
        let at = self
            .remote
            .as_ref()
            .map_or_else(Point::default, Remote::pixel);
        self.route(owner, out, |device| Emit::Button {
            device,
            button,
            down,
            at,
        })
    }

    /// Scrolling follows the current side (nowhere while a drop waits)
    fn wheel(&mut self, dx: i32, dy: i32, out: &mut Vec<Emit>) -> Verdict {
        if self.awaiting_drop {
            return Verdict::Swallow;
        }
        let Some(remote) = &self.remote else {
            return Verdict::Pass;
        };
        out.push(Emit::Wheel {
            device: remote.device.clone(),
            dx,
            dy,
        });
        Verdict::Swallow
    }

    /// A key: presses go to the current side, autorepeats and releases
    /// follow the press
    fn key(
        &mut self,
        key: u16,
        down: bool,
        out: &mut Vec<Emit>,
        cursor: &mut Option<CursorAction>,
    ) -> Verdict {
        tracing::trace!(key, down, remote = self.remote.is_some(), "key");
        // A press completing a recording goes nowhere, nor its release
        if let Some(recorded) = self.record_key(key, down) {
            out.push(Emit::Recorded(recorded));
            self.keys.insert(key, (Owner::Dropped, key));
            return Verdict::Swallow;
        }
        // Esc cancels a drag of files carried to another device, and goes
        // nowhere itself; so does a drop here waiting for its files
        if down && key == usage::ESCAPE && self.carry.is_some() {
            self.cancel_carry(out);
            self.keys.insert(key, (Owner::Dropped, key));
            return Verdict::Swallow;
        }
        if down && key == usage::ESCAPE && self.awaiting_drop && self.remote.is_none() {
            self.awaiting_drop = false;
            out.push(Emit::DragCancel {
                device: self.local.clone(),
                at: self.local_at,
            });
            self.keys.insert(key, (Owner::Dropped, key));
            return Verdict::Swallow;
        }
        if !down {
            let (owner, sent) = match self.keys.remove(&key) {
                Some((owner, sent)) => (Some(owner), sent),
                None => (None, key),
            };
            if owner == Some(Owner::Replayed) {
                out.push(Emit::Local { usage: sent, down });
                return Verdict::Swallow;
            }
            return self.route(owner, out, |device| Emit::Key {
                device,
                usage: sent,
                down,
            });
        }
        let fresh = !self.keys.contains_key(&key);
        // A fresh press may complete a hotkey; its release is dropped too
        if fresh
            && let Some(hotkey) = self.hotkey_for(key)
            && self.run_hotkey(hotkey, out, cursor)
        {
            self.keys.insert(key, (Owner::Dropped, key));
            return Verdict::Swallow;
        }
        if fresh && self.remote.is_some() && self.keeps_local(key) {
            return self.keep_here(key, out);
        }
        // Media keys kept here work this machine, autorepeats included
        if self.remote.is_some()
            && self.media_keys == MediaKeys::Local
            && keymap::is_media(key)
            && self
                .keys
                .get(&key)
                .is_none_or(|(owner, _)| *owner == Owner::Local)
        {
            self.keys.insert(key, (Owner::Local, key));
            return Verdict::Pass;
        }
        let (owner, sent) = match self.keys.get(&key) {
            Some(held) => held.clone(),
            None => {
                let side = self.side();
                let sent = match &side {
                    Owner::Remote(device) if self.swapped.contains(device) => {
                        keymap::swap_cmd_ctrl(key)
                    }
                    _ => key,
                };
                self.keys.insert(key, (side.clone(), sent));
                (side, sent)
            }
        };
        if owner == Owner::Replayed {
            out.push(Emit::Local { usage: sent, down });
            return Verdict::Swallow;
        }
        // Autorepeat of a key held down since before the crossing: the local
        // screen is not being looked at, so it must not type there
        if owner == Owner::Local && self.remote.is_some() {
            return Verdict::Swallow;
        }
        self.route(Some(owner), out, |device| Emit::Key {
            device,
            usage: sent,
            down,
        })
    }

    /// The pointer speed of `device`, as a factor
    fn speed_of(&self, device: &str) -> f64 {
        let speed = self.speeds.get(device).copied().unwrap_or(NORMAL_SPEED);
        f64::from(speed) / f64::from(NORMAL_SPEED)
    }

    /// The modifiers held now
    fn mods(&self) -> Mods {
        Mods::held(self.keys.keys().copied())
    }

    /// The hotkey `key` completes with the keys held now, if any: exactly
    /// its modifiers, with the left Alt where AltGr could be meant (see
    /// [`Chord::needs_left_alt`]); Scroll Lock alone locks too
    fn hotkey_for(&self, key: u16) -> Option<Hotkey> {
        if key == usage::SCROLL_LOCK {
            return Some(Hotkey::Lock);
        }
        let mods = self.mods();
        let fits = |want: Mods| {
            mods == want
                && (!Chord::needs_left_alt(mods, key) || self.keys.contains_key(&usage::LEFT_ALT))
        };
        let hotkeys = &self.hotkeys;
        if key == hotkeys.pause.key && fits(hotkeys.pause.mods) {
            return Some(Hotkey::Pause);
        }
        if key == hotkeys.lock.key && fits(hotkeys.lock.mods) {
            return Some(Hotkey::Lock);
        }
        let toward = match key {
            usage::ARROW_RIGHT => Edge::Right,
            usage::ARROW_LEFT => Edge::Left,
            usage::ARROW_DOWN => Edge::Bottom,
            usage::ARROW_UP => Edge::Top,
            usage::DIGIT_1..=usage::DIGIT_9 if fits(hotkeys.jump) => {
                return Some(Hotkey::Number(usize::from(key - usage::DIGIT_1)));
            }
            _ => return None,
        };
        fits(hotkeys.step).then_some(Hotkey::Toward(toward))
    }

    /// Whether the press of `key` with the modifiers held now is a
    /// combination kept on this machine
    fn keeps_local(&self, key: u16) -> bool {
        let chord = Chord::new(self.mods(), key);
        self.keep_local.contains(&chord)
    }

    /// Press a combination kept local here instead of on the device
    /// controlled: that device gets the modifiers it holds lifted, and this
    /// machine gets them pressed, then the key
    fn keep_here(&mut self, key: u16, out: &mut Vec<Emit>) -> Verdict {
        let mut modifiers: Vec<u16> = self
            .keys
            .keys()
            .copied()
            .filter(|k| is_modifier(*k))
            .collect();
        modifiers.sort_unstable();
        for modifier in modifiers {
            let Some((owner, sent)) = self.keys.get(&modifier).cloned() else {
                continue;
            };
            match owner {
                // Pressed here before the crossing: this machine has it
                Owner::Local | Owner::Replayed => continue,
                Owner::Remote(device) => {
                    if self.target() == Some(device.as_str()) {
                        out.push(Emit::Key {
                            device,
                            usage: sent,
                            down: false,
                        });
                    }
                }
                Owner::Dropped | Owner::Carried => {}
            }
            out.push(Emit::Local {
                usage: modifier,
                down: true,
            });
            self.keys.insert(modifier, (Owner::Replayed, modifier));
        }
        out.push(Emit::Local {
            usage: key,
            down: true,
        });
        self.keys.insert(key, (Owner::Replayed, key));
        Verdict::Swallow
    }

    /// Carry out a hotkey; false if it has nothing to act on (no such
    /// device), so the key goes on as an ordinary one
    ///
    /// Handled here, in the capture callback, so they work whatever state
    /// the other devices or the network are in.
    fn run_hotkey(
        &mut self,
        hotkey: Hotkey,
        out: &mut Vec<Emit>,
        cursor: &mut Option<CursorAction>,
    ) -> bool {
        match hotkey {
            Hotkey::Pause => {
                if self.remote.is_some() {
                    tracing::debug!("pause hotkey: taking control back");
                    *cursor = Some(self.come_home(Landing::Centre, out));
                    self.paused = true;
                } else {
                    self.paused = !self.paused;
                }
                out.push(Emit::Paused(self.paused));
            }
            Hotkey::Lock => {
                self.locked = !self.locked;
                out.push(Emit::Locked(self.locked));
            }
            Hotkey::Number(n) => {
                let Some(device) = self.numbered.get(n).cloned() else {
                    return false;
                };
                if self.world.device(&device).is_none() {
                    // Offline: taken, but nowhere to go
                    return true;
                }
                self.jump(device, out, cursor);
            }
            Hotkey::Toward(edge) => {
                let here = self.target().unwrap_or(&self.local).to_string();
                let Some(device) = self.world.neighbour(&here, edge) else {
                    return false;
                };
                let device = device.key.clone();
                self.jump(device, out, cursor);
            }
        }
        true
    }

    /// Carry out a request from outside the keyboard
    fn run_request(
        &mut self,
        request: Request,
        out: &mut Vec<Emit>,
        cursor: &mut Option<CursorAction>,
    ) {
        match request {
            Request::Pause => {
                self.run_hotkey(Hotkey::Pause, out, cursor);
            }
            Request::Lock => {
                self.run_hotkey(Hotkey::Lock, out, cursor);
            }
            Request::Jump(device) => {
                if device == self.local || self.world.device(&device).is_some() {
                    self.jump(device, out, cursor);
                }
            }
        }
    }

    /// Move control straight to `device`, landing mid-display (see
    /// [`Self::landing_on`]); a jump resumes a paused switch
    fn jump(&mut self, device: String, out: &mut Vec<Emit>, cursor: &mut Option<CursorAction>) {
        if std::mem::take(&mut self.paused) {
            out.push(Emit::Paused(false));
        }
        let here = self.target().unwrap_or(&self.local);
        if device == here {
            return;
        }
        if device == self.local {
            *cursor = Some(self.come_home(Landing::Centre, out));
            self.homecoming.jumped = true;
            return;
        }
        let Some(at) = self.landing_on(&device) else {
            return;
        };
        match self.leave_remote(out) {
            Some(_) => {}
            None => {
                self.departed = self.local_at;
                *cursor = Some(CursorAction::Park);
            }
        }
        self.enter(device, at, out);
    }

    /// Where a jump lands on `device`: the middle of the display its cursor
    /// was last on, or of its primary display. The middle, so that the next
    /// motion does not push it straight over an edge
    fn landing_on(&self, device: &str) -> Option<Point> {
        let desktop = &self.world.device(device)?.desktop;
        let display = self
            .last
            .get(device)
            .and_then(|p| desktop.display_at(*p))
            .or_else(|| desktop.display_at(Point::new(0, 0)))
            .or_else(|| desktop.displays().first())?;
        Some(display.centre())
    }

    /// Stop controlling the current device, remembering where its cursor
    /// was; returns it
    fn leave_remote(&mut self, out: &mut Vec<Emit>) -> Option<String> {
        // A drag carried there stays behind, cancelled, and so does a drop
        // waiting there
        self.cancel_carry(out);
        self.awaiting_drop = false;
        let remote = self.remote.take()?;
        self.last.insert(remote.device.clone(), remote.pixel());
        out.push(Emit::Leave {
            device: remote.device.clone(),
        });
        Some(remote.device)
    }

    /// Where new presses go right now
    fn side(&self) -> Owner {
        match &self.remote {
            Some(remote) => Owner::Remote(remote.device.clone()),
            None => Owner::Local,
        }
    }

    /// Deliver a press or release according to where its press went.
    /// Remote ones reach their device only while it is the one being
    /// controlled: once control left it, it released everything already
    fn route(
        &self,
        owner: Option<Owner>,
        out: &mut Vec<Emit>,
        emit: impl FnOnce(String) -> Emit,
    ) -> Verdict {
        match (owner, &self.remote) {
            (Some(Owner::Local), _) => Verdict::Pass,
            (Some(Owner::Remote(device)), Some(remote)) if remote.device == device => {
                out.push(emit(device));
                Verdict::Swallow
            }
            (Some(Owner::Remote(_) | Owner::Dropped | Owner::Replayed | Owner::Carried), _) => {
                Verdict::Swallow
            }
            // Pressed before the capture started
            (None, Some(_)) => Verdict::Swallow,
            (None, None) => Verdict::Pass,
        }
    }
}

/// Lock a switch shared between the capture thread and the engine
///
/// A panic while it was held cannot leave it inconsistent in a way worse
/// than losing input altogether, so a poisoned lock is simply taken over.
pub fn lock(switch: &Mutex<Switch>) -> MutexGuard<'_, Switch> {
    switch.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Desktop;
    use crate::world::Device;

    /// A device with one display of `w`x`h` at canvas `x`
    fn device(key: &str, x: i32, w: i32, h: i32, scale: f64) -> Device {
        Device::new(
            key,
            Desktop::new([Rect::new(0, 0, w, h)]),
            Point::new(x, 0),
            scale,
        )
    }

    /// `mac` (1512x1080) | `pc` (1920x1080) | `tv` (1920x1080), side by side,
    /// seen from the Mac; equal heights keep crossings one to one
    fn switch() -> Switch {
        let mut sw = Switch::new("mac");
        let world = World::new([
            device("mac", 0, 1512, 1080, 1.0),
            device("pc", 1512, 1920, 1080, 1.0),
            device("tv", 3432, 1920, 1080, 1.0),
        ]);
        sw.set_world(world, HashSet::new());
        sw
    }

    /// Feed one event and collect what it produced
    fn feed(sw: &mut Switch, event: InputEvent) -> (Decision, Vec<Emit>) {
        let mut out = Vec::new();
        let decision = sw.handle(event, &mut out);
        (decision, out)
    }

    /// A motion event
    fn motion(x: i32, y: i32, dx: f64, dy: f64) -> InputEvent {
        InputEvent::Motion {
            at: Point::new(x, y),
            dx,
            dy,
        }
    }

    /// A key event
    fn key(usage: u16, down: bool) -> InputEvent {
        InputEvent::Key { usage, down }
    }

    /// Push through the Mac's right edge at height `y`
    fn cross_to_pc(sw: &mut Switch, y: i32) -> (Decision, Vec<Emit>) {
        feed(sw, motion(1511, y, 5.0, 0.0))
    }

    /// Crossing parks the local cursor and enters the PC at the same height
    #[test]
    fn crosses_into_the_neighbour() {
        let mut sw = switch();
        let (d, out) = feed(&mut sw, motion(1500, 400, 5.0, 0.0));
        assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));
        let (d, out) = cross_to_pc(&mut sw, 400);
        assert_eq!(d.cursor, Some(CursorAction::Park));
        assert_eq!(
            out,
            [Emit::Enter {
                device: "pc".into(),
                at: Point::new(1, 400)
            }]
        );
        assert_eq!(sw.target(), Some("pc"));
    }

    /// Through the PC into the TV, and all the way back home
    #[test]
    fn hops_across_devices_and_back() {
        let mut sw = switch();
        cross_to_pc(&mut sw, 400);
        let (_, out) = feed(&mut sw, motion(0, 0, 1918.0, 0.0));
        assert_eq!(
            out,
            [Emit::Motion {
                device: "pc".into(),
                at: Point::new(1919, 400)
            }]
        );
        let (d, out) = feed(&mut sw, motion(0, 0, 3.0, 0.0));
        assert_eq!(d.cursor, None);
        assert_eq!(
            out,
            [
                Emit::Leave {
                    device: "pc".into()
                },
                Emit::Enter {
                    device: "tv".into(),
                    at: Point::new(1, 400)
                }
            ]
        );
        // Back left: TV → PC → Mac
        feed(&mut sw, motion(0, 0, -5.0, 0.0));
        assert_eq!(sw.target(), Some("pc"));
        feed(&mut sw, motion(0, 0, -1918.0, 0.0));
        let (d, out) = feed(&mut sw, motion(0, 0, -5.0, 0.0));
        assert_eq!(
            out,
            [Emit::Leave {
                device: "pc".into()
            }]
        );
        assert_eq!(d.cursor, Some(CursorAction::Release(Point::new(1510, 400))));
        assert_eq!(
            sw.homecoming(),
            Homecoming {
                at: Point::new(1510, 400),
                jumped: false
            }
        );
        assert!(!sw.is_remote());
    }

    /// Sides nobody faces are walls: the cursor slides along them
    #[test]
    fn walls_hold_the_cursor() {
        let mut sw = switch();
        cross_to_pc(&mut sw, 400);
        // Down past the PC's bottom, then up past its top: nothing there
        let (_, out) = feed(&mut sw, motion(0, 0, 10.0, 5000.0));
        assert_eq!(
            out,
            [Emit::Motion {
                device: "pc".into(),
                at: Point::new(11, 1079)
            }]
        );
        let (d, out) = feed(&mut sw, motion(0, 0, 5.0, -5000.0));
        assert_eq!(d.cursor, None);
        assert_eq!(
            out,
            [Emit::Motion {
                device: "pc".into(),
                at: Point::new(16, 0)
            }]
        );
        assert_eq!(sw.target(), Some("pc"));
    }

    /// Pointer travel is the same on a device with another scale
    #[test]
    fn motion_is_scaled() {
        let mut sw = Switch::new("mac");
        let world = World::new([
            device("mac", 0, 1512, 1080, 1.0),
            device("pc", 1512, 2880, 1620, 1.5),
        ]);
        sw.set_world(world, HashSet::new());
        cross_to_pc(&mut sw, 400);
        let (_, out) = feed(&mut sw, motion(0, 0, 10.0, 0.0));
        assert_eq!(
            out,
            [Emit::Motion {
                device: "pc".into(),
                at: Point::new(16, 600)
            }]
        );
    }

    /// The pointer goes at the speed of the device it is on
    #[test]
    fn pointer_speed() {
        let mut sw = switch();
        sw.set_pointer_speeds(HashMap::from([("pc".to_string(), 200)]));
        cross_to_pc(&mut sw, 400);
        let (_, out) = feed(&mut sw, motion(0, 0, 10.0, -5.0));
        assert_eq!(
            out,
            [Emit::Motion {
                device: "pc".into(),
                at: Point::new(21, 390)
            }]
        );
    }

    /// Media keys go to the device controlled, or stay here when the user
    /// keeps them here; other keys still go
    #[test]
    fn media_keys() {
        let mut sw = switch();
        cross_to_pc(&mut sw, 400);
        let (d, out) = feed(&mut sw, key(usage::VOLUME_UP, true));
        assert_eq!(d.verdict, Verdict::Swallow);
        assert_eq!(
            out,
            [Emit::Key {
                device: "pc".into(),
                usage: usage::VOLUME_UP,
                down: true
            }]
        );
        feed(&mut sw, key(usage::VOLUME_UP, false));
        sw.set_media_keys(MediaKeys::Local);
        for down in [true, true, false] {
            let (d, out) = feed(&mut sw, key(usage::MEDIA_PLAY_PAUSE, down));
            assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));
        }
        let (d, _) = feed(&mut sw, key(0x04, true));
        assert_eq!(d.verdict, Verdict::Swallow);
    }

    /// A button event
    fn button(button: MouseButton, down: bool) -> InputEvent {
        InputEvent::Button { button, down }
    }

    /// Held buttons keep the pointer on its side, both ways
    #[test]
    fn held_buttons_block_crossing() {
        let mut sw = switch();
        feed(&mut sw, button(MouseButton::Right, true));
        assert_eq!(cross_to_pc(&mut sw, 400).1, []);
        feed(&mut sw, button(MouseButton::Right, false));
        cross_to_pc(&mut sw, 400);
        let (_, out) = feed(&mut sw, button(MouseButton::Right, true));
        assert!(matches!(&out[..], [Emit::Button { device, down: true, .. }] if device == "pc"));
        let (d, out) = feed(&mut sw, motion(0, 0, -50.0, 0.0));
        assert_eq!(d.cursor, None);
        assert!(matches!(&out[..], [Emit::Motion { .. }]), "{out:?}");
        assert_eq!(sw.target(), Some("pc"));
    }

    /// A drag held here asks at the edge, once; dragging files, it goes
    /// along, and its release drops them there and ends the drag here
    #[test]
    fn files_dragged_from_here_go_along() {
        let mut sw = switch();
        let (_, out) = feed(&mut sw, button(MouseButton::Left, true));
        assert_eq!(out, [Emit::LocalPress { down: true }]);
        let (d, out) = cross_to_pc(&mut sw, 400);
        assert_eq!((d.verdict, d.cursor), (Verdict::Pass, None));
        let ask = Emit::DragAtEdge {
            device: "mac".into(),
            press: 1,
        };
        assert_eq!(out, [ask]);
        assert_eq!(cross_to_pc(&mut sw, 400).1, []);
        // An answer for another press changes nothing
        sw.allow_carry(7);
        assert_eq!(cross_to_pc(&mut sw, 400).1, []);

        sw.allow_carry(1);
        let (d, out) = cross_to_pc(&mut sw, 400);
        assert_eq!(d.cursor, Some(CursorAction::Park));
        let entry = Point::new(1, 400);
        assert_eq!(
            out,
            [
                Emit::Enter {
                    device: "pc".into(),
                    at: entry
                },
                Emit::Carry {
                    origin: "mac".into(),
                    to: "pc".into(),
                    at: entry,
                    press: 1
                }
            ]
        );
        feed(&mut sw, motion(0, 0, 20.0, 0.0));
        let (d, out) = feed(&mut sw, button(MouseButton::Left, false));
        assert_eq!(d.verdict, Verdict::Pass);
        assert_eq!(
            out,
            [
                Emit::Button {
                    device: "pc".into(),
                    button: MouseButton::Left,
                    down: false,
                    at: Point::new(21, 400)
                },
                Emit::LocalPress { down: false }
            ]
        );
        // The next press asks afresh
        feed(&mut sw, button(MouseButton::Left, true));
        let (_, out) = feed(&mut sw, motion(0, 0, -50.0, 0.0));
        let ask = Emit::DragAtEdge {
            device: "pc".into(),
            press: 2,
        };
        assert_eq!(out[0], ask);
    }

    /// Files dragged on another device come home with the pointer, and are
    /// dropped here when released
    #[test]
    fn files_dragged_there_come_home() {
        let mut sw = switch();
        cross_to_pc(&mut sw, 400);
        feed(&mut sw, button(MouseButton::Left, true));
        let (_, out) = feed(&mut sw, motion(0, 0, -5.0, 0.0));
        let ask = Emit::DragAtEdge {
            device: "pc".into(),
            press: 1,
        };
        assert_eq!(out[0], ask);
        assert_eq!(sw.target(), Some("pc"));
        sw.allow_carry(1);
        let (d, out) = feed(&mut sw, motion(0, 0, -5.0, 0.0));
        let entry = Point::new(1510, 400);
        assert_eq!(d.cursor, Some(CursorAction::Release(entry)));
        assert_eq!(
            out,
            [
                Emit::Leave {
                    device: "pc".into()
                },
                Emit::Carry {
                    origin: "pc".into(),
                    to: "mac".into(),
                    at: entry,
                    press: 1
                }
            ]
        );
        let (d, out) = feed(&mut sw, button(MouseButton::Left, false));
        assert_eq!(d.verdict, Verdict::Swallow);
        assert_eq!(out, [Emit::Drop { at: entry }]);
    }

    /// A carried drag goes no further, and goes back only to cancel; Esc
    /// cancels it too. A drag from here then carries on here
    #[test]
    fn carried_drags_are_cancelled() {
        let mut sw = switch();
        feed(&mut sw, button(MouseButton::Left, true));
        cross_to_pc(&mut sw, 400);
        sw.allow_carry(1);
        cross_to_pc(&mut sw, 400);
        // No further than the PC
        let (_, out) = feed(&mut sw, motion(0, 0, 5000.0, 0.0));
        assert!(matches!(&out[..], [Emit::Motion { .. }]), "{out:?}");
        assert_eq!(sw.target(), Some("pc"));
        // Back home: cancelled there, and the drag goes on here
        let (d, out) = feed(&mut sw, motion(0, 0, -5000.0, 0.0));
        assert!(matches!(d.cursor, Some(CursorAction::Release(_))));
        assert_eq!(
            out,
            [
                Emit::DragCancel {
                    device: "pc".into(),
                    at: Point::new(1919, 400)
                },
                Emit::Leave {
                    device: "pc".into()
                }
            ]
        );
        // Still known to drag files: over again, then Esc
        let (_, out) = cross_to_pc(&mut sw, 400);
        assert!(matches!(&out[..], [Emit::Enter { .. }, Emit::Carry { .. }]));
        let (d, out) = feed(&mut sw, key(usage::ESCAPE, true));
        assert_eq!(d.verdict, Verdict::Swallow);
        assert_eq!(
            out,
            [Emit::DragCancel {
                device: "pc".into(),
                at: Point::new(1, 400)
            }]
        );
        assert_eq!(feed(&mut sw, key(usage::ESCAPE, false)).1, []);
        // The release ends the drag held here
        let (d, out) = feed(&mut sw, button(MouseButton::Left, false));
        assert_eq!(d.verdict, Verdict::Pass);
        assert_eq!(out, [Emit::LocalPress { down: false }]);
    }

    /// A drop waiting here freezes the cursor until it is over; clicks and
    /// scrolling go nowhere meanwhile
    #[test]
    fn a_waiting_drop_holds_the_pointer_here() {
        let mut sw = switch();
        feed(&mut sw, motion(700, 300, 1.0, 0.0));
        sw.set_awaiting_drop(true);
        let (d, out) = feed(&mut sw, motion(1511, 300, 900.0, 0.0));
        assert_eq!(
            (d.verdict, d.cursor),
            (
                Verdict::Swallow,
                Some(CursorAction::Freeze(Point::new(700, 300)))
            )
        );
        assert_eq!(out, []);
        let (d, out) = cross_to_pc(&mut sw, 300);
        assert_eq!((d.verdict, d.cursor, out), (Verdict::Swallow, None, vec![]));
        let (d, out) = feed(&mut sw, button(MouseButton::Left, true));
        assert_eq!((d.verdict, out), (Verdict::Swallow, vec![]));
        assert_eq!(
            feed(&mut sw, button(MouseButton::Left, false)).0.verdict,
            Verdict::Swallow
        );
        let wheel = InputEvent::Wheel { dx: 0, dy: -120 };
        assert_eq!(feed(&mut sw, wheel).0.verdict, Verdict::Swallow);

        sw.set_awaiting_drop(false);
        let (d, _) = feed(&mut sw, motion(700, 300, 1.0, 0.0));
        assert_eq!(d.cursor, Some(CursorAction::Release(Point::new(700, 300))));
        assert_eq!(cross_to_pc(&mut sw, 300).0.cursor, Some(CursorAction::Park));
    }

    /// A drop waiting on the device controlled holds the pointer there,
    /// until it is over or the pointer jumps away
    #[test]
    fn a_waiting_drop_holds_the_pointer_there() {
        let mut sw = switch();
        cross_to_pc(&mut sw, 400);
        sw.set_awaiting_drop(true);
        let (d, out) = feed(&mut sw, motion(0, 0, -5000.0, 0.0));
        assert_eq!((d.verdict, d.cursor, out), (Verdict::Swallow, None, vec![]));
        assert_eq!(feed(&mut sw, button(MouseButton::Left, true)).1, []);
        // Esc reaches the device, which cancels the drop itself
        let (_, out) = feed(&mut sw, key(usage::ESCAPE, true));
        assert!(matches!(&out[..], [Emit::Key { device, .. }] if device == "pc"));
        sw.set_awaiting_drop(false);
        let (_, out) = feed(&mut sw, motion(0, 0, 10.0, 0.0));
        assert!(matches!(&out[..], [Emit::Motion { .. }]), "{out:?}");

        sw.set_awaiting_drop(true);
        sw.request(Request::Jump("mac".into()));
        let (d, _) = feed(&mut sw, motion(0, 0, 1.0, 0.0));
        assert!(matches!(d.cursor, Some(CursorAction::Release(_))));
        assert_eq!(
            feed(&mut sw, motion(700, 300, 1.0, 0.0)).0.verdict,
            Verdict::Pass
        );
    }

    /// Esc cancels a drop here waiting for its files, once
    #[test]
    fn esc_cancels_a_waiting_drop() {
        let mut sw = switch();
        feed(&mut sw, motion(700, 300, 1.0, 0.0));
        sw.set_awaiting_drop(true);
        let (d, out) = feed(&mut sw, key(usage::ESCAPE, true));
        assert_eq!(d.verdict, Verdict::Swallow);
        assert_eq!(
            out,
            [Emit::DragCancel {
                device: "mac".into(),
                at: Point::new(700, 300)
            }]
        );
        assert_eq!(
            feed(&mut sw, key(usage::ESCAPE, false)).0.verdict,
            Verdict::Swallow
        );
        assert_eq!(
            feed(&mut sw, key(usage::ESCAPE, true)).0.verdict,
            Verdict::Pass
        );
    }

    /// Files carried here from another device are cancelled here when the
    /// pointer leaves; the release then goes nowhere
    #[test]
    fn a_drag_carried_home_is_cancelled_on_leaving() {
        let mut sw = switch();
        cross_to_pc(&mut sw, 400);
        feed(&mut sw, button(MouseButton::Left, true));
        feed(&mut sw, motion(0, 0, -5.0, 0.0));
        sw.allow_carry(1);
        feed(&mut sw, motion(0, 0, -5.0, 0.0));
        assert!(!sw.is_remote());
        // Back to the PC, where the files come from
        let (_, out) = cross_to_pc(&mut sw, 400);
        assert_eq!(
            out,
            [
                Emit::DragCancel {
                    device: "mac".into(),
                    at: Point::new(1511, 400)
                },
                Emit::Enter {
                    device: "pc".into(),
                    at: Point::new(1, 400)
                }
            ]
        );
        let (d, out) = feed(&mut sw, button(MouseButton::Left, false));
        assert_eq!((d.verdict, out), (Verdict::Swallow, vec![]));
    }

    /// A key pressed on one device is released there, never on the next
    #[test]
    fn keys_follow_their_press() {
        let mut sw = switch();
        // Held locally, then autorepeating while remote: swallowed
        feed(&mut sw, key(0x04, true));
        cross_to_pc(&mut sw, 400);
        let (d, out) = feed(&mut sw, key(0x04, true));
        assert_eq!((d.verdict, out.len()), (Verdict::Swallow, 0));
        let (d, _) = feed(&mut sw, key(0x04, false));
        assert_eq!(d.verdict, Verdict::Pass);

        // Pressed on the PC, released after hopping to the TV: the PC let
        // go of it on Leave, and the TV never saw the press
        let (_, out) = feed(&mut sw, key(0x05, true));
        assert_eq!(
            out,
            [Emit::Key {
                device: "pc".into(),
                usage: 0x05,
                down: true
            }]
        );
        feed(&mut sw, motion(0, 0, 3000.0, 0.0));
        assert_eq!(sw.target(), Some("tv"));
        let (d, out) = feed(&mut sw, key(0x05, false));
        assert_eq!((d.verdict, out.len()), (Verdict::Swallow, 0));
    }

    /// Command and Control swap for configured devices, and a release is
    /// sent as the key its press was sent as
    #[test]
    fn cmd_ctrl_swap() {
        let mut sw = switch();
        let world = sw.world.clone();
        sw.set_world(world.clone(), HashSet::from(["pc".to_string()]));
        cross_to_pc(&mut sw, 400);
        let (_, out) = feed(&mut sw, key(usage::LEFT_CTRL, true));
        assert_eq!(
            out,
            [Emit::Key {
                device: "pc".into(),
                usage: usage::LEFT_META,
                down: true
            }]
        );
        // The setting changes mid-press: the release still matches
        sw.set_world(world, HashSet::new());
        let (_, out) = feed(&mut sw, key(usage::LEFT_CTRL, false));
        assert_eq!(
            out,
            [Emit::Key {
                device: "pc".into(),
                usage: usage::LEFT_META,
                down: false
            }]
        );
    }

    /// Ctrl + `alt` + `key`, pressed and released; what the press of
    /// `key` did
    fn chord(sw: &mut Switch, alt: u16, k: u16) -> (Decision, Vec<Emit>) {
        feed(sw, key(usage::LEFT_CTRL, true));
        feed(sw, key(alt, true));
        let pressed = feed(sw, key(k, true));
        feed(sw, key(k, false));
        feed(sw, key(alt, false));
        feed(sw, key(usage::LEFT_CTRL, false));
        pressed
    }

    /// The switch with its devices numbered in reading order
    fn numbered() -> Switch {
        let mut sw = switch();
        sw.set_numbering(vec!["mac".into(), "pc".into(), "tv".into()]);
        sw
    }

    /// Ctrl+Alt+Esc comes home to the middle of the display the cursor
    /// left from and pauses crossing; again, it resumes
    #[test]
    fn pause_toggles() {
        let mut sw = switch();
        cross_to_pc(&mut sw, 400);
        let (d, out) = chord(&mut sw, usage::LEFT_ALT, usage::ESCAPE);
        assert_eq!(d.verdict, Verdict::Swallow);
        assert_eq!(d.cursor, Some(CursorAction::Release(Point::new(756, 540))));
        // Mid-display, but not a jump
        assert!(!sw.homecoming().jumped);
        assert_eq!(
            out,
            [
                Emit::Leave {
                    device: "pc".into()
                },
                Emit::Paused(true)
            ]
        );
        let (d, out) = cross_to_pc(&mut sw, 400);
        assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));

        // Right Alt works for this one; locally it just toggles
        let (d, out) = chord(&mut sw, usage::RIGHT_ALT, usage::ESCAPE);
        assert_eq!(
            (d.verdict, out),
            (Verdict::Swallow, vec![Emit::Paused(false)])
        );
        assert!(cross_to_pc(&mut sw, 400).0.cursor.is_some());
    }

    /// Ctrl+Alt+n jumps to the n-th device, mid-display; a number without
    /// a device is an ordinary key
    #[test]
    fn number_jumps() {
        let mut sw = numbered();
        let (d, out) = chord(&mut sw, usage::LEFT_ALT, usage::DIGIT_1 + 2);
        assert_eq!(d.cursor, Some(CursorAction::Park));
        assert_eq!(
            out,
            [Emit::Enter {
                device: "tv".into(),
                at: Point::new(960, 540)
            }]
        );
        let (_, out) = chord(&mut sw, usage::LEFT_ALT, usage::DIGIT_1 + 1);
        assert_eq!(
            out,
            [
                Emit::Leave {
                    device: "tv".into()
                },
                Emit::Enter {
                    device: "pc".into(),
                    at: Point::new(960, 540)
                }
            ]
        );
        let (d, out) = chord(&mut sw, usage::LEFT_ALT, usage::DIGIT_1);
        assert_eq!(d.cursor, Some(CursorAction::Release(Point::new(756, 540))));
        assert_eq!(
            out,
            [Emit::Leave {
                device: "pc".into()
            }]
        );
        assert_eq!(
            sw.homecoming(),
            Homecoming {
                at: Point::new(756, 540),
                jumped: true
            }
        );

        let (d, out) = chord(&mut sw, usage::LEFT_ALT, usage::DIGIT_1 + 4);
        assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));
    }

    /// Requests from outside the keyboard wait for the next event, then do
    /// what their hotkeys do; a jump to a device that is not online is
    /// ignored
    #[test]
    fn requests_run_at_the_next_event() {
        let mut sw = numbered();
        sw.request(Request::Jump("tv".into()));
        assert_eq!(sw.target(), None);
        // The event that carries the request out is handled too; this one
        // moves nothing
        let (d, out) = feed(&mut sw, motion(700, 500, 0.0, 0.0));
        assert_eq!(d.cursor, Some(CursorAction::Park));
        assert_eq!(
            out,
            [Emit::Enter {
                device: "tv".into(),
                at: Point::new(960, 540)
            }]
        );

        sw.request(Request::Pause);
        sw.request(Request::Lock);
        let (d, out) = feed(&mut sw, motion(0, 0, 1.0, 0.0));
        assert_eq!(d.cursor, Some(CursorAction::Release(Point::new(756, 540))));
        assert_eq!(
            out,
            [
                Emit::Leave {
                    device: "tv".into()
                },
                Emit::Paused(true),
                Emit::Locked(true)
            ]
        );

        sw.request(Request::Jump("phone".into()));
        let (d, out) = feed(&mut sw, motion(700, 500, 1.0, 0.0));
        assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));
    }

    /// Ctrl+Alt+arrows walk to the neighbours; with none that way the
    /// arrow is an ordinary key
    #[test]
    fn arrow_jumps() {
        let mut sw = numbered();
        chord(&mut sw, usage::LEFT_ALT, usage::ARROW_RIGHT);
        assert_eq!(sw.target(), Some("pc"));
        chord(&mut sw, usage::LEFT_ALT, usage::ARROW_RIGHT);
        assert_eq!(sw.target(), Some("tv"));
        let (_, out) = chord(&mut sw, usage::LEFT_ALT, usage::ARROW_RIGHT);
        assert_eq!(
            out,
            [Emit::Key {
                device: "tv".into(),
                usage: usage::ARROW_RIGHT,
                down: true
            }]
        );
        chord(&mut sw, usage::LEFT_ALT, usage::ARROW_LEFT);
        let (d, _) = chord(&mut sw, usage::LEFT_ALT, usage::ARROW_LEFT);
        assert!(matches!(d.cursor, Some(CursorAction::Release(_))));
        assert!(!sw.is_remote());
    }

    /// Ctrl+Alt+L (or Scroll Lock) keeps the pointer on its device; hotkeys
    /// still move it
    #[test]
    fn lock_holds_the_pointer() {
        let mut sw = numbered();
        let (_, out) = chord(&mut sw, usage::LEFT_ALT, usage::KEY_L);
        assert_eq!(out, [Emit::Locked(true)]);
        assert_eq!(cross_to_pc(&mut sw, 400).0.verdict, Verdict::Pass);

        chord(&mut sw, usage::LEFT_ALT, usage::ARROW_RIGHT);
        let (d, out) = feed(&mut sw, motion(0, 0, 5000.0, 0.0));
        assert_eq!(d.cursor, None);
        assert_eq!(
            out,
            [Emit::Motion {
                device: "pc".into(),
                at: Point::new(1919, 540)
            }]
        );

        let (d, out) = feed(&mut sw, key(usage::SCROLL_LOCK, true));
        assert_eq!(
            (d.verdict, out),
            (Verdict::Swallow, vec![Emit::Locked(false)])
        );
        feed(&mut sw, key(usage::SCROLL_LOCK, false));
        feed(&mut sw, motion(0, 0, 5.0, 0.0));
        assert_eq!(sw.target(), Some("tv"));
    }

    /// AltGr (Control + right Alt on Windows) with a digit types a
    /// character on many layouts: not a hotkey
    #[test]
    fn altgr_is_not_a_hotkey() {
        let mut sw = numbered();
        let (d, out) = chord(&mut sw, usage::RIGHT_ALT, usage::DIGIT_1 + 1);
        assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));
        assert!(!sw.is_remote());
    }

    /// Jumping resumes a paused switch
    #[test]
    fn jumping_resumes() {
        let mut sw = numbered();
        chord(&mut sw, usage::LEFT_ALT, usage::ESCAPE);
        let (_, out) = chord(&mut sw, usage::LEFT_ALT, usage::ARROW_RIGHT);
        assert_eq!(out[0], Emit::Paused(false));
        assert!(matches!(&out[1], Emit::Enter { device, .. } if device == "pc"));
    }

    /// Requested releases apply to the device named (or any), at the next
    /// event; a device leaving the layout is released too
    #[test]
    fn requested_release() {
        let mut sw = switch();
        cross_to_pc(&mut sw, 400);
        sw.request_release(Some("tv"));
        let (d, _) = feed(&mut sw, motion(0, 0, 1.0, 0.0));
        assert_eq!(d.cursor, None);
        sw.request_release(Some("pc"));
        let (d, out) = feed(&mut sw, motion(0, 0, 1.0, 0.0));
        assert_eq!(d.cursor, Some(CursorAction::Release(Point::new(756, 540))));
        assert_eq!(
            out,
            [Emit::Leave {
                device: "pc".into()
            }]
        );
        // The same motion does not cross back at once
        assert!(!sw.is_remote());

        cross_to_pc(&mut sw, 400);
        let world = World::new([device("mac", 0, 1512, 1080, 1.0)]);
        sw.set_world(world, HashSet::new());
        let (d, _) = feed(&mut sw, key(0x04, true));
        assert!(matches!(d.cursor, Some(CursorAction::Release(_))));
        assert_eq!(d.verdict, Verdict::Pass);
    }

    /// Local input while controlled tells the controller to let go, once;
    /// small twitches do not count
    #[test]
    fn takeover() {
        let mut sw = switch();
        sw.set_controlled(true);
        let (d, out) = feed(&mut sw, motion(10, 10, 1.0, 1.0));
        assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));
        let (_, out) = feed(&mut sw, motion(10, 10, 2.0, 1.0));
        assert_eq!(out, [Emit::Takeover]);
        let (_, out) = feed(&mut sw, key(0x04, true));
        assert!(out.is_empty());
        sw.set_controlled(true);
        let (_, out) = feed(&mut sw, key(0x04, false));
        assert_eq!(out, [Emit::Takeover]);
    }

    /// Without the local device in the layout nothing crosses
    #[test]
    fn unplaced_device_stays_local() {
        let mut sw = Switch::new("mac");
        let (d, out) = cross_to_pc(&mut sw, 400);
        assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));
        let world = World::new([device("pc", 1512, 1920, 1080, 1.0)]);
        sw.set_world(world, HashSet::new());
        let (d, _) = cross_to_pc(&mut sw, 400);
        assert_eq!(d.verdict, Verdict::Pass);
    }

    /// Scrolling goes where the cursor is
    #[test]
    fn wheel() {
        let mut sw = switch();
        let scroll = InputEvent::Wheel { dx: 0, dy: -120 };
        assert_eq!(feed(&mut sw, scroll).0.verdict, Verdict::Pass);
        cross_to_pc(&mut sw, 400);
        let (_, out) = feed(&mut sw, scroll);
        assert_eq!(
            out,
            [Emit::Wheel {
                device: "pc".into(),
                dx: 0,
                dy: -120
            }]
        );
    }

    /// Hotkeys follow the settings and need exactly their modifiers; Scroll
    /// Lock locks whatever they are
    #[test]
    fn custom_hotkeys() {
        const KEY_P: u16 = 0x13;
        let mut sw = switch();
        // Shift on top is not the pause hotkey
        feed(&mut sw, key(usage::LEFT_SHIFT, true));
        let (d, out) = chord(&mut sw, usage::LEFT_ALT, usage::ESCAPE);
        assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));
        feed(&mut sw, key(usage::LEFT_SHIFT, false));

        let meta_shift = Mods {
            meta: true,
            shift: true,
            ..Mods::default()
        };
        sw.set_hotkeys(Hotkeys {
            pause: Chord::new(meta_shift, KEY_P),
            ..Hotkeys::default()
        });
        let (d, out) = chord(&mut sw, usage::LEFT_ALT, usage::ESCAPE);
        assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));
        feed(&mut sw, key(usage::LEFT_META, true));
        feed(&mut sw, key(usage::RIGHT_SHIFT, true));
        let (d, out) = feed(&mut sw, key(KEY_P, true));
        assert_eq!(
            (d.verdict, out),
            (Verdict::Swallow, vec![Emit::Paused(true)])
        );
        // Its release is dropped too
        assert_eq!(feed(&mut sw, key(KEY_P, false)).0.verdict, Verdict::Swallow);
        feed(&mut sw, key(usage::RIGHT_SHIFT, false));
        feed(&mut sw, key(usage::LEFT_META, false));

        let (_, out) = feed(&mut sw, key(usage::SCROLL_LOCK, true));
        assert_eq!(out, [Emit::Locked(true)]);
    }

    /// Recording reports the next combination instead of letting it
    /// through, a hotkey's included; Esc alone gives up; keys from the
    /// device controlling this one record too
    #[test]
    fn recording() {
        const SPACE: u16 = 0x2C;
        let mut sw = switch();
        sw.record(true);
        assert!(sw.is_recording());
        let (d, out) = chord(&mut sw, usage::LEFT_ALT, usage::KEY_L);
        assert_eq!(d.verdict, Verdict::Swallow);
        assert_eq!(
            out,
            [Emit::Recorded(Some(Chord::new(
                Mods::CTRL_ALT,
                usage::KEY_L
            )))]
        );
        assert!(!sw.is_recording());

        sw.record(true);
        let (_, out) = feed(&mut sw, key(usage::ESCAPE, true));
        assert_eq!(out, [Emit::Recorded(None)]);

        sw.record(true);
        assert_eq!(sw.record_key(usage::LEFT_META, true), None);
        let meta = Mods {
            meta: true,
            ..Mods::default()
        };
        assert_eq!(
            sw.record_key(SPACE, true),
            Some(Some(Chord::new(meta, SPACE)))
        );
    }

    /// A combination kept local, pressed while controlling the PC: the PC
    /// gets its modifier lifted, this machine the whole combination
    #[test]
    fn keeps_combinations_local() {
        const SPACE: u16 = 0x2C;
        let mut sw = switch();
        let meta = Mods {
            meta: true,
            ..Mods::default()
        };
        sw.set_keep_local(vec![Chord::new(meta, SPACE)]);
        cross_to_pc(&mut sw, 400);
        let pc_key = |usage, down| Emit::Key {
            device: "pc".into(),
            usage,
            down,
        };
        let (_, out) = feed(&mut sw, key(usage::LEFT_META, true));
        assert_eq!(out, [pc_key(usage::LEFT_META, true)]);
        let (d, out) = feed(&mut sw, key(SPACE, true));
        assert_eq!(d.verdict, Verdict::Swallow);
        assert_eq!(
            out,
            [
                pc_key(usage::LEFT_META, false),
                Emit::Local {
                    usage: usage::LEFT_META,
                    down: true
                },
                Emit::Local {
                    usage: SPACE,
                    down: true
                },
            ]
        );
        let (_, out) = feed(&mut sw, key(SPACE, false));
        assert_eq!(
            out,
            [Emit::Local {
                usage: SPACE,
                down: false
            }]
        );
        let (_, out) = feed(&mut sw, key(usage::LEFT_META, false));
        assert_eq!(
            out,
            [Emit::Local {
                usage: usage::LEFT_META,
                down: false
            }]
        );
        // Anything else still goes to the PC
        let (_, out) = feed(&mut sw, key(0x04, true));
        assert_eq!(out, [pc_key(0x04, true)]);
    }

    /// A closed edge is a wall; the corner guard follows the settings, an
    /// edge's own first
    #[test]
    fn edge_settings() {
        let crosses = |sw: &mut Switch, y| !cross_to_pc(sw, y).1.is_empty();
        let mut sw = switch();
        // 8 px by default: 4.5 from the top corner does not cross
        assert!(!crosses(&mut sw, 4));
        sw.set_switching(Switching {
            corner_px: 0,
            ..Switching::default()
        });
        assert!(crosses(&mut sw, 4));

        let mut sw = switch();
        let closed = EdgeSettings {
            crossable: false,
            ..EdgeSettings::default()
        };
        sw.set_edges(HashMap::from([(edge_key("pc", "mac"), closed)]));
        assert!(!crosses(&mut sw, 400));
        let unguarded = EdgeSettings {
            corner_px: Some(0),
            ..EdgeSettings::default()
        };
        sw.set_edges(HashMap::from([(edge_key("mac", "pc"), unguarded)]));
        assert!(crosses(&mut sw, 4));
    }

    /// In modifier mode the pointer crosses only with the modifier held,
    /// which stays on this machine
    #[test]
    fn modifier_mode() {
        let mut sw = switch();
        sw.set_switching(Switching {
            mode: SwitchMode::Modifier,
            ..Switching::default()
        });
        assert!(cross_to_pc(&mut sw, 400).1.is_empty());
        feed(&mut sw, key(usage::RIGHT_SHIFT, true));
        let (_, out) = cross_to_pc(&mut sw, 400);
        assert!(matches!(&out[..], [Emit::Enter { device, .. }] if device == "pc"));
        let (d, out) = feed(&mut sw, key(usage::RIGHT_SHIFT, false));
        assert_eq!((d.verdict, out.len()), (Verdict::Pass, 0));
    }

    /// In dwell mode the pointer crosses once it has stayed against the
    /// edge long enough, sliding along it and motions that do not push
    /// outwards included; leaving the edge starts over
    #[test]
    fn dwell_mode() {
        let dwelling = || {
            let mut sw = switch();
            let start = Instant::now();
            let elapsed = Arc::new(Mutex::new(Duration::ZERO));
            let clock = Arc::clone(&elapsed);
            sw.set_clock(Clock::new(move || start + *clock.lock().unwrap()));
            sw.set_switching(Switching {
                mode: SwitchMode::Dwell,
                ..Switching::default()
            });
            (sw, elapsed)
        };
        let wait =
            |elapsed: &Mutex<Duration>, ms| *elapsed.lock().unwrap() += Duration::from_millis(ms);

        let (mut sw, elapsed) = dwelling();
        assert!(cross_to_pc(&mut sw, 400).1.is_empty());
        wait(&elapsed, 150);
        // Down along the edge, then a motion with nothing outwards
        feed(&mut sw, motion(1511, 420, 0.0, 20.0));
        feed(&mut sw, motion(1511, 420, 0.0, 0.0));
        assert!(cross_to_pc(&mut sw, 420).1.is_empty());
        wait(&elapsed, 150);
        assert!(!cross_to_pc(&mut sw, 420).1.is_empty());

        let (mut sw, elapsed) = dwelling();
        assert!(cross_to_pc(&mut sw, 400).1.is_empty());
        wait(&elapsed, 200);
        // Off the edge: the count starts over
        feed(&mut sw, motion(1500, 400, -5.0, 0.0));
        wait(&elapsed, 200);
        assert!(cross_to_pc(&mut sw, 400).1.is_empty());
        wait(&elapsed, 299);
        assert!(cross_to_pc(&mut sw, 400).1.is_empty());
        wait(&elapsed, 1);
        assert!(!cross_to_pc(&mut sw, 400).1.is_empty());
    }
}
