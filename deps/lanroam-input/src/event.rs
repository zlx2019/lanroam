//! Platform-neutral input events, as captured on the source.

use serde::{Deserialize, Serialize};

use crate::geometry::Point;

/// Mouse buttons Lanroam forwards
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    /// Primary button
    Left,
    /// Secondary button
    Right,
    /// Wheel button
    Middle,
    /// Side button "back" (X1)
    Back,
    /// Side button "forward" (X2)
    Forward,
}

impl MouseButton {
    /// Number of buttons
    pub const COUNT: usize = 5;

    /// Every button, in [`Self::index`] order
    pub const ALL: [Self; Self::COUNT] = [
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Back,
        Self::Forward,
    ];

    /// Dense index, for per-button tables
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// One captured input event
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    /// The pointer moved
    Motion {
        /// Where the local cursor is
        at: Point,
        /// Horizontal motion in local units, reported even while the cursor
        /// is pinned against an edge or parked
        dx: f64,
        /// Vertical motion (positive is down)
        dy: f64,
    },
    /// A mouse button was pressed or released
    Button {
        /// Which button
        button: MouseButton,
        /// Pressed (true) or released
        down: bool,
    },
    /// Scrolling, in 1/120 of a wheel notch (the Windows unit)
    Wheel {
        /// Horizontal amount; positive scrolls right
        dx: i32,
        /// Vertical amount; positive scrolls up
        dy: i32,
    },
    /// A physical key was pressed (autorepeat included) or released
    Key {
        /// USB HID usage on the keyboard page (see [`crate::keymap`])
        usage: u16,
        /// Pressed (true) or released
        down: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Indexes are dense and follow `ALL`
    #[test]
    fn button_indexes() {
        for (i, button) in MouseButton::ALL.into_iter().enumerate() {
            assert_eq!(button.index(), i);
        }
    }
}
