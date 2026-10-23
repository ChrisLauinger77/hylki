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

/// One attachment of one message on the server, to be removed (#289).
#[derive(Debug, Clone)]
pub(crate) struct AttachmentTarget {
    pub(super) account_id: u32,
    pub(super) message_id: u32,
    pub(super) path: String,
    pub(super) uid: u32,
    pub(super) name: String,
    pub(super) size: u64,
}

impl AttachmentTarget {
    pub(super) fn gallery_key(&self) -> crate::ui::attachments_gallery::FileKey {
        (
            self.account_id,
            self.path.clone(),
            self.uid,
            self.name.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_targets_key_the_gallery_by_path_uid_and_name() {
        let t = AttachmentTarget {
            account_id: 1,
            message_id: 2,
            path: "INBOX".into(),
            uid: 3,
            name: "a.pdf".into(),
            size: 4,
        };
        assert_eq!(
            t.gallery_key(),
            (1, "INBOX".to_string(), 3, "a.pdf".to_string())
        );
    }
}
