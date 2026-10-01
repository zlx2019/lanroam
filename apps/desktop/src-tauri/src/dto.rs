//! What the frontend sees: plain serializable views of the engine's state.
//!
//! Field names go out in camelCase; the TypeScript side mirrors every type
//! here in `src/types.ts` (**a change must land on both sides**).

use std::collections::HashSet;

use lanroam_core::engine::{ControlEvent, EngineError, InputStatus};
use lanroam_core::group::join::JoinError;
use lanroam_core::group::{ClipboardShare, FileShare, GroupDoc};
use lanroam_core::lan_kit::{Peer, PeerInfo};
use lanroam_core::lanroam_input::Rect;
use lanroam_core::lanroam_input::config::EdgeSettings;
use lanroam_core::lanroam_input::world::Area;
use lanroam_core::layout;
use lanroam_core::protocol::{PROP_GROUP, join_denied};
use lanroam_core::transport::TransportError;
use serde::Serialize;

/// Everything the main window shows, sent whole on every change
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// This device
    pub device: SelfDto,
    /// The desk group; `None` outside one
    pub group: Option<GroupDto>,
    /// Who controls what
    pub control: ControlDto,
    /// Whether capture and injection run
    pub input: InputDto,
}

/// This device
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfDto {
    /// Certificate fingerprint
    pub fingerprint: String,
    /// Display name
    pub name: String,
    /// `macos`, `windows`, ...
    pub platform: String,
    /// App version
    pub version: String,
}

impl SelfDto {
    /// From this device's info
    pub fn new(info: &PeerInfo, version: &str) -> Self {
        Self {
            fingerprint: info.fingerprint.clone(),
            name: info.name.clone(),
            platform: info.platform.clone(),
            version: version.to_string(),
        }
    }
}

/// The desk group
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupDto {
    /// Group ID
    pub id: String,
    /// Members: placed ones in reading order, then the rest by name
    pub devices: Vec<DeviceDto>,
    /// Edges between members with settings of their own
    pub edges: Vec<EdgeDto>,
}

/// The settings of the edge between two members
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeDto {
    /// One member's fingerprint
    pub a: String,
    /// The other's
    pub b: String,
    /// The settings
    #[serde(flatten)]
    pub settings: EdgeSettings,
}

/// A member of the group
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDto {
    /// Certificate fingerprint
    pub fingerprint: String,
    /// Display name
    pub name: String,
    /// `macos`, `windows`, ...
    pub platform: String,
    /// Linked right now (always true for this device)
    pub online: bool,
    /// This device
    pub local: bool,
    /// Number of the Ctrl+Alt+n hotkey; `None` until placed
    pub number: Option<usize>,
    /// Number of displays
    pub displays: usize,
    /// Primary display size in the device's own units, e.g. `1920×1080`
    pub resolution: String,
    /// Display scale in percent
    pub scale: u32,
    /// Command and Control are swapped for input from the other platform
    pub swap: bool,
    /// Pointer speed while controlled, in percent
    pub pointer_speed: u32,
    /// What of its clipboard it shares
    pub clipboard: ClipboardShare,
    /// What it does with files from the group
    pub files: FileShare,
    /// Where the device sits on the layout canvas (logical pixels): the
    /// bounds of its displays
    pub rect: Option<RectDto>,
    /// Its origin on the canvas, which placing moves (the bounds start
    /// elsewhere when a display lies left of or above the primary one)
    pub origin: Option<PointDto>,
    /// Its displays on the canvas
    pub screens: Vec<RectDto>,
}

/// A point on the layout canvas
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PointDto {
    /// Horizontal
    pub x: i32,
    /// Vertical
    pub y: i32,
}

/// A rectangle on the layout canvas
#[derive(Debug, Clone, Copy, Serialize)]
pub struct RectDto {
    /// Left
    pub x: f64,
    /// Top
    pub y: f64,
    /// Width
    pub w: f64,
    /// Height
    pub h: f64,
}

impl From<Area> for RectDto {
    fn from(area: Area) -> Self {
        Self {
            x: area.left,
            y: area.top,
            w: area.right - area.left,
            h: area.bottom - area.top,
        }
    }
}

impl GroupDto {
    /// The group as seen from `own`, with the members linked right now
    pub fn new(doc: &GroupDoc, own: &str, online: &HashSet<String>) -> Self {
        let world = layout::world(doc);
        let numbered = layout::numbered(doc);
        let mut devices: Vec<DeviceDto> = doc
            .members()
            .map(|(fp, record)| {
                let profile = &record.profile;
                let placed = world.device(fp);
                let primary = profile
                    .displays
                    .iter()
                    .find(|d| d.x == 0 && d.y == 0)
                    .or_else(|| profile.displays.first());
                DeviceDto {
                    fingerprint: fp.to_string(),
                    name: profile.name.clone(),
                    platform: profile.platform.clone(),
                    online: fp == own || online.contains(fp),
                    local: fp == own,
                    number: numbered.iter().position(|n| n == fp).map(|i| i + 1),
                    displays: profile.displays.len(),
                    resolution: primary.map(resolution).unwrap_or_default(),
                    scale: profile.scale,
                    swap: profile.swap_cmd_ctrl,
                    pointer_speed: profile.pointer_speed,
                    clipboard: profile.clipboard,
                    files: profile.files,
                    rect: placed.and_then(|d| d.bounds()).map(RectDto::from),
                    origin: record.placement.as_ref().map(|p| PointDto {
                        x: p.at.x,
                        y: p.at.y,
                    }),
                    screens: placed
                        .map(|d| {
                            d.desktop
                                .displays()
                                .iter()
                                .map(|r| d.area(r).into())
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            })
            .collect();
        devices.sort_by(|a, b| match (a.number, b.number) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        });
        let edges = doc
            .edge_settings()
            .map(|((a, b), settings)| EdgeDto { a, b, settings })
            .collect();
        Self {
            id: doc.id.clone(),
            devices,
            edges,
        }
    }
}

/// `1920×1080`
fn resolution(display: &Rect) -> String {
    format!("{}×{}", display.width, display.height)
}

/// A device on the LAN that is not in this device's group
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NearbyDto {
    /// Certificate fingerprint
    pub fingerprint: String,
    /// Display name
    pub name: String,
    /// `macos`, `windows`, ...
    pub platform: String,
    /// An address it was seen at
    pub address: Option<String>,
    /// The ID of the group it is in, if any
    pub group: Option<String>,
}

impl From<&Peer> for NearbyDto {
    fn from(peer: &Peer) -> Self {
        Self {
            fingerprint: peer.info.fingerprint.clone(),
            name: peer.info.name.clone(),
            platform: peer.info.platform.clone(),
            address: peer.addrs.first().map(ToString::to_string),
            group: peer.info.props.get(PROP_GROUP).cloned(),
        }
    }
}

/// Who controls what
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlDto {
    /// Where input goes right now
    pub mode: ControlMode,
    /// The other device concerned (controlled or controlling): its name
    pub peer: Option<String>,
    /// Its fingerprint
    pub peer_fingerprint: Option<String>,
    /// Crossing edges is paused
    pub paused: bool,
    /// The pointer stays on its device
    pub locked: bool,
    /// The device controlling this one locked the pointer to it
    pub peer_locked: bool,
    /// Where the pointer is, as far as this device can tell: another
    /// device's fingerprint, `None` for this one
    pub pointer: Option<String>,
}

/// Where input goes right now
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlMode {
    /// This device's keyboard and mouse drive this device
    #[default]
    Idle,
    /// They drive another device
    Controlling,
    /// Another device drives this one
    Controlled,
}

impl ControlDto {
    /// Follow one event; whether anything changed
    pub fn apply(&mut self, event: &ControlEvent) -> bool {
        let before = self.clone();
        match event {
            ControlEvent::Controlling { name, fingerprint } => {
                self.mode = ControlMode::Controlling;
                self.peer = Some(name.clone());
                self.peer_fingerprint = Some(fingerprint.clone());
                self.peer_locked = false;
                self.pointer = Some(fingerprint.clone());
            }
            ControlEvent::ControlledBy {
                name, fingerprint, ..
            } => {
                self.mode = ControlMode::Controlled;
                self.peer = Some(name.clone());
                self.peer_fingerprint = Some(fingerprint.clone());
                self.peer_locked = false;
                self.pointer = None;
            }
            // The controller let go: the pointer went back to it (or on to
            // another device it drives, which only it knows)
            ControlEvent::Freed { fingerprint, .. } => {
                self.release();
                self.pointer = Some(fingerprint.clone());
            }
            // Control is back here, or about to come back at the next input
            ControlEvent::Home { .. }
            | ControlEvent::TookBack { .. }
            | ControlEvent::LetGo { .. }
            | ControlEvent::Unresponsive { .. }
            | ControlEvent::Lost { .. } => {
                self.release();
                self.pointer = None;
            }
            ControlEvent::Paused { on } => self.paused = *on,
            ControlEvent::Locked { on } => self.locked = *on,
            ControlEvent::LockedHere { on, .. } => self.peer_locked = *on,
            ControlEvent::Unavailable { .. } => {}
        }
        *self != before
    }

    /// Nothing controls anything any more: this device's input is its own
    fn release(&mut self) {
        self.mode = ControlMode::Idle;
        self.peer = None;
        self.peer_fingerprint = None;
        self.peer_locked = false;
    }
}

/// Whether capture and injection run: `None`, or why not
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDto {
    /// Why capturing local input does not run
    pub capture: Option<String>,
    /// Why injecting input from other devices does not run
    pub injection: Option<String>,
}

impl From<InputStatus> for InputDto {
    fn from(status: InputStatus) -> Self {
        Self {
            capture: status.capture.err(),
            injection: status.injection.err(),
        }
    }
}

/// The operating system's input permissions
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsDto {
    /// This platform has such permissions (macOS)
    pub required: bool,
    /// Accessibility
    pub accessibility: bool,
    /// Input Monitoring
    pub input_monitoring: bool,
}

/// A device asking to join through this one, with the PIN to show it
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinPromptDto {
    /// Its name
    pub name: String,
    /// Its platform
    pub platform: String,
    /// The address it came from
    pub address: Option<String>,
    /// The PIN
    pub pin: String,
    /// Attempts it has left, the current one included
    pub attempts_left: u32,
}

/// A join this device started, waiting for the PIN
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinStartDto {
    /// The device showing the PIN
    pub sponsor: String,
    /// Attempts left
    pub attempts_left: u32,
}

/// The outcome of one PIN typed
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinAnswerDto {
    /// In the group now
    pub joined: bool,
    /// Attempts left when the PIN was wrong
    pub attempts_left: u32,
}

/// The sponsor ended a join while its PIN was being typed
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoiningEndedDto {
    /// Why (a join denial code such as `rejected`), if it said
    pub reason: Option<String>,
}

/// A join this device sponsored is over
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinEndedDto {
    /// The device that asked
    pub name: String,
    /// It got in
    pub admitted: bool,
}

/// A failed command: a stable code for the frontend's own wording, and the
/// engine's message as a fallback
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    /// Machine-readable reason
    pub code: String,
    /// Human-readable detail
    pub message: String,
}

impl CommandError {
    /// An error with this code and message
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl From<EngineError> for CommandError {
    fn from(e: EngineError) -> Self {
        let message = e.to_string();
        let code = match &e {
            EngineError::Join(JoinError::Denied(code)) => code.as_str(),
            EngineError::Join(JoinError::PinUsedUp) => join_denied::WRONG_PIN,
            EngineError::Join(JoinError::Timeout(_)) => join_denied::TIMEOUT,
            // The device showing the PIN could not prove it knows it
            EngineError::Join(JoinError::Unverified) => "unverified",
            EngineError::Join(JoinError::Transport(_))
            | EngineError::Transport(TransportError::Unreachable) => "unreachable",
            EngineError::Transport(_) => "connection",
            EngineError::Grouped => "grouped",
            EngineError::JoinInProgress => "join_in_progress",
            EngineError::NoGroup => "no_group",
            EngineError::NotAMember(_) => "not_a_member",
            EngineError::InvalidName => "invalid_name",
            EngineError::InvalidSettings => "invalid_settings",
            EngineError::Layout(_) => "layout",
            EngineError::Stopped => "stopped",
            _ => "internal",
        };
        Self::new(code, message)
    }
}

impl From<anyhow::Error> for CommandError {
    fn from(e: anyhow::Error) -> Self {
        Self::new("internal", format!("{e:#}"))
    }
}

#[cfg(test)]
mod tests {
    use lanroam_core::lanroam_input::Point;

    use super::*;

    #[test]
    fn pointer_goes_back_to_the_controller() {
        let mut control = ControlDto::default();
        control.apply(&ControlEvent::ControlledBy {
            name: "Mac".into(),
            fingerprint: "mac".into(),
            at: Point::new(0, 0),
        });
        assert_eq!(control.pointer, None);

        assert!(control.apply(&ControlEvent::Freed {
            name: "Mac".into(),
            fingerprint: "mac".into(),
        }));
        assert_eq!(control.mode, ControlMode::Idle);
        assert_eq!(control.pointer.as_deref(), Some("mac"));

        // Local input here takes it: the pointer is on this device again
        control.apply(&ControlEvent::ControlledBy {
            name: "Mac".into(),
            fingerprint: "mac".into(),
            at: Point::new(0, 0),
        });
        control.apply(&ControlEvent::TookBack {
            name: "Mac".into(),
            fingerprint: "mac".into(),
        });
        assert_eq!(control.pointer, None);
    }

    #[test]
    fn pointer_follows_this_device_out_and_home() {
        let mut control = ControlDto::default();
        control.apply(&ControlEvent::Controlling {
            name: "PC".into(),
            fingerprint: "pc".into(),
        });
        assert_eq!(control.pointer.as_deref(), Some("pc"));

        control.apply(&ControlEvent::Home {
            at: Point::new(0, 0),
            jumped: false,
        });
        assert_eq!(control.mode, ControlMode::Idle);
        assert_eq!(control.pointer, None);
    }
}
