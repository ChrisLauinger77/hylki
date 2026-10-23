//! The composer in its host: inline or in a window, its size, its fields shown
//! or folded, and leaving it.

use super::messages::ComposeInput;
use super::Compose;
use super::ComposeWidgets;
use crate::i18n::i18n;
use adw::prelude::*;
use relm4::prelude::*;

/// The narrowest the compose pane goes (its `adw::BreakpointBin` floor).
pub(super) const COMPOSE_MIN_WIDTH: i32 = 360;

/// Fallback fold threshold when the toolbar could not be measured at init.
pub(super) const COMPOSE_ACTIONS_BREAKPOINT: f64 = 620.0;

/// The width a header bar's rows need side by side: the natural widths of
/// its centre box's children (start row, title, end row) summed — what the
/// bar itself reports doubles the wider side to keep the title centred.
/// `None` when the bar's insides are not the expected shape.
pub(super) fn header_rows_width(header: &gtk::Widget) -> Option<i32> {
    fn find_center_box(w: &gtk::Widget, depth: u32) -> Option<gtk::Widget> {
        if w.type_().name() == "GtkCenterBox" {
            return Some(w.clone());
        }
        if depth == 0 {
            return None;
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if let Some(found) = find_center_box(&child, depth - 1) {
                return Some(found);
            }
            c = child.next_sibling();
        }
        None
    }
    let cb = find_center_box(header, 6)?;
    let mut total = 0;
    let mut c = cb.first_child();
    while let Some(w) = c {
        total += w.measure(gtk::Orientation::Horizontal, -1).1;
        c = w.next_sibling();
    }
    Some(total)
}

/// Size the pane for its host. Both hosts impose a definite height now — the
/// reader-covering overlay inline (it fills the whole pane), the window itself
/// popped out — so the editor always expands to fill whatever it is given.
/// Inline, the compose header also stands in for the reader's (which it
/// covers, or tops), so it takes over the GNOME window decorations — unless
/// `decorations` is off: a split reply beneath the reader leaves the reader's
/// own header in place, and a second set of controls mid-pane would be wrong.
/// In a window of its own the header is the window's, close button and all.
pub(super) fn size_for_host(
    root: &adw::ToolbarView,
    header: &adw::HeaderBar,
    editor_holder: &gtk::Box,
    windowed: bool,
    decorations: bool,
) {
    root.set_vexpand(true);
    editor_holder.set_vexpand(true);
    editor_holder.set_height_request(-1);
    header.set_show_end_title_buttons(windowed || decorations);
}

/// Set the inline/window toggle button's icon + tooltip for the current host.
pub(super) fn set_toggle_icon(btn: &gtk::Button, windowed: bool) {
    if windowed {
        btn.set_icon_name("view-restore-symbolic");
        btn.set_tooltip_text(Some(i18n("Collapse into reader").as_str()));
    } else {
        btn.set_icon_name("view-fullscreen-symbolic");
        btn.set_tooltip_text(Some(i18n("Open in window").as_str()));
    }
}

/// Save the edited message, discard it, or go back to it. Escape answers
/// Keep Editing, so a second press never discards what the first one asked
/// about (#290).
pub(super) fn confirm_discard_dialog(
    parent: Option<&gtk::Window>,
    template: bool,
    sender: relm4::Sender<ComposeInput>,
) {
    let (heading, body, save) = if template {
        (
            i18n("Save the template?"),
            i18n("Its changes have not been saved. Save them to the template, or discard them."),
            i18n("Save Template"),
        )
    } else {
        (
            i18n("Save the message?"),
            i18n("It has not been sent. Save it to Drafts to finish later, or discard it."),
            i18n("Save Draft"),
        )
    };
    let dialog = adw::MessageDialog::new(parent, Some(heading.as_str()), Some(body.as_str()));
    dialog.add_response("keep", &i18n("Keep Editing"));
    dialog.add_response("discard", &i18n("Discard"));
    dialog.add_response("save", &save);
    dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("keep");
    dialog.connect_response(None, move |_, resp| {
        let _ = sender.send(match resp {
            "save" => ComposeInput::SaveDraft,
            "discard" => ComposeInput::Discard,
            _ => ComposeInput::KeepEditing,
        });
    });
    dialog.present();
}

impl Compose {
    /// Where the cursor starts: in To while the message is addressed to
    /// nobody, in Subject once it is addressed but untitled, and in the body
    /// when both are filled in, which is every reply. A new message used to
    /// open in the body like a reply, where other mail clients start from
    /// the recipients (#266). A compact reply with its field rows folded
    /// away always starts in the body.
    pub(super) fn focus_initial(&self, widgets: &ComposeWidgets) {
        let fields = widgets.fields_list.is_visible();
        if fields && widgets.to_row.text().trim().is_empty() {
            widgets.to_row.grab_focus();
        } else if fields && widgets.subject_row.text().trim().is_empty() {
            widgets.subject_row.grab_focus();
        } else {
            self.editor.grab_focus();
        }
    }
}
