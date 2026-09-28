//! Drags of files carried with the pointer, as the input actor sees them.
//!
//! - **Where the drag is held**: the switch asks whether it drags files
//!   ([`Emit::DragAtEdge`]); this device's native side answers, or the
//!   device controlled does ([`Control::DragProbe`] /
//!   [`Control::DragFiles`]), and the switch lets it go along
//!   ([`Switch::allow_carry`](lanroam_input::switch::Switch::allow_carry)).
//!   The drag held there ends on its catcher, which refuses the drop.
//! - **Where it is taken**: another device ([`Control::DragEnter`]) or this
//!   one ([`Emit::Carry`] with this device as `to`). The files are staged,
//!   the native side armed at the entry point, and a press injected there
//!   starts a native drag that follows the pointer. The release drops the
//!   files; released before they are ready, the drop waits (the pointer
//!   stays where it was let go), and lands there once they are.
//! - **Cancelled** (Esc, the pointer leaving): the native side refuses the
//!   drop first, then the button goes up.

use std::path::PathBuf;
use std::time::Duration;

use lanroam_input::switch::{self, Emit};
use lanroam_input::{MouseButton, Point};

use super::{Input, InputMsg, Op};
use crate::engine::drag::{self, DragBackend, DragEvent, Dragging, STAND_IN_TRANSFER};
use crate::protocol::{Control, DragItem};

/// How long a drag coming here may take to be ready for its press
const ARM_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a cancelled drag may take to refuse its drop before the button
/// goes up anyway
const CANCEL_TIMEOUT: Duration = Duration::from_secs(1);

/// Drags of files through this device
#[derive(Default)]
pub(super) struct Drags {
    /// The native side, if it runs here
    native: Option<Box<dyn Dragging>>,
    /// A probe running: its id, and who asked (`None`: this device's
    /// switch)
    probing: Option<(u64, Option<String>)>,
    /// What the latest probe answered with files
    found: Option<Found>,
    /// The drag carried to this device
    carried: Option<Carried>,
}

/// Files found by a probe
struct Found {
    /// The press they are dragged with
    press: u64,
    /// The device they are on
    origin: String,
    /// What they are
    files: Vec<DragItem>,
}

/// A drag carried to this device, from its arrival to its drop
struct Carried {
    /// Names it toward the native side
    id: u64,
    /// The device driving it; `None` when this device's own switch does
    controller: Option<String>,
    /// Where the pointer entered: the press starting the native drag lands
    /// there
    at: Point,
    /// How far it got
    stage: Stage,
    /// The files are in place
    ready: bool,
    /// Released before the files were ready: where, to drop them there
    release: Option<Point>,
    /// Pointer motion that came before the press, replayed after it
    motion: Option<(u32, Point)>,
}

/// How far a drag carried here got
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Its files are being staged
    Staging,
    /// The native side is getting ready for the press
    Arming,
    /// The press went in: the native drag runs
    Pressed,
    /// Cancelled, waiting for the native side to refuse the drop; then
    /// the left button goes up, or everything held when the controller
    /// left
    Cancelling {
        /// Release everything held
        all: bool,
        /// Where the button goes up, when the injector cannot know (this
        /// device's own mouse moved the cursor); where the cursor is
        /// otherwise
        at: Option<Point>,
    },
}

impl Drags {
    /// Start the native side, reporting into `inbox`
    pub(super) fn start(
        backend: &dyn DragBackend,
        inbox: &tokio::sync::mpsc::UnboundedSender<InputMsg>,
    ) -> Self {
        let inbox = inbox.clone();
        let sink = Box::new(move |event| {
            // Fails only once the engine is gone
            let _ = inbox.send(InputMsg::Drag(event));
        });
        let native = backend
            .start(sink)
            .inspect_err(|reason| {
                tracing::info!("dragging files between devices is unavailable: {reason}");
            })
            .ok();
        Self {
            native,
            ..Self::default()
        }
    }
}

impl Input {
    /// What the switch emitted about drags
    pub(super) fn on_drag_emit(&mut self, emit: Emit) {
        match emit {
            Emit::LocalPress { down: true } => {
                if let Some(native) = &self.drags.native {
                    native.pressed();
                }
            }
            Emit::LocalPress { down: false } => self.unprobe(None),
            Emit::DragAtEdge { device, press } if device == self.local => self.probe(press, None),
            Emit::DragAtEdge { device, press } => {
                self.send(&device, Control::DragProbe { id: press });
            }
            Emit::Carry {
                origin,
                to,
                at,
                press,
            } => self.carry(origin, to, at, press),
            Emit::Drop { at } => self.release_carried(None, at),
            Emit::DragCancel { device, at } if device == self.local => {
                self.cancel_carried(None, false, Some(at));
            }
            Emit::DragCancel { device, .. } => {
                let id = self.drags.found.as_ref().map_or(0, |found| found.press);
                self.send(&device, Control::DragCancel { id });
            }
            _ => {}
        }
    }

    /// A drag message from a member
    pub(super) fn on_drag_control(&mut self, from: &str, msg: Control) {
        let controlled = self.controlled_by(from);
        match msg {
            Control::DragProbe { id } if controlled => self.probe(id, Some(from.to_string())),
            Control::DragFiles { id, files } if self.target.as_deref() == Some(from) => {
                self.found(id, from.to_string(), files);
            }
            Control::DragEnter {
                id, x, y, files, ..
            } if controlled => {
                self.begin_carried(id, Some(from.to_string()), Point::new(x, y), files);
            }
            Control::DragCancel { .. } if controlled => {
                self.cancel_carried(Some(from), false, None);
            }
            _ => {}
        }
    }

    /// The left button of the controller `from` went down or up here: a
    /// drag may start, or the one probed is over. True when the event is
    /// taken care of here (the release of a drag carried here)
    pub(super) fn on_controller_left(&mut self, from: &str, down: bool, at: Point) -> bool {
        if down {
            if let Some(native) = &self.drags.native {
                native.pressed();
            }
            return false;
        }
        if self.carried_by(Some(from)) {
            self.release_carried(Some(from), at);
            return true;
        }
        self.unprobe(Some(from));
        false
    }

    /// Hold back or drop a motion from the controller `from` while a drag
    /// carried here waits: before its press, or for its drop. True when
    /// the motion is taken care of
    pub(super) fn hold_motion(&mut self, from: &str, seq: u32, at: Point) -> bool {
        let Some(carried) = self.drags.carried.as_mut() else {
            return false;
        };
        if carried.controller.as_deref() != Some(from) {
            return false;
        }
        match carried.stage {
            Stage::Staging | Stage::Arming => {
                carried.motion = Some((seq, at));
                true
            }
            // Let go of: the pointer stays where the files will land
            Stage::Pressed => carried.release.is_some(),
            Stage::Cancelling { .. } => false,
        }
    }

    /// The controller `from` is gone: a drag it held here is over, and a
    /// drag it carried here is cancelled. True when its buttons are
    /// released once the drag refuses its drop, not now
    pub(super) fn controller_gone(&mut self, from: &str) -> bool {
        self.unprobe(Some(from));
        self.cancel_carried(Some(from), true, None)
    }

    /// What the native side reported
    pub(super) fn on_drag_event(&mut self, event: DragEvent) {
        match event {
            DragEvent::Probed { id, files } => self.probed(id, files),
            DragEvent::Armed { id } => self.armed(id),
            DragEvent::Cancelling { id } => self.cancelled(id),
            DragEvent::Ended { id, dropped } => {
                tracing::info!(id, dropped, "a drag carried here ended");
                if self.drags.carried.as_ref().is_some_and(|c| c.id == id) {
                    self.drags.carried = None;
                }
            }
        }
    }

    /// The files of a probe, described off the runtime, for whoever asked
    pub(super) fn described(&mut self, id: u64, asker: Option<String>, files: Vec<DragItem>) {
        match asker {
            Some(asker) => self.send(&asker, Control::DragFiles { id, files }),
            None => self.found(id, self.local.clone(), files),
        }
    }

    /// The files of drag `id` are staged: arm the native side with them
    pub(super) fn staged(&mut self, id: u64, paths: Result<Vec<PathBuf>, String>) {
        let Some(carried) = self.carried_at(id, Stage::Staging) else {
            return;
        };
        match paths {
            Ok(paths) if !paths.is_empty() => {
                carried.stage = Stage::Arming;
                let at = carried.at;
                if let Some(native) = &self.drags.native {
                    native.arm(id, at, paths);
                }
            }
            Ok(_) => {
                tracing::warn!(id, "a drag came here with nothing to drag");
                self.drags.carried = None;
            }
            Err(e) => {
                tracing::warn!(id, "cannot stage the files of a drag: {e}");
                self.drags.carried = None;
            }
        }
    }

    /// The files of drag `id` are ready: a drop waiting for them happens
    pub(super) fn ready(&mut self, id: u64) {
        let Some(carried) = self.drags.carried.as_mut().filter(|c| c.id == id) else {
            return;
        };
        carried.ready = true;
        if carried.stage == Stage::Pressed
            && let Some(at) = carried.release
        {
            self.drop_carried(at);
        }
    }

    /// Drag `id` took too long to arm, or to refuse its drop
    pub(super) fn timed_out(&mut self, id: u64) {
        let Some(carried) = self.drags.carried.as_ref().filter(|c| c.id == id) else {
            return;
        };
        match carried.stage {
            Stage::Staging | Stage::Arming => {
                tracing::warn!(id, "a drag carried here was not ready in time");
                if let Some(native) = &self.drags.native {
                    native.cancel(id);
                }
                self.drags.carried = None;
            }
            Stage::Cancelling { .. } => {
                tracing::warn!(id, "a cancelled drag did not refuse its drop in time");
                self.cancelled(id);
            }
            Stage::Pressed => {}
        }
    }

    /// Ask the native side whether the left button held here drags files,
    /// for `asker` (`None`: this device's switch)
    fn probe(&mut self, id: u64, asker: Option<String>) {
        let Some(native) = &self.drags.native else {
            if let Some(asker) = asker {
                self.send(
                    &asker,
                    Control::DragFiles {
                        id,
                        files: Vec::new(),
                    },
                );
            }
            return;
        };
        native.probe(id);
        self.drags.probing = Some((id, asker));
    }

    /// The native side answered probe `id`: describe the files off the
    /// runtime, or answer at once that there are none
    fn probed(&mut self, id: u64, files: Vec<PathBuf>) {
        let Some((_, asker)) = self.drags.probing.clone().filter(|(probe, _)| *probe == id) else {
            return;
        };
        tracing::info!(id, files = files.len(), "probed a drag held here");
        if files.is_empty() {
            if let Some(asker) = asker {
                self.send(
                    &asker,
                    Control::DragFiles {
                        id,
                        files: Vec::new(),
                    },
                );
            }
            return;
        }
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            let files = tokio::task::spawn_blocking(move || drag::describe(&files))
                .await
                .unwrap_or_default();
            let _ = inbox.send(InputMsg::Described { id, asker, files });
        });
    }

    /// The press probed for `asker` is over: the catcher goes
    fn unprobe(&mut self, asker: Option<&str>) {
        if self
            .drags
            .probing
            .take_if(|(_, probing)| probing.as_deref() == asker)
            .is_some()
            && let Some(native) = &self.drags.native
        {
            native.unprobe();
        }
    }

    /// Press `press` drags `files`, on `origin`: the switch lets it go
    /// along
    fn found(&mut self, press: u64, origin: String, files: Vec<DragItem>) {
        if files.is_empty() {
            return;
        }
        self.drags.found = Some(Found {
            press,
            origin,
            files,
        });
        switch::lock(&self.switch).allow_carry(press);
    }

    /// The drag of `press` went from `origin` to `to` with the pointer,
    /// entering at `at`
    fn carry(&mut self, origin: String, to: String, at: Point, press: u64) {
        let Some(found) = self
            .drags
            .found
            .as_ref()
            .filter(|found| found.press == press && found.origin == origin)
        else {
            tracing::warn!(press, "a drag went along without its files");
            return;
        };
        let files = found.files.clone();
        tracing::info!(files = files.len(), from = %self.name(&origin), to = %self.name(&to), "a drag of files goes along");
        if to == self.local {
            self.begin_carried(press, None, at, files);
        } else {
            let msg = Control::DragEnter {
                id: press,
                origin,
                x: at.x,
                y: at.y,
                files,
            };
            self.send(&to, msg);
        }
    }

    /// A drag of `files` comes here, driven by `controller` (`None`: this
    /// device's switch), entering at `at`: stage the files, then arm
    fn begin_carried(
        &mut self,
        id: u64,
        controller: Option<String>,
        at: Point,
        files: Vec<DragItem>,
    ) {
        if self.drags.native.is_none() || self.replay.is_none() {
            tracing::warn!(id, "a drag came here, but this device cannot drag");
            return;
        }
        if let Some(earlier) = self.drags.carried.take() {
            tracing::debug!(id = earlier.id, "an earlier drag gives way");
        }
        tracing::info!(id, files = files.len(), ?at, "a drag of files comes here");
        self.drags.carried = Some(Carried {
            id,
            controller,
            at,
            stage: Stage::Staging,
            ready: false,
            release: None,
            motion: None,
        });
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            let staged = tokio::task::spawn_blocking(move || drag::stage_stand_ins(id, &files))
                .await
                .map_err(|e| e.to_string())
                .and_then(|staged| staged.map_err(|e| e.to_string()));
            let _ = inbox.send(InputMsg::Staged { id, paths: staged });
        });
        self.after(STAND_IN_TRANSFER, InputMsg::DragReady(id));
        self.after(ARM_TIMEOUT, InputMsg::DragTimeout(id));
    }

    /// The native side is ready for drag `id`: press at the entry point,
    /// then catch up with the pointer
    fn armed(&mut self, id: u64) {
        let Some(carried) = self.carried_at(id, Stage::Arming) else {
            return;
        };
        carried.stage = Stage::Pressed;
        let (at, motion, release, ready) = (
            carried.at,
            carried.motion.take(),
            carried.release,
            carried.ready,
        );
        self.replay(Op::Button(MouseButton::Left, true, at));
        if let Some((seq, to)) = motion {
            self.replay(Op::Motion(seq, to));
        }
        if ready && let Some(release) = release {
            self.drop_carried(release);
        }
    }

    /// The button carrying the drag here went up at `at`: drop now, or once
    /// the files are ready
    fn release_carried(&mut self, controller: Option<&str>, at: Point) {
        let Some(carried) = self
            .drags
            .carried
            .as_mut()
            .filter(|c| c.controller.as_deref() == controller)
        else {
            return;
        };
        match carried.stage {
            Stage::Pressed if carried.ready => self.drop_carried(at),
            Stage::Cancelling { .. } => {}
            _ => {
                tracing::info!(
                    id = carried.id,
                    "released before the files are ready: the drop waits"
                );
                carried.release = Some(at);
            }
        }
    }

    /// Let the button go at `at`: the native drag drops the files there
    fn drop_carried(&mut self, at: Point) {
        if let Some(carried) = self.drags.carried.take() {
            tracing::info!(id = carried.id, ?at, "dropping a drag carried here");
        }
        self.replay(Op::Button(MouseButton::Left, false, at));
    }

    /// Cancel the drag `controller` carried here, if any: the native side
    /// refuses the drop first, then the left button goes up (at `at` if
    /// given; everything held with `all`). True when a release waits for
    /// that
    fn cancel_carried(&mut self, controller: Option<&str>, all: bool, at: Option<Point>) -> bool {
        if !self.carried_by(controller) {
            return false;
        }
        let Some(carried) = self.drags.carried.as_mut() else {
            return false;
        };
        let id = carried.id;
        if let Some(native) = &self.drags.native {
            native.cancel(id);
        }
        match carried.stage {
            Stage::Staging | Stage::Arming => {
                tracing::info!(id, "a drag carried here is cancelled before it started");
                self.drags.carried = None;
                false
            }
            Stage::Pressed => {
                tracing::info!(id, "cancelling a drag carried here");
                carried.stage = Stage::Cancelling { all, at };
                self.after(CANCEL_TIMEOUT, InputMsg::DragTimeout(id));
                true
            }
            Stage::Cancelling {
                all: before,
                at: then,
            } => {
                carried.stage = Stage::Cancelling {
                    all: all || before,
                    at: at.or(then),
                };
                true
            }
        }
    }

    /// Drag `id` refuses its drop now: let the button go
    fn cancelled(&mut self, id: u64) {
        let Some(carried) = self.drags.carried.as_ref().filter(|c| c.id == id) else {
            return;
        };
        let Stage::Cancelling { all, at } = carried.stage else {
            return;
        };
        self.drags.carried = None;
        match at {
            Some(at) => self.replay(Op::Button(MouseButton::Left, false, at)),
            None => self.replay(Op::Release(MouseButton::Left)),
        }
        if all {
            self.replay(Op::ReleaseAll);
        }
    }

    /// Whether a drag carried here is driven by `controller`
    fn carried_by(&self, controller: Option<&str>) -> bool {
        self.drags
            .carried
            .as_ref()
            .is_some_and(|carried| carried.controller.as_deref() == controller)
    }

    /// The drag carried here, if it is `id` at `stage`
    fn carried_at(&mut self, id: u64, stage: Stage) -> Option<&mut Carried> {
        self.drags
            .carried
            .as_mut()
            .filter(|carried| carried.id == id && carried.stage == stage)
    }

    /// Send `msg` to the actor's own inbox after `delay`
    fn after(&self, delay: Duration, msg: InputMsg) {
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = inbox.send(msg);
        });
    }
}
