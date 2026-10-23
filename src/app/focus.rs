//! Focus Mode: the chrome that slides away, and back.

use super::*;

/// Slide a Focus Mode revealer's content away (and fade it, through the
/// `away` class's opacity transition) or bring it back.
pub(super) fn focus_reveal(r: &gtk::Revealer, shown: bool) {
    if shown {
        r.remove_css_class("away");
    } else {
        r.add_css_class("away");
    }
    r.set_reveal_child(shown);
}
