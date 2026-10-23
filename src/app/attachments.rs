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

impl AppModel {
    pub(super) fn sync_attachment_drawer(&self) {
        // Switched off (#213), the drawer is simply never given anything:
        // empty hides it, seam and all.
        let items = if self.drawer_enabled {
            self.attachments.clone()
        } else {
            Vec::new()
        };
        self.attachment_drawer
            .emit(AttachmentDrawerInput::SetItems(items));
        self.push_card_attachments();
    }

    /// The messages on screen: the open conversation, or the lone message.
    pub(super) fn conversation_members(&self) -> Vec<(u32, u32)> {
        if self.current_thread.is_empty() {
            self.current.iter().map(|c| (c.account_id, c.id)).collect()
        } else {
            self.current_thread
                .iter()
                .map(|m| (m.account_id, m.id))
                .collect()
        }
    }

    /// Where the message on screen that goes by `(account, id)` lives, as
    /// the caches key it (see [`body_key`]).
    pub(super) fn member_key(&self, (account_id, id): (u32, u32)) -> Option<BodyKey> {
        self.current_thread
            .iter()
            .chain(self.current.iter())
            .find(|m| m.account_id == account_id && m.id == id)
            .map(body_key)
    }

    /// The message a worker event is about, as the caches key it: `message_id`
    /// in the folder at `path`. An event names a message by the id it was
    /// asked for under, which for a member opened out from another folder is
    /// the id it goes by here, so that is taken back to its UID. `None` for a
    /// folder the app does not know.
    pub(super) fn event_key(
        &self,
        account_id: u32,
        path: &str,
        message_id: u32,
    ) -> Option<BodyKey> {
        let fid = self
            .folders
            .get(&account_id)?
            .iter()
            .find(|f| f.path == path)?
            .id;
        let uid = self
            .related_ids
            .iter()
            .find(|((a, f, _), id)| *a == account_id && *f == fid && **id == message_id)
            .map_or(message_id, |((_, _, uid), _)| *uid);
        Some((account_id, fid, uid))
    }

    /// Give the message at `key` its paperclip or take it away, in its own
    /// folder only, where the list keeps it and on its row.
    pub(super) fn set_has_attachment(&mut self, key: Option<BodyKey>, has: bool) {
        let Some((account_id, folder_id, uid)) = key else {
            return;
        };
        if let Some(msgs) = self.message_cache.get_mut(&(account_id, folder_id)) {
            for m in msgs.iter_mut().filter(|m| m.uid == uid) {
                m.has_attachment = has;
            }
        }
        self.message_list.emit(MessageListInput::SetHasAttachment {
            account_id,
            folder_id,
            uid,
            has,
        });
    }

    /// Hand the reader what each message on screen has attached (#213), as
    /// far as the cache knows: names and sizes only, never the bytes — the
    /// document lists them, and the app opens or saves them on request.
    fn push_card_attachments(&self) {
        use crate::ui::message_view::CardAttachment;
        let mut map: HashMap<(u32, u32), Vec<CardAttachment>> = HashMap::new();
        for key in self.conversation_members() {
            if let Some(items) = self
                .member_key(key)
                .and_then(|k| self.attachment_cache.get(&k))
            {
                let atts: Vec<CardAttachment> = items
                    .iter()
                    .map(|a| CardAttachment {
                        name: a.name.clone(),
                        size: a.data.len() as u64,
                    })
                    .collect();
                if !atts.is_empty() {
                    map.insert(key, atts);
                }
            }
        }
        self.message_view
            .emit(MessageViewInput::SetCardAttachments(map));
    }

    /// Fill the drawer for a whole conversation: ask the disk cache for every
    /// member's attachments (never the network) and show what's already in
    /// hand. Opening a thread used to load nothing at all — the drawer only
    /// appeared after clicking a member, which runs the single-message path.
    pub(super) fn load_thread_attachments(&mut self) {
        let wanted: Vec<(u32, u32, u32, String)> = self
            .current_thread
            .iter()
            .filter(|tm| tm.has_attachment)
            .filter(|tm| !self.attachment_cache.contains_key(&body_key(tm)))
            .filter_map(|tm| {
                self.resolve_folder_path(tm)
                    .map(|p| (tm.account_id, tm.id, tm.uid, p))
            })
            .collect();
        if !wanted.is_empty() {
            self.attachments_loading = true;
        }
        for (account_id, message_id, uid, path) in wanted {
            self.send_to(
                account_id,
                MailRequest::LoadAttachments {
                    message_id,
                    path,
                    uid,
                    download: true,
                },
            );
        }
        self.refresh_thread_attachments();
    }

    /// Point the drawer at the union of cached attachments across the open
    /// conversation. A reply pulled in from Sent can duplicate what it was
    /// sent with, so identical (name, size) pairs are shown once.
    pub(super) fn refresh_thread_attachments(&mut self) {
        let mut seen = HashSet::new();
        let mut merged = Vec::new();
        for tm in &self.current_thread {
            if let Some(items) = self.attachment_cache.get(&body_key(tm)) {
                for a in items {
                    if seen.insert((a.name.clone(), a.data.len())) {
                        merged.push(a.clone());
                    }
                }
            }
        }
        self.attachments = merged;
        self.sync_attachment_drawer();
    }

    /// The message on screen that carries the attachment `name` (of `len`
    /// bytes, when known), as the drawer's Show in Message finds it.
    pub(super) fn attachment_target(
        &self,
        name: &str,
        len: Option<usize>,
    ) -> Option<AttachmentTarget> {
        let (m, size) = self
            .current_thread
            .iter()
            .chain(self.current.iter())
            .find_map(|m| {
                let items = self.attachment_cache.get(&body_key(m))?;
                let a = items
                    .iter()
                    .find(|a| a.name == name && len.is_none_or(|l| a.data.len() == l))?;
                Some((m, a.data.len() as u64))
            })?;
        Some(AttachmentTarget {
            account_id: m.account_id,
            message_id: m.id,
            path: self.resolve_folder_path(m)?,
            uid: m.uid,
            name: name.to_string(),
            size,
        })
    }

    /// Forget a deletion the worker has answered, handing back what it was.
    pub(super) fn take_deleting(
        &mut self,
        account_id: u32,
        message_id: u32,
        name: &str,
    ) -> Option<AttachmentTarget> {
        let at = self.deleting_attachments.iter().position(|t| {
            t.account_id == account_id && t.message_id == message_id && t.name == name
        })?;
        Some(self.deleting_attachments.remove(at))
    }

    /// Removing an attachment cannot be undone, and reaches every device
    /// that reads the account: ask (#289).
    pub(super) fn confirm_delete_attachment(
        &self,
        target: AttachmentTarget,
        parent: &gtk::Window,
        sender: &ComponentSender<Self>,
    ) {
        let heading = i18n("Delete the attachment from the server?");
        let body = i18n_f(
            "“{name}” will be removed from the message on the server. The rest of the message stays. This can’t be undone.",
            &[("name", &target.name)],
        );
        let dialog = adw::MessageDialog::new(Some(parent), Some(&heading), Some(&body));
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("delete", &i18n("Delete"));
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let s = sender.clone();
        let target = std::cell::RefCell::new(Some(target));
        dialog.connect_response(None, move |_, resp| {
            if resp == "delete" {
                if let Some(t) = target.borrow_mut().take() {
                    s.input(AppMsg::DeleteAttachment(t));
                }
            }
        });
        dialog.present();
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
