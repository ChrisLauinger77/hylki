//! Messages opened in their own window.

use super::*;

/// A message displayed in its own top-level window (double-click to pop out).
pub(super) struct PopOut {
    pub(super) window: adw::Window,
    pub(super) controller: Controller<MessageWindow>,
    /// The popped-out message, where it lives (see [`body_key`]), for the
    /// events that arrive about it by folder and UID.
    pub(super) message: BodyKey,
    /// Every message the window shows, the same way, with its (account,
    /// id): the files of each are the window's to show.
    pub(super) members: Vec<(BodyKey, (u32, u32))>,
}
