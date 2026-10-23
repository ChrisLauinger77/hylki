//! Star, read and tag state: changing it on the server and showing the change
//! before the server confirms it.

use super::*;

/// What the Undo menu calls a star change.
pub(super) fn star_label(starred: bool) -> String {
    if starred {
        i18n("Star")
    } else {
        i18n("Unstar")
    }
}

/// The same for a read/unread change.
pub(super) fn read_label(read: bool) -> String {
    if read {
        i18n("Mark as Read")
    } else {
        i18n("Mark as Unread")
    }
}

/// A sender as a dialog or toast names them: the display name, else the address.
pub(super) fn sender_label(message: &Message) -> String {
    let name = message.from_name.trim();
    if name.is_empty() {
        message.from_addr.clone()
    } else {
        name.to_string()
    }
}
