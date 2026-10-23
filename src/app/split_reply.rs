//! The split reply: a composer under the reader on a Paned.

use super::*;

/// The split reply's own height on its Paned: the divider position above
/// the reader, and what the position leaves of the pane beneath it, where
/// the position is the reader's height (#212).
pub(super) fn split_reply_height(split: &gtk::Paned, bottom: bool) -> i32 {
    if bottom {
        split.height() - split.position()
    } else {
        split.position()
    }
}

/// Let (or forbid) the split reply's child of `split` shrink below the
/// composer's minimum — needed for the slide, which runs the panel through
/// heights the composer could not otherwise be allocated.
pub(super) fn set_split_shrink(split: &gtk::Paned, bottom: bool, shrink: bool) {
    if bottom {
        split.set_shrink_end_child(shrink);
    } else {
        split.set_shrink_start_child(shrink);
    }
}
