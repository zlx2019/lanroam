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
//! The switch runs inside the capture callback, synchronously: it must
//! decide before the OS delivers the event, and never blocks.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

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
    /// Keys currently pressed (by physical usage): where each press went,
    /// and the usage it was sent as
    keys: HashMap<u16, (Owner, u16)>,
    /// Buttons currently pressed, and where each press went
    buttons: [Option<Owner>; MouseButton::COUNT],
    /// Hand control back at the next event (set from outside the capture)
    release_requested: bool,
    /// Devices that get Command and Control swapped
    swapped: HashSet<String>,
    /// Another device controls this machine right now
    controlled: bool,
    /// Local pointer travel since control was taken
    local_travel: f64,
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
            keys: HashMap::new(),
            buttons: std::array::from_fn(|_| None),
            release_requested: false,
            swapped: HashSet::new(),
            controlled: false,
            local_travel: 0.0,
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

    /// Whether another device controls this machine; while it does, local
    /// input emits [`Emit::Takeover`] once
    pub fn set_controlled(&mut self, controlled: bool) {
        self.controlled = controlled;
        self.local_travel = 0.0;
    }

    /// Decide what happens to one captured event; messages for other
    /// devices are appended to `out`
    pub fn handle(&mut self, event: InputEvent, out: &mut Vec<Emit>) -> Decision {
        let mut cursor = None;
        if std::mem::take(&mut self.release_requested) && self.remote.is_some() {
            tracing::debug!("control comes back on request");
            cursor = Some(self.come_home(Landing::Centre, out));
        }
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
            // No crossing right after a forced release, or a motion that
            // arrives with it would take control straight back
            if cursor.is_none()
                && let Some((device, entry, departed)) = self.crossing(at, dx, dy)
            {
                self.departed = departed;
                self.enter(device, entry, out);
                *cursor = Some(CursorAction::Park);
                return Verdict::Swallow;
            }
            return Verdict::Pass;
        };
        let Some(target) = self.world.device(&remote.device) else {
            // Gone from the layout; the release is already requested
            return Verdict::Swallow;
        };
        // Same physical travel on every device: source units to logical
        // pixels to the target's units
        let local_scale = self.world.device(&self.local).map_or(1.0, |d| d.scale);
        let k = target.scale / local_scale;
        let (x, y) = match target.desktop.step((remote.x, remote.y), dx * k, dy * k) {
            Step::Inside(x, y) => (x, y),
            // A held button keeps the pointer on the device: dragging across
            // devices is not supported yet
            Step::Blocked { x, y, .. } if self.holds_remote_button() => (x, y),
            Step::Blocked { x, y, stop, edges } => {
                let next = edges.into_iter().find_map(|edge| {
                    self.world
                        .cross(&remote.device, stop, edge, LANDING_INSET)
                        .map(|(device, entry)| (device.key.clone(), entry))
                });
                match next {
                    Some((device, entry)) if device == self.local => {
                        *cursor = Some(self.come_home(Landing::At(entry), out));
                        return Verdict::Swallow;
                    }
                    Some((device, entry)) => {
                        out.push(Emit::Leave {
                            device: remote.device,
                        });
                        self.enter(device, entry, out);
                        return Verdict::Swallow;
                    }
                    // A wall: slide along it
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
    /// machine: (device, entry point, departure point)
    fn crossing(&self, at: Point, dx: f64, dy: f64) -> Option<(String, Point, Point)> {
        if self.buttons.contains(&Some(Owner::Local)) {
            return None;
        }
        let local = self.world.device(&self.local)?;
        let at = local.desktop.clamp(at);
        Edge::ALL
            .into_iter()
            .filter(|edge| edge.pushed_by(dx, dy) && local.desktop.on_edge(at, *edge))
            .find_map(|edge| self.world.cross(&self.local, at, edge, LANDING_INSET))
            .map(|(device, entry)| (device.key.clone(), entry, at))
    }

    /// Take control of `device` with its cursor at `at`
    fn enter(&mut self, device: String, at: Point, out: &mut Vec<Emit>) {
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
        if let Some(remote) = self.remote.take() {
            out.push(Emit::Leave {
                device: remote.device,
            });
        }
        let local = self.world.device(&self.local).map(|d| &d.desktop);
        let back = match landing {
            Landing::At(entry) => entry,
            Landing::Centre => local
                .and_then(|desktop| desktop.display_at(self.departed))
                .map_or(self.departed, Rect::centre),
        };
        CursorAction::Release(back)
    }

    /// A mouse button: the release goes where the press went
    fn button(&mut self, button: MouseButton, down: bool, out: &mut Vec<Emit>) -> Verdict {
        let side = self.side();
        let slot = &mut self.buttons[button.index()];
        let owner = if down {
            *slot = Some(side.clone());
            Some(side)
        } else {
            slot.take()
        };
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

    /// Scrolling follows the current side
    fn wheel(&mut self, dx: i32, dy: i32, out: &mut Vec<Emit>) -> Verdict {
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
        if !down {
            let (owner, sent) = match self.keys.remove(&key) {
                Some((owner, sent)) => (Some(owner), sent),
                None => (None, key),
            };
            return self.route(owner, out, |device| Emit::Key {
                device,
                usage: sent,
                down,
            });
        }
        let (owner, sent) = match self.keys.get(&key) {
            Some(held) => held.clone(),
            None if key == usage::ESCAPE && self.hotkey_modifiers_held() => {
                return self.hotkey(out, cursor);
            }
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

    /// Ctrl+Alt+Esc (Ctrl+Option+Esc on macOS): take control back at once
    ///
    /// Handled here, before anything else, so it works whatever state the
    /// other device or the network is in.
    fn hotkey(&mut self, out: &mut Vec<Emit>, cursor: &mut Option<CursorAction>) -> Verdict {
        if self.remote.is_none() {
            // Nothing to take back: an ordinary key press
            self.keys
                .insert(usage::ESCAPE, (Owner::Local, usage::ESCAPE));
            return Verdict::Pass;
        }
        tracing::debug!("escape hotkey: taking control back");
        self.keys
            .insert(usage::ESCAPE, (Owner::Dropped, usage::ESCAPE));
        *cursor = Some(self.come_home(Landing::Centre, out));
        Verdict::Swallow
    }

    /// Whether a Control and an Alt key are both held (either side)
    fn hotkey_modifiers_held(&self) -> bool {
        let held = |a: u16, b: u16| self.keys.contains_key(&a) || self.keys.contains_key(&b);
        held(usage::LEFT_CTRL, usage::RIGHT_CTRL) && held(usage::LEFT_ALT, usage::RIGHT_ALT)
    }

    /// Where new presses go right now
    fn side(&self) -> Owner {
        match &self.remote {
            Some(remote) => Owner::Remote(remote.device.clone()),
            None => Owner::Local,
        }
    }

    /// Whether a mouse button pressed on some other device is still held
    fn holds_remote_button(&self) -> bool {
        self.buttons
            .iter()
            .any(|b| matches!(b, Some(Owner::Remote(_))))
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
            (Some(Owner::Remote(_) | Owner::Dropped), _) => Verdict::Swallow,
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

    /// Held buttons keep the pointer on its side, both ways
    #[test]
    fn held_buttons_block_crossing() {
        let mut sw = switch();
        let press = InputEvent::Button {
            button: MouseButton::Left,
            down: true,
        };
        let release = InputEvent::Button {
            button: MouseButton::Left,
            down: false,
        };
        feed(&mut sw, press);
        assert_eq!(cross_to_pc(&mut sw, 400).1, []);
        feed(&mut sw, release);
        cross_to_pc(&mut sw, 400);
        let (_, out) = feed(&mut sw, press);
        assert!(matches!(&out[..], [Emit::Button { device, down: true, .. }] if device == "pc"));
        let (d, _) = feed(&mut sw, motion(0, 0, -50.0, 0.0));
        assert_eq!(d.cursor, None);
        assert_eq!(sw.target(), Some("pc"));
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

    /// Ctrl+Alt+Esc brings control back to the middle of the display the
    /// cursor left from; locally it is an ordinary key
    #[test]
    fn escape_hotkey() {
        let mut sw = switch();
        feed(&mut sw, key(usage::LEFT_CTRL, true));
        feed(&mut sw, key(usage::LEFT_ALT, true));
        let (d, _) = feed(&mut sw, key(usage::ESCAPE, true));
        assert_eq!(d.verdict, Verdict::Pass);
        feed(&mut sw, key(usage::ESCAPE, false));
        cross_to_pc(&mut sw, 400);
        let (d, out) = feed(&mut sw, key(usage::ESCAPE, true));
        assert_eq!(d.verdict, Verdict::Swallow);
        assert_eq!(d.cursor, Some(CursorAction::Release(Point::new(756, 540))));
        assert_eq!(
            out,
            [Emit::Leave {
                device: "pc".into()
            }]
        );
        let (d, _) = feed(&mut sw, key(usage::ESCAPE, false));
        assert_eq!(d.verdict, Verdict::Swallow);
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
}
