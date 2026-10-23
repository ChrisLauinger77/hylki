//! Settings → System helpers: the GNOME Files extension, downloaded mail,
//! links and error reports.

use super::*;

/// What the Links group says above the combo — which depends on whether
/// there are any browsers to name.
pub(super) fn link_group_description(browsers: &[crate::ui::launch::Browser]) -> String {
    if browsers.is_empty() {
        i18n(
            "Inside the Flatpak sandbox, links open through the desktop's chooser. \"Ask each time\" picks the browser per link with the option to remember it.",
        )
    } else {
        i18n("\"Ask each time\" lets the desktop ask before each link.")
    }
}

/// The Files extension row's subtitle: where it stands, and where it lives.
pub(super) fn nautilus_status_text(state: &crate::nautilus_ext::State) -> String {
    use crate::nautilus_ext::Status;
    let state_text = match state.status {
        Status::NotInstalled => i18n("Not installed"),
        Status::Installed if state.loaded => i18n("Installed and loaded by Files"),
        Status::Installed if state.loader == Some(false) => {
            i18n("Installed, but nautilus-python is missing")
        }
        Status::Installed => i18n("Installed. Restart Files to load it; it needs nautilus-python"),
        Status::Outdated => i18n("Installed, but from another version"),
    };
    let state = state_text;
    match crate::nautilus_ext::path() {
        Some(p) => {
            let shown = p.display().to_string();
            let shown = match std::env::var("HOME") {
                Ok(home) if !home.is_empty() && shown.starts_with(&home) => {
                    format!("~{}", &shown[home.len()..])
                }
                _ => shown,
            };
            format!("{state} — {shown}")
        }
        None => state,
    }
}

/// Hide the "editable" pencil an `adw::EntryRow` draws whatever its
/// `editable` says (the icon carries the `edit-icon` style class).
pub(super) fn hide_edit_icon(widget: &gtk::Widget) {
    let mut child = widget.first_child();
    while let Some(c) = child {
        if c.has_css_class("edit-icon") {
            c.set_visible(false);
        } else {
            hide_edit_icon(&c);
        }
        child = c.next_sibling();
    }
}

/// What the Downloaded mail row says.
pub(super) fn storage_text(storage: Option<(u64, u64, u64)>) -> String {
    match storage {
        None => i18n("Measuring…"),
        Some((bodies, files, total)) => i18n_f(
            "Messages {bodies}, attachments {files}. {total} in all, with the message list.",
            &[
                ("bodies", gtk::glib::format_size(bodies).as_str()),
                ("files", gtk::glib::format_size(files).as_str()),
                ("total", gtk::glib::format_size(total).as_str()),
            ],
        ),
    }
}

/// A plain error alert over the settings window.
pub(super) fn report(parent: &adw::Window, heading: &str, body: &str) {
    let dialog = adw::MessageDialog::new(Some(parent), Some(heading), Some(body));
    dialog.add_response("ok", &i18n("OK"));
    dialog.set_close_response("ok");
    dialog.present();
}

#[cfg(test)]
mod tests {
    use super::super::choices::{KEEP_DOWNLOADS_DAYS, RENDERERS};
    use super::*;

    #[test]
    fn system_page_choices() {
        assert_eq!(RENDERERS, ["", "vulkan", "gl", "cairo"]);
        assert_eq!(KEEP_DOWNLOADS_DAYS, [0, 365, 90, 30]);
        assert_eq!(storage_text(None), "Measuring…");
        assert_eq!(
            storage_text(Some((1000, 2000, 3000))),
            "Messages 1.0\u{a0}kB, attachments 2.0\u{a0}kB. 3.0\u{a0}kB in all, with the message list."
        );
    }
}
