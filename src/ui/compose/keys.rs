//! The composer's keyboard shortcuts.

use super::messages::ComposeInput;
use super::types::Field;
use adw::prelude::*;
use relm4::prelude::*;

/// Ctrl+Shift+A, T, C or B as what it asks the composer for.
pub(crate) fn composer_key(
    keyval: gtk::gdk::Key,
    state: gtk::gdk::ModifierType,
) -> Option<ComposeInput> {
    use gtk::gdk::{Key, ModifierType as M};
    if state & (M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK)
        != M::CONTROL_MASK | M::SHIFT_MASK
    {
        return None;
    }
    match keyval.to_lower() {
        Key::a => Some(ComposeInput::AttachFiles),
        Key::t => Some(ComposeInput::FocusField(Field::To)),
        Key::c => Some(ComposeInput::FocusField(Field::Cc)),
        Key::b => Some(ComposeInput::FocusField(Field::Bcc)),
        _ => None,
    }
}

/// Ctrl+ (or Ctrl+=, for layouts where + needs Shift), Ctrl- and Ctrl+0,
/// keypad included, as the zoom step they ask for: the reader's keys.
pub(super) fn zoom_key(keyval: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> Option<i8> {
    use gtk::gdk::Key;
    if !state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
        || state.contains(gtk::gdk::ModifierType::ALT_MASK)
    {
        return None;
    }
    // By value, as the suggestion keys are.
    if [Key::plus, Key::equal, Key::KP_Add].contains(&keyval) {
        Some(1)
    } else if [Key::minus, Key::KP_Subtract].contains(&keyval) {
        Some(-1)
    } else if [Key::_0, Key::KP_0].contains(&keyval) {
        Some(0)
    } else {
        None
    }
}

/// Whether keyboard focus is in a one-line text field of the composer — an
/// address row or the subject. Those keep their own native undo; everything
/// else in the composer answers to its history. The body is a WebView, not a
/// `GtkEditable`, so it never matches here.
pub(super) fn focus_is_entry(root: &impl IsA<gtk::Widget>) -> bool {
    let mut node = root
        .as_ref()
        .root()
        .and_downcast::<gtk::Window>()
        .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w));
    while let Some(widget) = node {
        if widget.is::<gtk::Editable>() || widget.is::<gtk::TextView>() {
            return true;
        }
        node = widget.parent();
    }
    false
}
