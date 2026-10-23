//! Background downloads while the connection is idle: new mail's bodies,
//! so it opens with no wait, and attachments, so the paperclip and the
//! gallery are right.

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::ni18n_f;
use crate::models::Message;
use crate::worker::imap::connect::connect_and_list;
use crate::worker::imap::retry::load_raw_retry;
use crate::worker::imap::{ImapSession, PREFETCH_BODY_LIMIT, PREFETCH_LIMIT};
use crate::worker::render::attachments::attachments_of;
use crate::worker::{render_raw, WorkerEvent};

/// Messages the attachment prefetch has already tried this session, as
/// (account, folder path, uid). A message the prefetch cannot finish — OpenPGP
/// mail, which is left for an explicit open, or a read that fails to parse —
/// is never marked checked in the cache, and without this it was queued again
/// by the very resync the prefetch triggers, then fetched again, then queued
/// again: a full download of the same message several times a second, each
/// one followed by a folder listing and a rebuild of the message list, for as
/// long as it stayed among the newest rows. PGP/MIME mail always carries the
/// attachment flag (its encrypted part), so every PGP user with a recent
/// encrypted message sat in that loop.
static ATTACHMENT_PREFETCH_TRIED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<(u32, String, u32)>>,
> = std::sync::LazyLock::new(Default::default);

pub(in crate::worker) fn attachment_prefetch_tried(account_id: u32, path: &str, uid: u32) -> bool {
    ATTACHMENT_PREFETCH_TRIED
        .lock()
        .map(|s| s.contains(&(account_id, path.to_string(), uid)))
        .unwrap_or(false)
}

pub(in crate::worker) fn note_attachment_prefetch_tried(account_id: u32, path: &str, uid: u32) {
    if let Ok(mut s) = ATTACHMENT_PREFETCH_TRIED.lock() {
        s.insert((account_id, path.to_string(), uid));
    }
}

/// Download and cache one queued attachment (body + attachments), if the session
/// is live. Connects on demand; clears the queue if offline.
pub(in crate::worker) async fn run_one_prefetch(
    prefetch: &mut std::collections::VecDeque<(String, u32)>,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some((path, uid)) = prefetch.front().cloned() else {
        return;
    };
    if session.is_none() {
        *session = connect_and_list(account_id, account, cache, emit).await;
    }
    if session.is_some() {
        prefetch.pop_front();
        // Tried is tried, whatever comes of it: the resync this pass ends
        // with must not queue the same message again.
        note_attachment_prefetch_tried(account_id, &path, uid);
        let already = cache
            .map(|c| c.attachments_checked(account_id, &path, uid))
            .unwrap_or(false);
        if !already {
            if let Ok(raw) = load_raw_retry(session, account, &path, uid).await {
                // OpenPGP mail is left for an explicit open (#133): nothing
                // of it is cached, and decrypting here would raise the
                // passphrase prompt out of nowhere.
                if crate::pgp::detect(&raw).is_some() {
                    emit(WorkerEvent::Status(prefetch_status(prefetch.len())));
                    return;
                }
                let (body, check, _) = render_raw(&raw);
                let (attachments, _) = attachments_of(&raw);
                emit(WorkerEvent::SenderChecked {
                    path: path.to_string(),
                    message_id: uid,
                    check: check.clone(),
                });
                if let Some(c) = cache {
                    c.save_body(account_id, &path, uid, &body);
                    c.save_sender_check(account_id, &path, uid, &check);
                    c.save_attachments(account_id, &path, uid, &attachments);
                    // Mark as fetched so it's never re-downloaded to re-check,
                    // even if it had no attachments after all.
                    c.mark_attachments_checked(account_id, &path, uid);
                }
                // Correct a false paperclip live: a message flagged as having an
                // attachment but with none once fetched (iCloud multipart/mixed
                // wrapping only inline images).
                if attachments.is_empty() {
                    emit(WorkerEvent::NoAttachments {
                        path: path.to_string(),
                        message_id: uid,
                    });
                }
            }
        }
        emit(WorkerEvent::Status(prefetch_status(prefetch.len())));
    } else {
        prefetch.clear();
        emit(WorkerEvent::Status(String::new()));
    }
}

/// Queue the newest messages for background body prefetch, so opening new mail is
/// instant. The prefetch both caches the body on disk *and* pushes it to the UI's
/// in-memory cache (see [`run_one_body_prefetch`]), so a click needs no round-trip.
/// Skips messages already pushed this session. Called after every folder sync.
pub(super) fn queue_body_prefetch(
    queue: &mut std::collections::VecDeque<(String, u32)>,
    path: &str,
    messages: &[Message],
    emitted: &std::collections::HashSet<(String, u32)>,
) {
    let mut recent: Vec<&Message> = messages.iter().collect();
    recent.sort_by(|a, b| b.uid.cmp(&a.uid)); // newest first
    for m in recent.into_iter().take(PREFETCH_BODY_LIMIT) {
        let key = (path.to_string(), m.uid);
        if emitted.contains(&key) {
            continue;
        }
        if queue.iter().any(|(p, u)| p == path && *u == m.uid) {
            continue;
        }
        queue.push_back(key);
    }
}

/// Whether a folder's attachments feed the gallery (so its mail is worth
/// prefetching): everything except Trash, Junk and Drafts.
pub(super) fn gallery_folder(kind: crate::models::FolderKind) -> bool {
    use crate::models::FolderKind::*;
    !matches!(kind, Trash | Junk | Drafts)
}

/// Gallery eligibility of a folder by path, looked up from the cached folder list.
/// Unknown folders default to eligible.
pub(super) fn folder_is_gallery(cache: Option<&Cache>, account_id: u32, path: &str) -> bool {
    cache
        .map(|c| {
            c.load_folders(account_id)
                .iter()
                .find(|f| f.path == path)
                .map(|f| gallery_folder(f.kind))
                .unwrap_or(true)
        })
        .unwrap_or(true)
}

/// Queue the newest messages that have attachments (not already cached) for
/// background attachment pre-download, so new mail's attachments are ready too.
pub(in crate::worker) fn queue_attachment_prefetch(
    queue: &mut std::collections::VecDeque<(String, u32)>,
    path: &str,
    messages: &[Message],
    cache: Option<&Cache>,
    account_id: u32,
) {
    let mut recent: Vec<&Message> = messages.iter().collect();
    recent.sort_by(|a, b| b.uid.cmp(&a.uid)); // newest first
    for m in recent.into_iter().take(PREFETCH_LIMIT) {
        if !m.has_attachment {
            continue;
        }
        // Skip messages we've already fetched attachments for — including ones
        // that turned out to have none (so false "has attachment" flags, e.g.
        // iCloud's multipart/mixed, aren't re-downloaded on every sync) — and
        // ones this session already tried and could not finish (see
        // [`ATTACHMENT_PREFETCH_TRIED`]).
        let checked = cache
            .map(|c| c.attachments_checked(account_id, path, m.uid))
            .unwrap_or(false);
        let tried = attachment_prefetch_tried(account_id, path, m.uid);
        let queued = queue.iter().any(|(p, u)| p == path && *u == m.uid);
        if !checked && !tried && !queued {
            queue.push_back((path.to_string(), m.uid));
        }
    }
}

/// Prefetch one queued message body: serve it from the disk cache if present,
/// otherwise fetch and cache it. Either way, push it to the UI so its in-memory
/// cache is warm and clicking the message renders instantly with no round-trip.
pub(super) async fn run_one_body_prefetch(
    queue: &mut std::collections::VecDeque<(String, u32)>,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    emitted: &mut std::collections::HashSet<(String, u32)>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some((path, uid)) = queue.front().cloned() else {
        return;
    };
    // A cached body needs no connection — serve it straight to the UI, with the
    // verdict stored alongside it. The two must always travel together: the UI
    // renders from whichever arrives, and a body without its verdict leaves the
    // sender badge blank with no second chance to fill it.
    if let Some(body) = cache.and_then(|c| c.load_body(account_id, &path, uid)) {
        queue.pop_front();
        emit(WorkerEvent::Body {
            message_id: uid,
            path: path.clone(),
            body,
        });
        if let Some(check) = cache.and_then(|c| c.load_sender_check(account_id, &path, uid)) {
            emit(WorkerEvent::SenderChecked {
                path: path.to_string(),
                message_id: uid,
                check,
            });
        }
        emitted.insert((path, uid));
        return;
    }
    if session.is_none() {
        *session = connect_and_list(account_id, account, cache, emit).await;
    }
    if session.is_none() {
        queue.clear();
        return;
    }
    queue.pop_front();
    let Ok(raw) = load_raw_retry(session, account, &path, uid).await else {
        // One failed read is enough for this session: left out of `emitted`,
        // the message would be queued again by every resync and fetched
        // again each time.
        emitted.insert((path, uid));
        return;
    };
    {
        // OpenPGP mail (#133) is left for an explicit open: nothing of it is
        // cached or pushed ahead, and rendering it here would decrypt in the
        // background — a passphrase prompt out of nowhere.
        if crate::pgp::detect(&raw).is_some() {
            emitted.insert((path, uid));
            return;
        }
        let (body, check, has_attachments) = render_raw(&raw);
        if let Some(c) = cache {
            c.save_body(account_id, &path, uid, &body);
            c.save_sender_check(account_id, &path, uid, &check);
            c.set_has_attachment(account_id, &path, uid, has_attachments);
        }
        emit(WorkerEvent::Body {
            message_id: uid,
            path: path.clone(),
            body,
        });
        emit(WorkerEvent::SenderChecked {
            path: path.to_string(),
            message_id: uid,
            check,
        });
        // The background prefetch reads the whole message anyway, so a wrong
        // paperclip corrects itself before the message is ever opened.
        if has_attachments {
            emit(WorkerEvent::HasAttachments {
                path: path.to_string(),
                message_id: uid,
            });
        }
        emitted.insert((path, uid));
    }
}

/// Status text for the attachment pre-download queue (empty = idle).
pub(super) fn prefetch_status(remaining: usize) -> String {
    if remaining == 0 {
        String::new()
    } else {
        ni18n_f(
            "Downloading attachments… {n} remaining",
            "Downloading attachments… {n} remaining",
            remaining as u32,
            &[("n", &remaining.to_string())],
        )
    }
}
