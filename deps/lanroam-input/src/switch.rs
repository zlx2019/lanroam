//! Source side: event by event, decides whether local input stays on this
//! machine or goes to the target, and moves the virtual cursor across the
//! target's desktop.
//!
//! Every key and button remembers where its press went, and its release
//! follows it there. That single rule keeps keys from getting stuck when
//! control changes hands in the middle of a press.
//!
//! The switch runs inside the capture callback, synchronously: it must
//! decide before the OS delivers the event, and never blocks.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::event::{InputEvent, MouseButton};
use crate::geometry::{Desktop, Edge, Point, Step};
use crate::keymap::usage;

/// How far inside the edge the pointer lands after a crossing, so a
/// one-pixel jitter does not bounce it straight back
const LANDING_INSET: i32 = 1;

/// What the capture backend does with the event it just reported
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Deliver it locally as usual
    Pass,
    /// Drop it; it was forwarded to the target, or belongs to nobody
    Swallow,
}

/// Change of the local cursor the capture backend must apply
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorAction {
    /// Control moved to the target: freeze the local cursor where it is
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

/// Message for the target, produced by [`Switch::handle`]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Emit {
    /// Control moved to the target; its cursor goes here
    Enter(Point),
    /// Control came back; the target releases whatever is still held
    Leave,
    /// The virtual cursor moved here (target coordinates)
    Motion(Point),
    /// A key press, autorepeat or release
    Key {
        /// USB HID usage
        usage: u16,
        /// Pressed (true) or released
        down: bool,
    },
    /// A mouse button, with the cursor position it happened at
    Button {
        /// Which button
        button: MouseButton,
        /// Pressed (true) or released
        down: bool,
        /// Virtual cursor position (target coordinates)
        at: Point,
    },
    /// Scrolling (see [`InputEvent::Wheel`])
    Wheel {
        /// Horizontal amount
        dx: i32,
        /// Vertical amount
        dy: i32,
    },
}

/// Where a key or button press went
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// Delivered locally
    Local,
    /// Forwarded to the target
    Remote,
    /// Consumed by Lanroam itself (a hotkey)
    Dropped,
}

/// The source-side state machine
#[derive(Debug)]
pub struct Switch {
    /// This machine's displays
    local: Desktop,
    /// The target's displays (empty until it reports them)
    target: Desktop,
    /// Side of the local desktop the target sits on
    edge: Edge,
    /// Virtual cursor on the target while controlling it
    remote: Option<(f64, f64)>,
    /// Keys currently pressed, and where each press went
    keys: HashMap<u16, Owner>,
    /// Buttons currently pressed, and where each press went
    buttons: [Option<Owner>; MouseButton::COUNT],
    /// Hand control back at the next event (set from outside the capture)
    release_requested: bool,
}

impl Switch {
    /// A switch with the target on `edge` of the local desktop
    pub fn new(local: Desktop, target: Desktop, edge: Edge) -> Self {
        Self {
            local,
            target,
            edge,
            remote: None,
            keys: HashMap::new(),
            buttons: [None; MouseButton::COUNT],
            release_requested: false,
        }
    }

    /// Whether input currently goes to the target
    pub fn is_remote(&self) -> bool {
        self.remote.is_some()
    }

    /// Update the target's displays; control comes back if it has none left
    pub fn set_target(&mut self, target: Desktop) {
        if let Some((x, y)) = self.remote {
            if target.is_empty() {
                self.release_requested = true;
            } else {
                let p = target.clamp(Point::floor(x, y));
                self.remote = Some((f64::from(p.x), f64::from(p.y)));
            }
        }
        self.target = target;
    }

    /// Hand control back to this machine at the next event (the target
    /// stopped answering, the link dropped, ...)
    ///
    /// Deferred to the next event because only the capture callback may
    /// move the local cursor; the user touching the mouse or keyboard is
    /// exactly when it matters.
    pub fn request_release(&mut self) {
        if self.remote.is_some() {
            self.release_requested = true;
        }
    }

    /// Decide what happens to one captured event; messages for the target
    /// are appended to `out`
    pub fn handle(&mut self, event: InputEvent, out: &mut Vec<Emit>) -> Decision {
        let mut cursor = None;
        if std::mem::take(&mut self.release_requested)
            && let Some(at) = self.remote
        {
            cursor = Some(self.leave(at, out));
        }
        let verdict = match event {
            InputEvent::Motion { at, dx, dy } => self.motion(at, dx, dy, out, &mut cursor),
            InputEvent::Button { button, down } => self.button(button, down, out),
            InputEvent::Wheel { dx, dy } => self.wheel(dx, dy, out),
            InputEvent::Key { usage, down } => self.key(usage, down, out, &mut cursor),
        };
        Decision { verdict, cursor }
    }

    /// Pointer motion: cross into the target, move the virtual cursor, or
    /// come back
    fn motion(
        &mut self,
        at: Point,
        dx: f64,
        dy: f64,
        out: &mut Vec<Emit>,
        cursor: &mut Option<CursorAction>,
    ) -> Verdict {
        let Some(from) = self.remote else {
            // No crossing right after a forced release, or a motion that
            // arrives with it would take control straight back
            if cursor.is_none()
                && let Some(entry) = self.crossing(at, dx, dy)
            {
                self.remote = Some((f64::from(entry.x), f64::from(entry.y)));
                out.push(Emit::Enter(entry));
                *cursor = Some(CursorAction::Park);
                return Verdict::Swallow;
            }
            return Verdict::Pass;
        };
        match self.target.step(from, dx, dy, self.edge.opposite()) {
            Step::Inside(x, y) => {
                self.remote = Some((x, y));
                let now = Point::floor(x, y);
                if now != Point::floor(from.0, from.1) {
                    out.push(Emit::Motion(now));
                }
            }
            // A held button keeps the pointer on the target: dragging across
            // devices is not supported yet
            Step::Exit(stop) if self.holds_button(Owner::Remote) => {
                self.remote = Some((f64::from(stop.x), f64::from(stop.y)));
            }
            Step::Exit(_) => *cursor = Some(self.leave(from, out)),
        }
        Verdict::Swallow
    }

    /// Where the pointer enters the target, if this motion pushes it out
    /// through the shared edge
    fn crossing(&self, at: Point, dx: f64, dy: f64) -> Option<Point> {
        if self.target.is_empty() || !self.edge.pushed_by(dx, dy) || self.holds_button(Owner::Local)
        {
            return None;
        }
        let at = self.local.clamp(at);
        if !self.local.on_edge(at, self.edge) {
            return None;
        }
        self.local
            .map_across(at, self.edge, &self.target, LANDING_INSET)
    }

    /// Give control back: tell the target, and work out where the local
    /// cursor reappears (facing the virtual cursor's last position)
    fn leave(&mut self, from: (f64, f64), out: &mut Vec<Emit>) -> CursorAction {
        self.remote = None;
        out.push(Emit::Leave);
        let last = Point::floor(from.0, from.1);
        let back = self
            .target
            .map_across(last, self.edge.opposite(), &self.local, LANDING_INSET)
            .unwrap_or_else(|| self.local.clamp(last));
        CursorAction::Release(back)
    }

    /// A mouse button: the release goes where the press went
    fn button(&mut self, button: MouseButton, down: bool, out: &mut Vec<Emit>) -> Verdict {
        let side = self.side();
        let slot = &mut self.buttons[button.index()];
        let owner = if down {
            *slot = Some(side);
            *slot
        } else {
            slot.take()
        };
        let at = self
            .remote
            .map_or_else(Point::default, |(x, y)| Point::floor(x, y));
        self.route(owner, Emit::Button { button, down, at }, out)
    }

    /// Scrolling follows the current side
    fn wheel(&mut self, dx: i32, dy: i32, out: &mut Vec<Emit>) -> Verdict {
        if self.remote.is_none() {
            return Verdict::Pass;
        }
        out.push(Emit::Wheel { dx, dy });
        Verdict::Swallow
    }

    /// A key: presses go to the current side, autorepeats and releases
    /// follow the press
    fn key(
        &mut self,
        usage: u16,
        down: bool,
        out: &mut Vec<Emit>,
        cursor: &mut Option<CursorAction>,
    ) -> Verdict {
        if !down {
            let owner = self.keys.remove(&usage);
            return self.route(owner, Emit::Key { usage, down }, out);
        }
        let owner = match self.keys.get(&usage) {
            Some(&held) => held,
            None if usage == usage::ESCAPE && self.hotkey_modifiers_held() => {
                return self.hotkey(out, cursor);
            }
            None => {
                let side = self.side();
                self.keys.insert(usage, side);
                side
            }
        };
        // Autorepeat of a key held down since before the crossing: the local
        // screen is not being looked at, so it must not type there
        if owner == Owner::Local && self.remote.is_some() {
            return Verdict::Swallow;
        }
        self.route(Some(owner), Emit::Key { usage, down }, out)
    }

    /// Ctrl+Alt+Esc (Ctrl+Option+Esc on macOS): take control back at once
    ///
    /// Handled here, before anything else, so it works whatever state the
    /// target or the network is in.
    fn hotkey(&mut self, out: &mut Vec<Emit>, cursor: &mut Option<CursorAction>) -> Verdict {
        let Some(at) = self.remote else {
            // Nothing to take back: an ordinary key press
            self.keys.insert(usage::ESCAPE, Owner::Local);
            return Verdict::Pass;
        };
        self.keys.insert(usage::ESCAPE, Owner::Dropped);
        *cursor = Some(self.leave(at, out));
        Verdict::Swallow
    }

    /// Whether a Control and an Alt key are both held (either side)
    fn hotkey_modifiers_held(&self) -> bool {
        let held = |a: u16, b: u16| self.keys.contains_key(&a) || self.keys.contains_key(&b);
        held(usage::LEFT_CTRL, usage::RIGHT_CTRL) && held(usage::LEFT_ALT, usage::RIGHT_ALT)
    }

    /// Where new presses go right now
    fn side(&self) -> Owner {
        if self.remote.is_some() {
            Owner::Remote
        } else {
            Owner::Local
        }
    }

    /// Whether any mouse button pressed on `owner`'s side is still held
    fn holds_button(&self, owner: Owner) -> bool {
        self.buttons.contains(&Some(owner))
    }

    /// Deliver a press or release according to where its press went;
    /// remote ones reach the target only while it is being controlled
    /// (after a hand-back it has released everything already)
    fn route(&self, owner: Option<Owner>, emit: Emit, out: &mut Vec<Emit>) -> Verdict {
        match owner {
            Some(Owner::Local) => Verdict::Pass,
            Some(Owner::Remote) if self.remote.is_some() => {
                out.push(emit);
                Verdict::Swallow
            }
            Some(Owner::Remote | Owner::Dropped) => Verdict::Swallow,
            // Pressed before the capture started
            None if self.remote.is_some() => Verdict::Swallow,
            None => Verdict::Pass,
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
    use crate::geometry::Rect;

    /// A 1512x982 Mac with a 1920x1080 PC on its right
    fn switch() -> Switch {
        Switch::new(
            Desktop::new([Rect::new(0, 0, 1512, 982)]),
            Desktop::new([Rect::new(0, 0, 1920, 1080)]),
            Edge::Right,
        )
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

    /// Push through the right edge at mid-height
    fn cross(sw: &mut Switch) {
        let (decision, out) = feed(sw, motion(1511, 491, 3.0, 0.0));
        assert_eq!(decision.cursor, Some(CursorAction::Park));
        assert_eq!(out, [Emit::Enter(Point::new(1, 540))]);
    }

    /// Ordinary motion stays local; pushing through the shared edge crosses
    #[test]
    fn crosses_at_the_shared_edge_only() {
        let mut sw = switch();
        let (decision, out) = feed(&mut sw, motion(700, 400, 5.0, 0.0));
        assert_eq!(decision.verdict, Verdict::Pass);
        assert!(out.is_empty());
        // At the edge but moving away from it
        let (decision, _) = feed(&mut sw, motion(1511, 400, -2.0, 0.0));
        assert_eq!(decision.verdict, Verdict::Pass);
        // The left edge is not shared
        let (decision, _) = feed(&mut sw, motion(0, 400, -5.0, 0.0));
        assert_eq!(decision.verdict, Verdict::Pass);
        assert!(!sw.is_remote());

        cross(&mut sw);
        assert!(sw.is_remote());
    }

    /// The virtual cursor moves on the target and comes back through the
    /// facing edge, the local cursor reappearing opposite
    #[test]
    fn moves_remotely_and_comes_back() {
        let mut sw = switch();
        cross(&mut sw);
        let (decision, out) = feed(&mut sw, motion(1511, 491, 10.0, 20.0));
        assert_eq!(decision.verdict, Verdict::Swallow);
        assert_eq!(out, [Emit::Motion(Point::new(11, 560))]);
        // Sub-pixel motion accumulates without emitting
        let (_, out) = feed(&mut sw, motion(1511, 491, 0.4, 0.0));
        assert!(out.is_empty());
        let (decision, out) = feed(&mut sw, motion(1511, 491, -50.0, 0.0));
        assert_eq!(out, [Emit::Leave]);
        // 560 of 1080 maps to 509 of 982, one pixel inside the right edge
        assert_eq!(
            decision.cursor,
            Some(CursorAction::Release(Point::new(1510, 509)))
        );
        assert!(!sw.is_remote());
    }

    /// A held button blocks crossing in both directions
    #[test]
    fn held_buttons_block_crossing() {
        let mut sw = switch();
        let left = |down| InputEvent::Button {
            button: MouseButton::Left,
            down,
        };
        feed(&mut sw, left(true));
        let (decision, _) = feed(&mut sw, motion(1511, 491, 3.0, 0.0));
        assert_eq!(decision.verdict, Verdict::Pass);
        assert!(!sw.is_remote());
        let (decision, _) = feed(&mut sw, left(false));
        assert_eq!(decision.verdict, Verdict::Pass);

        cross(&mut sw);
        let (_, out) = feed(&mut sw, left(true));
        assert!(matches!(out[..], [Emit::Button { down: true, .. }]));
        let (decision, out) = feed(&mut sw, motion(1511, 491, -50.0, 0.0));
        assert!(decision.cursor.is_none() && out.is_empty() && sw.is_remote());
        let (_, out) = feed(&mut sw, left(false));
        assert_eq!(
            out,
            [Emit::Button {
                button: MouseButton::Left,
                down: false,
                at: Point::new(0, 540)
            }]
        );
    }

    /// A key held while crossing is released locally, not on the target
    #[test]
    fn local_key_releases_locally() {
        let mut sw = switch();
        let shift = usage::LEFT_SHIFT;
        assert_eq!(feed(&mut sw, key(shift, true)).0.verdict, Verdict::Pass);
        cross(&mut sw);
        // Its autorepeat must not type on the hidden local screen
        let (decision, out) = feed(&mut sw, key(shift, true));
        assert_eq!(decision.verdict, Verdict::Swallow);
        assert!(out.is_empty());
        let (decision, out) = feed(&mut sw, key(shift, false));
        assert_eq!(decision.verdict, Verdict::Pass);
        assert!(out.is_empty());
    }

    /// Keys pressed remotely are forwarded with their autorepeat; after a
    /// hand-back their release is swallowed (the target already let go)
    #[test]
    fn remote_key_follows_its_press() {
        let mut sw = switch();
        cross(&mut sw);
        let a = 0x04;
        assert_eq!(
            feed(&mut sw, key(a, true)).1,
            [Emit::Key {
                usage: a,
                down: true
            }]
        );
        assert_eq!(
            feed(&mut sw, key(a, true)).1,
            [Emit::Key {
                usage: a,
                down: true
            }]
        );
        feed(&mut sw, motion(1511, 491, -50.0, 0.0));
        assert!(!sw.is_remote());
        // Still held: its autorepeat must not start typing locally
        let (decision, out) = feed(&mut sw, key(a, true));
        assert_eq!(decision.verdict, Verdict::Swallow);
        assert!(out.is_empty());
        let (decision, out) = feed(&mut sw, key(a, false));
        assert_eq!(decision.verdict, Verdict::Swallow);
        assert!(out.is_empty());
    }

    /// Ctrl+Alt+Esc takes control back; locally it is an ordinary key
    #[test]
    fn escape_hotkey() {
        let mut sw = switch();
        cross(&mut sw);
        feed(&mut sw, key(usage::LEFT_CTRL, true));
        feed(&mut sw, key(usage::RIGHT_ALT, true));
        let (decision, out) = feed(&mut sw, key(usage::ESCAPE, true));
        assert_eq!(decision.verdict, Verdict::Swallow);
        assert!(matches!(decision.cursor, Some(CursorAction::Release(_))));
        assert_eq!(out, [Emit::Leave]);
        // Its autorepeat and release, and the modifiers', stay swallowed
        for event in [
            key(usage::ESCAPE, true),
            key(usage::ESCAPE, false),
            key(usage::LEFT_CTRL, false),
            key(usage::RIGHT_ALT, false),
        ] {
            let (decision, out) = feed(&mut sw, event);
            assert_eq!(decision.verdict, Verdict::Swallow);
            assert!(out.is_empty());
        }

        // Locally the combination passes through untouched
        feed(&mut sw, key(usage::LEFT_CTRL, true));
        feed(&mut sw, key(usage::LEFT_ALT, true));
        let (decision, out) = feed(&mut sw, key(usage::ESCAPE, true));
        assert_eq!(decision.verdict, Verdict::Pass);
        assert!(decision.cursor.is_none() && out.is_empty());
    }

    /// A requested release happens at the next event, which is then
    /// handled locally without crossing straight back
    #[test]
    fn requested_release() {
        let mut sw = switch();
        sw.request_release();
        assert_eq!(feed(&mut sw, motion(5, 5, 1.0, 0.0)).0.cursor, None);

        cross(&mut sw);
        sw.request_release();
        let (decision, out) = feed(&mut sw, motion(1511, 491, 3.0, 0.0));
        assert_eq!(out, [Emit::Leave]);
        assert!(matches!(decision.cursor, Some(CursorAction::Release(_))));
        assert_eq!(decision.verdict, Verdict::Pass);
        assert!(!sw.is_remote());
    }

    /// Scrolling follows the side; nothing crosses before the target
    /// reported its displays
    #[test]
    fn wheel_and_unknown_target() {
        let mut sw = switch();
        let wheel = InputEvent::Wheel { dx: 0, dy: 120 };
        assert_eq!(feed(&mut sw, wheel).0.verdict, Verdict::Pass);
        cross(&mut sw);
        assert_eq!(feed(&mut sw, wheel).1, [Emit::Wheel { dx: 0, dy: 120 }]);

        let mut blind = Switch::new(
            Desktop::new([Rect::new(0, 0, 1512, 982)]),
            Desktop::default(),
            Edge::Right,
        );
        let (decision, _) = feed(&mut blind, motion(1511, 491, 3.0, 0.0));
        assert_eq!(decision.verdict, Verdict::Pass);
    }

    /// Losing every target display hands control back
    #[test]
    fn target_without_displays_releases() {
        let mut sw = switch();
        cross(&mut sw);
        sw.set_target(Desktop::default());
        let (decision, out) = feed(&mut sw, motion(1511, 491, 1.0, 0.0));
        assert_eq!(out, [Emit::Leave]);
        assert!(decision.cursor.is_some());
    }
}
