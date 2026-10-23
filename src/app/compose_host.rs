//! Where composers live: their own windows, or the reader's split pane.

use super::*;

/// A standalone compose window (New Message, compose-to, edit-draft, or a
/// popped-out reply) and its component. Both refs must stay alive: the window
/// holds the content, the controller holds the component root.
pub(super) struct ComposeHost {
    pub(super) id: u32,
    /// Held only to keep the component (and its widget tree) alive for the
    /// window's lifetime; dropped when the host is removed.
    #[allow(dead_code)]
    pub(super) controller: Controller<Compose>,
    pub(super) window: adw::Window,
}

/// The reader's inline reply/forward composer. `window` is `Some` only while the
/// pane has been promoted to a floating window (else it lives in the reader's
/// drop-down revealer).
pub(super) struct ReaderCompose {
    pub(super) id: u32,
    pub(super) controller: Controller<Compose>,
    pub(super) window: Option<adw::Window>,
    /// The message a split reply answers, by (account, id): its card in the
    /// reader is the one the reply was opened from.
    pub(super) answering: Option<(u32, u32)>,
}
