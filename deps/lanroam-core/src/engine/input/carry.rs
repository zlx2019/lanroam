//! Drags of files carried with the pointer, as the input actor sees them.
//!
//! - **Where the drag is held**: the switch asks whether it drags files
//!   ([`Emit::DragAtEdge`]); this device's native side answers, or the
//!   device controlled does ([`Control::DragProbe`] /
//!   [`Control::DragFiles`]), and the switch lets it go along
//!   ([`Switch::allow_carry`](lanroam_input::switch::Switch::allow_carry)).
//!   The device the files are on offers them under a token. The drag held
//!   there ends on its catcher, which refuses the drop.
//! - **Where it is taken**: another device ([`Control::DragEnter`]) or this
//!   one ([`Emit::Carry`] with this device as `to`). Stand-ins for the
//!   files are staged, the native side armed at the entry point, and a
//!   press injected there starts a native drag that follows the pointer;
//!   meanwhile the files are pulled from where they are, over the
//!   stand-ins. The release drops them; released before they are all
//!   there, the drop waits (the pointer stays where it was let go, and the
//!   user sees how far they are), and lands there once they are.
//! - **Cancelled** (Esc, the pointer leaving, the files not arriving): the
//!   native side refuses the drop first, then the button goes up, and the
//!   files already there go.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lanroam_input::keymap::usage;
use lanroam_input::switch::{self, Emit};
use lanroam_input::{MouseButton, Point};
use tokio::task::JoinHandle;

use super::{Input, InputMsg, Op};
use crate::engine::EngineEvent;
use crate::engine::drag::{
    self, DragBackend, DragEvent, Dragging, KEEP_AFTER_DROP, Receiving, failed,
};
use crate::engine::files::{self, Offers};
use crate::protocol::{Control, DragItem};

/// How long a drag coming here may take to be ready for its press
const ARM_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a cancelled drag may take to refuse its drop before the button
/// goes up anyway
const CANCEL_TIMEOUT: Duration = Duration::from_secs(1);

/// How often the files' progress is reported while they arrive
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Drags of files through this device
#[derive(Default)]
pub(super) struct Drags {
    /// The native side, if it runs here
    native: Option<Box<dyn Dragging>>,
    /// Files this device offers to the devices its drags go to
    offers: Offers,
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
    /// What pulls them from `origin`
    token: String,
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
    /// The files are all there
    ready: bool,
    /// The files will not arrive: the release cancels the drag
    failed: bool,
    /// Released before the files were there: where, to drop them there
    release: Option<Point>,
    /// Pointer motion that came before the press, replayed after it
    motion: Option<(u32, Point)>,
    /// The folder the files land in, once staged
    dir: Option<PathBuf>,
    /// Staging the stand-ins and pulling the files
    pull: JoinHandle<()>,
    /// What the user sees while the drop waits
    receiving: Receiving,
}

/// How far a drag carried here got
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Stand-ins for its files are being staged
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
    /// Start the native side, reporting into `inbox`, with the files this
    /// device offers in `offers`
    pub(super) fn start(
        backend: &dyn DragBackend,
        inbox: &tokio::sync::mpsc::UnboundedSender<InputMsg>,
        offers: Offers,
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
            offers,
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
            Control::DragFiles { id, files, token } if self.target.as_deref() == Some(from) => {
                self.found(id, from.to_string(), files, token);
            }
            Control::DragEnter {
                id,
                origin,
                x,
                y,
                files,
                token,
            } if controlled => {
                let at = Point::new(x, y);
                self.begin_carried(id, Some(from.to_string()), at, files, &origin, token);
            }
            Control::DragCancel { .. } if controlled => {
                self.cancel_carried(Some(from), false, None);
            }
            // The drop waits there: the pointer holds still
            Control::DropWaiting { on } if self.target.as_deref() == Some(from) => {
                switch::lock(&self.switch).set_awaiting_drop(on);
            }
            _ => {}
        }
    }

    /// The left button of the controller `from` went down or up here: a
    /// drag may start, or the one probed is over. True when the event is
    /// taken care of here (the release of a drag carried here)
    pub(super) fn on_controller_left(&mut self, from: &str, down: bool, at: Point) -> bool {
        // A drop waits here: a click would move it
        let waiting = self
            .drags
            .carried
            .as_ref()
            .is_some_and(|c| c.controller.as_deref() == Some(from) && c.release.is_some());
        if waiting {
            return true;
        }
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

    /// Esc from the controller `from`: it cancels a drop of its waiting
    /// here. True when that is what it did
    pub(super) fn on_controller_escape(&mut self, from: &str, key: u16, down: bool) -> bool {
        let waiting = self
            .drags
            .carried
            .as_ref()
            .is_some_and(|c| c.controller.as_deref() == Some(from) && c.release.is_some());
        if key != usage::ESCAPE || !down || !waiting {
            return false;
        }
        self.cancel_carried(Some(from), false, None);
        true
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
                // Ended without a drop of ours (it could not start)
                if self.drags.carried.as_ref().is_some_and(|c| c.id == id)
                    && let Some(carried) = self.end_carried()
                    && let Some(dir) = carried.dir
                {
                    discard_later(dir, Duration::ZERO);
                }
            }
        }
    }

    /// The files of a probe, described and offered off the runtime, for
    /// whoever asked
    pub(super) fn described(
        &mut self,
        id: u64,
        asker: Option<String>,
        files: Vec<DragItem>,
        token: String,
    ) {
        match asker {
            Some(asker) => self.send(&asker, Control::DragFiles { id, files, token }),
            None => self.found(id, self.local.clone(), files, token),
        }
    }

    /// Stand-ins for the files of drag `id` are staged in `dir`: arm the
    /// native side with them
    pub(super) fn staged(&mut self, id: u64, staged: Result<(PathBuf, Vec<PathBuf>), String>) {
        let Some(carried) = self.carried_at(id, Stage::Staging) else {
            if let Ok((dir, _)) = staged {
                discard_later(dir, Duration::ZERO);
            }
            return;
        };
        match staged {
            Ok((dir, paths)) if !paths.is_empty() => {
                carried.stage = Stage::Arming;
                carried.dir = Some(dir);
                let at = carried.at;
                if let Some(native) = &self.drags.native {
                    native.arm(id, at, paths);
                }
            }
            Ok((dir, _)) => {
                tracing::warn!(id, "a drag came here with nothing to drag");
                carried.dir = Some(dir);
                self.give_up();
            }
            Err(e) => {
                tracing::warn!(id, "cannot stage the files of a drag: {e}");
                self.give_up();
            }
        }
    }

    /// `done` of the `total` bytes of drag `id` are there
    pub(super) fn progress(&mut self, id: u64, done: u64, total: u64) {
        let Some(carried) = self.drags.carried.as_mut().filter(|c| c.id == id) else {
            return;
        };
        (carried.receiving.done, carried.receiving.total) = (done, total);
        if carried.release.is_some() {
            self.show_receiving();
        }
    }

    /// The files of drag `id` are all there: a drop waiting for them
    /// happens
    pub(super) fn ready(&mut self, id: u64) {
        let Some(carried) = self.drags.carried.as_mut().filter(|c| c.id == id) else {
            return;
        };
        tracing::info!(
            id,
            bytes = carried.receiving.total,
            "the files of a drag are here"
        );
        carried.ready = true;
        if carried.stage == Stage::Pressed
            && let Some(at) = carried.release
        {
            self.drop_carried(at);
        }
    }

    /// The files of drag `id` will not arrive (`reason`, a [`failed`]
    /// code): tell the user, and cancel it, at once or when the button
    /// goes up
    pub(super) fn transfer_failed(&mut self, id: u64, reason: &str, detail: &str) {
        let Some(carried) = self.drags.carried.as_mut().filter(|c| c.id == id) else {
            return;
        };
        tracing::warn!(id, reason, "the files of a drag did not arrive: {detail}");
        carried.failed = true;
        let (name, controller, release) = (
            carried.receiving.name.clone(),
            carried.controller.clone(),
            carried.release,
        );
        let _ = self.events.send(EngineEvent::DragFailed {
            reason: reason.to_string(),
            name,
        });
        match carried.stage {
            Stage::Staging | Stage::Arming => self.give_up(),
            Stage::Pressed if release.is_some() => {
                self.cancel_carried(controller.as_deref(), false, release);
            }
            Stage::Pressed | Stage::Cancelling { .. } => {}
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
                self.give_up();
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
                self.send(&asker, no_files(id));
            }
            return;
        };
        native.probe(id);
        self.drags.probing = Some((id, asker));
    }

    /// The native side answered probe `id`: describe and offer the files
    /// off the runtime, or answer at once that there are none
    fn probed(&mut self, id: u64, files: Vec<PathBuf>) {
        let Some((_, asker)) = self.drags.probing.clone().filter(|(probe, _)| *probe == id) else {
            return;
        };
        tracing::info!(id, files = files.len(), "probed a drag held here");
        if files.is_empty() {
            if let Some(asker) = asker {
                self.send(&asker, no_files(id));
            }
            return;
        }
        let (inbox, offers) = (self.inbox.clone(), self.drags.offers.clone());
        tokio::spawn(async move {
            let described = tokio::task::spawn_blocking(move || {
                let items = drag::describe(&files);
                offers.offer(files).map(|token| (items, token))
            })
            .await;
            let (files, token) = match described {
                Ok(Ok(described)) => described,
                Ok(Err(e)) => {
                    tracing::warn!("cannot offer the files dragged: {e}");
                    (Vec::new(), String::new())
                }
                Err(_) => (Vec::new(), String::new()),
            };
            let msg = InputMsg::Described {
                id,
                asker,
                files,
                token,
            };
            let _ = inbox.send(msg);
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

    /// Press `press` drags `files`, on `origin`, pulled with `token`: the
    /// switch lets it go along
    fn found(&mut self, press: u64, origin: String, files: Vec<DragItem>, token: String) {
        if files.is_empty() {
            return;
        }
        self.drags.found = Some(Found {
            press,
            origin,
            files,
            token,
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
        let (files, token) = (found.files.clone(), found.token.clone());
        tracing::info!(files = files.len(), from = %self.name(&origin), to = %self.name(&to), "a drag of files goes along");
        if to == self.local {
            self.begin_carried(press, None, at, files, &origin, token);
        } else {
            let msg = Control::DragEnter {
                id: press,
                origin,
                x: at.x,
                y: at.y,
                files,
                token,
            };
            self.send(&to, msg);
        }
    }

    /// A drag of `files` comes here, driven by `controller` (`None`: this
    /// device's switch), entering at `at`: stage stand-ins and arm, and
    /// pull the files from `origin` with `token` meanwhile
    fn begin_carried(
        &mut self,
        id: u64,
        controller: Option<String>,
        at: Point,
        files: Vec<DragItem>,
        origin: &str,
        token: String,
    ) {
        if self.drags.native.is_none() || self.replay.is_none() {
            tracing::warn!(id, "a drag came here, but this device cannot drag");
            return;
        }
        let conn = self
            .links
            .borrow()
            .get(origin)
            .map(|link| link.conn.clone());
        let Some(conn) = conn.filter(|_| !token.is_empty()) else {
            tracing::warn!(id, "a drag came here from a device it cannot pull from");
            return;
        };
        if let Some(earlier) = self.end_carried() {
            tracing::debug!(id = earlier.id, "an earlier drag gives way");
            if let Some(dir) = earlier.dir {
                discard_later(dir, Duration::ZERO);
            }
        }
        let total = files.iter().map(|item| item.size).sum();
        let receiving = Receiving {
            at,
            name: files
                .first()
                .map(|item| item.name.clone())
                .unwrap_or_default(),
            count: files.len(),
            done: 0,
            total,
        };
        tracing::info!(
            id,
            files = files.len(),
            bytes = total,
            ?at,
            "a drag of files comes here"
        );
        let pull = tokio::spawn(pull_files(self.inbox.clone(), id, files, conn, token));
        self.drags.carried = Some(Carried {
            id,
            controller,
            at,
            stage: Stage::Staging,
            ready: false,
            failed: false,
            release: None,
            motion: None,
            dir: None,
            pull,
            receiving,
        });
        self.after(ARM_TIMEOUT, InputMsg::DragTimeout(id));
    }

    /// The native side is ready for drag `id`: press at the entry point,
    /// then catch up with the pointer
    fn armed(&mut self, id: u64) {
        let Some(carried) = self.carried_at(id, Stage::Arming) else {
            return;
        };
        carried.stage = Stage::Pressed;
        let (at, motion, release) = (carried.at, carried.motion.take(), carried.release);
        let (ready, failed, controller) =
            (carried.ready, carried.failed, carried.controller.clone());
        self.replay(Op::Button(MouseButton::Left, true, at));
        if let Some((seq, to)) = motion {
            self.replay(Op::Motion(seq, to));
        }
        match release {
            Some(release) if failed => {
                self.cancel_carried(controller.as_deref(), false, Some(release));
            }
            Some(release) if ready => self.drop_carried(release),
            _ => {}
        }
    }

    /// The button carrying the drag here went up at `at`: drop now, or once
    /// the files are there
    fn release_carried(&mut self, controller: Option<&str>, at: Point) {
        let Some(carried) = self
            .drags
            .carried
            .as_mut()
            .filter(|c| c.controller.as_deref() == controller && c.release.is_none())
        else {
            return;
        };
        match carried.stage {
            Stage::Pressed if carried.failed => {
                self.cancel_carried(controller, false, Some(at));
            }
            Stage::Pressed if carried.ready => self.drop_carried(at),
            Stage::Cancelling { .. } => {}
            _ => {
                tracing::info!(
                    id = carried.id,
                    "released before the files are here: the drop waits"
                );
                carried.release = Some(at);
                carried.receiving.at = at;
                // The pointer holds still where it lands, and Esc cancels
                // it: the switch driving it sees to that
                match controller {
                    Some(controller) => {
                        self.send(controller, Control::DropWaiting { on: true });
                    }
                    None => switch::lock(&self.switch).set_awaiting_drop(true),
                }
                self.show_receiving();
            }
        }
    }

    /// Let the button go at `at`: the native drag drops the files there.
    /// Their folder goes a while later, once the app they went to is
    /// surely done with it
    fn drop_carried(&mut self, at: Point) {
        if let Some(carried) = self.end_carried() {
            tracing::info!(id = carried.id, ?at, "dropping a drag carried here");
            if let Some(dir) = carried.dir {
                discard_later(dir, KEEP_AFTER_DROP);
            }
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
        carried.pull.abort();
        match carried.stage {
            Stage::Staging | Stage::Arming => {
                tracing::info!(id, "a drag carried here is cancelled before it started");
                self.give_up();
                false
            }
            Stage::Pressed => {
                tracing::info!(id, "cancelling a drag carried here");
                carried.stage = Stage::Cancelling { all, at };
                if let Some(native) = &self.drags.native {
                    native.cancel(id);
                }
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

    /// Drag `id` refuses its drop now: let the button go, and the files
    /// already there
    fn cancelled(&mut self, id: u64) {
        let Some(carried) = self.drags.carried.as_ref().filter(|c| c.id == id) else {
            return;
        };
        let Stage::Cancelling { all, at } = carried.stage else {
            return;
        };
        if let Some(carried) = self.end_carried()
            && let Some(dir) = carried.dir
        {
            discard_later(dir, Duration::ZERO);
        }
        match at {
            Some(at) => self.replay(Op::Button(MouseButton::Left, false, at)),
            None => self.replay(Op::Release(MouseButton::Left)),
        }
        if all {
            self.replay(Op::ReleaseAll);
        }
    }

    /// Give up a drag carried here before its press: the native side lets
    /// go of it, and its folder goes
    fn give_up(&mut self) {
        let Some(carried) = self.end_carried() else {
            return;
        };
        if let Some(native) = &self.drags.native {
            native.cancel(carried.id);
        }
        if let Some(dir) = carried.dir {
            discard_later(dir, Duration::ZERO);
        }
    }

    /// The drag carried here is over: stop pulling its files, and stop
    /// waiting for its drop
    fn end_carried(&mut self) -> Option<Carried> {
        let carried = self.drags.carried.take()?;
        carried.pull.abort();
        if carried.release.is_some() {
            match &carried.controller {
                Some(controller) => {
                    self.send(controller, Control::DropWaiting { on: false });
                }
                None => switch::lock(&self.switch).set_awaiting_drop(false),
            }
            let _ = self.events.send(EngineEvent::Receiving(None));
        }
        Some(carried)
    }

    /// Tell the user how far the files of the drop waiting here are
    fn show_receiving(&self) {
        if let Some(carried) = &self.drags.carried {
            let receiving = carried.receiving.clone();
            let _ = self.events.send(EngineEvent::Receiving(Some(receiving)));
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

/// The answer of a probe that found no files
fn no_files(id: u64) -> Control {
    Control::DragFiles {
        id,
        files: Vec::new(),
        token: String::new(),
    }
}

/// Stage stand-ins for `files` of drag `id`, see that they fit, and pull
/// them over the stand-ins from the device behind `conn`; every step is
/// reported to `inbox`
async fn pull_files(
    inbox: tokio::sync::mpsc::UnboundedSender<InputMsg>,
    id: u64,
    files: Vec<DragItem>,
    conn: quinn::Connection,
    token: String,
) {
    let total: u64 = files.iter().map(|item| item.size).sum();
    let staged = tokio::task::spawn_blocking(move || {
        let (dir, paths) = drag::stage(id, &files)?;
        let free = files::available_space(&dir);
        Ok::<_, std::io::Error>((dir, paths, free))
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|staged| staged.map_err(|e| e.to_string()));
    let (dir, paths, free) = match staged {
        Ok(staged) => staged,
        Err(e) => {
            let _ = inbox.send(InputMsg::Staged { id, staged: Err(e) });
            return;
        }
    };
    if free.is_some_and(|free| free < total) {
        let _ = tokio::task::spawn_blocking(move || drag::discard(&dir)).await;
        let _ = inbox.send(InputMsg::DragFailed {
            id,
            reason: failed::NO_SPACE,
            detail: format!("{total} bytes needed, {} free", free.unwrap_or_default()),
        });
        return;
    }
    let _ = inbox.send(InputMsg::Staged {
        id,
        staged: Ok((dir.clone(), paths)),
    });
    let mut last = Instant::now();
    let pulled = files::pull(&conn, token, &dir, |done, total| {
        if last.elapsed() >= PROGRESS_EVERY || done == total {
            last = Instant::now();
            let _ = inbox.send(InputMsg::DragProgress { id, done, total });
        }
    })
    .await;
    let _ = inbox.send(match pulled {
        Ok(()) => InputMsg::DragReady(id),
        Err(e) => InputMsg::DragFailed {
            id,
            reason: failed::TRANSFER,
            detail: e.to_string(),
        },
    });
}

/// Remove the folder of a drag after `delay`, off the runtime
fn discard_later(dir: PathBuf, delay: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = tokio::task::spawn_blocking(move || drag::discard(&dir)).await;
    });
}
