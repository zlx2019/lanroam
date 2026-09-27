//! `listen --dry-run`: an injector that prints what it would inject, to try
//! a source against a second instance on the same machine.

use std::time::{Duration, Instant};

use lanroam_core::lanroam_input::inject::Injector;
use lanroam_core::lanroam_input::keymap;
use lanroam_core::lanroam_input::{InputError, MouseButton, Point};

/// Pointer positions are printed at most this often
const MOTION_EVERY: Duration = Duration::from_millis(250);

/// Prints instead of injecting
pub(crate) struct PrintInjector {
    /// When a pointer position was last printed
    last_motion: Option<Instant>,
}

impl PrintInjector {
    /// A printer that has printed nothing yet
    pub(crate) fn new() -> Self {
        Self { last_motion: None }
    }
}

/// "down" or "up"
fn state(down: bool) -> &'static str {
    if down { "down" } else { "up" }
}

impl Injector for PrintInjector {
    /// Print the pointer position, throttled
    fn move_to(&mut self, at: Point) -> Result<(), InputError> {
        if self.last_motion.is_none_or(|t| t.elapsed() >= MOTION_EVERY) {
            self.last_motion = Some(Instant::now());
            println!("  pointer  ({}, {})", at.x, at.y);
        }
        Ok(())
    }

    /// Print the button
    fn button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
        println!("  button   {button:?} {}", state(down));
        Ok(())
    }

    /// Print the scroll amounts
    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        println!("  wheel    dx {dx} dy {dy}");
        Ok(())
    }

    /// Print the key by name
    fn key(&mut self, usage: u16, down: bool) -> Result<(), InputError> {
        let name = keymap::name(usage).unwrap_or("?");
        println!("  key      {name} ({usage:#04x}) {}", state(down));
        Ok(())
    }
}
