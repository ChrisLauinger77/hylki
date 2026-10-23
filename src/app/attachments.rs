//! Attachments: saving them to disk, and pruning old downloads.

use super::*;

/// Save one attachment via a file chooser (a card chip's save button, #213).
pub(super) fn save_attachment(att: Attachment, parent: Option<adw::ApplicationWindow>) {
    let dialog = gtk::FileDialog::builder()
        .initial_name(&att.name)
        .title(&i18n("Save Attachment"))
        .build();
    dialog.save(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res {
            if let Some(path) = file.path() {
                let _ = std::fs::write(path, &att.data);
            }
        }
    });
}

/// Ask for a folder and write every attachment into it.
fn save_all_attachments(atts: Vec<Attachment>, parent: Option<adw::ApplicationWindow>) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title(&i18n("Save All Attachments"));
    dialog.select_folder(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
        if let Ok(folder) = res {
            if let Some(dir) = folder.path() {
                for att in &atts {
                    let safe = att.name.replace(['/', '\\'], "_");
                    let _ = std::fs::write(dir.join(&safe), &att.data);
                }
            }
        }
    });
}

/// Remove downloaded mail older than `days` (#394), off the main thread;
/// 0 keeps it all.
pub(super) fn prune_downloads(days: u32) {
    if days == 0 {
        return;
    }
    let before = crate::datefmt::now() - i64::from(days) * 86_400;
    std::thread::spawn(move || {
        match crate::cache::Cache::open().and_then(|c| c.remove_downloaded(Some(before))) {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!("storage: removed {n} downloaded message(s) older than {days} days")
            }
            Err(e) => tracing::warn!("storage: could not remove old downloaded mail: {e}"),
        }
    });
}
