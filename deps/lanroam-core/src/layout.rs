//! Layout: where each member's desktop sits on the shared canvas.
//!
//! Placements live in the group document (see [`crate::group`]); this
//! module turns the document into a [`World`] and works out placements:
//! where a newcomer goes, and where a device goes to sit beside another.
//! Placements are integer logical pixels; a device's origin is its primary
//! display's top-left corner.

use lanroam_input::world::{Device, World};
use lanroam_input::{Desktop, Edge, Point};
use thiserror::Error;

use crate::group::{DeviceRecord, GroupDoc};

/// Layout errors
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LayoutError {
    /// The device is not a current member
    #[error("{0} is not a member of the desk group")]
    NotAMember(String),
    /// The device has not reported its displays yet
    #[error("{0} has not reported its displays yet (is it online?)")]
    NoDisplays(String),
    /// The anchor has no place on the canvas yet
    #[error("{0} has no place in the layout yet")]
    Unplaced(String),
    /// The spot would put the device on top of another
    #[error("{0} would overlap {1}")]
    Overlap(String, String),
}

/// A member's device on the canvas with its origin at `at`
fn device_at(fingerprint: &str, record: &DeviceRecord, at: Point) -> Device {
    Device::new(
        fingerprint,
        Desktop::new(record.profile.displays.iter().copied()),
        at,
        f64::from(record.profile.scale) / 100.0,
    )
}

/// The canvas device of a member, if it is placed
fn device(fingerprint: &str, record: &DeviceRecord) -> Option<Device> {
    let placement = record.placement.as_ref()?;
    Some(device_at(fingerprint, record, placement.at))
}

/// The world of the current members that are placed and report displays
pub fn world(doc: &GroupDoc) -> World {
    World::new(doc.members().filter_map(|(fp, record)| device(fp, record)))
}

/// Placed members in reading order (left to right, then top to bottom),
/// online or not: device n of the Ctrl+Alt+n hotkeys is entry n - 1
pub fn numbered(doc: &GroupDoc) -> Vec<String> {
    world(doc).ordered().iter().map(|d| d.key.clone()).collect()
}

/// Where a newcomer goes: right of every placed device, top-aligned with
/// the device `beside` (the origin for the first one)
pub fn newcomer_spot(doc: &GroupDoc, beside: &str) -> Point {
    let world = world(doc);
    let right = world
        .devices()
        .iter()
        .filter_map(Device::bounds)
        .map(|area| area.right.ceil() as i32)
        .max();
    let Some(right) = right else {
        return Point::new(0, 0);
    };
    let top = world
        .device(beside)
        .and_then(Device::bounds)
        .map_or(0, |area| area.top.round() as i32);
    Point::new(right, top)
}

/// Where device `fingerprint` goes to sit on `side` of `anchor`, shifted
/// `offset` logical pixels along the shared edge (right for above / below,
/// down for left / right), with the edges aligned at the start
pub fn spot_beside(
    doc: &GroupDoc,
    fingerprint: &str,
    side: Edge,
    anchor: &str,
    offset: i32,
) -> Result<Point, LayoutError> {
    let record = member(doc, fingerprint)?;
    let anchor_record = member(doc, anchor)?;
    let bounds = Desktop::new(record.profile.displays.iter().copied())
        .bounds()
        .ok_or_else(|| LayoutError::NoDisplays(record.profile.name.clone()))?;
    let anchor_area = device(anchor, anchor_record)
        .and_then(|d| d.bounds())
        .ok_or_else(|| LayoutError::Unplaced(anchor_record.profile.name.clone()))?;

    // The device's block on the canvas: its size, and where it starts
    // relative to its origin
    let scale = f64::from(record.profile.scale) / 100.0;
    let (width, height) = (
        f64::from(bounds.width) / scale,
        f64::from(bounds.height) / scale,
    );
    let (dx, dy) = (f64::from(bounds.x) / scale, f64::from(bounds.y) / scale);
    let offset = f64::from(offset);
    let (left, top) = match side {
        Edge::Right => (anchor_area.right, anchor_area.top + offset),
        Edge::Left => (anchor_area.left - width, anchor_area.top + offset),
        Edge::Bottom => (anchor_area.left + offset, anchor_area.bottom),
        Edge::Top => (anchor_area.left + offset, anchor_area.top - height),
    };
    Ok(Point::new(
        (left - dx).round() as i32,
        (top - dy).round() as i32,
    ))
}

/// Check that device `fingerprint` at `spot` overlaps no other device
pub fn check(doc: &GroupDoc, fingerprint: &str, spot: Point) -> Result<(), LayoutError> {
    let record = member(doc, fingerprint)?;
    let mover = device_at(fingerprint, record, spot);
    // Pair by pair: an overlap elsewhere is not this spot's fault
    for (fp, other) in doc.members().filter(|(fp, _)| *fp != fingerprint) {
        let Some(other_device) = device(fp, other) else {
            continue;
        };
        if World::new([mover.clone(), other_device])
            .overlapping()
            .is_some()
        {
            return Err(LayoutError::Overlap(
                record.profile.name.clone(),
                other.profile.name.clone(),
            ));
        }
    }
    Ok(())
}

/// The record of a current member
fn member<'a>(doc: &'a GroupDoc, fingerprint: &str) -> Result<&'a DeviceRecord, LayoutError> {
    doc.devices
        .get(fingerprint)
        .filter(|record| !record.removed)
        .ok_or_else(|| LayoutError::NotAMember(fingerprint.chars().take(12).collect()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use lan_kit::PeerInfo;
    use lanroam_input::Rect;

    use super::*;
    use crate::group::Profile;

    /// A group of `names`, each with one display of the given size and
    /// scale; nobody placed yet
    fn group(devices: &[(&str, i32, i32, u32)]) -> GroupDoc {
        let info = |name: &str| PeerInfo {
            device_id: format!("id-{name}"),
            name: name.to_string(),
            fingerprint: name.to_string(),
            platform: "macos".to_string(),
            os_version: None,
            props: BTreeMap::new(),
        };
        let mut doc = GroupDoc::new(&info(devices[0].0));
        for &(name, width, height, scale) in devices {
            doc.admit(&info(name));
            let mut profile = Profile::new(&info(name));
            profile.displays = vec![Rect::new(0, 0, width, height)];
            profile.scale = scale;
            doc.update_profile(name, profile);
        }
        doc
    }

    /// Newcomers line up to the right, top-aligned with their sponsor
    #[test]
    fn newcomers_line_up() {
        let mut doc = group(&[
            ("mac", 2560, 1440, 100),
            ("pc", 2880, 1620, 150),
            ("tv", 1920, 1080, 100),
        ]);
        assert_eq!(newcomer_spot(&doc, "mac"), Point::new(0, 0));
        doc.place("mac", Point::new(0, 0), "mac");
        assert_eq!(newcomer_spot(&doc, "mac"), Point::new(2560, 0));
        doc.place("pc", Point::new(2560, 200), "mac");
        // Right of the PC's 1920 logical pixels, level with the PC
        assert_eq!(newcomer_spot(&doc, "pc"), Point::new(4480, 200));
        assert_eq!(world(&doc).devices().len(), 2);
        assert_eq!(numbered(&doc), ["mac", "pc"]);
    }

    /// Beside another device, on each side, with offsets and scales
    #[test]
    fn beside() {
        let mut doc = group(&[("mac", 2560, 1440, 100), ("pc", 2880, 1620, 150)]);
        doc.place("mac", Point::new(0, 0), "mac");
        let spot = |side, offset| spot_beside(&doc, "pc", side, "mac", offset).unwrap();
        assert_eq!(spot(Edge::Right, 0), Point::new(2560, 0));
        assert_eq!(spot(Edge::Right, 180), Point::new(2560, 180));
        assert_eq!(spot(Edge::Left, 0), Point::new(-1920, 0));
        assert_eq!(spot(Edge::Bottom, 320), Point::new(320, 1440));
        assert_eq!(spot(Edge::Top, 0), Point::new(0, -1080));

        doc.place("pc", spot(Edge::Right, 180), "mac");
        let edges = world(&doc).shared_edges();
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (edges[0].first_span, edges[0].second_span),
            ((0.0, 1440.0), (180.0, 1260.0))
        );
    }

    /// Spots are refused where devices would overlap, and errors name the
    /// problem
    #[test]
    fn checks() {
        let mut doc = group(&[("mac", 2560, 1440, 100), ("pc", 2880, 1620, 150)]);
        assert_eq!(
            spot_beside(&doc, "pc", Edge::Right, "mac", 0),
            Err(LayoutError::Unplaced("mac".into()))
        );
        doc.place("mac", Point::new(0, 0), "mac");
        assert_eq!(check(&doc, "pc", Point::new(2560, 0)), Ok(()));
        assert_eq!(
            check(&doc, "pc", Point::new(2000, 0)),
            Err(LayoutError::Overlap("pc".into(), "mac".into()))
        );
        doc.devices.get_mut("pc").unwrap().profile.displays.clear();
        assert_eq!(
            spot_beside(&doc, "pc", Edge::Right, "mac", 0),
            Err(LayoutError::NoDisplays("pc".into()))
        );
        assert!(matches!(
            spot_beside(&doc, "ghost", Edge::Right, "mac", 0),
            Err(LayoutError::NotAMember(_))
        ));
    }
}
