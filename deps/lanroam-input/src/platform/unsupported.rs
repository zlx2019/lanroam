//! Placeholder backend for platforms without input support yet (Linux is
//! planned for v2).

use std::sync::{Arc, Mutex};

use super::{Capture, EmitSink, Permissions};
use crate::InputError;
use crate::geometry::Rect;
use crate::inject::Injector;
use crate::switch::Switch;

/// Nothing to hold here; capture itself is unsupported
pub(super) fn permissions() -> Permissions {
    Permissions {
        accessibility: true,
        input_monitoring: true,
    }
}

/// Nothing to ask for
pub(super) fn request_permissions() {}

/// Not available
pub(super) fn displays() -> Result<Vec<Rect>, InputError> {
    Err(InputError::Unsupported("reading the display layout"))
}

/// Not available
pub(super) fn scale() -> Result<u32, InputError> {
    Err(InputError::Unsupported("reading the display scale"))
}

/// Not available
pub(super) fn screens() -> Result<(Vec<Rect>, Vec<u32>), InputError> {
    Err(InputError::Unsupported("reading the display layout"))
}

/// Not available
pub(super) fn start_capture(
    _switch: Arc<Mutex<Switch>>,
    _sink: EmitSink,
) -> Result<Capture, InputError> {
    Err(InputError::Unsupported("input capture"))
}

/// Not available
pub(super) fn injector() -> Result<Box<dyn Injector>, InputError> {
    Err(InputError::Unsupported("input injection"))
}
