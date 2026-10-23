//! Deleting an attachment from a message on the server (#289): append the
//! rewritten copy, find it again, then purge the original.

use async_imap::types::Fetch;
use futures::TryStreamExt;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::worker::imap::mutate::{kept_flags, purge_messages};
use crate::worker::imap::retry::load_raw_retry;
use crate::worker::imap::summary::message_id_of;
use crate::worker::imap::wire::{append_dated, fetch_uids, sel};
use crate::worker::imap::ImapSession;
use crate::worker::render::attachments::{extract_attachments, guess_mime};
use crate::worker::{strip, WorkerEvent};

/// Put what a message holds after an attachment was removed (#289) into
/// the cache: the files, the paperclip, and the gallery's index of them.
pub(in crate::worker) fn cache_rewritten(
    c: &Cache,
    account_id: u32,
    path: &str,
    uid: u32,
    raw: &[u8],
) {
    let items = extract_attachments(raw);
    c.save_attachments(account_id, path, uid, &items);
    c.mark_attachments_checked(account_id, path, uid);
    c.set_has_attachment(account_id, path, uid, !items.is_empty());
    let metas: Vec<crate::models::AttachmentMeta> = items
        .iter()
        .enumerate()
        .map(|(i, a)| crate::models::AttachmentMeta {
            idx: i as u32,
            name: a.name.clone(),
            mime: guess_mime(&a.name),
            size: a.data.len() as u64,
            section: String::new(),
        })
        .collect();
    c.save_attachment_meta(account_id, path, uid, &metas);
}

/// An attachment removal that did not happen (#289): the error, and word
/// to the views showing the file as being deleted that it is not.
pub(in crate::worker) fn strip_refused(
    emit: &impl Fn(WorkerEvent),
    text: String,
    message_id: u32,
    name: String,
    size: u64,
) {
    emit(WorkerEvent::error(text));
    emit(WorkerEvent::AttachmentNotDeleted {
        message_id,
        name,
        size,
    });
}

/// Why [`strip_imap_attachment`] failed, and whether the connection went
/// with it.
pub(in crate::worker) struct StripFailure {
    pub(in crate::worker) text: String,
    pub(in crate::worker) connection: bool,
}

impl From<async_imap::error::Error> for StripFailure {
    fn from(e: async_imap::error::Error) -> Self {
        StripFailure {
            text: e.to_string(),
            connection: true,
        }
    }
}

/// Replace message `uid` in `path` with a copy that lacks the attachment
/// (#289): the copy keeps the original's flags and arrival date, and is
/// stored before the original is deleted, so a failure part way leaves the
/// message as it was, or at worst twice. Returns the copy's UID, when it
/// can be found again, and its bytes.
pub(in crate::worker) async fn strip_imap_attachment(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uid: u32,
    name: &str,
    size: u64,
) -> Result<(Option<u32>, Vec<u8>), StripFailure> {
    let raw = load_raw_retry(session, account, path, uid).await?;
    let stripped =
        strip::without_attachment(&raw, name, size, &strip::note_date()).map_err(|e| {
            StripFailure {
                text: e.text(),
                connection: false,
            }
        })?;
    let sess = session.as_mut().expect("session ensured before call");
    let uid_next = sel(sess, path).await?.uid_next;
    let fetches: Vec<Fetch> = fetch_uids(
        sess,
        uid.to_string(),
        "(UID FLAGS INTERNALDATE BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
    )
    .await?
    .try_collect()
    .await?;
    let meta = fetches.iter().find(|f| f.uid == Some(uid));
    let flags = meta
        .map(|f| kept_flags(f.flags()))
        .filter(|f| !f.is_empty())
        .map(|f| format!("({})", f.join(" ")));
    let date = meta
        .and_then(|f| f.internal_date())
        .map(|d| format!("\"{}\"", d.format("%d-%b-%Y %H:%M:%S %z")));
    let msgid = meta.map(message_id_of).unwrap_or_default();
    append_dated(sess, path, flags.as_deref(), date.as_deref(), &stripped).await?;
    let new_uid = find_appended(sess, uid, uid_next, &msgid).await;
    purge_messages(sess, path, &[uid]).await?;
    Ok((new_uid, stripped))
}

/// The UID an APPEND just gave a message: among those at or above the
/// folder's UIDNEXT from before it, the one with its Message-ID (or, with
/// none, the lowest). Read from the headers rather than searched for, as
/// Stalwart's full-text index answers `SEARCH HEADER Message-ID` with nothing.
pub(in crate::worker) async fn find_appended(
    sess: &mut ImapSession,
    old_uid: u32,
    uid_next: Option<u32>,
    msgid: &str,
) -> Option<u32> {
    let from = uid_next.unwrap_or(0).max(old_uid + 1);
    let fetches: Vec<Fetch> = fetch_uids(
        sess,
        format!("{from}:*"),
        "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
    )
    .await
    .ok()?
    .try_collect()
    .await
    .ok()?;
    // `n:*` always takes in the highest UID, even one below `n`.
    let mut found: Vec<(u32, String)> = fetches
        .iter()
        .filter_map(|f| f.uid.filter(|&u| u >= from).map(|u| (u, message_id_of(f))))
        .collect();
    found.sort();
    match msgid {
        "" => found.first().map(|(u, _)| *u),
        id => found.iter().find(|(_, m)| m == id).map(|(u, _)| *u),
    }
}
