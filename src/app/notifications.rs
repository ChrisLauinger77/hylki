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
