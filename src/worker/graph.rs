//! Microsoft Graph path (issue #36)
//!
//! Microsoft 365 accounts imported from GNOME Online Accounts authenticate with
//! a GOA token scoped to the Graph API — it cannot log in to IMAP or SMTP at
//! all. So these accounts speak Graph (REST) end to end: folders, summaries,
//! raw MIME bodies (`/$value`, which feeds the exact same parsing pipeline as
//! IMAP), flags, moves, drafts, and `sendMail`. Message uids are the same
//! stable string-hash the POP3 path uses (Graph ids are strings); the
//! uid → Graph-id map is rebuilt from every folder listing. Threading uses a
//! synthetic `graph-conv:<conversationId>` reference token (stripped before any
//! wire header in `build_email`) because the real References header isn't
//! available from list queries.

use super::*;
use crate::worker::graph::chores::{auto_empty_graph, emit_graph_body_hits, graph_preset_color};
use crate::worker::graph::folders::{
    graph_poll_inbox, graph_refresh_unread, refresh_graph_folders,
};
use crate::worker::graph::http::{
    graph_delete_req, graph_get_json, graph_post_mime, graph_send_json, graph_token, GRAPH_BASE,
};
use crate::worker::graph::index::{
    backfill_pause, graph_backfill_step, graph_load_folder, graph_sync_folder, GraphState,
};
use crate::worker::graph::messages::graph_list_ids;
use crate::worker::graph::ops::{
    graph_delete_attachment, graph_drop_draft_origin, graph_fetch_raw, graph_move_uids,
    graph_patch_message, graph_purge_uids, graph_undo_move,
};
use crate::worker::graph::send::{graph_flush_outbox, graph_mark_answered, graph_send_message};

mod chores;
mod folders;
mod http;
mod index;
mod messages;
mod ops;
mod send;
use crate::worker::compose::build::{build_draft, draft_message_id, restore_msgid_case};
use crate::worker::drafts::{draft_not_saved, saving_text};
use crate::worker::ids::hash_uid;
use crate::worker::outbox::{
    delete_queued, drop_superseded_outbox, emit_outbox, schedule_send, send_failed,
};
use crate::worker::render::attachments::extract_attachments;
use crate::worker::render::body_cacheable;
use crate::worker::strip::{cache_rewritten, strip_refused};

pub(super) async fn run_graph(
    account_id: u32,
    account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    // A label whose name the message leaves out is named from the
    // organization's list (#411) on its way to the reader, whether the
    // check was just made or comes from the cache, which keeps the header's.
    let emit = |mut ev: WorkerEvent| {
        if let WorkerEvent::SenderChecked { check, .. } = &mut ev {
            if let Some(label) = check.label.as_mut() {
                crate::graph_labels::fill(&account, label);
            }
        }
        emit(ev)
    };
    let cache = Cache::open()
        .map_err(|e| tracing::warn!("cache unavailable: {e}"))
        .ok();

    emit(WorkerEvent::Account(Account {
        id: account_id,
        name: account.name.clone(),
        email: account.email.clone(),
        label: account.display_label(),
        accent: accent_for(account_id).into(),
    }));

    // Cached folders immediately, then the live list.
    let cached_folders = cache
        .as_ref()
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    if !cached_folders.is_empty() {
        emit(WorkerEvent::Folders(cached_folders));
    }

    // When the Junk / Trash auto-empty (#140) last ran for this worker.
    let mut last_auto_empty: Option<std::time::Instant> = None;
    let mut state = GraphState::new(&account);

    // Fetch a token and the folder list. A GOA token failure here is the one
    // users actually hit (signed out in GNOME Settings), so say that.
    if let Some(token) = graph_token(&account, &emit).await {
        refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit).await;
    }

    // Graph has no push channel (nothing like IMAP IDLE is available to this
    // token), so new mail arrives on a poll. The auto-fetch preference sets the
    // cadence when it's on; otherwise a quiet couple of minutes.
    let poll_secs = match crate::config::load_privacy().fetch_interval_secs {
        0 => 120,
        s => s.max(60),
    };
    let mut poll = tokio::time::interval(Duration::from_secs(poll_secs));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    poll.tick().await; // consume the interval's immediate first tick

    loop {
        // The background index (#429) only runs while nothing else is
        // asked: a request, then the poll, come first, and each step is a
        // single request so neither waits long behind one.
        // Offline or on a metered connection it waits; the request that
        // follows the change wakes this loop to look again (#442).
        let indexing = !state.backfill.is_empty()
            && crate::power::reaches_internet()
            && !crate::power::saving_data();
        let wait = state.backfill_wait;
        let req = tokio::select! {
            biased;
            req = rx.recv() => match req {
                Some(req) => req,
                None => break,
            },
            _ = poll.tick() => {
                if !crate::power::reaches_internet() {
                    continue;
                }
                graph_poll_inbox(account_id, &account, cache.as_ref(), &mut state, &emit).await;
                continue;
            }
            _ = backfill_pause(wait), if indexing => {
                graph_backfill_step(&account, account_id, cache.as_ref(), &mut state, &emit).await;
                continue;
            }
        };
        // A folder the user opened, rather than a background refresh.
        let user = matches!(req, MailRequest::LoadMessages { .. });
        match req {
            MailRequest::Locate { message_id } => {
                // No folders to search here beyond what the cache already
                // held: answer "not found" so the app can report the miss.
                emit(WorkerEvent::Located {
                    message_id: message_id.clone(),
                    hit: None,
                });
            }
            // Graph reports no mailbox size or limit to the user it signs in.
            MailRequest::Quota => emit(WorkerEvent::Quota(None)),
            MailRequest::FindKeywords => {
                // Categories are defined once per mailbox, with a name and a
                // color: the master list is the whole answer. Counts come
                // from the cache, the only place Hylki has them.
                let mut found: Vec<KeywordFinding> = Vec::new();
                if let Some(token) = graph_token(&account, &emit).await {
                    let t = token.clone();
                    let cats = blocking(move || {
                        graph_get_json(&t, &format!("{GRAPH_BASE}/me/outlook/masterCategories"))
                    })
                    .await;
                    match cats {
                        Ok(v) => {
                            for c in v["value"].as_array().into_iter().flatten() {
                                let Some(name) = c["displayName"].as_str() else {
                                    continue;
                                };
                                let count = cache
                                    .as_ref()
                                    .map(|c| c.count_with_keyword(account_id, name))
                                    .unwrap_or(0);
                                found.push(KeywordFinding {
                                    keyword: name.to_string(),
                                    name: Some(name.to_string()),
                                    color: graph_preset_color(c["color"].as_str().unwrap_or("")),
                                    count,
                                    folders: Vec::new(),
                                });
                            }
                        }
                        Err(e) => tracing::warn!(
                            "[account {account_id}] find tags: categories failed: {e}"
                        ),
                    }
                }
                emit(WorkerEvent::KeywordsFound(found));
            }
            MailRequest::RefreshKeywords { .. } => {
                // Categories arrive with each folder's listing, so the
                // listing is the sync: every folder is re-read, which
                // rewrites its cached keywords (#166).
                let mut paths = Vec::new();
                if let Some(token) = graph_token(&account, &emit).await {
                    let folders: Vec<(String, u32)> = state
                        .folders
                        .iter()
                        .map(|(p, (id, _))| (p.clone(), *id))
                        .collect();
                    for (path, folder_id) in folders {
                        if graph_load_folder(
                            &token,
                            account_id,
                            folder_id,
                            &path,
                            cache.as_ref(),
                            &mut state,
                        )
                        .await
                        .is_ok()
                        {
                            paths.push(path);
                        }
                    }
                }
                emit(WorkerEvent::KeywordsSynced { paths });
            }

            // Cache-only, exactly like the IMAP path: assemble the conversation
            // from every folder's cached summaries.
            // Graph has no equivalent of BODYSTRUCTURE in the shape this path
            // uses: attachments come out of the raw MIME it downloads, so
            // describing one costs the same as fetching it. Listing them
            // through /messages/{id}/attachments would work, but it is a
            // different call on a backend there is no account here to test
            // against — so a Microsoft account's gallery still shows what has
            // been downloaded rather than reaching back through the archive.
            MailRequest::ScanAttachments { folder_path } => {
                emit(WorkerEvent::AttachmentsScanned {
                    folder_path,
                    added: 0,
                    remaining: 0,
                });
            }
            MailRequest::LoadRelated { message_id, ids } => {
                let messages = cache
                    .as_ref()
                    .map(|c| related_from_cache(c, account_id, &ids))
                    .unwrap_or_default();
                emit(WorkerEvent::Related {
                    message_id,
                    messages,
                });
            }
            MailRequest::LoadThreadSummaries { groups } => {
                let summaries = cache
                    .as_ref()
                    .map(|c| thread_summaries_with_members(c, account_id, &groups))
                    .unwrap_or_default();
                emit(WorkerEvent::ThreadSummaries { summaries });
            }

            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => {
                if let Some(c) = cache.as_ref() {
                    let cached = c.load_messages(account_id, &path, folder_id);
                    if !cached.is_empty() {
                        emit(WorkerEvent::Messages {
                            folder_id,
                            messages: cached,
                        });
                    }
                }
                emit(WorkerEvent::Status(i18n("Syncing…")));
                let Some(token) = graph_token(&account, &emit).await else {
                    // No token, so no mail is coming: end the folder's index
                    // (#218) rather than leave the list spinning.
                    emit(WorkerEvent::BackfillDone { folder_id });
                    emit(WorkerEvent::Status(String::new()));
                    continue;
                };
                match graph_sync_folder(
                    &token,
                    account_id,
                    folder_id,
                    &path,
                    cache.as_ref(),
                    &mut state,
                )
                .await
                {
                    Ok((messages, complete)) => {
                        let unread = state.chip_count(folder_id, &messages);
                        emit_graph_body_hits(
                            &token, &account, folder_id, &path, &messages, &state, &emit,
                        )
                        .await;
                        emit(WorkerEvent::Messages {
                            folder_id,
                            messages,
                        });
                        emit(WorkerEvent::FolderUnread { folder_id, unread });
                        // The index is complete when the first page held the
                        // whole folder, or once the background index has run
                        // (#429). Said at once when it is, so the list's
                        // "Loading more…" tail never waits on nothing (#218).
                        if state.index_after_load(&path, complete, user) {
                            emit(WorkerEvent::BackfillDone { folder_id });
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::net_error(i18n_f(
                            "Could not fetch mail: {e}",
                            &[("e", &(e).to_string())],
                        )));
                        emit(WorkerEvent::BackfillDone { folder_id });
                    }
                }
                emit(WorkerEvent::Status(String::new()));
            }

            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => {
                if let Some(body) = cache
                    .as_ref()
                    .and_then(|c| c.load_body(account_id, &path, uid))
                {
                    let check = cache
                        .as_ref()
                        .and_then(|c| c.load_sender_check(account_id, &path, uid));
                    emit(WorkerEvent::Body {
                        message_id,
                        path: path.clone(),
                        body,
                    });
                    if let Some(check) = check {
                        emit(WorkerEvent::SenderChecked {
                            path: path.to_string(),
                            message_id,
                            check,
                        });
                    }
                    continue;
                }
                match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                    Ok(raw) => {
                        let (body, check, _) = render_raw(&raw);
                        if let Some(c) = cache.as_ref() {
                            if body_cacheable(&check) {
                                c.save_body(account_id, &path, uid, &body);
                            }
                            c.save_sender_check(account_id, &path, uid, &check);
                        }
                        emit(WorkerEvent::Body {
                            message_id,
                            path: path.clone(),
                            body,
                        });
                        emit(WorkerEvent::SenderChecked {
                            path: path.to_string(),
                            message_id,
                            check,
                        });
                    }
                    Err(e) => emit(WorkerEvent::net_error(i18n_f(
                        "Could not load message: {e}",
                        &[("e", &(e).to_string())],
                    ))),
                }
            }

            MailRequest::LoadBodies { items, path } => {
                for (message_id, uid) in items {
                    if let Some(body) = cache
                        .as_ref()
                        .and_then(|c| c.load_body(account_id, &path, uid))
                    {
                        emit(WorkerEvent::Body {
                            message_id,
                            path: path.clone(),
                            body,
                        });
                        if let Some(check) = cache
                            .as_ref()
                            .and_then(|c| c.load_sender_check(account_id, &path, uid))
                        {
                            emit(WorkerEvent::SenderChecked {
                                path: path.to_string(),
                                message_id,
                                check,
                            });
                        }
                        continue;
                    }
                    match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                        Ok(raw) => {
                            let (body, check, _) = render_raw(&raw);
                            if let Some(c) = cache.as_ref() {
                                if body_cacheable(&check) {
                                    c.save_body(account_id, &path, uid, &body);
                                }
                                c.save_sender_check(account_id, &path, uid, &check);
                            }
                            emit(WorkerEvent::Body {
                                message_id,
                                path: path.clone(),
                                body,
                            });
                            emit(WorkerEvent::SenderChecked {
                                path: path.to_string(),
                                message_id,
                                check,
                            });
                        }
                        Err(e) => emit(WorkerEvent::net_error(i18n_f(
                            "Could not load message: {e}",
                            &[("e", &(e).to_string())],
                        ))),
                    }
                }
            }

            MailRequest::LoadSource {
                message_id: _,
                path,
                uid,
            } => match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                Ok(raw) => emit(WorkerEvent::Source {
                    text: String::from_utf8_lossy(&raw).into_owned(),
                }),
                Err(e) => emit(WorkerEvent::net_error(i18n_f(
                    "Could not load source: {e}",
                    &[("e", &(e).to_string())],
                ))),
            },

            MailRequest::LoadAttachments {
                message_id,
                path,
                uid,
                download,
            } => {
                if let Some(c) = cache.as_ref() {
                    let items = c.load_attachments(account_id, &path, uid);
                    if !items.is_empty() {
                        emit(WorkerEvent::Attachments {
                            path: path.to_string(),
                            message_id,
                            items,
                        });
                        continue;
                    }
                }
                if !download {
                    emit(WorkerEvent::AttachmentsPending {
                        path: path.to_string(),
                        message_id,
                    });
                    continue;
                }
                match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                    Ok(raw) => {
                        let items = extract_attachments(&raw);
                        if let Some(c) = cache.as_ref() {
                            c.save_attachments(account_id, &path, uid, &items);
                        }
                        emit(WorkerEvent::Attachments {
                            path: path.to_string(),
                            message_id,
                            items,
                        });
                    }
                    Err(e) => emit(WorkerEvent::net_error(i18n_f(
                        "Could not load attachments: {e}",
                        &[("e", &(e).to_string())],
                    ))),
                }
            }

            MailRequest::SetSeen { path, uid, seen } => {
                if let Some(c) = cache.as_ref() {
                    c.set_unread(account_id, &path, uid, !seen);
                }
                graph_patch_message(
                    &account,
                    &mut state,
                    &path,
                    uid,
                    serde_json::json!({ "isRead": seen }),
                    &emit,
                )
                .await;
                emit(WorkerEvent::SeenSettled { path, uid });
            }

            MailRequest::SetFlagged { path, uid, flagged } => {
                if let Some(c) = cache.as_ref() {
                    c.set_starred(account_id, &path, uid, flagged);
                }
                let status = if flagged { "flagged" } else { "notFlagged" };
                graph_patch_message(
                    &account,
                    &mut state,
                    &path,
                    uid,
                    serde_json::json!({ "flag": { "flagStatus": status } }),
                    &emit,
                )
                .await;
            }

            MailRequest::SetKeyword {
                path,
                uid,
                keyword,
                add,
                ..
            } => {
                // Categories travel as the whole list, so the cached row is
                // the base: patched first, then sent as it now stands.
                let categories = match cache.as_ref() {
                    Some(c) => {
                        c.set_keyword(account_id, &path, uid, &keyword, add);
                        c.keywords_of(account_id, &path, uid)
                    }
                    None if add => vec![keyword.clone()],
                    None => Vec::new(),
                };
                graph_patch_message(
                    &account,
                    &mut state,
                    &path,
                    uid,
                    serde_json::json!({ "categories": categories }),
                    &emit,
                )
                .await;
                emit(WorkerEvent::TagSettled { path, uid, keyword });
            }

            MailRequest::MarkAllRead { folder_id, path } => {
                // The server side is one PATCH per message; run it over the
                // cached unread rows (the listing window), then settle the cache.
                let unread_uids: Vec<u32> = cache
                    .as_ref()
                    .map(|c| {
                        c.load_messages(account_id, &path, folder_id)
                            .into_iter()
                            .filter(|m| m.unread)
                            .map(|m| m.uid)
                            .collect()
                    })
                    .unwrap_or_default();
                for uid in unread_uids {
                    graph_patch_message(
                        &account,
                        &mut state,
                        &path,
                        uid,
                        serde_json::json!({ "isRead": true }),
                        &emit,
                    )
                    .await;
                }
                if let Some(c) = cache.as_ref() {
                    c.mark_folder_read(account_id, &path);
                }
                emit(WorkerEvent::FolderUnread {
                    folder_id,
                    unread: 0,
                });
            }

            MailRequest::MoveMessage { path, uid, dest } => {
                if let Err(e) = graph_move_uids(
                    &account,
                    account_id,
                    &mut state,
                    &path,
                    &[uid],
                    &dest,
                    cache.as_ref(),
                )
                .await
                {
                    emit(WorkerEvent::error(i18n_f(
                        "Could not move message: {e}",
                        &[("e", &(e).to_string())],
                    )));
                }
            }

            MailRequest::MarkSpam { path, uid, dest } => {
                if let Err(e) = graph_move_uids(
                    &account,
                    account_id,
                    &mut state,
                    &path,
                    &[uid],
                    &dest,
                    cache.as_ref(),
                )
                .await
                {
                    emit(WorkerEvent::error(i18n_f(
                        "Could not mark as spam: {e}",
                        &[("e", &(e).to_string())],
                    )));
                }
            }

            // Microsoft 365 learns from the move itself: back to the Inbox.
            MailRequest::MarkHam { path, uid, dest } => {
                if let Err(e) = graph_move_uids(
                    &account,
                    account_id,
                    &mut state,
                    &path,
                    &[uid],
                    &dest,
                    cache.as_ref(),
                )
                .await
                {
                    emit(WorkerEvent::error(i18n_f(
                        "Could not mark as not spam: {e}",
                        &[("e", &(e).to_string())],
                    )));
                }
            }
            MailRequest::MarkHamMany { path, uids, dest } => {
                if let Err(e) = graph_move_uids(
                    &account,
                    account_id,
                    &mut state,
                    &path,
                    &uids,
                    &dest,
                    cache.as_ref(),
                )
                .await
                {
                    emit(WorkerEvent::error(i18n_f(
                        "Could not mark {len} messages as not spam: {e}",
                        &[("len", &(uids.len()).to_string()), ("e", &(e).to_string())],
                    )));
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::MoveMessages { path, uids, dest } => {
                if let Err(e) = graph_move_uids(
                    &account,
                    account_id,
                    &mut state,
                    &path,
                    &uids,
                    &dest,
                    cache.as_ref(),
                )
                .await
                {
                    emit(WorkerEvent::error(i18n_f(
                        "Could not move messages: {e}",
                        &[("e", &(e).to_string())],
                    )));
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::PurgeMessages { path, uids } => {
                graph_purge_uids(
                    &account,
                    account_id,
                    &mut state,
                    &path,
                    uids,
                    cache.as_ref(),
                    &emit,
                )
                .await;
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::EmptyFolder { folder_id, path } => {
                // No bulk endpoint for an arbitrary folder: list it, then
                // delete each message the way a purge does.
                let Some(token) = graph_token(&account, &emit).await else {
                    continue;
                };
                // Every message in it, not just the newest page or what the
                // cache has indexed so far (#429).
                let listed = match state.folders.get(&path).cloned() {
                    Some((_, gid)) => {
                        let (t, base) = (token.clone(), state.base.clone());
                        blocking(move || graph_list_ids(&t, &base, &gid)).await
                    }
                    None => Err(format!("unknown folder {path}")),
                };
                match listed {
                    Ok(ids) => {
                        let uids: Vec<u32> = ids
                            .into_iter()
                            .map(|(id, _)| {
                                let uid = hash_uid(&id);
                                state.uids.insert(uid, id);
                                uid
                            })
                            .collect();
                        let n = uids.len();
                        graph_purge_uids(
                            &account,
                            account_id,
                            &mut state,
                            &path,
                            uids,
                            cache.as_ref(),
                            &emit,
                        )
                        .await;
                        tracing::info!("emptied {path}: {n} message(s) erased");
                        emit(WorkerEvent::Messages {
                            folder_id,
                            messages: Vec::new(),
                        });
                        emit(WorkerEvent::FolderUnread {
                            folder_id,
                            unread: 0,
                        });
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f(
                        "Could not empty the folder: {e}",
                        &[("e", &e.to_string())],
                    ))),
                }
            }

            MailRequest::UndoMove {
                path,
                dest,
                dest_folder_id,
                message_ids,
            } => {
                match graph_undo_move(
                    &account,
                    account_id,
                    &mut state,
                    &path,
                    &dest,
                    &message_ids,
                    cache.as_ref(),
                )
                .await
                {
                    Ok(0) => {
                        tracing::info!("undo: the messages are no longer where that move put them");
                    }
                    Ok(_) => {
                        // Reload the restored folder so the messages reappear.
                        if let Some(token) = graph_token(&account, &emit).await {
                            if let Ok(messages) = graph_load_folder(
                                &token,
                                account_id,
                                dest_folder_id,
                                &dest,
                                cache.as_ref(),
                                &mut state,
                            )
                            .await
                            {
                                let unread = state.chip_count(dest_folder_id, &messages);
                                emit(WorkerEvent::Messages {
                                    folder_id: dest_folder_id,
                                    messages,
                                });
                                emit(WorkerEvent::FolderUnread {
                                    folder_id: dest_folder_id,
                                    unread,
                                });
                            }
                        }
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f(
                        "Undo failed: {e}",
                        &[("e", &(e).to_string())],
                    ))),
                }
                // The app spins its busy indicator until an undo answers.
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::CreateFolder { path } => {
                let Some(token) = graph_token(&account, &emit).await else {
                    continue;
                };
                // "A/B" nests under A (resolved from the last listing);
                // otherwise a top-level folder.
                let (url, name) = match path.rsplit_once('/') {
                    Some((parent, leaf)) => match state.folders.get(parent) {
                        Some((_, pgid)) => (
                            format!("{GRAPH_BASE}/me/mailFolders/{pgid}/childFolders"),
                            leaf.to_string(),
                        ),
                        None => (format!("{GRAPH_BASE}/me/mailFolders"), path.clone()),
                    },
                    None => (format!("{GRAPH_BASE}/me/mailFolders"), path.clone()),
                };
                let t = token.clone();
                let body = serde_json::json!({ "displayName": name });
                let r = blocking(move || graph_send_json(&t, "POST", &url, &body)).await;
                match r {
                    Ok(_) => {
                        refresh_graph_folders(
                            &token,
                            account_id,
                            cache.as_ref(),
                            &mut state,
                            &emit,
                        )
                        .await;
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f(
                        "Could not create folder: {e}",
                        &[("e", &(e).to_string())],
                    ))),
                }
            }

            MailRequest::RenameFolder { old_path, new_path } => {
                let Some(token) = graph_token(&account, &emit).await else {
                    continue;
                };
                let Some((_, gid)) = state.folders.get(&old_path).cloned() else {
                    emit(WorkerEvent::error(i18n(
                        "Could not rename folder: unknown folder",
                    )));
                    continue;
                };
                // Graph renames by displayName; moving between parents would be
                // a different call — the sidebar only renames leaves here.
                let leaf = new_path.rsplit('/').next().unwrap_or(&new_path).to_string();
                let t = token.clone();
                let url = format!("{GRAPH_BASE}/me/mailFolders/{gid}");
                let body = serde_json::json!({ "displayName": leaf });
                let r = blocking(move || graph_send_json(&t, "PATCH", &url, &body)).await;
                match r {
                    Ok(_) => {
                        refresh_graph_folders(
                            &token,
                            account_id,
                            cache.as_ref(),
                            &mut state,
                            &emit,
                        )
                        .await;
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f(
                        "Could not rename folder: {e}",
                        &[("e", &(e).to_string())],
                    ))),
                }
            }

            MailRequest::SetHiddenFolders { paths } => {
                state.hidden = paths;
                let Some(token) = graph_token(&account, &emit).await else {
                    continue;
                };
                refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit).await;
            }

            MailRequest::DeleteFolder { path, trash: _ } => {
                // Graph's folder delete moves the folder (contents included) to
                // Deleted Items itself; no separate content move needed.
                let Some(token) = graph_token(&account, &emit).await else {
                    continue;
                };
                let Some((_, gid)) = state.folders.get(&path).cloned() else {
                    emit(WorkerEvent::error(i18n(
                        "Could not delete folder: unknown folder",
                    )));
                    continue;
                };
                let t = token.clone();
                let url = format!("{GRAPH_BASE}/me/mailFolders/{gid}");
                let r = blocking(move || graph_delete_req(&t, &url)).await;
                match r {
                    Ok(()) => {
                        refresh_graph_folders(
                            &token,
                            account_id,
                            cache.as_ref(),
                            &mut state,
                            &emit,
                        )
                        .await;
                    }
                    Err(e) => emit(WorkerEvent::error(i18n_f(
                        "Could not delete folder: {e}",
                        &[("e", &(e).to_string())],
                    ))),
                }
            }

            MailRequest::SaveDraft {
                message,
                folder_id,
                path,
                autosave,
                template,
            } => {
                if autosave.is_none() {
                    emit(WorkerEvent::Status(saving_text(template)));
                }
                // A template (#360) is made where every new message is, in
                // Drafts, and moved on: so its folder is first made if the
                // account has none, and found by its Graph id.
                let mut folder_id = folder_id;
                let mut template_gid = None;
                if template {
                    if !state.folders.contains_key(&path) {
                        if let Some(token) = graph_token(&account, &emit).await {
                            let (t, url) = (token.clone(), format!("{GRAPH_BASE}/me/mailFolders"));
                            let body = serde_json::json!({ "displayName": path });
                            match blocking(move || graph_send_json(&t, "POST", &url, &body)).await {
                                Ok(_) => {
                                    refresh_graph_folders(
                                        &token,
                                        account_id,
                                        cache.as_ref(),
                                        &mut state,
                                        &emit,
                                    )
                                    .await
                                }
                                Err(e) => tracing::warn!("graph: could not create {path}: {e}"),
                            }
                        }
                    }
                    if let Some((id, gid)) = state.folders.get(&path) {
                        folder_id = *id;
                        template_gid = Some(gid.clone());
                    }
                }
                let mut message = OutgoingMessage {
                    sign: false,
                    encrypt: false,
                    ..*message
                };
                restore_msgid_case(cache.as_ref(), &mut message);
                let mut why = String::new();
                let saved = match build_draft(&account, &message) {
                    Ok(email) => {
                        let message_id = draft_message_id(&email);
                        let raw = email.formatted();
                        match graph_token(&account, &emit).await {
                            Some(token) => {
                                let t = token.clone();
                                let url = format!("{GRAPH_BASE}/me/messages");
                                let r = blocking(move || graph_post_mime(&t, &url, &raw)).await;
                                let r = match (r, &template_gid) {
                                    (Ok(created), Some(dest)) => match created["id"].as_str() {
                                        Some(gid) => {
                                            let (t, url) = (
                                                token.clone(),
                                                format!("{GRAPH_BASE}/me/messages/{gid}/move"),
                                            );
                                            let body = serde_json::json!({ "destinationId": dest });
                                            blocking(move || {
                                                graph_send_json(&t, "POST", &url, &body)
                                            })
                                            .await
                                        }
                                        None => Err("the new message has no id".to_string()),
                                    },
                                    (Ok(_), None) if template => Err(i18n("unknown folder")),
                                    (r, _) => r,
                                };
                                match r {
                                    Ok(_) => {
                                        // Replace the previous version of this draft.
                                        if let Some(o) = &message.draft_origin {
                                            graph_drop_draft_origin(
                                                &account,
                                                &mut state,
                                                account_id,
                                                o,
                                                cache.as_ref(),
                                                &emit,
                                            )
                                            .await;
                                        }
                                        if let Ok(messages) = graph_load_folder(
                                            &token,
                                            account_id,
                                            folder_id,
                                            &path,
                                            cache.as_ref(),
                                            &mut state,
                                        )
                                        .await
                                        {
                                            emit(WorkerEvent::Messages {
                                                folder_id,
                                                messages,
                                            });
                                        }
                                        Some(message_id)
                                    }
                                    Err(e) => {
                                        why = e.to_string();
                                        None
                                    }
                                }
                            }
                            None => {
                                why = i18n("Could not reach the server");
                                None
                            }
                        }
                    }
                    Err(e) => {
                        why = e.to_string();
                        None
                    }
                };
                emit(WorkerEvent::Status(String::new()));
                match saved {
                    Some(id) => emit(WorkerEvent::DraftSaved {
                        autosave,
                        message_id: Some(id),
                    }),
                    None => draft_not_saved(autosave, template, why, message, &emit),
                }
            }

            // `sent_path` is unused: Graph's sendMail files the Sent copy itself.
            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send {
                mut message,
                sent_path: _,
                ticket,
            } if message.send_at.is_some_and(|t| t > crate::datefmt::now()) => {
                let at = message.send_at.unwrap_or_default();
                restore_msgid_case(cache.as_ref(), &mut message);
                schedule_send(
                    cache.as_ref(),
                    account_id,
                    &account,
                    &message,
                    None,
                    at,
                    &emit,
                );
                emit(WorkerEvent::SendDone {
                    ticket,
                    sent: false,
                });
                if let Some(o) = &message.draft_origin {
                    graph_drop_draft_origin(
                        &account,
                        &mut state,
                        account_id,
                        o,
                        cache.as_ref(),
                        &emit,
                    )
                    .await;
                }
            }

            MailRequest::Send {
                mut message,
                sent_path: _,
                ticket,
            } => {
                emit(WorkerEvent::Status(i18n("Sending…")));
                restore_msgid_case(cache.as_ref(), &mut message);
                match graph_send_message(&account, &message, &emit).await {
                    Ok(()) => {
                        emit(WorkerEvent::Status(String::new()));
                        graph_mark_answered(
                            &account,
                            &mut state,
                            cache.as_ref(),
                            account_id,
                            &message.in_reply_to,
                            &message.forwards,
                            &emit,
                        )
                        .await;
                        // If sending an edited draft, remove the obsolete draft.
                        if let Some(o) = message.draft_origin.clone() {
                            if o.account_id == account_id {
                                graph_drop_draft_origin(
                                    &account,
                                    &mut state,
                                    account_id,
                                    &o,
                                    cache.as_ref(),
                                    &emit,
                                )
                                .await;
                                if let Some(token) = graph_token(&account, &emit).await {
                                    if let Ok(messages) = graph_load_folder(
                                        &token,
                                        account_id,
                                        o.folder_id,
                                        &o.path,
                                        cache.as_ref(),
                                        &mut state,
                                    )
                                    .await
                                    {
                                        emit(WorkerEvent::Messages {
                                            folder_id: o.folder_id,
                                            messages,
                                        });
                                    }
                                }
                            }
                        }
                        drop_superseded_outbox(cache.as_ref(), account_id, &message, &emit);
                        // The server files the copy in Sent Items itself; list
                        // that folder now so the copy is in the cache and can
                        // join the conversation it answers (#199).
                        let sent = cache.as_ref().and_then(|c| {
                            c.load_folders(account_id)
                                .into_iter()
                                .find(|f| f.kind == FolderKind::Sent)
                        });
                        if let (Some(c), Some(sent)) = (cache.as_ref(), sent) {
                            if let Some(token) = graph_token(&account, &emit).await {
                                if let Ok(messages) = graph_load_folder(
                                    &token,
                                    account_id,
                                    sent.id,
                                    &sent.path,
                                    Some(c),
                                    &mut state,
                                )
                                .await
                                {
                                    emit(WorkerEvent::Messages {
                                        folder_id: sent.id,
                                        messages,
                                    });
                                }
                            }
                        }
                        emit(WorkerEvent::Sent);
                        emit(WorkerEvent::SendDone { ticket, sent: true });
                    }
                    Err(e) => {
                        emit(WorkerEvent::Status(String::new()));
                        send_failed(
                            cache.as_ref(),
                            account_id,
                            &account,
                            &message,
                            None,
                            &e,
                            false,
                            &emit,
                        );
                        emit(WorkerEvent::SendDone {
                            ticket,
                            sent: false,
                        });
                    }
                }
            }

            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, &emit),

            MailRequest::DeleteOutbox { id } => {
                delete_queued(cache.as_ref(), account_id, id, &emit)
            }

            MailRequest::FlushOutbox { id } => {
                graph_flush_outbox(cache.as_ref(), account_id, &account, id, &mut state, &emit)
                    .await;
            }

            MailRequest::RefreshUnread => {
                // Quiet token fetch: this rides the auto-fetch tick, and a
                // signed-out account already errors on interactive actions.
                let quiet = |_: WorkerEvent| {};
                if let Some(token) = graph_token(&account, &quiet).await {
                    graph_refresh_unread(&token, account_id, cache.as_ref(), &mut state, &emit)
                        .await;
                    auto_empty_graph(
                        &token,
                        account_id,
                        &account,
                        cache.as_ref(),
                        &mut state,
                        &mut last_auto_empty,
                        &emit,
                    )
                    .await;
                }
            }

            MailRequest::Reconnect => {
                if let Some(token) = graph_token(&account, &emit).await {
                    refresh_graph_folders(&token, account_id, cache.as_ref(), &mut state, &emit)
                        .await;
                }
            }

            MailRequest::Settle { path, uids } => emit(WorkerEvent::MovesSettled { path, uids }),

            MailRequest::ExportRaw {
                token, path, uid, ..
            } => {
                let raw = graph_fetch_raw(&account, &mut state, &path, uid, &emit).await;
                emit(WorkerEvent::RawExported { token, raw });
            }

            // Graph files a message posted to a folder as a draft; it is never
            // offered as a destination.
            MailRequest::ImportRaw { token, .. } => emit(WorkerEvent::RawImported {
                token,
                result: Err(i18n(
                    "A Microsoft account can't receive mail from another account",
                )),
            }),

            // Microsoft 365 deletes an attachment in place: the message
            // keeps its id, and only what the cache holds of it changes.
            MailRequest::DeleteAttachment {
                message_id,
                path,
                uid,
                name,
                size,
            } => {
                emit(WorkerEvent::Status(i18n("Removing the attachment…")));
                let result =
                    graph_delete_attachment(&account, &mut state, &path, uid, &name, size, &emit)
                        .await;
                emit(WorkerEvent::Status(String::new()));
                match result {
                    Ok(()) => {
                        if let Some(c) = cache.as_ref() {
                            c.delete_body(account_id, &path, uid);
                            match graph_fetch_raw(&account, &mut state, &path, uid, &emit).await {
                                Ok(raw) => cache_rewritten(c, account_id, &path, uid, &raw),
                                // Fetched again at the next open.
                                Err(_) => {
                                    c.save_attachments(account_id, &path, uid, &[]);
                                    c.save_attachment_meta(account_id, &path, uid, &[]);
                                }
                            }
                        }
                        emit(WorkerEvent::AttachmentDeleted {
                            message_id,
                            path,
                            uid,
                            new_uid: Some(uid),
                            name,
                            size,
                        });
                    }
                    Err(e) => strip_refused(
                        &emit,
                        i18n_f("Could not remove the attachment: {e}", &[("e", &e)]),
                        message_id,
                        name,
                        size,
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::models::Message;
    use crate::worker::graph::index::graph_backfill_with;
    use crate::worker::graph::index::graph_index_plan;
    use crate::worker::graph::index::GraphRun;
    use crate::worker::graph::index::GRAPH_BACKFILL_CHUNK;
    use crate::worker::graph::messages::graph_due;
    use crate::worker::graph::messages::graph_find_by_msgid;
    use crate::worker::graph::messages::graph_id_page;
    use crate::worker::graph::messages::graph_ids_url;
    use crate::worker::graph::messages::graph_message;
    use crate::worker::graph::messages::GRAPH_FIRST_PAGE;
    use crate::worker::reconcile::window_gone;

    use super::*;

    /// A Graph message summary as `/messages` lists it.
    fn graph_msg(id: &str, at: &str) -> serde_json::Value {
        serde_json::json!({
            "@odata.etag": "W/\"CQAAABYAAAD\"",
            "id": id,
            "receivedDateTime": at,
            "subject": format!("Subject {id}"),
            "bodyPreview": "Hello there",
            "isRead": true,
            "hasAttachments": false,
            "internetMessageId": format!("<{id}@example.com>"),
            "conversationId": format!("AAQk-{id}"),
            "importance": "normal",
            "categories": [],
            "flag": { "flagStatus": "notFlagged" },
            "from": { "emailAddress": { "name": "Ada", "address": "ada@example.com" } },
            "toRecipients": [{ "emailAddress": { "name": "Me", "address": "me@example.com" } }],
            "ccRecipients": [],
            "replyTo": [],
        })
    }

    /// The same message as an id-only listing names it.
    fn graph_id(id: &str, at: &str) -> serde_json::Value {
        serde_json::json!({ "@odata.etag": "W/\"CQAAABYAAAD\"", "id": id, "receivedDateTime": at })
    }

    fn day(d: u32) -> String {
        format!("2026-09-{d:02}T09:00:00Z")
    }

    /// A cached row for `id`, received at `ts`.
    fn row(id: &str, ts: i64, folder_id: u32) -> Message {
        let v = graph_msg(id, "2026-09-01T09:00:00Z");
        let (mut m, _) = graph_message(&v, 1, folder_id).expect("summary");
        m.timestamp = ts;
        m
    }

    fn ts(at: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(at)
            .unwrap()
            .timestamp()
    }

    /// One answer of the loopback server: the first unused one whose `key`
    /// is part of the request's path and query.
    struct Answer {
        key: String,
        status: u16,
        headers: &'static str,
        body: String,
    }

    fn answer(key: &str, status: u16, body: serde_json::Value) -> Answer {
        Answer {
            key: key.to_string(),
            status,
            headers: "",
            body: body.to_string(),
        }
    }

    /// An HTTP server on loopback for `answers.len()` requests, as in the
    /// JMAP tests; hands back the request targets it was sent. Each body is
    /// formatted with `{origin}` replaced, for the next-page links.
    fn serve(answers: Vec<Answer>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        let o = origin.clone();
        let handle = std::thread::spawn(move || {
            let mut answers: Vec<Option<Answer>> = answers.into_iter().map(Some).collect();
            let mut seen = Vec::new();
            for _ in 0..answers.len() {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                reader.read_line(&mut line).expect("request line");
                let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                }
                let hit = answers
                    .iter_mut()
                    .find(|a| a.as_ref().is_some_and(|a| target.contains(&a.key)))
                    .and_then(Option::take);
                let (status, headers, body) = match hit {
                    Some(a) => (a.status, a.headers, a.body.replace("{origin}", &o)),
                    None => (404, "", String::new()),
                };
                let mut out = stream;
                let _ = write!(
                    out,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                seen.push(target);
            }
            seen
        });
        (origin, handle)
    }

    fn state_for(origin: &str, path: &str, folder_id: u32, gid: &str) -> GraphState {
        let mut state = GraphState::new(&super::super::sample_account());
        state.base = origin.to_string();
        state
            .folders
            .insert(path.to_string(), (folder_id, gid.to_string()));
        state
    }

    /// #429: missing mail comes in runs, newest first, none longer than a
    /// chunk; a cached row the server has not got is gone, unless it is
    /// newer than the listing.
    /// Exchange's last verb is the replied or forwarded arrow (#454).
    #[test]
    fn the_last_verb_marks_replied_and_forwarded() {
        let verb = |value: Option<&str>| {
            let mut v = graph_msg("M1", "2026-09-10T00:00:00Z");
            if let Some(value) = value {
                v["singleValueExtendedProperties"] =
                    serde_json::json!([{ "id": "Integer 0x1081", "value": value }]);
            }
            let (m, _) = graph_message(&v, 1, 1).unwrap();
            (m.answered, m.forwarded)
        };
        assert_eq!(verb(None), (false, false));
        assert_eq!(verb(Some("102")), (true, false));
        assert_eq!(verb(Some("103")), (true, false), "reply all");
        assert_eq!(verb(Some("104")), (false, true));
        assert_eq!(verb(Some("0")), (false, false));
    }

    #[test]
    fn the_index_plan_fetches_runs_and_drops_what_went() {
        let server: Vec<(String, String)> =
            (1..=8).map(|i| (format!("M{i}"), day(9 - i))).collect();
        let since = ts("2026-09-10T00:00:00Z");
        let cached: std::collections::HashMap<u32, i64> = [
            (hash_uid("M1"), ts(&day(8))),
            (hash_uid("M2"), ts(&day(7))),
            (hash_uid("M5"), ts(&day(4))),
            (hash_uid("GONE"), ts("2026-01-01T00:00:00Z")),
            (hash_uid("NEW"), since + 60),
        ]
        .into_iter()
        .collect();
        let (gone, runs) = graph_index_plan(&server, &cached, since);
        assert_eq!(gone, vec![hash_uid("GONE")]);
        assert_eq!(
            runs,
            vec![
                GraphRun {
                    ids: vec!["M3".into(), "M4".into()],
                    newest: day(6),
                    oldest: day(5)
                },
                GraphRun {
                    ids: vec!["M6".into(), "M7".into(), "M8".into()],
                    newest: day(3),
                    oldest: day(1)
                },
            ]
        );

        // A long run is cut into chunks.
        let many: Vec<(String, String)> = (0..GRAPH_BACKFILL_CHUNK + 5)
            .map(|i| (format!("X{i}"), day(1)))
            .collect();
        let (_, runs) = graph_index_plan(&many, &Default::default(), since);
        assert_eq!(
            runs.iter().map(|r| r.ids.len()).collect::<Vec<_>>(),
            vec![GRAPH_BACKFILL_CHUNK, 5]
        );
    }

    /// #429: a listing of the newest page speaks only for the mail it
    /// covers; a complete one for the whole folder.
    #[test]
    fn a_window_listing_keeps_older_rows() {
        let listed = vec![row("A", 300, 1), row("B", 200, 1)];
        let cached: std::collections::HashMap<u32, i64> = [
            (hash_uid("A"), 300),
            (hash_uid("B"), 200),
            (hash_uid("DEL"), 250),
            (hash_uid("OLD"), 100),
        ]
        .into_iter()
        .collect();
        assert_eq!(window_gone(&cached, &listed, false), vec![hash_uid("DEL")]);
        let mut all = vec![hash_uid("DEL"), hash_uid("OLD")];
        all.sort_unstable();
        assert_eq!(window_gone(&cached, &listed, true), all);
        assert!(window_gone(&cached, &[], false).is_empty());
    }

    /// #429: a throttled request waits as asked and goes again.
    #[test]
    fn a_throttled_request_is_retried() {
        let (origin, server) = serve(vec![
            Answer {
                key: "/messages".into(),
                status: 429,
                headers: "Retry-After: 0\r\n",
                body: serde_json::json!({ "error": { "code": "ApplicationThrottled", "message": "slow down" } })
                    .to_string(),
            },
            answer("/messages", 200, serde_json::json!({ "value": [graph_id("M1", &day(1))] })),
        ]);
        let (ids, next) = graph_id_page("t", &graph_ids_url(&origin, "F")).expect("retried");
        assert_eq!(ids, vec![("M1".to_string(), day(1))]);
        assert_eq!(next, None);
        assert_eq!(server.join().expect("server").len(), 2);
    }

    /// #429: the whole background pass over a folder, against recorded
    /// answers: the ids in two pages, a short run fetched message by
    /// message (one gone meanwhile), a long one by its dates; the cache
    /// keeps what it had, loses what the server lost, and the list hears
    /// of each step and of the end.
    #[tokio::test]
    async fn the_background_index_fills_and_prunes_a_folder() {
        let page1: Vec<serde_json::Value> = (1..=4)
            .map(|i| graph_id(&format!("M{i}"), &day(9 - i)))
            .collect();
        let page2: Vec<serde_json::Value> = (5..=8)
            .map(|i| graph_id(&format!("M{i}"), &day(9 - i)))
            .collect();
        let ranged: Vec<serde_json::Value> = (5..=8)
            .map(|i| graph_msg(&format!("M{i}"), &day(9 - i)))
            .collect();
        let (origin, server) = serve(vec![
            answer(
                "$top=1000&$orderby=receivedDateTime%20desc&$select=id,receivedDateTime",
                200,
                serde_json::json!({
                    "value": page1,
                    "@odata.nextLink": "{origin}/me/mailFolders/F-archive/messages?$select=id,receivedDateTime&$skip=4",
                }),
            ),
            answer("$skip=4", 200, serde_json::json!({ "value": page2 })),
            answer("/me/messages/M3?", 200, graph_msg("M3", &day(6))),
            answer(
                "/me/messages/M4?",
                404,
                serde_json::json!({ "error": { "code": "ErrorItemNotFound", "message": "Not found." } }),
            ),
            answer(
                "$filter=receivedDateTime%20ge%20",
                200,
                serde_json::json!({ "value": ranged }),
            ),
        ]);
        let cache = Cache::in_memory().expect("cache");
        let now = crate::datefmt::now();
        cache.upsert_messages(
            1,
            "Archive",
            &[
                row("M1", ts(&day(8)), 4),
                row("M2", ts(&day(7)), 4),
                row("M5", ts(&day(4)), 4),
                row("GONE", ts("2026-01-01T00:00:00Z"), 4),
                row("NEW", now + 60, 4),
            ],
        );
        let mut state = state_for(&origin, "Archive", 4, "F-archive");
        state.queue_backfill("Archive", true, false);
        let events = std::cell::RefCell::new(Vec::new());
        let emit = |e: WorkerEvent| events.borrow_mut().push(e);
        for _ in 0..4 {
            assert!(graph_backfill_with("t", 1, Some(&cache), &mut state, &emit).await);
        }
        assert!(state.backfill.is_empty(), "the folder's index is done");

        let mut have: Vec<u32> = cache.cached_uids(1, "Archive").into_iter().collect();
        have.sort_unstable();
        let mut want: Vec<u32> = ["M1", "M2", "M3", "M5", "M6", "M7", "M8", "NEW"]
            .iter()
            .map(|id| hash_uid(id))
            .collect();
        want.sort_unstable();
        assert_eq!(have, want);
        assert_eq!(
            state.uids.get(&hash_uid("M7")).map(String::as_str),
            Some("M7")
        );

        let events = events.into_inner();
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match e {
                WorkerEvent::Messages { folder_id: 4, .. } => "messages",
                WorkerEvent::MessagesAppend { folder_id: 4, .. } => "append",
                WorkerEvent::BackfillDone { folder_id: 4 } => "done",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["messages", "append", "append", "done"]);

        let seen = server.join().expect("server");
        assert_eq!(seen.len(), 5);
        assert!(
            seen[4].contains("ge%202026-09-01T09%3A00%3A00Z%20and%20receivedDateTime%20le%202026-09-03T09%3A00%3A00Z"),
            "{}",
            seen[4]
        );
    }

    /// #429: re-listing the newest page leaves the older cached mail be,
    /// and says the folder holds more than the page.
    #[tokio::test]
    async fn a_folder_load_merges_over_the_cache() {
        let page = |from: usize| -> Vec<serde_json::Value> {
            (from..from + 100)
                .map(|i| {
                    graph_msg(
                        &format!("N{i}"),
                        &format!("2026-10-01T{:02}:{:02}:00Z", 23 - i / 60, 59 - i % 60),
                    )
                })
                .collect()
        };
        let (origin, server) = serve(vec![
            answer(
                "$top=100&$orderby",
                200,
                serde_json::json!({ "value": page(0), "@odata.nextLink": "{origin}/me/mailFolders/F-inbox/messages?$skip=100" }),
            ),
            answer(
                "$skip=100",
                200,
                serde_json::json!({ "value": page(100), "@odata.nextLink": "{origin}/me/mailFolders/F-inbox/messages?$skip=200" }),
            ),
            answer(
                "$skip=200",
                200,
                serde_json::json!({ "value": page(200), "@odata.nextLink": "{origin}/me/mailFolders/F-inbox/messages?$skip=300" }),
            ),
        ]);
        let cache = Cache::in_memory().expect("cache");
        // An old message from the background index, and a new one deleted
        // on the server since it was cached.
        cache.upsert_messages(
            1,
            "Inbox",
            &[
                row("OLD", ts("2025-01-01T00:00:00Z"), 1),
                row("DELETED", ts("2026-10-01T23:59:30Z"), 1),
            ],
        );
        let mut state = state_for(&origin, "Inbox", 1, "F-inbox");
        let (messages, complete) = graph_sync_folder("t", 1, 1, "Inbox", Some(&cache), &mut state)
            .await
            .expect("listed");
        assert!(!complete, "the server has more than the first page");
        assert_eq!(messages.len(), GRAPH_FIRST_PAGE + 1);
        assert!(messages.iter().any(|m| m.uid == hash_uid("OLD")));
        assert!(!messages.iter().any(|m| m.uid == hash_uid("DELETED")));
        assert_eq!(server.join().expect("server").len(), 3);

        // Not complete and never indexed: queued, first, for the folder on
        // screen; the list keeps its "Loading more…".
        assert!(!state.index_after_load("Inbox", complete, true));
        assert_eq!(
            state.backfill.front().map(|j| j.path.as_str()),
            Some("Inbox")
        );
        // A folder that fitted in one page is complete as listed.
        assert!(state.index_after_load("Inbox", true, true));
        assert!(state.backfill.is_empty());
    }

    /// #429: an undone move finds an old message by its Message-ID.
    #[test]
    fn a_message_is_found_by_its_message_id() {
        let (origin, server) = serve(vec![answer(
            "$filter=internetMessageId",
            200,
            serde_json::json!({ "value": [graph_msg("M9", &day(1))] }),
        )]);
        let hits = graph_find_by_msgid("t", &origin, "F-trash", "M9@Example.com").expect("found");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "M9");
        let seen = server.join().expect("server");
        assert!(
            seen[0].contains("$filter=internetMessageId%20eq%20%27%3CM9%40Example.com%3E%27"),
            "{}",
            seen[0]
        );
    }

    /// #334: a flagged message's due date, in UTC; none once the flag is
    /// complete or cleared, or without a date.
    #[test]
    fn a_due_date_is_read_only_from_an_open_flag() {
        let flag = |status: &str| {
            serde_json::json!({
                "flagStatus": status,
                "dueDateTime": { "dateTime": "2026-10-05T04:00:00.0000000", "timeZone": "UTC" },
            })
        };
        assert_eq!(graph_due(&flag("flagged")), 1_791_172_800);
        assert_eq!(graph_due(&flag("complete")), 0);
        assert_eq!(graph_due(&flag("notFlagged")), 0);
        assert_eq!(
            graph_due(&serde_json::json!({ "flagStatus": "flagged" })),
            0
        );
    }
}

/// Pins of current behaviour for the Graph error helpers (Area B, step B0).
#[cfg(test)]
mod characterization_tests {
    use super::*;
    use crate::worker::graph::http::graph_err;
    use crate::worker::graph::http::graph_throttle_wait;

    fn status(code: u16, body: &str) -> ureq::Error {
        ureq::Error::Status(code, ureq::Response::new(code, "x", body).unwrap())
    }

    #[test]
    fn graph_errors_surface_the_nested_message_or_the_body() {
        assert_eq!(
            graph_err(status(
                403,
                r#"{"error":{"code":"ErrorAccessDenied","message":"Access is denied."}}"#
            )),
            "Microsoft Graph returned 403: Access is denied."
        );
        let long = "x".repeat(300);
        assert_eq!(
            graph_err(status(500, &long)),
            format!("Microsoft Graph returned 500: {}", "x".repeat(200))
        );
    }

    #[test]
    fn only_429_and_503_are_waited_on_five_seconds_by_default() {
        assert_eq!(
            graph_throttle_wait(&status(429, "")),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            graph_throttle_wait(&status(503, "")),
            Some(Duration::from_secs(5))
        );
        assert_eq!(graph_throttle_wait(&status(500, "")), None);
        assert_eq!(graph_throttle_wait(&status(404, "")), None);
    }
}
