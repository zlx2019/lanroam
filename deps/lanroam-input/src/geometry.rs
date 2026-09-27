//! Screen geometry: display rectangles, desktop edges, and where the pointer
//! lands when it crosses from one desktop to another.
//!
//! Coordinates are each desktop's native ones (macOS: points, Windows:
//! physical pixels), with the origin at the primary display's top-left
//! corner and y growing downwards.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A position on a desktop
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Point {
    /// Horizontal coordinate
    pub x: i32,
    /// Vertical coordinate (positive is down)
    pub y: i32,
}

impl Point {
    /// A point from its coordinates
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// The pixel containing a fractional position
    pub fn floor(x: f64, y: f64) -> Self {
        Self::new(x.floor() as i32, y.floor() as i32)
    }
}

/// A display's bounds on its desktop
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    /// Left column
    pub x: i32,
    /// Top row
    pub y: i32,
    /// Width
    pub width: i32,
    /// Height
    pub height: i32,
}

impl Rect {
    /// A rectangle from its top-left corner and size
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// One past the rightmost column
    pub const fn right(&self) -> i32 {
        self.x + self.width
    }

    /// One past the bottom row
    pub const fn bottom(&self) -> i32 {
        self.y + self.height
    }

    /// Whether the rectangle covers no pixel at all
    pub const fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// Whether `p` lies inside
    pub const fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    /// The point inside closest to `p`
    pub fn clamp(&self, p: Point) -> Point {
        Point::new(
            p.x.min(self.right() - 1).max(self.x),
            p.y.min(self.bottom() - 1).max(self.y),
        )
    }

    /// Squared distance from `p` to the rectangle (0 inside)
    fn distance_sq(&self, p: Point) -> i64 {
        let c = self.clamp(p);
        let (dx, dy) = (i64::from(p.x - c.x), i64::from(p.y - c.y));
        dx * dx + dy * dy
    }
}

/// A side of a desktop
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    /// Left side
    Left,
    /// Right side
    Right,
    /// Top side
    Top,
    /// Bottom side
    Bottom,
}

impl Edge {
    /// The facing side on the neighbouring desktop
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }

    /// Whether a motion pushes out through this side
    pub fn pushed_by(self, dx: f64, dy: f64) -> bool {
        match self {
            Self::Left => dx < 0.0,
            Self::Right => dx > 0.0,
            Self::Top => dy < 0.0,
            Self::Bottom => dy > 0.0,
        }
    }

    /// One step out of a desktop through this side
    const fn outward(self) -> (i32, i32) {
        match self {
            Self::Left => (-1, 0),
            Self::Right => (1, 0),
            Self::Top => (0, -1),
            Self::Bottom => (0, 1),
        }
    }
}

impl fmt::Display for Edge {
    /// Lowercase name, as accepted by [`FromStr`]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        })
    }
}

impl FromStr for Edge {
    type Err = String;

    /// Parse `left`, `right`, `top` or `bottom` (any case)
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "top" => Ok(Self::Top),
            "bottom" => Ok(Self::Bottom),
            _ => Err(format!("{s:?} is not an edge (left, right, top, bottom)")),
        }
    }
}

/// Outcome of moving a virtual cursor with [`Desktop::step`]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// Still on the desktop, at this position
    Inside(f64, f64),
    /// Pushed off the desktop through the exit edge, last seen here
    Exit(Point),
}

/// The displays of one machine
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Desktop {
    /// Non-empty display rectangles
    displays: Vec<Rect>,
}

impl Desktop {
    /// A desktop from its displays; empty rectangles are dropped
    pub fn new(displays: impl IntoIterator<Item = Rect>) -> Self {
        Self {
            displays: displays.into_iter().filter(|d| !d.is_empty()).collect(),
        }
    }

    /// The display rectangles
    pub fn displays(&self) -> &[Rect] {
        &self.displays
    }

    /// Whether there is no display at all
    pub fn is_empty(&self) -> bool {
        self.displays.is_empty()
    }

    /// Bounding box of every display
    pub fn bounds(&self) -> Option<Rect> {
        let first = self.displays.first()?;
        let (mut left, mut top, mut right, mut bottom) =
            (first.x, first.y, first.right(), first.bottom());
        for d in &self.displays {
            left = left.min(d.x);
            top = top.min(d.y);
            right = right.max(d.right());
            bottom = bottom.max(d.bottom());
        }
        Some(Rect::new(left, top, right - left, bottom - top))
    }

    /// The display containing `p`
    pub fn display_at(&self, p: Point) -> Option<&Rect> {
        self.displays.iter().find(|d| d.contains(p))
    }

    /// The display closest to `p` (the one containing it, if any)
    fn nearest(&self, p: Point) -> Option<&Rect> {
        self.displays.iter().min_by_key(|d| d.distance_sq(p))
    }

    /// The point on the desktop closest to `p`
    pub fn clamp(&self, p: Point) -> Point {
        self.nearest(p).map_or(p, |d| d.clamp(p))
    }

    /// Whether `p` is on the desktop's outer boundary at `edge`: on the
    /// edge row or column of its display, with no display beyond it
    pub fn on_edge(&self, p: Point, edge: Edge) -> bool {
        let Some(d) = self.display_at(p) else {
            return false;
        };
        let at_side = match edge {
            Edge::Left => p.x == d.x,
            Edge::Right => p.x == d.right() - 1,
            Edge::Top => p.y == d.y,
            Edge::Bottom => p.y == d.bottom() - 1,
        };
        let (ox, oy) = edge.outward();
        at_side && self.display_at(Point::new(p.x + ox, p.y + oy)).is_none()
    }

    /// Where a pointer leaving this desktop at `p` through `edge` enters
    /// `other`: at the same relative position along the edge, `inset`
    /// pixels inside `other`'s facing side
    pub fn map_across(&self, p: Point, edge: Edge, other: &Desktop, inset: i32) -> Option<Point> {
        let (from, to) = (self.bounds()?, other.bounds()?);
        let entry = match edge {
            Edge::Right => Point::new(
                to.x + inset,
                along(p.y, from.y, from.height, to.y, to.height),
            ),
            Edge::Left => Point::new(
                to.right() - 1 - inset,
                along(p.y, from.y, from.height, to.y, to.height),
            ),
            Edge::Bottom => {
                Point::new(along(p.x, from.x, from.width, to.x, to.width), to.y + inset)
            }
            Edge::Top => Point::new(
                along(p.x, from.x, from.width, to.x, to.width),
                to.bottom() - 1 - inset,
            ),
        };
        Some(other.clamp(entry))
    }

    /// Move a virtual cursor at `from` by (`dx`, `dy`)
    ///
    /// Moving between displays is free; where no display continues, the
    /// cursor slides along the display it is on, unless it was pushed out
    /// through `exit` on the desktop's outer boundary.
    pub fn step(&self, from: (f64, f64), dx: f64, dy: f64, exit: Edge) -> Step {
        let to = (from.0 + dx, from.1 + dy);
        if self.display_at(Point::floor(to.0, to.1)).is_some() {
            return Step::Inside(to.0, to.1);
        }
        let Some(d) = self.nearest(Point::floor(from.0, from.1)) else {
            return Step::Inside(from.0, from.1);
        };
        let x = to.0.min(f64::from(d.right() - 1)).max(f64::from(d.x));
        let y = to.1.min(f64::from(d.bottom() - 1)).max(f64::from(d.y));
        let pushed = match exit {
            Edge::Left => to.0 < f64::from(d.x),
            Edge::Right => to.0 >= f64::from(d.right()),
            Edge::Top => to.1 < f64::from(d.y),
            Edge::Bottom => to.1 >= f64::from(d.bottom()),
        };
        let stop = Point::floor(x, y);
        if pushed && self.on_edge(stop, exit) {
            Step::Exit(stop)
        } else {
            Step::Inside(x, y)
        }
    }
}

/// Scale a coordinate along one axis from one span to another
fn along(pos: i32, from_start: i32, from_len: i32, to_start: i32, to_len: i32) -> i32 {
    let offset = i64::from(pos - from_start) * i64::from(to_len) / i64::from(from_len.max(1));
    to_start.saturating_add(i32::try_from(offset).unwrap_or(i32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1920x1080 main display with a 1280x1024 one to its right, lower by
    /// 100 pixels
    fn two_displays() -> Desktop {
        Desktop::new([
            Rect::new(0, 0, 1920, 1080),
            Rect::new(1920, 100, 1280, 1024),
        ])
    }

    /// Empty rectangles are dropped and the bounds cover every display
    #[test]
    fn bounds_and_empties() {
        let desk = Desktop::new([Rect::new(0, 0, 0, 10), Rect::new(-100, 5, 100, 10)]);
        assert_eq!(desk.displays().len(), 1);
        assert_eq!(two_displays().bounds(), Some(Rect::new(0, 0, 3200, 1124)));
        assert_eq!(Desktop::default().bounds(), None);
    }

    /// Only the outer boundary counts as an edge; where two displays meet
    /// is not one
    #[test]
    fn outer_edges_only() {
        let desk = two_displays();
        // Where the displays meet
        assert!(!desk.on_edge(Point::new(1919, 500), Edge::Right));
        // The main display's right side above the second display is exposed
        assert!(desk.on_edge(Point::new(1919, 50), Edge::Right));
        assert!(desk.on_edge(Point::new(3199, 500), Edge::Right));
        assert!(desk.on_edge(Point::new(0, 500), Edge::Left));
        assert!(!desk.on_edge(Point::new(1, 500), Edge::Left));
        // Off every display
        assert!(!desk.on_edge(Point::new(3200, 500), Edge::Right));
    }

    /// Crossing keeps the relative position along the edge and lands inset
    #[test]
    fn map_across_is_proportional() {
        let mac = Desktop::new([Rect::new(0, 0, 1512, 982)]);
        let pc = Desktop::new([Rect::new(0, 0, 1920, 1080)]);
        assert_eq!(
            mac.map_across(Point::new(1511, 491), Edge::Right, &pc, 1),
            Some(Point::new(1, 540))
        );
        assert_eq!(
            pc.map_across(Point::new(0, 1079), Edge::Left, &mac, 1),
            Some(Point::new(1510, 981))
        );
        assert_eq!(
            mac.map_across(Point::new(756, 0), Edge::Top, &pc, 0),
            Some(Point::new(960, 1079))
        );
        assert_eq!(
            mac.map_across(Point::new(0, 0), Edge::Right, &Desktop::default(), 1),
            None
        );
    }

    /// The landing point is clamped onto a display of an irregular desktop
    #[test]
    fn map_across_clamps_into_a_display() {
        let mac = Desktop::new([Rect::new(0, 0, 1000, 1000)]);
        // The left display starts halfway down, so the top of the facing
        // side has no display
        let pcs = Desktop::new([Rect::new(0, 500, 1000, 500), Rect::new(1000, 0, 1000, 1000)]);
        assert_eq!(
            mac.map_across(Point::new(999, 0), Edge::Right, &pcs, 0),
            Some(Point::new(0, 500))
        );
    }

    /// The cursor moves freely between displays, slides along blocked sides
    /// and only exits through the exit edge
    #[test]
    fn step_moves_slides_and_exits() {
        let desk = two_displays();
        // Across the shared side
        assert_eq!(
            desk.step((1919.0, 500.0), 5.0, 0.0, Edge::Left),
            Step::Inside(1924.0, 500.0)
        );
        // Blocked at the top: slide
        assert_eq!(
            desk.step((100.0, 0.0), 3.0, -4.0, Edge::Left),
            Step::Inside(103.0, 0.0)
        );
        // Out through the exit edge
        assert_eq!(
            desk.step((0.5, 300.0), -2.0, 0.0, Edge::Left),
            Step::Exit(Point::new(0, 300))
        );
        // Past the right edge, which is not the exit
        assert_eq!(
            desk.step((3199.0, 300.0), 10.0, 1.0, Edge::Left),
            Step::Inside(3199.0, 301.0)
        );
    }

    /// Edges parse from any case and print back lowercase
    #[test]
    fn edge_parse_and_display() {
        assert_eq!("Right".parse::<Edge>(), Ok(Edge::Right));
        assert_eq!(Edge::Bottom.to_string(), "bottom");
        assert!("north".parse::<Edge>().is_err());
        assert_eq!(Edge::Top.opposite(), Edge::Bottom);
    }
}
