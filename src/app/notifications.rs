//! Desktop notifications for new mail and what their buttons do.

use super::*;

/// Resolve a notification's `(account_id, folder_id, message_id)` to the full
/// cached [`Message`] (#38): the notified message may not be in the current
/// view, but the worker upserts it into the cache before notifying, so the
/// cache always has it.
pub(super) fn notified_message(
    account_id: u32,
    folder_id: u32,
    message_id: u32,
    folders: &std::collections::HashMap<u32, Vec<Folder>>,
) -> Option<Message> {
    let path = folders
        .get(&account_id)?
        .iter()
        .find(|f| f.id == folder_id)?
        .path
        .clone();
    let cache = crate::cache::Cache::open().ok()?;
    cache
        .load_messages(account_id, &path, folder_id)
        .into_iter()
        .find(|m| m.id == message_id)
}

impl AppModel {
    /// Tell the person about new Inbox mail: the notification, with its
    /// sound, while the window is in the background or closed; the sound
    /// alone while it is in front, when Settings asks for that (#337).
    /// GNOME shows no banner for the app in front, and neither does Hylki.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn announce_new_mail(
        &self,
        account_id: u32,
        folder_id: u32,
        message_id: u32,
        from: &str,
        subject: &str,
        others: usize,
        in_place: bool,
    ) {
        // HYLKI_SHOWCASE_IN_FRONT (demo) stands in for the focus, which a
        // window opened from a script never gets.
        let in_front = self.window.is_active()
            || (demo_mode() && std::env::var_os("HYLKI_SHOWCASE_IN_FRONT").is_some());
        if !in_front {
            crate::notify::new_mail(
                account_id, folder_id, message_id, from, subject, others, in_place,
            );
        } else if crate::config::sound_when_focused() {
            tracing::info!("new mail with the window in front: the sound alone");
            crate::notify::sound_for_new_mail();
        } else {
            tracing::debug!("new mail with the window in front: no notification, no sound");
        }
    }

    pub(super) fn show_toast(&self, toast: &adw::Toast) {
        if let Some(t) = self.toasts.get() {
            t.add_toast(toast.clone());
        }
    }
}
