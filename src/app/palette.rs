//! The keyboard help: the command palette's key, menu entries that show their
//! keys, and the shortcut tables the shortcuts window lists.

use adw::prelude::*;
use relm4::prelude::*;

use super::{AppModel, AppMsg};
use crate::i18n::i18n_noop;

/// Ctrl+K opens the command palette (#399) wherever the keyboard is, a
/// composer included: in the capture phase, ahead of a focused editor that
/// keeps the other Ctrl keys for its text. Ctrl+Shift+K opens it with `>`
/// typed, on the actions only.
pub(super) fn palette_key(sender: &ComponentSender<AppModel>) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let s = sender.input_sender().clone();
    keys.connect_key_pressed(move |keys, keyval, _, state| {
        use gtk::gdk::ModifierType as M;
        let mods = state & (M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK);
        let ctrl = mods == M::CONTROL_MASK || mods == M::CONTROL_MASK | M::SHIFT_MASK;
        if ctrl && matches!(keyval, gtk::gdk::Key::k | gtk::gdk::Key::K) {
            // Not over a dialog already up, the palette itself among them.
            let dialog = keys.widget().is_some_and(|w| {
                w.downcast_ref::<adw::ApplicationWindow>()
                    .map(|w| w.visible_dialog().is_some())
                    .or_else(|| {
                        w.downcast_ref::<adw::Window>()
                            .map(|w| w.visible_dialog().is_some())
                    })
                    .unwrap_or(false)
            });
            if dialog {
                return gtk::glib::Propagation::Proceed;
            }
            let _ = s.send(AppMsg::CommandPalette(mods.contains(M::SHIFT_MASK)));
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    keys
}

/// A menu entry that shows its keys even though they belong to another
/// action (see `accounts-key`).
pub(super) fn keyed_item(label: &str, action: &str, accel: &str) -> gtk::gio::MenuItem {
    let item = gtk::gio::MenuItem::new(Some(label), Some(action));
    item.set_attribute_value("accel", Some(&accel.to_variant()));
    item
}

/// Every shortcut with its key and description, for the reference window.
pub(super) const SHORTCUT_HELP: &[(&str, &[(&str, &str)])] = &[
    (
        i18n_noop("Move around"),
        &[
            ("j  or  ↓", i18n_noop("Next message")),
            ("k  or  ↑", i18n_noop("Previous message")),
            ("l  or  →", i18n_noop("Open the selected message")),
            ("h  or  ←  or  u", i18n_noop("Back to the message list")),
            ("w", i18n_noop("Next message in the conversation")),
            ("b", i18n_noop("Previous message in the conversation")),
            ("/  or  Ctrl+F", i18n_noop("Search")),
        ],
    ),
    (
        i18n_noop("Act on a message"),
        &[
            ("r  or  Ctrl+R", i18n_noop("Reply")),
            ("R  or  Ctrl+Shift+R", i18n_noop("Reply to all")),
            ("f", i18n_noop("Forward")),
            ("a", i18n_noop("Archive")),
            ("d  or  Delete", i18n_noop("Delete")),
            (
                "D  or  Shift+Delete",
                i18n_noop("Delete permanently, without going to Trash"),
            ),
            ("!", i18n_noop("Mark as spam")),
            ("s", i18n_noop("Star or unstar")),
            ("m", i18n_noop("Mark read or unread")),
            ("x", i18n_noop("Select this row (for a bulk action)")),
            (
                "1 … 9",
                i18n_noop("Add or remove a tag (the first nine, in Settings order)"),
            ),
            ("0", i18n_noop("Remove every tag")),
            ("Ctrl+U", i18n_noop("View Source")),
            ("Ctrl+P", i18n_noop("Print the message you are reading")),
            ("Ctrl+Shift+P", i18n_noop("Preview it as a PDF first")),
        ],
    ),
    (
        i18n_noop("Writing"),
        &[
            ("c  or  Ctrl+N", i18n_noop("Compose")),
            ("Ctrl+Shift+N", i18n_noop("New message from a template")),
            ("Ctrl+Enter", i18n_noop("Send the message you are writing")),
            ("Ctrl+Shift+A", i18n_noop("Attach files")),
            (
                "Ctrl+Shift+T  /  C  /  B",
                i18n_noop("Go to To, Cc or Bcc, showing the row"),
            ),
            (
                "Esc",
                i18n_noop("Back out of a reply and return to the list"),
            ),
            (
                "Ctrl+.  or  Ctrl+;",
                i18n_noop("Emoji chooser, at the cursor"),
            ),
            (
                "Shift+Return",
                i18n_noop("New paragraph (Return starts a new line, unless Settings swaps them)"),
            ),
            (
                "Ctrl+B  /  Ctrl+I  /  Ctrl+U",
                i18n_noop("Bold, italic, underline"),
            ),
        ],
    ),
    (
        i18n_noop("Attachments"),
        &[
            (
                "Space  or  Enter",
                i18n_noop("Preview the highlighted attachment"),
            ),
            (
                "←  /  →",
                i18n_noop("Previous or next in the gallery's preview"),
            ),
            ("Esc", i18n_noop("Close the gallery's preview")),
        ],
    ),
    (
        i18n_noop("Everything else"),
        &[
            (
                "Ctrl+Z",
                i18n_noop("Undo the last action, or the last edit while you are writing"),
            ),
            ("Ctrl+Shift+Z", i18n_noop("Redo it (Ctrl+Y does the same)")),
            (
                "Ctrl+Shift+S",
                i18n_noop("Reveal the status bar (also: long-press Refresh)"),
            ),
            (
                "Ctrl+Shift+A",
                i18n_noop("Show or hide the accounts in the sidebar"),
            ),
            ("Ctrl+Shift+F", i18n_noop("Focus Mode on or off")),
            ("Ctrl++  /  Ctrl+-", i18n_noop("Message zoom in or out")),
            ("Ctrl+0", i18n_noop("Message zoom back to the default")),
            ("Ctrl+,", i18n_noop("Settings")),
            (
                "Ctrl+F",
                i18n_noop("Search the Settings window, when it is open"),
            ),
            (
                "Ctrl+Shift+C",
                i18n_noop("Console mode (when enabled in Settings)"),
            ),
            (
                "Ctrl+W",
                i18n_noop("Close the window (background sync keeps running)"),
            ),
            ("Ctrl+Q", i18n_noop("Quit Hylki entirely")),
            (
                "Ctrl+K",
                i18n_noop("Command palette: search actions, folders and tags"),
            ),
            ("Ctrl+Shift+K", i18n_noop("Command palette, actions only")),
            ("?  or  Ctrl+?  or  F1", i18n_noop("This list")),
        ],
    ),
];

/// Gmail's keys (#392), shown in place of the first two sections of
/// `SHORTCUT_HELP` when Settings turns them on.
pub(super) const GMAIL_HELP: &[(&str, &[(&str, &str)])] = &[
    (
        i18n_noop("Move around"),
        &[
            ("j  /  k", i18n_noop("Move down or up the list without opening; while reading, open the next or previous conversation")),
            ("o  or  Enter", i18n_noop("Open the message the cursor is on")),
            ("u  or  Esc", i18n_noop("Close the conversation, back to the list")),
            ("n  /  p", i18n_noop("Next or previous message in the conversation")),
            (";  /  :", i18n_noop("Expand or collapse every message of the conversation")),
            ("g  i", i18n_noop("Go to Inbox")),
            ("g  s", i18n_noop("Go to Starred")),
            ("g  t", i18n_noop("Go to Sent")),
            ("g  d", i18n_noop("Go to Drafts")),
            ("g  a", i18n_noop("Go to Archive")),
            ("g  c", i18n_noop("Go to Contacts")),
            ("/", i18n_noop("Search")),
        ],
    ),
    (
        i18n_noop("Act on a message"),
        &[
            ("r", i18n_noop("Reply")),
            ("a", i18n_noop("Reply to all")),
            ("f", i18n_noop("Forward")),
            ("e", i18n_noop("Archive the conversation")),
            ("]  or  }", i18n_noop("Archive the conversation and open the older one")),
            ("[  or  {", i18n_noop("Archive the conversation and open the newer one")),
            ("#", i18n_noop("Delete")),
            ("Shift+Delete", i18n_noop("Delete permanently, without going to Trash")),
            ("!", i18n_noop("Mark as spam")),
            ("s", i18n_noop("Star or unstar")),
            ("Shift+I", i18n_noop("Mark the conversation as read")),
            ("Shift+U", i18n_noop("Mark the conversation as unread and close it")),
            ("_", i18n_noop("Mark as unread from this message on and close it")),
            ("x", i18n_noop("Select this row without opening it")),
            ("*  a  /  *  n", i18n_noop("Select every row, or none")),
            ("*  r  /  *  u", i18n_noop("Select the read or the unread conversations")),
            ("*  s  /  *  t", i18n_noop("Select the starred or the unstarred conversations")),
            ("v", i18n_noop("Move to a folder")),
            ("l", i18n_noop("Tags")),
            ("z", i18n_noop("Undo the last action")),
            ("1 … 9", i18n_noop("Add or remove a tag (the first nine, in Settings order)")),
            ("Shift+R  /  Shift+A", i18n_noop("Reply, or reply to all, in a window")),
            ("Shift+F", i18n_noop("Forward in a window")),
            ("c", i18n_noop("Compose")),
            ("d", i18n_noop("Compose in a window")),
            ("Shift+N", i18n_noop("Check for new mail")),
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shortcut_is_documented() {
        // The reference window is the only place the keys are written down, so a
        // new shortcut without a line there would be invisible.
        let documented: Vec<&str> = SHORTCUT_HELP
            .iter()
            .flat_map(|(_, keys)| keys.iter().map(|(key, _)| *key))
            .collect();
        for key in [
            "j  or  ↓",
            "r  or  Ctrl+R",
            "R  or  Ctrl+Shift+R",
            "c  or  Ctrl+N",
            "Ctrl+U",
            "a",
            "d  or  Delete",
            "w",
            "b",
            "x",
            "?  or  Ctrl+?  or  F1",
            "1 … 9",
            "0",
            "/  or  Ctrl+F",
            "Ctrl+.  or  Ctrl+;",
        ] {
            assert!(documented.contains(&key), "{key} is not in the reference");
        }
        // Every documented line has a description.
        for (_, keys) in SHORTCUT_HELP {
            for (key, what) in *keys {
                assert!(!key.trim().is_empty() && !what.trim().is_empty());
            }
        }
    }
}
