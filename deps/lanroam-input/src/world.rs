//! The world: every device's desktop on one shared canvas, which decides
//! where the pointer goes when it leaves a device.
//!
//! Canvas units are logical pixels. A device maps onto the canvas through
//! its origin (where its desktop's (0, 0) sits) and its scale (device units
//! per canvas unit: 1 for macOS points, the DPI scale for Windows' physical
//! pixels), so a 150% Windows display and a Mac display of the same logical
//! size line up. Displays of one device may have scales of their own (a
//! Windows PC with displays at 200% and 100%): each then takes its own
//! logical size, placed from the primary display out so that displays
//! touching on the device touch on the canvas too.
//!
//! Crossing is proportional: where two devices touch, the whole of each
//! one's side crosses, position mapped by its share of the side (the bottom
//! of a tall display leads to the bottom of a short one), so going across
//! and back returns to the same height. A device's side is every display of
//! it on that line; several devices facing one side count as one stretch.
//! Where no device faces a side, it is a wall.

use crate::geometry::{Desktop, Edge, Point, Rect};

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
    /// Device units per canvas unit on its primary display (see
    /// [`primary`]), and on the others unless they have scales of their own
    pub scale: f64,
    /// Where each display sits on the canvas, in the desktop's order
    frames: Vec<Frame>,
}

/// Where a display sits on the canvas, relative to its device's origin
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frame {
    /// Canvas position of the display's left edge
    left: f64,
    /// Canvas position of the display's top edge
    top: f64,
    /// Device units per canvas unit on the display
    scale: f64,
}

impl Device {
    /// A device from its displays, placement and scale, the same on every
    /// display (a non-positive scale counts as 1)
    pub fn new(key: impl Into<String>, desktop: Desktop, origin: Point, scale: f64) -> Self {
        let scale = valid_scale(scale);
        let frames = uniform(desktop.displays(), scale);
        Self {
            key: key.into(),
            desktop,
            origin,
            scale,
            frames,
        }
    }

    /// A device whose displays each have a scale of their own (a Windows PC
    /// with displays at 200% and 100%): each display takes its own logical
    /// size on the canvas, and displays that touch on the device still
    /// touch there. Where that cannot be (displays apart, or overlapping
    /// once placed), every display takes the primary display's scale
    pub fn scaled(
        key: impl Into<String>,
        displays: impl IntoIterator<Item = (Rect, f64)>,
        origin: Point,
    ) -> Self {
        let (rects, scales): (Vec<Rect>, Vec<f64>) = displays
            .into_iter()
            .filter(|(display, _)| !display.is_empty())
            .map(|(display, scale)| (display, valid_scale(scale)))
            .unzip();
        let scale = scales.get(primary(&rects)).copied().unwrap_or(1.0);
        let frames = place(&rects, &scales)
            .filter(|frames| !frames_overlap(&rects, frames))
            .unwrap_or_else(|| uniform(&rects, scale));
        Self {
            key: key.into(),
            desktop: Desktop::new(rects),
            origin,
            scale,
            frames,
        }
    }

    /// The frame of `display`, one of the desktop's (the primary display's
    /// scale applied to any other rectangle)
    fn frame(&self, display: &Rect) -> Frame {
        let index = self.desktop.displays().iter().position(|d| d == display);
        index
            .and_then(|index| self.frames.get(index).copied())
            .unwrap_or_else(|| uniform(std::slice::from_ref(display), self.scale)[0])
    }

    /// Device units per canvas unit at `p`: on the display there, the
    /// primary display's off every display
    pub fn scale_at(&self, p: Point) -> f64 {
        self.desktop
            .display_at(p)
            .map_or(self.scale, |display| self.frame(display).scale)
    }

    /// A device position on `display` on the canvas
    pub fn to_canvas(&self, display: &Rect, x: f64, y: f64) -> (f64, f64) {
        let frame = self.frame(display);
        (
            f64::from(self.origin.x) + frame.left + (x - f64::from(display.x)) / frame.scale,
            f64::from(self.origin.y) + frame.top + (y - f64::from(display.y)) / frame.scale,
        )
    }

    /// A canvas position in the device's coordinates, as `display` maps it
    pub fn from_canvas(&self, display: &Rect, x: f64, y: f64) -> (f64, f64) {
        let frame = self.frame(display);
        (
            f64::from(display.x) + (x - f64::from(self.origin.x) - frame.left) * frame.scale,
            f64::from(display.y) + (y - f64::from(self.origin.y) - frame.top) * frame.scale,
        )
    }

    /// A display's area on the canvas
    pub fn area(&self, display: &Rect) -> Area {
        let (left, top) = self.to_canvas(display, f64::from(display.x), f64::from(display.y));
        let (right, bottom) = self.to_canvas(
            display,
            f64::from(display.right()),
            f64::from(display.bottom()),
        );
        Area {
            left,
            top,
            right,
            bottom,
        }
    }

    /// The area all displays cover on the canvas
    pub fn bounds(&self) -> Option<Area> {
        self.desktop
            .displays()
            .iter()
            .map(|display| self.area(display))
            .reduce(|a, b| Area {
                left: a.left.min(b.left),
                top: a.top.min(b.top),
                right: a.right.max(b.right),
                bottom: a.bottom.max(b.bottom),
            })
    }
}

/// A usable scale: a non-positive one counts as 1
fn valid_scale(scale: f64) -> f64 {
    if scale > 0.0 { scale } else { 1.0 }
}

/// Which of `displays` is the primary one: the display holding the
/// device's (0, 0), else the first
pub fn primary(displays: &[Rect]) -> usize {
    displays
        .iter()
        .position(|display| display.contains(Point::new(0, 0)))
        .unwrap_or(0)
}

/// The frames of `displays` all at one `scale`: their device positions
/// scaled
fn uniform(displays: &[Rect], scale: f64) -> Vec<Frame> {
    displays
        .iter()
        .map(|display| Frame {
            left: f64::from(display.x) / scale,
            top: f64::from(display.y) / scale,
            scale,
        })
        .collect()
}

/// The frames of `displays` at their own `scales`: the primary display
/// where one scale would put it, then each display touching one placed
/// already beside it. `None` when some display touches none of them
fn place(displays: &[Rect], scales: &[f64]) -> Option<Vec<Frame>> {
    let first = primary(displays);
    let start = uniform(displays.get(first..=first)?, *scales.get(first)?).pop();
    let mut frames: Vec<Option<Frame>> = vec![None; displays.len()];
    frames[first] = start;
    let mut queue = std::collections::VecDeque::from([first]);
    while let Some(a) = queue.pop_front() {
        let Some(placed) = frames[a] else {
            continue;
        };
        for b in 0..displays.len() {
            if frames[b].is_none()
                && let Some(frame) = beside(&displays[a], placed, &displays[b], scales[b])
            {
                frames[b] = Some(frame);
                queue.push_back(b);
            }
        }
    }
    frames.into_iter().collect()
}

/// The frame of display `b`, at `scale`, where it shares a side with
/// display `a`, placed at `at`: flush against it, and as far along the
/// side as on the device, counted in `a`'s scale. `None` if they share no
/// side
fn beside(a: &Rect, at: Frame, b: &Rect, scale: f64) -> Option<Frame> {
    let along = |offset: i32| f64::from(offset) / at.scale;
    let rows = b.y < a.bottom() && a.y < b.bottom();
    let columns = b.x < a.right() && a.x < b.right();
    let (left, top) = if rows && b.x == a.right() {
        (at.left + along(a.width), at.top + along(b.y - a.y))
    } else if rows && b.right() == a.x {
        (
            at.left - f64::from(b.width) / scale,
            at.top + along(b.y - a.y),
        )
    } else if columns && b.y == a.bottom() {
        (at.left + along(b.x - a.x), at.top + along(a.height))
    } else if columns && b.bottom() == a.y {
        (
            at.left + along(b.x - a.x),
            at.top - f64::from(b.height) / scale,
        )
    } else {
        return None;
    };
    Some(Frame { left, top, scale })
}

/// Whether two of `displays`, placed at `frames`, overlap on the canvas
fn frames_overlap(displays: &[Rect], frames: &[Frame]) -> bool {
    let areas: Vec<Area> = displays
        .iter()
        .zip(frames)
        .map(|(display, frame)| Area {
            left: frame.left,
            top: frame.top,
            right: frame.left + f64::from(display.width) / frame.scale,
            bottom: frame.top + f64::from(display.height) / frame.scale,
        })
        .collect();
    areas
        .iter()
        .enumerate()
        .any(|(i, a)| areas[i + 1..].iter().any(|b| a.overlaps(b)))
}

/// Where two devices touch: the pointer crosses between their sides
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
    /// Span of the first device's side along the edge
    pub first_span: (f64, f64),
    /// Span of the second device's side
    pub second_span: (f64, f64),
}

/// The span `device` covers on the line `at` with its `side`: every display
/// whose side lies on that line, from the first to the last
fn span_on(device: &Device, side: Edge, at: f64) -> Option<(f64, f64)> {
    device
        .desktop
        .displays()
        .iter()
        .map(|d| device.area(d).side(side))
        .filter(|(line, _, _)| (line - at).abs() <= TOUCH)
        .map(|(_, lo, hi)| (lo, hi))
        .reduce(|(lo, hi), (a, b)| (lo.min(a), hi.max(b)))
}

/// Length two spans have in common (negative when apart)
fn overlap((lo, hi): (f64, f64), (a, b): (f64, f64)) -> f64 {
    hi.min(b) - lo.max(a)
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
    /// lands: on the devices facing that side, at the same share of their
    /// side as it left at, `inset` pixels inside. `None` at a wall. How
    /// close to a corner it may cross is the caller's to decide (see
    /// [`Self::corner_distance`])
    ///
    /// `at` must be on `from`'s outer boundary at `edge` (see
    /// [`Desktop::on_edge`]).
    pub fn cross(&self, from: &str, at: Point, edge: Edge, inset: i32) -> Option<(&Device, Point)> {
        let source = self.device(from)?;
        let display = source.desktop.display_at(at)?;
        let (line, _, _) = source.area(display).side(edge);
        // The pointer's pixel centre, projected on the canvas
        let (cx, cy) = source.to_canvas(display, f64::from(at.x) + 0.5, f64::from(at.y) + 0.5);
        let along = match edge {
            Edge::Left | Edge::Right => cy,
            Edge::Top | Edge::Bottom => cx,
        };
        let from_span = span_on(source, edge, line)?;
        let facing = edge.opposite();
        let targets: Vec<&Device> = self
            .devices
            .iter()
            .filter(|d| d.key != from)
            .filter(|d| {
                span_on(d, facing, line).is_some_and(|span| overlap(from_span, span) > TOUCH)
            })
            .collect();
        let to_span = targets
            .iter()
            .filter_map(|d| span_on(d, facing, line))
            .reduce(|(lo, hi), (a, b)| (lo.min(a), hi.max(b)))?;
        let share = ((along - from_span.0) / (from_span.1 - from_span.0)).clamp(0.0, 1.0);
        let mapped = to_span.0 + share * (to_span.1 - to_span.0);
        // The facing display closest to that spot (it falls in a gap when
        // several devices share the side)
        let (target, entry) = targets
            .iter()
            .flat_map(|d| d.desktop.displays().iter().map(move |e| (*d, e)))
            .filter_map(|(d, e)| {
                let (at, lo, hi) = d.area(e).side(facing);
                let miss = (lo - mapped).max(mapped - hi).max(0.0);
                ((at - line).abs() <= TOUCH).then_some((d, e, miss))
            })
            .min_by(|x, y| x.2.total_cmp(&y.2))
            .map(|(d, e, _)| (d, e))?;
        Some((target, landing(target, entry, edge, mapped, inset)))
    }

    /// How far `at`, on device `from`'s side at `edge`, is from the nearer
    /// end of its display's side, in canvas units: the corner guard keeps
    /// the pointer from crossing that close to a screen corner
    pub fn corner_distance(&self, from: &str, at: Point, edge: Edge) -> Option<f64> {
        let source = self.device(from)?;
        let display = source.desktop.display_at(at)?;
        let (_, lo, hi) = source.area(display).side(edge);
        let (cx, cy) = source.to_canvas(display, f64::from(at.x) + 0.5, f64::from(at.y) + 0.5);
        let along = match edge {
            Edge::Left | Edge::Right => cy,
            Edge::Top | Edge::Bottom => cx,
        };
        Some((along - lo).min(hi - along))
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

    /// Every place where two devices touch
    pub fn shared_edges(&self) -> Vec<SharedEdge> {
        let mut edges = Vec::new();
        for a in &self.devices {
            for b in self.devices.iter().filter(|b| b.key != a.key) {
                for edge in [Edge::Right, Edge::Bottom] {
                    // Each line a's displays end on, once
                    let mut lines: Vec<f64> = a
                        .desktop
                        .displays()
                        .iter()
                        .map(|d| a.area(d).side(edge).0)
                        .collect();
                    lines.sort_by(f64::total_cmp);
                    lines.dedup_by(|x, y| (*x - *y).abs() <= TOUCH);
                    for at in lines {
                        let (Some(first_span), Some(second_span)) =
                            (span_on(a, edge, at), span_on(b, edge.opposite(), at))
                        else {
                            continue;
                        };
                        if overlap(first_span, second_span) > TOUCH {
                            edges.push(SharedEdge {
                                first: a.key.clone(),
                                second: b.key.clone(),
                                edge,
                                at,
                                first_span,
                                second_span,
                            });
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
        Edge::Left | Edge::Right => target.from_canvas(display, 0.0, along),
        Edge::Top | Edge::Bottom => target.from_canvas(display, along, 0.0),
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

    /// The pointer enters at the same share of the side, in the target's
    /// own (scaled) coordinates, and comes back where it left
    #[test]
    fn crosses_proportionally() {
        let world = mac_and_pc();
        let (pc, at) = world
            .cross("mac", Point::new(2559, 600), Edge::Right, 1)
            .unwrap();
        assert_eq!(pc.key, "pc");
        // 600.5 of 1440 → 41.7% of the PC's 1080 logical (1620 physical)
        assert_eq!(at, Point::new(1, 675));
        let (mac, back) = world
            .cross("pc", Point::new(0, 675), Edge::Left, 1)
            .unwrap();
        assert_eq!(mac.key, "mac");
        assert_eq!(back, Point::new(2558, 600));
        // The bottom of the tall side reaches the bottom of the short one
        let (_, low) = world
            .cross("mac", Point::new(2559, 1430), Edge::Right, 1)
            .unwrap();
        assert_eq!(low, Point::new(1, 1609));
    }

    /// Only a side nobody faces is a wall; corners cross too, and how
    /// close to one the pointer is decides the corner guard
    #[test]
    fn walls_and_corners() {
        let world = mac_and_pc();
        // Above the PC's top on the canvas, still part of the Mac's side
        assert!(
            world
                .cross("mac", Point::new(2559, 50), Edge::Right, 1)
                .is_some()
        );
        // Other sides: nothing there
        assert!(
            world
                .cross("mac", Point::new(0, 600), Edge::Left, 1)
                .is_none()
        );
        assert!(
            world
                .cross("pc", Point::new(500, 1619), Edge::Bottom, 1)
                .is_none()
        );
        // Near both ends of a display side (canvas units: the PC's pixels
        // are scaled)
        assert!(
            world
                .cross("mac", Point::new(2559, 1435), Edge::Right, 1)
                .is_some()
        );
        let near = world.corner_distance("mac", Point::new(2559, 1435), Edge::Right);
        assert_eq!(near, Some(4.5));
        let top = world.corner_distance("pc", Point::new(0, 5), Edge::Left);
        assert!(top.is_some_and(|d| d < 8.0), "{top:?}");
        let lower = world.corner_distance("pc", Point::new(0, 20), Edge::Left);
        assert!(lower.is_some_and(|d| d > 8.0), "{lower:?}");
        // Devices on the same line that do not overlap are not neighbours
        let apart = World::new([
            single("a", (0, 0), 1000, 1000, 1.0),
            single("b", (1000, 1000), 1000, 1000, 1.0),
        ]);
        assert!(
            apart
                .cross("a", Point::new(999, 990), Edge::Right, 1)
                .is_none()
        );
    }

    /// Stacked displays of one device make one side
    #[test]
    fn stacked_displays_form_one_side() {
        let mac = Device::new(
            "mac",
            Desktop::new([Rect::new(0, 0, 1000, 500), Rect::new(0, 500, 1000, 500)]),
            Point::new(0, 0),
            1.0,
        );
        let world = World::new([mac, single("pc", (1000, 0), 1000, 250, 1.0)]);
        let (_, at) = world
            .cross("mac", Point::new(999, 750), Edge::Right, 1)
            .unwrap();
        assert_eq!(at, Point::new(1, 187));
        let (_, back) = world
            .cross("pc", Point::new(0, 187), Edge::Left, 1)
            .unwrap();
        assert_eq!(back, Point::new(998, 750));
    }

    /// Several devices along one side share it as one stretch
    #[test]
    fn devices_sharing_a_side() {
        let world = World::new([
            single("tall", (0, 0), 1000, 2000, 1.0),
            single("upper", (1000, 0), 800, 1000, 1.0),
            single("lower", (1000, 1000), 800, 1000, 1.0),
        ]);
        let (upper, _) = world
            .cross("tall", Point::new(999, 500), Edge::Right, 1)
            .unwrap();
        let (lower, at) = world
            .cross("tall", Point::new(999, 1500), Edge::Right, 1)
            .unwrap();
        assert_eq!((upper.key.as_str(), lower.key.as_str()), ("upper", "lower"));
        assert_eq!(at, Point::new(1, 500));
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
        assert_eq!((edge.edge, edge.at), (Edge::Right, 2560.0));
        assert_eq!(
            (edge.first_span, edge.second_span),
            ((0.0, 1440.0), (100.0, 1180.0))
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

    /// A Windows laptop at 200% (2880x1800 physical, its primary display)
    /// with a 100% display (1920x1080) at `external`
    fn laptop_with(external: Rect) -> Device {
        Device::scaled(
            "pc",
            [(external, 1.0), (Rect::new(0, 0, 2880, 1800), 2.0)],
            Point::new(0, 0),
        )
    }

    /// The canvas area as (left, top, right, bottom)
    fn edges(area: Area) -> (f64, f64, f64, f64) {
        (area.left, area.top, area.right, area.bottom)
    }

    /// Displays of their own scales keep their logical sizes and still
    /// touch, wherever they sit around the primary one
    #[test]
    fn displays_keep_their_own_scales() {
        let laptop = Rect::new(0, 0, 2880, 1800);
        // Right of the laptop, 360 physical pixels lower: a fifth of its side
        let right = Rect::new(2880, 360, 1920, 1080);
        let pc = laptop_with(right);
        assert_eq!(pc.scale, 2.0);
        assert_eq!(edges(pc.area(&laptop)), (0.0, 0.0, 1440.0, 900.0));
        assert_eq!(edges(pc.area(&right)), (1440.0, 180.0, 3360.0, 1260.0));
        assert_eq!(edges(pc.bounds().unwrap()), (0.0, 0.0, 3360.0, 1260.0));

        let left = Rect::new(-1920, 900, 1920, 1080);
        let pc = laptop_with(left);
        assert_eq!(edges(pc.area(&left)), (-1920.0, 450.0, 0.0, 1530.0));
        let above = Rect::new(480, -1080, 1920, 1080);
        let pc = laptop_with(above);
        assert_eq!(edges(pc.area(&above)), (240.0, -1080.0, 2160.0, 0.0));

        // Positions go to the canvas and back on either display
        let pc = laptop_with(right);
        for (display, x, y) in [(laptop, 100.5, 1700.5), (right, 4000.5, 400.5)] {
            let (cx, cy) = pc.to_canvas(&display, x, y);
            assert_eq!(pc.from_canvas(&display, cx, cy), (x, y));
        }
        assert_eq!(pc.scale_at(Point::new(10, 10)), 2.0);
        assert_eq!(pc.scale_at(Point::new(3000, 400)), 1.0);
        assert_eq!(pc.scale_at(Point::new(-5, -5)), 2.0);
    }

    /// Displays of one scale go where one scale puts them, and so do
    /// displays that would overlap once each took its own size
    #[test]
    fn one_scale_where_own_scales_cannot_be() {
        let displays = [
            Rect::new(0, 0, 2880, 1620),
            Rect::new(2880, 0, 1920, 1080),
            Rect::new(0, 1620, 1920, 1080),
        ];
        let areas = |device: &Device| -> Vec<(f64, f64, f64, f64)> {
            displays.iter().map(|d| edges(device.area(d))).collect()
        };
        let one = Device::new("pc", Desktop::new(displays), Point::new(10, 20), 1.5);
        let own = Device::scaled("pc", displays.map(|d| (d, 1.5)), Point::new(10, 20));
        assert_eq!(areas(&own), areas(&one));

        // Below the primary, a display as wide as both at 100% would run
        // into the one at its right
        let clashing = [
            Rect::new(0, 0, 2000, 1000),
            Rect::new(2000, 0, 1000, 1000),
            Rect::new(0, 1000, 3000, 1000),
        ];
        let one = Device::new("pc", Desktop::new(clashing), Point::new(0, 0), 2.0);
        let own = Device::scaled(
            "pc",
            [(clashing[0], 2.0), (clashing[1], 1.0), (clashing[2], 1.0)],
            Point::new(0, 0),
        );
        let areas = |device: &Device| -> Vec<(f64, f64, f64, f64)> {
            clashing.iter().map(|d| edges(device.area(d))).collect()
        };
        assert_eq!(areas(&own), areas(&one));
    }

    /// The pointer crosses into a display of its own scale where it faces
    /// on the canvas, and comes back where it left
    #[test]
    fn crosses_into_a_display_of_its_own_scale() {
        let external = Rect::new(2880, 0, 1920, 1080);
        let world = World::new([
            laptop_with(external),
            single("mac", (3360, 0), 1440, 900, 1.0),
        ]);
        // Half way down the Mac's 900 points, half way down the external
        // display's 1080 pixels
        let (pc, at) = world
            .cross("mac", Point::new(0, 450), Edge::Left, 1)
            .unwrap();
        assert_eq!(pc.key, "pc");
        assert_eq!(at, Point::new(4798, 540));
        let (mac, back) = world
            .cross("pc", Point::new(4799, 540), Edge::Right, 1)
            .unwrap();
        assert_eq!(mac.key, "mac");
        assert_eq!(back, Point::new(1, 450));
    }
}
