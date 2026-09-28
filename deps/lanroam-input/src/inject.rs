//! Target side: replays a source's input through the platform injector and
//! remembers what it holds, so that everything is released when the source
//! leaves or the link drops.

use std::collections::BTreeSet;

use crate::InputError;
use crate::config::{NORMAL_SPEED, Scrolling};
use crate::event::MouseButton;
use crate::geometry::Point;

/// Synthesizes input in the local session (one implementation per OS)
pub trait Injector: Send {
    /// Move the cursor to `at` (desktop coordinates)
    fn move_to(&mut self, at: Point) -> Result<(), InputError>;
    /// Press or release a mouse button where the cursor is
    fn button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError>;
    /// Scroll, in 1/120 of a notch; `dy > 0` is up, `dx > 0` is right
    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError>;
    /// Press or release a key given as a USB HID usage
    fn key(&mut self, usage: u16, down: bool) -> Result<(), InputError>;
}

/// What a target did with a source's input
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InjectStats {
    /// Cursor positions applied
    pub motions: u32,
    /// Cursor positions dropped because a newer one had arrived first
    pub stale_motions: u32,
    /// Key presses, autorepeats and releases applied
    pub keys: u32,
    /// Button presses and releases applied
    pub buttons: u32,
    /// Scroll steps applied
    pub wheels: u32,
    /// Injections the OS refused
    pub failures: u32,
}

/// A source's input being replayed on this machine
///
/// Dropping it releases every key and button the source still holds.
pub struct RemoteInput {
    /// The platform injector
    injector: Box<dyn Injector>,
    /// Keys the source pressed and has not released
    keys: BTreeSet<u16>,
    /// Buttons the source pressed and has not released
    buttons: [bool; MouseButton::COUNT],
    /// Sequence number of the newest cursor position applied
    last_motion: Option<u32>,
    /// Counters
    stats: InjectStats,
}

impl RemoteInput {
    /// Replay through `injector`
    pub fn new(injector: Box<dyn Injector>) -> Self {
        Self {
            injector,
            keys: BTreeSet::new(),
            buttons: [false; MouseButton::COUNT],
            last_motion: None,
            stats: InjectStats::default(),
        }
    }

    /// Counters so far
    pub fn stats(&self) -> &InjectStats {
        &self.stats
    }

    /// A source took control; its cursor starts at `at`. Its motion
    /// numbering starts afresh
    pub fn enter(&mut self, at: Point) {
        self.last_motion = None;
        let result = self.injector.move_to(at);
        self.check(result);
    }

    /// The source moved its cursor; `seq` orders the unreliable updates
    pub fn motion(&mut self, seq: u32, at: Point) {
        // Datagrams may overtake each other; an older position is stale
        if let Some(last) = self.last_motion
            && (seq.wrapping_sub(last) as i32) <= 0
        {
            self.stats.stale_motions += 1;
            return;
        }
        self.last_motion = Some(seq);
        self.stats.motions += 1;
        let result = self.injector.move_to(at);
        self.check(result);
    }

    /// A key press, autorepeat or release; releasing a key this source never
    /// pressed does nothing
    pub fn key(&mut self, usage: u16, down: bool) {
        if down {
            self.keys.insert(usage);
        } else if !self.keys.remove(&usage) {
            return;
        }
        self.stats.keys += 1;
        let result = self.injector.key(usage, down);
        self.check(result);
    }

    /// A button press or release at `at`; releasing a button this source
    /// never pressed does nothing
    pub fn button(&mut self, button: MouseButton, down: bool, at: Point) {
        let held = &mut self.buttons[button.index()];
        if !down && !*held {
            return;
        }
        *held = down;
        self.stats.buttons += 1;
        let result = self.injector.move_to(at);
        self.check(result);
        let result = self.injector.button(button, down);
        self.check(result);
    }

    /// Scrolling
    pub fn wheel(&mut self, dx: i32, dy: i32) {
        self.stats.wheels += 1;
        let result = self.injector.wheel(dx, dy);
        self.check(result);
    }

    /// Release every key and button the source still holds
    pub fn release_all(&mut self) {
        for usage in std::mem::take(&mut self.keys) {
            let result = self.injector.key(usage, false);
            self.check(result);
        }
        for button in MouseButton::ALL {
            if std::mem::take(&mut self.buttons[button.index()]) {
                let result = self.injector.button(button, false);
                self.check(result);
            }
        }
    }

    /// Count a failed injection; only the first one is a warning, since the
    /// cause (an elevated window, the secure desktop) usually persists
    fn check(&mut self, result: Result<(), InputError>) {
        if let Err(e) = result {
            self.stats.failures += 1;
            if self.stats.failures == 1 {
                tracing::warn!("input injection failed: {e}");
            } else {
                tracing::debug!("input injection failed: {e}");
            }
        }
    }
}

/// Scrolling from a controller as this device wants it: faster or slower,
/// maybe turned around. Fractions of a unit carry over, so slow scrolling
/// at a low speed is not lost
#[derive(Debug, Default)]
pub struct WheelScale {
    /// The settings
    scrolling: Scrolling,
    /// Scrolling not yet worth a whole unit (x, y)
    rest: (f64, f64),
}

impl WheelScale {
    /// Use `scrolling` from now on
    pub fn set(&mut self, scrolling: Scrolling) {
        self.scrolling = scrolling;
        self.rest = (0.0, 0.0);
    }

    /// One scroll step (1/120 notch units) as it is to be replayed
    pub fn apply(&mut self, dx: i32, dy: i32) -> (i32, i32) {
        let sign = if self.scrolling.reverse { -1.0 } else { 1.0 };
        let k = sign * f64::from(self.scrolling.speed) / f64::from(NORMAL_SPEED);
        let (x, y) = (
            f64::from(dx) * k + self.rest.0,
            f64::from(dy) * k + self.rest.1,
        );
        let (whole_x, whole_y) = (x.trunc(), y.trunc());
        self.rest = (x - whole_x, y - whole_y);
        (whole_x as i32, whole_y as i32)
    }
}

impl Drop for RemoteInput {
    /// Never leave a key or button held after the source is gone
    fn drop(&mut self) {
        self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// What the fake injector was asked to do
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Op {
        Move(Point),
        Button(MouseButton, bool),
        Wheel(i32, i32),
        Key(u16, bool),
    }

    /// Records every call
    struct Recorder(Arc<Mutex<Vec<Op>>>);

    impl Injector for Recorder {
        fn move_to(&mut self, at: Point) -> Result<(), InputError> {
            self.0.lock().unwrap().push(Op::Move(at));
            Ok(())
        }
        fn button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
            self.0.lock().unwrap().push(Op::Button(button, down));
            Ok(())
        }
        fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
            self.0.lock().unwrap().push(Op::Wheel(dx, dy));
            Ok(())
        }
        fn key(&mut self, usage: u16, down: bool) -> Result<(), InputError> {
            self.0.lock().unwrap().push(Op::Key(usage, down));
            Ok(())
        }
    }

    /// A replayer over a recorder, and the recorded calls
    fn recorded() -> (RemoteInput, Arc<Mutex<Vec<Op>>>) {
        let ops = Arc::new(Mutex::new(Vec::new()));
        (RemoteInput::new(Box::new(Recorder(Arc::clone(&ops)))), ops)
    }

    /// Stale positions are dropped, including across the u32 wrap
    #[test]
    fn stale_motion_is_dropped() {
        let (mut input, ops) = recorded();
        input.motion(u32::MAX, Point::new(1, 1));
        input.motion(0, Point::new(2, 2));
        input.motion(u32::MAX, Point::new(3, 3));
        input.motion(0, Point::new(4, 4));
        assert_eq!(
            *ops.lock().unwrap(),
            [Op::Move(Point::new(1, 1)), Op::Move(Point::new(2, 2))]
        );
        assert_eq!(input.stats().motions, 2);
        assert_eq!(input.stats().stale_motions, 2);
    }

    /// Dropping the replayer releases what the source still holds, and
    /// stray releases are ignored
    #[test]
    fn releases_on_drop() {
        let (mut input, ops) = recorded();
        input.key(0x04, true);
        input.key(0x05, false);
        input.button(MouseButton::Right, true, Point::new(7, 8));
        input.button(MouseButton::Left, false, Point::new(7, 8));
        drop(input);
        assert_eq!(
            *ops.lock().unwrap(),
            [
                Op::Key(0x04, true),
                Op::Move(Point::new(7, 8)),
                Op::Button(MouseButton::Right, true),
                Op::Key(0x04, false),
                Op::Button(MouseButton::Right, false),
            ]
        );
    }

    /// A new source's motions are not stale against an earlier source's
    #[test]
    fn enter_restarts_numbering() {
        let (mut input, ops) = recorded();
        input.motion(500, Point::new(1, 1));
        input.enter(Point::new(2, 2));
        input.motion(1, Point::new(3, 3));
        assert_eq!(
            *ops.lock().unwrap(),
            [
                Op::Move(Point::new(1, 1)),
                Op::Move(Point::new(2, 2)),
                Op::Move(Point::new(3, 3))
            ]
        );
    }

    /// Scrolling is scaled with the fractions kept, and turned around
    #[test]
    fn wheel_scale() {
        let mut scale = WheelScale::default();
        assert_eq!(scale.apply(-7, 120), (-7, 120));
        scale.set(Scrolling {
            speed: 150,
            reverse: false,
        });
        assert_eq!(scale.apply(0, 1), (0, 1));
        assert_eq!(scale.apply(0, 1), (0, 2));
        scale.set(Scrolling {
            speed: 50,
            reverse: true,
        });
        assert_eq!(scale.apply(0, 1), (0, 0));
        assert_eq!(scale.apply(0, 1), (0, -1));
        assert_eq!(scale.apply(240, -120), (-120, 60));
    }

    /// Released keys are not released again
    #[test]
    fn release_all_is_idempotent() {
        let (mut input, ops) = recorded();
        input.key(0x04, true);
        input.key(0x04, false);
        input.wheel(0, -120);
        input.release_all();
        assert_eq!(
            *ops.lock().unwrap(),
            [
                Op::Key(0x04, true),
                Op::Key(0x04, false),
                Op::Wheel(0, -120)
            ]
        );
        assert_eq!(input.stats().keys, 2);
    }
}
