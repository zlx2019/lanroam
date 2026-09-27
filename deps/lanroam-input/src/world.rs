//! The world: every device's desktop on one shared canvas, which decides
//! where the pointer goes when it leaves a device.
//!
//! Canvas units are logical pixels. A device maps onto the canvas through
//! its origin (where its desktop's (0, 0) sits) and its scale (device units
//! per canvas unit: 1 for macOS points, the DPI scale for Windows' physical
//! pixels), so a 150% Windows display and a Mac display of the same logical
//! size line up.
//!
//! Crossing is "what you see is what you get": the pointer leaves one
//! display and enters the facing display of another device at the same spot
//! on the canvas. Where no display faces it, the edge is a wall.

use crate::geometry::{Desktop, Edge, Point, Rect};

/// Width of the zone at each end of a display edge that never crosses, in
/// canvas units: aiming for a screen corner must not throw the pointer to
/// another device
pub const CORNER_GUARD: f64 = 8.0;

/// How far apart two edges may be and still touch, in canvas units; absorbs
/// the rounding of scaled display sizes
const TOUCH: f64 = 1.0;

/// A rectangle on the canvas
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    /// Left edge
    pub left: f64,
    /// Top edge
    pub top: f64,
    /// Right edge (exclusive)
    pub right: f64,
    /// Bottom edge (exclusive)
    pub bottom: f64,
}

impl Area {
    /// The span along `edge`, and where the edge lies across it
    fn side(&self, edge: Edge) -> (f64, f64, f64) {
        match edge {
            Edge::Left => (self.left, self.top, self.bottom),
            Edge::Right => (self.right, self.top, self.bottom),
            Edge::Top => (self.top, self.left, self.right),
            Edge::Bottom => (self.bottom, self.left, self.right),
        }
    }

    /// Whether two areas share more than a boundary
    fn overlaps(&self, other: &Self) -> bool {
        self.left < other.right - TOUCH
            && other.left < self.right - TOUCH
            && self.top < other.bottom - TOUCH
            && other.top < self.bottom - TOUCH
    }
}

/// One device on the canvas
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// The caller's key for the device
    pub key: String,
    /// Displays in the device's own coordinates
    pub desktop: Desktop,
    /// Canvas position of the device's origin
    pub origin: Point,
    /// Device units per canvas unit
    pub scale: f64,
}

impl Device {
    /// A device from its displays, placement and scale (a non-positive
    /// scale counts as 1)
    pub fn new(key: impl Into<String>, desktop: Desktop, origin: Point, scale: f64) -> Self {
        Self {
            key: key.into(),
            desktop,
            origin,
            scale: if scale > 0.0 { scale } else { 1.0 },
        }
    }

    /// A device position on the canvas
    pub fn to_canvas(&self, x: f64, y: f64) -> (f64, f64) {
        (
            f64::from(self.origin.x) + x / self.scale,
            f64::from(self.origin.y) + y / self.scale,
        )
    }

    /// A canvas position in the device's coordinates
    pub fn from_canvas(&self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - f64::from(self.origin.x)) * self.scale,
            (y - f64::from(self.origin.y)) * self.scale,
        )
    }

    /// A display's area on the canvas
    pub fn area(&self, display: &Rect) -> Area {
        let (left, top) = self.to_canvas(f64::from(display.x), f64::from(display.y));
        let (right, bottom) =
            self.to_canvas(f64::from(display.right()), f64::from(display.bottom()));
        Area {
            left,
            top,
            right,
            bottom,
        }
    }

    /// The area all displays cover on the canvas
    pub fn bounds(&self) -> Option<Area> {
        self.desktop.bounds().map(|b| self.area(&b))
    }
}

/// A stretch of edge where two devices' displays touch
#[derive(Debug, Clone, PartialEq)]
pub struct SharedEdge {
    /// The device on the left of a vertical edge, or above a horizontal one
    pub first: String,
    /// The device across the edge
    pub second: String,
    /// The first device's side the edge is on (right or bottom)
    pub edge: Edge,
    /// Where the edge lies across its direction, on the canvas
    pub at: f64,
    /// Start of the shared stretch along the edge
    pub from: f64,
    /// End of the shared stretch
    pub to: f64,
}

/// Every device on the canvas
#[derive(Debug, Clone, Default, PartialEq)]
pub struct World {
    /// Devices with at least one display
    devices: Vec<Device>,
}

impl World {
    /// A world of `devices`; those without displays are left out
    pub fn new(devices: impl IntoIterator<Item = Device>) -> Self {
        Self {
            devices: devices
                .into_iter()
                .filter(|d| !d.desktop.is_empty())
                .collect(),
        }
    }

    /// The devices
    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    /// The device with `key`
    pub fn device(&self, key: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.key == key)
    }

    /// Where a pointer pushed out of device `from` at `at` through `edge`
    /// lands: the facing display of another device at the same canvas
    /// position, `inset` pixels inside it. `None` at a wall or in a corner
    /// guard
    ///
    /// `at` must be on `from`'s outer boundary at `edge` (see
    /// [`Desktop::on_edge`]).
    pub fn cross(&self, from: &str, at: Point, edge: Edge, inset: i32) -> Option<(&Device, Point)> {
        let source = self.device(from)?;
        let display = source.desktop.display_at(at)?;
        let (boundary, lo, hi) = source.area(display).side(edge);
        // The pointer's pixel centre, projected on the canvas
        let (cx, cy) = source.to_canvas(f64::from(at.x) + 0.5, f64::from(at.y) + 0.5);
        let along = match edge {
            Edge::Left | Edge::Right => cy,
            Edge::Top | Edge::Bottom => cx,
        };
        if along < lo + CORNER_GUARD || along > hi - CORNER_GUARD {
            return None;
        }
        self.devices
            .iter()
            .filter(|d| d.key != from)
            .find_map(|target| {
                let entry = target.desktop.displays().iter().find(|e| {
                    let (facing, lo, hi) = target.area(e).side(edge.opposite());
                    (facing - boundary).abs() <= TOUCH && along >= lo && along < hi
                })?;
                Some((target, landing(target, entry, edge, along, inset)))
            })
    }

    /// The nearest device in `direction` from device `from`: devices lined
    /// up with it first, then the closest
    pub fn neighbour(&self, from: &str, direction: Edge) -> Option<&Device> {
        let a = self.device(from)?.bounds()?;
        let (boundary, lo, hi) = a.side(direction);
        self.devices
            .iter()
            .filter(|d| d.key != from)
            .filter_map(|d| {
                let b = d.bounds()?;
                let (facing, blo, bhi) = b.side(direction.opposite());
                let gap = match direction {
                    Edge::Right | Edge::Bottom => facing - boundary,
                    Edge::Left | Edge::Top => boundary - facing,
                };
                // Only devices beyond the side, not behind or overlapping it
                (gap >= -TOUCH).then(|| {
                    let overlap = hi.min(bhi) - lo.max(blo);
                    (d, overlap <= TOUCH, gap.max(0.0) + (-overlap).max(0.0))
                })
            })
            .min_by(|x, y| {
                (x.1, x.2)
                    .partial_cmp(&(y.1, y.2))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(d, _, _)| d)
    }

    /// Devices in reading order: left to right, then top to bottom
    pub fn ordered(&self) -> Vec<&Device> {
        let mut devices: Vec<(&Device, Area)> = self
            .devices
            .iter()
            .filter_map(|d| Some((d, d.bounds()?)))
            .collect();
        devices.sort_by(|(_, a), (_, b)| {
            (a.left.round(), a.top.round())
                .partial_cmp(&(b.left.round(), b.top.round()))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        devices.into_iter().map(|(d, _)| d).collect()
    }

    /// Two devices whose displays overlap on the canvas, if any: such a
    /// layout is invalid
    pub fn overlapping(&self) -> Option<(&str, &str)> {
        for (i, a) in self.devices.iter().enumerate() {
            for b in &self.devices[i + 1..] {
                let clash = a.desktop.displays().iter().any(|da| {
                    let da = a.area(da);
                    b.desktop
                        .displays()
                        .iter()
                        .any(|db| da.overlaps(&b.area(db)))
                });
                if clash {
                    return Some((&a.key, &b.key));
                }
            }
        }
        None
    }

    /// Every stretch of edge where two devices' displays touch
    pub fn shared_edges(&self) -> Vec<SharedEdge> {
        let mut edges = Vec::new();
        for a in &self.devices {
            for b in self.devices.iter().filter(|b| b.key != a.key) {
                for da in a.desktop.displays() {
                    for db in b.desktop.displays() {
                        let (area_a, area_b) = (a.area(da), b.area(db));
                        for edge in [Edge::Right, Edge::Bottom] {
                            let (at, a_lo, a_hi) = area_a.side(edge);
                            let (facing, b_lo, b_hi) = area_b.side(edge.opposite());
                            let (from, to) = (a_lo.max(b_lo), a_hi.min(b_hi));
                            if (at - facing).abs() <= TOUCH && to - from > TOUCH {
                                edges.push(SharedEdge {
                                    first: a.key.clone(),
                                    second: b.key.clone(),
                                    edge,
                                    at,
                                    from,
                                    to,
                                });
                            }
                        }
                    }
                }
            }
        }
        edges
    }
}

/// The point on `display` of `target` where a pointer arrives through the
/// side facing `edge`, at canvas position `along` on that side
fn landing(target: &Device, display: &Rect, edge: Edge, along: f64, inset: i32) -> Point {
    let (x, y) = match edge {
        Edge::Left | Edge::Right => target.from_canvas(0.0, along),
        Edge::Top | Edge::Bottom => target.from_canvas(along, 0.0),
    };
    let inside = display.clamp(Point::floor(x, y));
    display.clamp(match edge {
        Edge::Right => Point::new(display.x + inset, inside.y),
        Edge::Left => Point::new(display.right() - 1 - inset, inside.y),
        Edge::Bottom => Point::new(inside.x, display.y + inset),
        Edge::Top => Point::new(inside.x, display.bottom() - 1 - inset),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device with one display of `w`x`h` at its origin
    fn single(key: &str, origin: (i32, i32), w: i32, h: i32, scale: f64) -> Device {
        Device::new(
            key,
            Desktop::new([Rect::new(0, 0, w, h)]),
            Point::new(origin.0, origin.1),
            scale,
        )
    }

    /// A Mac (2560x1440 points) with a 150% Windows PC (2880x1620 physical,
    /// 1920x1080 logical) to its right, 100 units lower
    fn mac_and_pc() -> World {
        World::new([
            single("mac", (0, 0), 2560, 1440, 1.0),
            single("pc", (2560, 100), 2880, 1620, 1.5),
        ])
    }

    /// The pointer enters at the same canvas height, in the target's own
    /// (scaled) coordinates
    #[test]
    fn crosses_at_the_same_spot() {
        let world = mac_and_pc();
        let (pc, at) = world
            .cross("mac", Point::new(2559, 600), Edge::Right, 1)
            .unwrap();
        assert_eq!(pc.key, "pc");
        // Canvas y 600.5 is 500.5 below the PC's origin: 750.75 physical
        assert_eq!(at, Point::new(1, 750));
        let (mac, back) = world
            .cross("pc", Point::new(0, 750), Edge::Left, 1)
            .unwrap();
        assert_eq!(mac.key, "mac");
        assert_eq!(back, Point::new(2558, 600));
    }

    /// Beside the stretch the displays share there is wall, and corners
    /// never cross
    #[test]
    fn walls_and_corners() {
        let world = mac_and_pc();
        // Above the PC's top (canvas y < 100): wall
        assert!(
            world
                .cross("mac", Point::new(2559, 50), Edge::Right, 1)
                .is_none()
        );
        // Other sides: nothing there
        assert!(
            world
                .cross("mac", Point::new(0, 600), Edge::Left, 1)
                .is_none()
        );
        // Within the corner guard of the PC's top-left corner (canvas y 100)
        assert!(world.cross("pc", Point::new(0, 5), Edge::Left, 1).is_none());
        assert!(
            world
                .cross("pc", Point::new(0, 20), Edge::Left, 1)
                .is_some()
        );
        // The PC's bottom reaches canvas 1180; the Mac continues below it
        let (_, at) = world
            .cross("mac", Point::new(2559, 1150), Edge::Right, 1)
            .unwrap();
        assert_eq!(at, Point::new(1, 1575));
        assert!(
            world
                .cross("mac", Point::new(2559, 1185), Edge::Right, 1)
                .is_none()
        );
    }

    /// Crossing works vertically and between displays of multi-display
    /// devices
    #[test]
    fn vertical_and_multi_display() {
        let laptop = Device::new(
            "laptop",
            Desktop::new([Rect::new(0, 0, 1440, 900), Rect::new(1440, 0, 1920, 1080)]),
            Point::new(0, 0),
            1.0,
        );
        let below = single("tv", (1440, 1080), 1920, 1080, 1.0);
        let world = World::new([laptop, below]);
        let (tv, at) = world
            .cross("laptop", Point::new(2000, 1079), Edge::Bottom, 1)
            .unwrap();
        assert_eq!((tv.key.as_str(), at), ("tv", Point::new(560, 1)));
        // The laptop's own display ends higher up: nothing below it
        assert!(
            world
                .cross("laptop", Point::new(700, 899), Edge::Bottom, 1)
                .is_none()
        );
        let (_, up) = world.cross("tv", Point::new(560, 0), Edge::Top, 1).unwrap();
        assert_eq!(up, Point::new(2000, 1078));
    }

    /// Neighbours go by direction, preferring devices lined up with the
    /// origin
    #[test]
    fn neighbours() {
        // a | b, and c below a but far to the right
        let world = World::new([
            single("a", (0, 0), 1000, 1000, 1.0),
            single("b", (1000, 0), 1000, 1000, 1.0),
            single("c", (3000, 1000), 1000, 1000, 1.0),
            single("d", (0, 1000), 1000, 1000, 1.0),
        ]);
        let key = |from, dir| world.neighbour(from, dir).map(|d| d.key.clone());
        assert_eq!(key("a", Edge::Right).as_deref(), Some("b"));
        assert_eq!(key("b", Edge::Left).as_deref(), Some("a"));
        assert_eq!(key("a", Edge::Bottom).as_deref(), Some("d"));
        assert_eq!(key("b", Edge::Bottom).as_deref(), Some("d"));
        assert_eq!(key("d", Edge::Right).as_deref(), Some("c"));
        assert_eq!(key("a", Edge::Left), None);
        assert_eq!(key("a", Edge::Top), None);
    }

    /// Reading order, overlap detection and shared edges
    #[test]
    fn order_overlaps_and_edges() {
        let world = mac_and_pc();
        let keys: Vec<&str> = world.ordered().iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["mac", "pc"]);
        assert_eq!(world.overlapping(), None);

        let edges = world.shared_edges();
        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert_eq!((edge.first.as_str(), edge.second.as_str()), ("mac", "pc"));
        assert_eq!(
            (edge.edge, edge.at, edge.from, edge.to),
            (Edge::Right, 2560.0, 100.0, 1180.0)
        );

        let clash = World::new([
            single("a", (0, 0), 1000, 1000, 1.0),
            single("b", (999, 500), 1000, 1000, 1.0),
            single("c", (900, 0), 1000, 1000, 1.0),
        ]);
        assert_eq!(clash.overlapping(), Some(("a", "c")));
        // Touching within rounding is not overlapping
        let touching = World::new([
            single("a", (0, 0), 1000, 1000, 1.0),
            single("b", (999, 0), 1000, 1000, 1.0),
        ]);
        assert_eq!(touching.overlapping(), None);
    }

    /// Devices without displays are left out
    #[test]
    fn empty_devices_are_ignored() {
        let world = World::new([
            single("a", (0, 0), 1000, 1000, 1.0),
            Device::new("b", Desktop::default(), Point::new(1000, 0), 1.0),
        ]);
        assert_eq!(world.devices().len(), 1);
        assert!(
            world
                .cross("a", Point::new(999, 500), Edge::Right, 1)
                .is_none()
        );
    }
}
