//! Wire protocol between Lanroam nodes.
//!
//! One QUIC connection per pair of nodes, carrying:
//! - **Control stream**: the first bidirectional stream, opened by the
//!   dialer. Length-prefixed JSON [`Control`] messages, reliable and
//!   ordered. It opens with the Hello gate:
//!   `Hello → HelloAck | Rejected`. Keys, buttons and scrolling travel here
//!   too: none of them may be lost or reordered
//! - **Datagrams**: unreliable and unordered, for high-frequency data where
//!   only the latest value matters (pointer motion). A compact binary
//!   [`Datagram`] codec; the first byte is the kind
//! - **Content streams**: a bidirectional stream per transfer, opened by
//!   the side that wants the content, so a large one never holds up input.
//!   It opens with a [`StreamRequest`] frame
//!
//! Hello declares what the connection is for ([`Purpose`]): a desk group
//! member's link, a join through a PIN (see [`crate::group::join`]), or
//! diagnostics. Members exchange their group documents with
//! [`Control::Group`] right after the gate.
//!
//! Input between members: the controlling side takes and gives back
//! control with [`Control::Enter`] / [`Control::Leave`] and streams input in
//! the controlled device's coordinates (pointer motion as
//! [`Datagram::Motion`]). The controlled side may end it with
//! [`Control::Released`]. Displays travel in the group document.
//!
//! Clipboards follow the pointer: a member offers its clipboard with
//! [`Control::ClipOffer`] (a summary), and the receiver fetches the content
//! over a content stream ([`StreamRequest::Clipboard`], answered with a
//! [`ClipReply`] and the bytes) unless it holds it already.
//!
//! Drags of files follow the pointer too: a drag held on the controlled
//! device reaches an edge, the controller asks what it drags
//! ([`Control::DragProbe`], answered with [`Control::DragFiles`]), and the
//! device the pointer takes it to drags the files on from there
//! ([`Control::DragEnter`], or [`Control::DragCancel`] to let go of them).
//! That device pulls the files from where they are over a content stream
//! ([`StreamRequest::Drag`], answered with a [`DragReply`] and the files
//! part by part, see [`DragPart`]), naming the drag by a token only the
//! devices it went through know. Since 2.6 it may ask for a listing of
//! every file first: a drop there may happen before they are all there,
//! and the app it lands on learns what is coming.
//!
//! The protocol version is `major.minor`; a different major refuses the
//! connection, a newer minor only adds messages an older peer may ignore.

use bytes::{BufMut, Bytes, BytesMut};
use lan_kit::PeerInfo;
use lan_kit::frame::Framing;
use lanroam_clipboard::Kind;
use lanroam_input::MouseButton;
use lanroam_input::switch::Request;
use serde::{Deserialize, Serialize};

use crate::group::GroupDoc;

/// Protocol version (major.minor), checked by the Hello gate
pub const PROTOCOL_VERSION: &str = "2.6";

/// ALPN of the QUIC connections; a client speaking anything else is refused
/// during the TLS handshake
pub const ALPN: &[u8] = b"lanroam/1";

/// Frame codec of the control stream (control messages are small; 64 KiB is
/// ample)
pub const FRAMING: Framing = Framing::new(64 * 1024);

/// Discovery prop advertising the protocol version, so a scan can flag an
/// incompatible node before dialing it
pub const PROP_PROTOCOL: &str = "pv";

/// Discovery prop advertising the desk group ID of a grouped node
pub const PROP_GROUP: &str = "g";

/// Structured rejection codes: the dialer renders them in its own language
pub mod reason_code {
    /// The identity declared in Hello does not match the TLS certificate
    pub const IDENTITY_MISMATCH: &str = "identity_mismatch";
    /// The protocol major versions differ
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    /// The first message was not a Hello
    pub const PROTOCOL_VIOLATION: &str = "protocol_violation";
    /// The dialer is not a member of the acceptor's desk group
    pub const NOT_A_MEMBER: &str = "not_a_member";
    /// The dialer was removed from the acceptor's desk group
    pub const REMOVED: &str = "removed";
}

/// Why a controlled device let go ([`Control::Released`])
pub mod released {
    /// Another device took control of it
    pub const PREEMPTED: &str = "preempted";
    /// Someone used its own keyboard or mouse
    pub const LOCAL_INPUT: &str = "local_input";
    /// It cannot inject input (no permission, unsupported platform)
    pub const UNAVAILABLE: &str = "unavailable";
}

/// What a controlled device asks its controller for ([`Control::Request`]):
/// a [`Request`] on the wire
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Ask {
    /// Come home and pause crossing, or resume
    Pause,
    /// Lock the pointer to its device, or unlock
    Lock,
    /// Move control to a device
    Jump {
        /// Its fingerprint
        device: String,
    },
}

impl From<Request> for Ask {
    fn from(request: Request) -> Self {
        match request {
            Request::Pause => Self::Pause,
            Request::Lock => Self::Lock,
            Request::Jump(device) => Self::Jump { device },
        }
    }
}

impl From<Ask> for Request {
    fn from(ask: Ask) -> Self {
        match ask {
            Ask::Pause => Self::Pause,
            Ask::Lock => Self::Lock,
            Ask::Jump { device } => Self::Jump(device),
        }
    }
}

/// Why a join was turned down ([`Control::JoinDenied`])
pub mod join_denied {
    /// Another join is in progress on the sponsor
    pub const BUSY: &str = "busy";
    /// The sponsor pauses joins after a failed one
    pub const COOLDOWN: &str = "cooldown";
    /// The PIN was wrong too many times
    pub const WRONG_PIN: &str = "wrong_pin";
    /// The joiner took too long to answer
    pub const TIMEOUT: &str = "timeout";
    /// The sponsor failed on its side
    pub const INTERNAL: &str = "internal";
    /// The user of the sponsor turned the join down
    pub const REJECTED: &str = "rejected";
}

/// What a connection is for, declared in Hello
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// A desk group member's link: group sync and input
    Member,
    /// Joining the acceptor's desk group with a PIN
    Join,
    /// Round-trip measurement only
    Diag,
}

/// Control stream message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    /// Opens the control stream (dialer → acceptor)
    Hello {
        /// Protocol version (major.minor)
        version: String,
        /// Dialer's device info
        info: PeerInfo,
        /// What the connection is for
        purpose: Purpose,
    },
    /// Accepts the Hello (acceptor → dialer)
    HelloAck {
        /// Protocol version (major.minor)
        version: String,
        /// Acceptor's device info
        info: PeerInfo,
    },
    /// Refuses the connection; the acceptor closes it right after
    Rejected {
        /// Structured reason (see [`reason_code`])
        reason_code: String,
    },
    /// Round-trip probe on the reliable path
    Ping {
        /// Sequence number
        seq: u32,
        /// Sender's clock in microseconds, echoed back untouched
        sent_us: u64,
    },
    /// Answer to [`Control::Ping`]
    Pong {
        /// Echoed sequence number
        seq: u32,
        /// Echoed sender clock
        sent_us: u64,
    },
    /// The sender takes control; the cursor goes to (`x`, `y`)
    Enter {
        /// Horizontal position (receiver's coordinates)
        x: i32,
        /// Vertical position
        y: i32,
    },
    /// The sender gives control back; the receiver releases every key and
    /// button the sender still holds
    Leave,
    /// Key press, autorepeat or release
    Key {
        /// USB HID usage (keyboard page)
        usage: u16,
        /// Pressed (true) or released
        down: bool,
    },
    /// Mouse button, at the cursor position it happened at (the latest
    /// motion datagram may not have arrived yet)
    Button {
        /// Which button
        button: MouseButton,
        /// Pressed (true) or released
        down: bool,
        /// Horizontal position (receiver's coordinates)
        x: i32,
        /// Vertical position
        y: i32,
    },
    /// Scrolling, in 1/120 of a notch
    Wheel {
        /// Horizontal amount; positive scrolls right
        dx: i32,
        /// Vertical amount; positive scrolls up
        dy: i32,
    },
    /// The sender no longer takes input from the receiver, which stops
    /// controlling it (see [`released`] for the reasons)
    Released {
        /// Why
        reason_code: String,
    },
    /// The sender's copy of the group document (members, after the gate and
    /// on every change)
    Group {
        /// The document
        doc: GroupDoc,
    },
    /// Show your number in the layout on your screens for a moment, so the
    /// user can tell the devices apart (to every online member; since 2.1)
    Identify,
    /// The controller locked the pointer to the receiver, or unlocked it,
    /// for the receiver to show where the user looks (since 2.1)
    PointerLocked {
        /// Locked
        on: bool,
    },
    /// The sender's tray or window asked for this while the receiver
    /// controls it: the user works the sender with the receiver's keyboard
    /// and mouse, so the receiver carries it out (since 2.1)
    Request {
        /// What
        request: Ask,
    },
    /// What the sender's clipboard holds, for the receiver to fetch over a
    /// content stream unless it holds the same (since 2.2)
    ClipOffer {
        /// Text or image
        kind: Kind,
        /// Bytes before encoding (text, or RGBA pixels)
        size: u64,
        /// The content's hash ([`lanroam_clipboard::Content::hash`])
        hash: String,
    },
    /// The sender's clipboard holds files copied there, for the receiver to
    /// fetch ahead of a paste unless it holds the same (since 2.5)
    ClipFiles {
        /// The copy's hash ([`lanroam_clipboard::Content::hash`])
        hash: String,
        /// The first file or folder copied
        name: String,
        /// How many were copied (top level)
        count: usize,
        /// Bytes in all
        bytes: u64,
        /// What pulls the files from the sender ([`StreamRequest::Drag`])
        token: String,
    },
    /// Whether the left button held on the receiver drags files: its drag
    /// reached an edge that lets the pointer through (controller →
    /// controlled; since 2.3)
    DragProbe {
        /// Names the press; the answer carries it back
        id: u64,
    },
    /// Answer to [`Control::DragProbe`]: the files dragged, none if the
    /// drag holds anything else
    DragFiles {
        /// The probe's id
        id: u64,
        /// What is dragged (top level)
        files: Vec<DragItem>,
        /// What pulls the files from the sender ([`StreamRequest::Drag`]);
        /// empty before 2.4
        #[serde(default)]
        token: String,
    },
    /// A drag of files comes here with the pointer (right after
    /// [`Control::Enter`]): drag them on from (`x`, `y`) until the button
    /// held goes up
    DragEnter {
        /// Names the drag
        id: u64,
        /// Fingerprint of the device the files are on
        origin: String,
        /// Horizontal position (receiver's coordinates)
        x: i32,
        /// Vertical position
        y: i32,
        /// What is dragged (top level)
        files: Vec<DragItem>,
        /// What pulls the files from `origin` ([`StreamRequest::Drag`]);
        /// empty before 2.4
        #[serde(default)]
        token: String,
    },
    /// Let go of the drag carried here without dropping anything (Esc)
    DragCancel {
        /// The drag's id
        id: u64,
    },
    /// The drop of a drag carried to the sender waits for its files, or not
    /// any more (controlled → controller): the pointer holds still there
    /// meanwhile (since 2.4)
    DropWaiting {
        /// Waiting
        on: bool,
    },
    /// One PIN attempt begins (sponsor → joiner): the sponsor's SPAKE2
    /// message
    JoinChallenge {
        /// SPAKE2 message
        #[serde(with = "hex::serde")]
        pake: Vec<u8>,
        /// Attempts left for this PIN, this one included
        attempts_left: u32,
    },
    /// The joiner's side of the attempt (joiner → sponsor)
    JoinAnswer {
        /// SPAKE2 message
        #[serde(with = "hex::serde")]
        pake: Vec<u8>,
        /// Proof that the joiner derived the same key
        #[serde(with = "hex::serde")]
        confirm: Vec<u8>,
    },
    /// The joiner is in (sponsor → joiner)
    JoinAccepted {
        /// Proof that the sponsor derived the same key
        #[serde(with = "hex::serde")]
        confirm: Vec<u8>,
        /// The group document, the joiner included
        doc: GroupDoc,
    },
    /// The join is over without success (sponsor → joiner)
    JoinDenied {
        /// Structured reason (see [`join_denied`])
        reason_code: String,
    },
}

impl Control {
    /// Short name of the message type (logs and errors)
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::HelloAck { .. } => "hello_ack",
            Self::Rejected { .. } => "rejected",
            Self::Ping { .. } => "ping",
            Self::Pong { .. } => "pong",
            Self::Enter { .. } => "enter",
            Self::Leave => "leave",
            Self::Key { .. } => "key",
            Self::Button { .. } => "button",
            Self::Wheel { .. } => "wheel",
            Self::Released { .. } => "released",
            Self::Group { .. } => "group",
            Self::Identify => "identify",
            Self::PointerLocked { .. } => "pointer_locked",
            Self::Request { .. } => "request",
            Self::ClipOffer { .. } => "clip_offer",
            Self::ClipFiles { .. } => "clip_files",
            Self::DragProbe { .. } => "drag_probe",
            Self::DragFiles { .. } => "drag_files",
            Self::DragEnter { .. } => "drag_enter",
            Self::DragCancel { .. } => "drag_cancel",
            Self::DropWaiting { .. } => "drop_waiting",
            Self::JoinChallenge { .. } => "join_challenge",
            Self::JoinAnswer { .. } => "join_answer",
            Self::JoinAccepted { .. } => "join_accepted",
            Self::JoinDenied { .. } => "join_denied",
        }
    }
}

/// A file or folder of a drag, as the device it goes to shows it (since
/// 2.3)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DragItem {
    /// File name
    pub name: String,
    /// Size in bytes (a folder: of everything in it)
    pub size: u64,
    /// A folder
    pub dir: bool,
}

/// First frame on a content stream: what the opener wants (since 2.2)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamRequest {
    /// The clipboard content offered with this hash
    Clipboard {
        /// Its hash, from the [`Control::ClipOffer`]
        hash: String,
    },
    /// The files of a drag (since 2.4), or of a copy (since 2.5)
    Drag {
        /// The token, from [`Control::DragFiles`], [`Control::DragEnter`]
        /// or [`Control::ClipFiles`]
        token: String,
        /// List every file and folder first ([`DragReply::Listing`], since
        /// 2.6); an older peer answers without
        #[serde(default)]
        listing: bool,
    },
}

/// Answer to [`StreamRequest::Clipboard`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipReply {
    /// Here it comes: `len` bytes follow, then the stream ends (text as
    /// UTF-8, an image as PNG)
    Content {
        /// How many bytes
        len: u64,
    },
    /// Not held any more, or not for the asker
    Gone,
}

/// Answer to [`StreamRequest::Drag`] (since 2.4)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DragReply {
    /// Here they come, as [`DragPart`]s up to [`DragPart::Done`]
    Files {
        /// How many files (folders not counted)
        count: u64,
        /// Their bytes in all
        bytes: u64,
    },
    /// Here they come, listed first (since 2.6, when asked): every folder
    /// and file as a [`DragPart::Dir`] or [`DragPart::File`] (no bytes
    /// follow) up to [`DragPart::Listed`], then as after
    /// [`DragReply::Files`]
    Listing {
        /// How many files (folders not counted)
        count: u64,
        /// Their bytes in all
        bytes: u64,
    },
    /// Not dragged any more, or never by this token
    Gone,
}

/// One part of the files of a drag, after [`DragReply::Files`]; paths are
/// relative, `/`-separated, starting with the name of what was dragged
/// (since 2.4)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DragPart {
    /// A folder (empty ones included)
    Dir {
        /// Its path
        path: String,
    },
    /// A file: `size` bytes follow, then its [`DragPart::Hash`]
    File {
        /// Its path
        path: String,
        /// How many bytes
        size: u64,
    },
    /// BLAKE3 of the file just sent, in hex
    Hash {
        /// The hash
        hash: String,
    },
    /// The listing is over; the files follow (since 2.6)
    Listed,
    /// Nothing more
    Done,
}

/// Whether a peer's protocol version is compatible (same major)
pub fn version_compatible(peer_version: &str) -> bool {
    major_of(peer_version) == major_of(PROTOCOL_VERSION)
}

/// Major segment of a version string
fn major_of(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

/// Datagram message
///
/// Layout: `kind: u8` followed by the kind's fields in big-endian. Unknown
/// kinds decode to `None` and are dropped, so a newer minor version can add
/// kinds without breaking older peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Datagram {
    /// Round-trip probe on the unreliable path
    Ping {
        /// Sequence number
        seq: u32,
        /// Sender's clock in microseconds, echoed back untouched
        sent_us: u64,
    },
    /// Answer to [`Datagram::Ping`]
    Pong {
        /// Echoed sequence number
        seq: u32,
        /// Echoed sender clock
        sent_us: u64,
    },
    /// The controlled cursor moved; a lower `seq` than the last one applied
    /// is stale and dropped
    Motion {
        /// Sequence number, increasing over the session
        seq: u32,
        /// Horizontal position (receiver's coordinates)
        x: i32,
        /// Vertical position
        y: i32,
    },
    /// Receipt of [`Datagram::Motion`], for measuring input latency
    MotionAck {
        /// Sequence number of the motion
        seq: u32,
    },
}

/// Datagram kind tags
mod kind {
    /// [`super::Datagram::Ping`]
    pub(super) const PING: u8 = 1;
    /// [`super::Datagram::Pong`]
    pub(super) const PONG: u8 = 2;
    /// [`super::Datagram::Motion`]
    pub(super) const MOTION: u8 = 3;
    /// [`super::Datagram::MotionAck`]
    pub(super) const MOTION_ACK: u8 = 4;
}

impl Datagram {
    /// Encode into a datagram payload
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(13);
        match *self {
            Self::Ping { seq, sent_us } => {
                buf.put_u8(kind::PING);
                buf.put_u32(seq);
                buf.put_u64(sent_us);
            }
            Self::Pong { seq, sent_us } => {
                buf.put_u8(kind::PONG);
                buf.put_u32(seq);
                buf.put_u64(sent_us);
            }
            Self::Motion { seq, x, y } => {
                buf.put_u8(kind::MOTION);
                buf.put_u32(seq);
                buf.put_i32(x);
                buf.put_i32(y);
            }
            Self::MotionAck { seq } => {
                buf.put_u8(kind::MOTION_ACK);
                buf.put_u32(seq);
            }
        }
        buf.freeze()
    }

    /// Decode a datagram payload; `None` for unknown kinds or bad lengths
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let (&kind, rest) = buf.split_first()?;
        match kind {
            kind::PING | kind::PONG => {
                let rest: &[u8; 12] = rest.try_into().ok()?;
                let (seq, sent_us) = rest.split_at(4);
                let seq = u32::from_be_bytes(seq.try_into().ok()?);
                let sent_us = u64::from_be_bytes(sent_us.try_into().ok()?);
                Some(if kind == kind::PING {
                    Self::Ping { seq, sent_us }
                } else {
                    Self::Pong { seq, sent_us }
                })
            }
            kind::MOTION => {
                let rest: &[u8; 12] = rest.try_into().ok()?;
                let [s0, s1, s2, s3, x0, x1, x2, x3, y0, y1, y2, y3] = *rest;
                Some(Self::Motion {
                    seq: u32::from_be_bytes([s0, s1, s2, s3]),
                    x: i32::from_be_bytes([x0, x1, x2, x3]),
                    y: i32::from_be_bytes([y0, y1, y2, y3]),
                })
            }
            kind::MOTION_ACK => {
                let rest: &[u8; 4] = rest.try_into().ok()?;
                Some(Self::MotionAck {
                    seq: u32::from_be_bytes(*rest),
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Device info for the tests
    fn info() -> PeerInfo {
        PeerInfo {
            device_id: "d1".into(),
            name: "Desk PC".into(),
            fingerprint: "f".repeat(64),
            platform: "windows".into(),
            os_version: Some("Windows 11".into()),
            props: BTreeMap::from([(PROP_PROTOCOL.to_string(), PROTOCOL_VERSION.to_string())]),
        }
    }

    /// Every control message survives the frame codec
    #[tokio::test]
    async fn control_roundtrip() {
        let samples = [
            Control::Hello {
                version: PROTOCOL_VERSION.into(),
                info: info(),
                purpose: Purpose::Join,
            },
            Control::HelloAck {
                version: PROTOCOL_VERSION.into(),
                info: info(),
            },
            Control::Rejected {
                reason_code: reason_code::IDENTITY_MISMATCH.into(),
            },
            Control::Ping {
                seq: 7,
                sent_us: 123_456,
            },
            Control::Pong {
                seq: 7,
                sent_us: 123_456,
            },
            Control::Enter { x: 1, y: 540 },
            Control::Leave,
            Control::Key {
                usage: 0x04,
                down: true,
            },
            Control::Button {
                button: MouseButton::Back,
                down: false,
                x: -3,
                y: 7,
            },
            Control::Wheel { dx: 0, dy: -120 },
            Control::Released {
                reason_code: released::PREEMPTED.into(),
            },
            Control::Group {
                doc: GroupDoc::new(&info()),
            },
            Control::JoinChallenge {
                pake: vec![0, 1, 0xfe],
                attempts_left: 3,
            },
            Control::JoinAnswer {
                pake: vec![7; 33],
                confirm: vec![9; 32],
            },
            Control::JoinAccepted {
                confirm: vec![],
                doc: GroupDoc::new(&info()),
            },
            Control::JoinDenied {
                reason_code: join_denied::WRONG_PIN.into(),
            },
            Control::ClipOffer {
                kind: Kind::Image,
                size: 33_177_600,
                hash: "ab".repeat(32),
            },
            Control::ClipFiles {
                hash: "cd".repeat(32),
                name: "照片".into(),
                count: 2,
                bytes: 5 << 30,
                token: "ef".repeat(16),
            },
            Control::DragProbe { id: 3 },
            Control::DragFiles {
                id: 3,
                files: vec![DragItem {
                    name: "报告 final.pdf".into(),
                    size: 1 << 20,
                    dir: false,
                }],
                token: "ab".repeat(16),
            },
            Control::DragEnter {
                id: 3,
                origin: "f".repeat(64),
                x: 1,
                y: 400,
                files: vec![DragItem {
                    name: "photos".into(),
                    size: 0,
                    dir: true,
                }],
                token: "ab".repeat(16),
            },
            Control::DragCancel { id: 3 },
            Control::DropWaiting { on: true },
        ];
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        for msg in &samples {
            FRAMING.write(&mut a, msg).await.unwrap();
            let got: Control = FRAMING.read(&mut b).await.unwrap();
            assert_eq!(&got, msg);
        }
    }

    /// A 2.3 drag carries no token; content streams and drag parts
    /// roundtrip
    #[test]
    fn drag_messages() {
        let old = r#"{"type":"drag_files","id":3,"files":[]}"#;
        let parsed: Control = serde_json::from_str(old).unwrap();
        assert_eq!(
            parsed,
            Control::DragFiles {
                id: 3,
                files: vec![],
                token: String::new()
            }
        );
        let request = StreamRequest::Drag {
            token: "t".into(),
            listing: true,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<StreamRequest>(&json).unwrap(),
            request
        );
        // A 2.5 peer asks without a listing
        let old = r#"{"type":"drag","token":"t"}"#;
        assert_eq!(
            serde_json::from_str::<StreamRequest>(old).unwrap(),
            StreamRequest::Drag {
                token: "t".into(),
                listing: false
            }
        );
        for part in [
            DragPart::Dir {
                path: "photos/2026".into(),
            },
            DragPart::File {
                path: "photos/a.jpg".into(),
                size: 7,
            },
            DragPart::Hash { hash: "ab".into() },
            DragPart::Listed,
            DragPart::Done,
        ] {
            let json = serde_json::to_string(&part).unwrap();
            assert_eq!(serde_json::from_str::<DragPart>(&json).unwrap(), part);
        }
    }

    /// Same major is compatible, a different one is not
    #[test]
    fn version_compat() {
        assert!(version_compatible("2.0"));
        assert!(version_compatible("2.9"));
        assert!(!version_compatible("1.0"));
        assert!(!version_compatible("garbage"));
    }

    /// Datagrams roundtrip; truncated, padded and unknown payloads are
    /// dropped
    #[test]
    fn datagram_codec() {
        for dg in [
            Datagram::Ping {
                seq: 1,
                sent_us: u64::MAX,
            },
            Datagram::Pong {
                seq: u32::MAX,
                sent_us: 0,
            },
        ] {
            let bytes = dg.encode();
            assert_eq!(bytes.len(), 13);
            assert_eq!(Datagram::decode(&bytes), Some(dg));
        }
        let motion = Datagram::Motion {
            seq: 9,
            x: -1280,
            y: i32::MAX,
        };
        assert_eq!(motion.encode().len(), 13);
        assert_eq!(Datagram::decode(&motion.encode()), Some(motion));
        let ack = Datagram::MotionAck { seq: 9 };
        assert_eq!(ack.encode().len(), 5);
        assert_eq!(Datagram::decode(&ack.encode()), Some(ack));
        assert_eq!(Datagram::decode(&ack.encode()[..4]), None);

        let ping = Datagram::Ping { seq: 1, sent_us: 2 }.encode();
        assert_eq!(Datagram::decode(&ping[..12]), None);
        let mut padded = ping.to_vec();
        padded.push(0);
        assert_eq!(Datagram::decode(&padded), None);
        assert_eq!(Datagram::decode(&[0xff, 0, 0]), None);
        assert_eq!(Datagram::decode(&[]), None);
    }
}
