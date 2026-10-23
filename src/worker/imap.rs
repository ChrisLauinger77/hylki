//! The IMAP backend. One connection serves the requests in order, with the
//! reader's loads first; between requests it prefetches, indexes, sweeps
//! unread counts and sits in IDLE. More connections watch folders for new
//! mail.

use std::time::Duration;

use async_imap::types::{Fetch, Flag};
use async_imap::Session;
use async_native_tls::TlsStream;
use futures::TryStreamExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::models::{Account, FolderKind, KeywordFinding};
use crate::worker::compose::build::{build_draft, draft_message_id, restore_msgid_case};
use crate::worker::drafts::{draft_not_saved, saving_text};
use crate::worker::imap::backfill::{
    restore_interrupted_backfill, run_one_backfill, run_one_refs_repair, Backfill,
};
use crate::worker::imap::bodies::settle_missing_bodies;
use crate::worker::imap::connect::{connect, connect_and_list};
use crate::worker::imap::fetch_mode::seed_fetch_mode;
use crate::worker::imap::folders::{
    cached_folder_kind, imap_storage_quota, list_folders, refresh_folder_names, refresh_folders,
};
use crate::worker::imap::gmail::{
    drop_gmail_label_copies, gmail_erase, gmail_labels_wanted, gmail_trash, has_gmail_labels,
    note_gmail_account, run_one_label_fill, store_gmail_labels, tag_labels,
};
use crate::worker::imap::idle::{idle_wait, IdleOutcome};
use crate::worker::imap::keywords::{is_system_keyword, refresh_keywords};
use crate::worker::imap::mutate::{
    create_folder, delete_folder, import_flags, mark_all_read, mark_ham, mark_spam, move_message,
    move_messages, purge_messages, rename_folder, store_flag, store_keyword,
};
use crate::worker::imap::prefetch::{
    folder_is_gallery, gallery_folder, prefetch_status, queue_attachment_prefetch,
    queue_body_prefetch, run_one_body_prefetch, run_one_prefetch,
};
use crate::worker::imap::retry::{
    load_bodies_retry, load_body_retry, load_messages_retry, load_raw_retry, load_source_retry,
};
use crate::worker::imap::send::{
    append_draft, delete_draft, file_sent_copy, flush_outbox, index_sent_copy, mark_answered,
};
use crate::worker::imap::stream::ImapIo;
use crate::worker::imap::strip_attachment::{
    cache_rewritten, strip_imap_attachment, strip_refused,
};
use crate::worker::imap::structure::structure_attachments;
use crate::worker::imap::summary::message_id_of;
use crate::worker::imap::sweep::{
    auto_empty_imap, clean_lists_imap, emit_body_hits, refresh_unread_counts, selected_chip_count,
};
use crate::worker::imap::watch::{
    watch_active_folder, watch_changed_folders, watch_folder, watchable, FolderWatch,
};
use crate::worker::imap::wire::{append_msg, exam, fetch_uids, search_uids, sel, uid_set};
use crate::worker::lane::{related_from_cache, serve_cached_body, thread_summaries_with_members};
use crate::worker::outbox::{
    delete_queued, drop_superseded_outbox, emit_outbox, schedule_send, send_failed,
};
use crate::worker::preempt::{interrupt_read_for_reader, reorder_reader_loads};
use crate::worker::render::attachments::attachments_of;
use crate::worker::render::body_cacheable;
use crate::worker::smtp::{send_smtp, smtp_refused};
use crate::worker::{
    accent_for, offline_text, reply_copy_path, MailRequest, OutgoingMessage, WorkerEvent,
};

pub(super) mod backfill;
pub(super) mod bodies;
pub(super) mod connect;
pub(super) mod fetch_mode;
pub(super) mod folders;
pub(super) mod gmail;
pub(super) mod idle;
pub(super) mod keywords;
pub(super) mod mutate;
pub(super) mod prefetch;
pub(super) mod preview_fetch;
pub(super) mod retry;
pub(super) mod send;
pub(super) mod stream;
pub(super) mod strip_attachment;
pub(super) mod structure;
pub(super) mod summary;
pub(super) mod sweep;
pub(super) mod sync;
pub(super) mod watch;
pub(super) mod wire;

/// Number of most-recent messages to fetch attachment info (BODYSTRUCTURE) for;
/// older messages get an envelope-only index row and resolve attachments on open.
pub(in crate::worker) const PAGE_SIZE: u32 = 50;

/// Number of most-recent messages shown instantly when a folder is first opened
/// (never synced before). The rest of the folder is indexed in the background so
/// browsing is immediate and search fills in shortly after — like Apple Mail.
pub(in crate::worker) const FIRST_PAGE: u32 = 200;

/// The threading headers to pull alongside the ENVELOPE.
///
/// The IMAP ENVELOPE carries In-Reply-To but *not* References, and In-Reply-To
/// alone only links a reply to its immediate parent. When that parent isn't in
/// the folder being listed — the usual case, since your own replies are filed in
/// Sent — the chain breaks and each incoming message starts a thread of its own,
/// which is why conversations hardly ever grouped (#21). References carries the
/// whole ancestry, so one of its ids is almost always present locally.
///
/// The priority headers ride along for the list's Importance column (#334):
/// the ENVELOPE has none of them either.
pub(in crate::worker) const REFS_FETCH_ITEM: &str =
    " BODY.PEEK[HEADER.FIELDS (REFERENCES X-PRIORITY IMPORTANCE PRIORITY)]";

/// Background index backfill: how many messages to fetch per idle drain step.
/// Bigger = fewer round-trips; smaller = more responsive to interleaved requests.
pub(in crate::worker) const BACKFILL_CHUNK: usize = 1_000;

/// Pre-download attachments only for this many of the most recent messages;
/// older attachment messages download on demand (with a spinner) when opened.
pub(in crate::worker) const PREFETCH_LIMIT: usize = 25;

/// How many messages one attachment scan describes. One FETCH per chunk, so
/// bigger means fewer round trips; small enough that a chunk lands quickly and
/// the gallery fills in visibly while the user is looking at it.
const SCAN_CHUNK: u32 = 200;

/// Most FETCHes one scan request will spend, counting the extra ones a failing
/// batch costs as it is halved. Bounds the work a folder full of responses we
/// cannot parse can demand in one pass; whatever is left keeps its place in the
/// queue and is picked up by the next.
const SCAN_SPLIT_BUDGET: u32 = 64;

/// How many messages one body fetch asks for.
///
/// `BODY.PEEK[]` returns whole messages, attachments included, so a whole
/// conversation in one command — up to `THREAD_MEMBER_LIMIT` members — can be
/// tens of megabytes arriving as a single response: nothing on screen until all
/// of it lands, and nothing to show for it if the connection drops on the way.
/// Ten still cuts the round trips to a fifth of what asking one at a time cost,
/// while the reader fills in as each batch arrives.
const BODY_FETCH_BATCH: usize = 10;

/// Pre-download message *bodies* for this many of the most recent messages in a
/// synced folder, so new mail opens instantly with no network wait. Bodies are
/// small, so this stays cheap; older messages load on demand.
pub(in crate::worker) const PREFETCH_BODY_LIMIT: usize = 50;

/// Floor between two unread-count sweeps out of the IDLE loop, so a burst of
/// new mail (one `Refreshed` wake per delivery) doesn't STATUS the whole folder
/// tree over and over. Explicit [`MailRequest::RefreshUnread`]s are never
/// throttled — a refresh the user asked for always runs.
const UNREAD_SWEEP_MIN: Duration = Duration::from_secs(60);

/// How many leased per-folder IDLE watcher connections one account may hold at
/// once (see [`watch_folder`]). The Inbox's permanent watcher and the main
/// session sit outside this cap, so the ceiling is six connections per account
/// — comfortably under every major provider's per-user cap (Gmail 15, Outlook
/// 20, Dovecot's default 10). When the pool is full, fresh activity evicts the
/// stalest lease; folders without a watcher stay covered by the unread sweep.
pub(in crate::worker) const WATCHER_LIMIT: usize = 4;

/// How long one burst of activity keeps a folder's watcher alive. Every touch
/// — the sweep noticing the folder changed, the watcher itself seeing server
/// activity, the user opening the folder — restarts the clock; a folder left
/// quiet for the whole hour logs its watcher out and falls back to sweep
/// coverage until it changes again.
pub(in crate::worker) const WATCH_LEASE: Duration = Duration::from_secs(3600);

/// How long a watcher lets IDLE sit before verifying the count itself. IDLE
/// still wakes it instantly where servers announce changes, but a flag flipped
/// from another client is exactly what some servers (iCloud, observed
/// 2026-08-28) never announce to an idling session — so the watcher recounts
/// every minute regardless and reports only when the number moved. One UID
/// SEARCH per watcher per minute; the ceiling on a watched folder's staleness
/// whatever the server chooses to say.
pub(in crate::worker) const WATCH_VERIFY: Duration = Duration::from_secs(60);

/// How often a watcher waiting out a network outage looks again: no network
/// traffic, only a flag read, so it can be short (#442).
pub(in crate::worker) const NETWORK_WAIT: Duration = Duration::from_secs(5);

/// How long ending an IDLE (the DONE handshake) may take before the connection
/// is declared wedged and dropped (#91). Unbounded, a server that never answers
/// DONE — or a middlebox that silently killed the idle connection — swallowed
/// the folder click that interrupted the IDLE, forever.
pub(in crate::worker) const IDLE_DONE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on the SELECT / IDLE-init round trips entering an IDLE, for the same
/// wedged-connection reason as [`IDLE_DONE_TIMEOUT`].
pub(in crate::worker) const IDLE_START_TIMEOUT: Duration = Duration::from_secs(10);

pub(in crate::worker) type ImapSession = Session<ImapIo<TlsStream<TcpStream>>>;

pub(in crate::worker) async fn run_imap(
    account_id: u32,
    mut account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent) + Clone + Send + 'static,
) {
    // Resolve the password off the main thread: migrate a legacy plaintext
    // password into the keyring (and strip it from disk), otherwise load it from
    // the keyring.
    if account.password.is_empty() {
        if let Some(pw) = crate::config::load_password(&account.email) {
            account.password = pw;
        }
    } else {
        let _ = crate::config::store_password(&account.email, &account.password);
        crate::config::strip_passwords_on_disk();
    }
    // Resolve the separate SMTP password from the keyring when in use.
    if account.smtp_separate && account.smtp_password.is_empty() {
        if let Some(pw) = crate::config::load_smtp_password(&account.email) {
            account.smtp_password = pw;
        }
    }
    // An account from GNOME Online Accounts takes its password from GOA at
    // every connect, so one changed in GNOME Settings reaches Hylki without
    // being typed again (#254). The keyring copy is kept for a GOA that
    // cannot answer yet; an imported account shows no password field, so
    // without GOA there would be no way to fix a missing one (#17).
    if !account.oauth {
        if let Some(goa_id) = account.goa_id.clone() {
            let (imap, smtp) =
                tokio::task::spawn_blocking(move || crate::goa::mail_passwords(&goa_id))
                    .await
                    .unwrap_or((None, None));
            if let Some(pw) = imap.filter(|pw| *pw != account.password) {
                let _ = crate::config::store_password(&account.email, &pw);
                account.password = pw;
            }
            if let (true, Some(pw)) = (account.smtp_separate, smtp) {
                if pw != account.smtp_password {
                    let _ = crate::config::store_smtp_password(&account.email, &pw);
                    account.smtp_password = pw;
                }
            }
            if account.password.is_empty() {
                emit(WorkerEvent::error(format!(
                    "GNOME Online Accounts has no password for {}. Open Settings → Online \
                         Accounts and sign in again.",
                    account.email
                )));
            }
        }
    }

    let cache = Cache::open()
        .map_err(|e| tracing::warn!("cache unavailable: {e}"))
        .ok();

    // Show the account + any cached folders immediately, before any network.
    emit(WorkerEvent::Account(Account {
        id: account_id,
        name: account.name.clone(),
        email: account.email.clone(),
        label: account.display_label(),
        accent: accent_for(account_id).into(),
    }));

    let cached_folders = cache
        .as_ref()
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    let have_cached_folders = !cached_folders.is_empty();
    note_gmail_account(account_id, &account, &cached_folders);
    if have_cached_folders {
        emit(WorkerEvent::Folders(cached_folders));
    }

    // The worker stays alive even if connecting fails, so the UI can retry. With
    // cached folders we connect lazily (on the first request) so cached mail can
    // render without waiting on the network; with an empty cache we connect now
    // to bootstrap the folder list.
    let mut session = if have_cached_folders {
        None
    } else {
        connect_and_list(account_id, &account, cache.as_ref(), &emit).await
    };

    // Attachments queued for background pre-download (folder_path, uid).
    let mut prefetch: std::collections::VecDeque<(String, u32)> = std::collections::VecDeque::new();
    // Message bodies queued for background pre-download, so new mail opens with no
    // network wait (folder_path, uid).
    let mut body_prefetch: std::collections::VecDeque<(String, u32)> =
        std::collections::VecDeque::new();
    // Bodies already pushed to the UI's in-memory cache this session, so they're
    // not re-sent on every folder re-sync.
    let mut body_emitted: std::collections::HashSet<(String, u32)> =
        std::collections::HashSet::new();
    // Folders queued for background index backfill (the rest of the mailbox past
    // the fast first page), and the set already enqueued this session.
    let mut backfill: std::collections::VecDeque<Backfill> = std::collections::VecDeque::new();
    let mut backfill_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Folders queued for the one-time References repair (see
    // [`run_one_refs_repair`]). Every folder gets one: a message indexed by an
    // older build carries only its In-Reply-To, and threading reads References.
    let mut refs_repair: std::collections::VecDeque<(u32, String)> =
        std::collections::VecDeque::new();
    // Folders queued for the one-time Gmail label fill (see
    // [`run_one_label_fill`]).
    let mut label_fill: std::collections::VecDeque<(u32, String)> =
        std::collections::VecDeque::new();
    // Requests already queued when one was taken, reordered so the reader's
    // loads come before list syncs (see `reorder_reader_loads`).
    let mut backlog: std::collections::VecDeque<MailRequest> = std::collections::VecDeque::new();
    // IMAP IDLE push: watch the most recently loaded folder for new mail.
    // The account's own setting wins over the global switch (#91).
    let push_enabled = account
        .push
        .unwrap_or_else(|| crate::config::load_privacy().push);
    let mut idle_folder: Option<(u32, String)> = None;
    // When the other folders' unread chips were last re-checked (None = not
    // yet this session; the first idle pass counts the startup listing).
    let mut last_unread_sweep: Option<std::time::Instant> = None;
    // When the Junk / Trash auto-empty (#140) last ran for this connection.
    let mut last_auto_empty: Option<std::time::Instant> = None;
    let mut last_list_clean: Option<std::time::Instant> = None;
    // Whether the Inbox's permanent IDLE watcher has been spawned. It waits for
    // the main session to be up first — its connection must win the first slot
    // on servers with a per-user cap — and needs the folder list in the cache.
    // Push off = the user opted out of persistent connections entirely.
    let mut inbox_watch_spawned = !push_enabled;
    // The dynamic watcher pool: folders recently seen changing (or opened) each
    // hold an IDLE connection for a [`WATCH_LEASE`], keyed by path.
    let mut watchers: std::collections::HashMap<String, FolderWatch> =
        std::collections::HashMap::new();
    // Each folder's (unseen, UIDNEXT) from the previous sweep, for spotting the
    // folders that changed between sweeps.
    let mut sweep_baseline: std::collections::HashMap<String, (u32, Option<u32>)> =
        std::collections::HashMap::new();
    // The unread sweep's place (folders still to count) and whether one is
    // owed: IDLE waking, or a sweep a request interrupted. It runs from the
    // idle chain below, after the new mail's body prefetch, never inline.
    let mut sweep_pending: Vec<crate::models::Folder> = Vec::new();
    // Count folders from the idle chain, after queued reader requests.
    let mut sweep_due = true;
    let mut folder_refresh_due = false;
    // Set after prefetching; triggers one re-sync (to catch mail that arrived
    // while the connection was busy) before settling into the long IDLE.
    let mut pending_resync = false;
    // Whether to use IMAP's structured ENVELOPE/BODYSTRUCTURE. Disabled for the
    // session (falling back to raw-header parsing) if the server sends responses
    // our IMAP parser can't handle (e.g. iCloud), and remembered across
    // sessions once a way of asking has worked (#226).
    let mut use_envelope = seed_fetch_mode(account_id, &account.email);
    // Whether the Outbox has been retried since this connection came up. A queued
    // message is almost always waiting on the network, so having a session again
    // is the moment worth retrying — not a timer.
    let mut outbox_flushed = false;

    // Queue every known folder for background indexing so search covers the whole
    // mailbox shortly after the first sync (like Apple Mail). The backfill skips
    // UIDs already cached, so this is cheap on subsequent runs.
    for f in cache
        .as_ref()
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default()
    {
        if backfill_seen.insert(f.path.clone()) {
            // Messages indexed before Hylki fetched References thread by their
            // immediate parent alone — which for an incoming reply is usually
            // your own message, filed in Sent, so the link points outside the
            // folder and the reply starts a thread of its own.
            if cache
                .as_ref()
                .is_some_and(|c| !c.refs_repair_state(account_id, &f.path).1)
            {
                refs_repair.push_back((f.id, f.path.clone()));
            }
            if gmail_labels_wanted(account_id)
                && cache
                    .as_ref()
                    .is_some_and(|c| !c.label_fill_state(account_id, &f.path).1)
            {
                label_fill.push_back((f.id, f.path.clone()));
            }
            backfill.push_back(Backfill {
                folder_id: f.id,
                gallery: gallery_folder(f.kind),
                path: f.path,
                remaining: None,
            });
        }
    }

    // Whether the user has been told this account's server keeps no tags
    // (one notice per session, not one per tagged message).
    let mut local_tags_noticed = false;
    loop {
        // Back online with something queued: send it before anything else, so a
        // message the user thinks they sent doesn't sit behind a mailbox sync.
        if session.is_some() {
            if !outbox_flushed {
                outbox_flushed = true;
                flush_outbox(
                    cache.as_ref(),
                    account_id,
                    &account,
                    None,
                    &mut session,
                    &mut use_envelope,
                    &emit,
                    false,
                )
                .await;
            }
        } else {
            outbox_flushed = false;
        }

        if !inbox_watch_spawned && session.is_some() {
            match cache.as_ref().map(|c| c.load_folders(account_id)) {
                // No cache = no folder list to watch from; stop checking.
                None => inbox_watch_spawned = true,
                Some(folders) if !folders.is_empty() => {
                    inbox_watch_spawned = true;
                    // The Inbox is watched permanently (no lease): it's where
                    // unfiltered mail lands, so its chip staying live is the
                    // baseline expectation whatever folder is being viewed.
                    if let Some(inbox) = folders.iter().find(|f| f.kind == FolderKind::Inbox) {
                        tokio::task::spawn_local(watch_folder(
                            account.clone(),
                            inbox.path.clone(),
                            Duration::ZERO,
                            None,
                            emit.clone(),
                        ));
                    }
                    // The initial sweep is already owed. Seed its baseline in
                    // the idle chain, after the startup reader requests.
                }
                // Connected but the listing hasn't landed in the cache yet
                // (fresh account, first moments): check again next pass.
                Some(_) => {}
            }
        }

        // Always prefer incoming requests. When idle: drain the attachment prefetch
        // queue (fast), then index a chunk of the background backfill, then — if
        // push is on — re-sync once to catch any mail that arrived while busy, then
        // sit in a long IMAP IDLE for instant delivery. Without push, block for the
        // next request.
        let req = match backlog.pop_front().map_or_else(|| rx.try_recv(), Ok) {
            Ok(req) => req,
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(mpsc::error::TryRecvError::Empty) => {
                // Offline, the queues keep for when the network is back; on
                // a metered connection the work done ahead of the user waits
                // for an unmetered one (#442).
                let net = crate::power::reaches(&account.imap_host);
                let ahead = net && !crate::power::saving_data();
                if !body_prefetch.is_empty() && ahead {
                    // Highest priority: get new mail's body cached so opening it is
                    // instant (no network wait).
                    let interrupted_body = body_prefetch.front().cloned();
                    let result = interrupt_read_for_reader(
                        run_one_body_prefetch(
                            &mut body_prefetch,
                            &mut session,
                            &account,
                            account_id,
                            cache.as_ref(),
                            &mut body_emitted,
                            &emit,
                        ),
                        &mut rx,
                        &mut backlog,
                    )
                    .await;
                    if result.is_none() {
                        if let Some(body) = interrupted_body {
                            if !body_prefetch.contains(&body) {
                                body_prefetch.push_front(body);
                            }
                        }
                        tracing::debug!(account_id, "body prefetch yielded to the reader");
                    }
                    continue;
                } else if folder_refresh_due && session.is_some() && net {
                    folder_refresh_due = false;
                    refresh_folder_names(
                        account_id,
                        &account,
                        session.as_mut().unwrap(),
                        cache.as_ref(),
                        &emit,
                    )
                    .await;
                    continue;
                } else if sweep_due && session.is_some() && net {
                    // The unread chips' re-count, owed since IDLE last woke
                    // (or left half done by a request): folder by folder,
                    // yielding to whatever arrives, its place kept.
                    let sess = session.as_mut().unwrap();
                    let (changed, complete) = refresh_unread_counts(
                        account_id,
                        sess,
                        cache.as_ref(),
                        idle_folder.as_ref().map(|(_, p)| p.as_str()),
                        &mut sweep_baseline,
                        &mut sweep_pending,
                        &rx,
                        &emit,
                    )
                    .await;
                    watch_changed_folders(&mut watchers, &account, &changed, &emit);
                    if complete {
                        sweep_due = false;
                        last_unread_sweep = Some(std::time::Instant::now());
                        auto_empty_imap(
                            account_id,
                            &account,
                            sess,
                            cache.as_ref(),
                            &mut last_auto_empty,
                            &emit,
                        )
                        .await;
                        clean_lists_imap(
                            account_id,
                            &account,
                            sess,
                            cache.as_ref(),
                            &mut last_list_clean,
                            &emit,
                        )
                        .await;
                    }
                    continue;
                } else if !prefetch.is_empty() && ahead {
                    run_one_prefetch(
                        &mut prefetch,
                        &mut session,
                        &account,
                        account_id,
                        cache.as_ref(),
                        &emit,
                    )
                    .await;
                    pending_resync = true;
                    continue;
                } else if !refs_repair.is_empty()
                    && session.is_some()
                    && backfill.is_empty()
                    && ahead
                {
                    // Lower priority than indexing new mail: repair the threading
                    // references of already-indexed replies, a chunk at a time.
                    run_one_refs_repair(
                        &mut refs_repair,
                        &mut session,
                        account_id,
                        cache.as_ref(),
                        &emit,
                    )
                    .await;
                    continue;
                } else if !label_fill.is_empty()
                    && session.is_some()
                    && backfill.is_empty()
                    && ahead
                {
                    run_one_label_fill(
                        &mut label_fill,
                        &mut session,
                        account_id,
                        cache.as_ref(),
                        &emit,
                    )
                    .await;
                    continue;
                } else if !backfill.is_empty() && ahead {
                    // Index the rest of the mailbox in the background. Connect first
                    // for cached-folder accounts (which connect lazily); if still
                    // offline, wait for a request instead of spinning.
                    if session.is_none() {
                        // A request beats the handshake. The first thing the UI
                        // asks for at startup is the visible folder, which the
                        // cache can answer without a network round trip — awaiting
                        // the connect here left the message list empty until the
                        // IMAP handshake finished (or timed out). Dropping the
                        // connect future only cancels this attempt; the next idle
                        // pass starts a fresh one.
                        tokio::select! {
                            biased;
                            req = rx.recv() => match req {
                                Some(req) => req,
                                None => break,
                            },
                            connected = connect_and_list(
                                account_id,
                                &account,
                                cache.as_ref(),
                                &emit,
                            ) => {
                                if connected.is_some() {
                                    session = connected;
                                    continue;
                                }
                                // Still offline: wait for a request rather than
                                // spinning on back-to-back reconnect attempts.
                                match rx.recv().await {
                                    Some(req) => req,
                                    None => break,
                                }
                            }
                        }
                    } else {
                        // The job owns its remaining UIDs while pending. Keep
                        // its original place so cancelling a network read cannot
                        // skip a chunk of the search index.
                        let interrupted_job = backfill.front().cloned();
                        let result = interrupt_read_for_reader(
                            run_one_backfill(
                                &mut backfill,
                                &mut session,
                                &account,
                                account_id,
                                cache.as_ref(),
                                &mut prefetch,
                                &mut use_envelope,
                                &emit,
                            ),
                            &mut rx,
                            &mut backlog,
                        )
                        .await;
                        if result.is_none() {
                            if let Some(job) = interrupted_job {
                                tracing::debug!(
                                    account_id,
                                    "background index yielded to the reader"
                                );
                                restore_interrupted_backfill(&mut backfill, job);
                            }
                        }
                        continue;
                    }
                } else if push_enabled && session.is_some() && idle_folder.is_some() && net {
                    let (fid, fpath) = idle_folder.clone().unwrap();
                    // Catch mail delivered while the connection was busy prefetching.
                    if pending_resync {
                        pending_resync = false;
                        let result = interrupt_read_for_reader(
                            load_messages_retry(
                                account_id,
                                &mut session,
                                &account,
                                fid,
                                &fpath,
                                &mut use_envelope,
                                cache.as_ref(),
                            ),
                            &mut rx,
                            &mut backlog,
                        )
                        .await;
                        if result.is_none() {
                            pending_resync = true;
                            tracing::debug!(account_id, "idle resync yielded to the reader");
                            continue;
                        }
                        if let Some(Ok(messages)) = result {
                            if let Some(c) = cache.as_ref() {
                                c.upsert_messages(account_id, &fpath, &messages);
                            }
                            queue_body_prefetch(
                                &mut body_prefetch,
                                &fpath,
                                &messages,
                                &body_emitted,
                            );
                            queue_attachment_prefetch(
                                &mut prefetch,
                                &fpath,
                                &messages,
                                cache.as_ref(),
                                account_id,
                            );
                            emit_body_hits(
                                &mut session,
                                &account,
                                account_id,
                                fid,
                                &fpath,
                                &messages,
                                cache.as_ref(),
                                &emit,
                            )
                            .await;
                            emit(WorkerEvent::Messages {
                                folder_id: fid,
                                messages,
                            });
                        }
                        continue;
                    }
                    match idle_wait(
                        &mut session,
                        &account,
                        account_id,
                        fid,
                        &fpath,
                        &mut rx,
                        cache.as_ref(),
                        &mut use_envelope,
                        &mut body_prefetch,
                        &mut prefetch,
                        &body_emitted,
                        &emit,
                        1740,
                    )
                    .await
                    {
                        IdleOutcome::Request(req) => req,
                        IdleOutcome::Refreshed | IdleOutcome::Quiet => {
                            // IDLE only watches one folder; mail filed by
                            // server-side rules lands in the others without ever
                            // waking it. Re-check their unread chips whenever
                            // IDLE surfaces — on new mail, and on the quiet
                            // timeout, which is the only clock a push-only
                            // (manual-fetch) account has.
                            // Owed, not run here: the idle chain runs it
                            // after the new mail's body is prefetched, and
                            // any request that arrives meanwhile goes first.
                            if last_unread_sweep.map_or(true, |t| t.elapsed() >= UNREAD_SWEEP_MIN) {
                                sweep_due = true;
                            }
                            continue;
                        }
                        IdleOutcome::Closed => break,
                    }
                } else {
                    match rx.recv().await {
                        Some(req) => req,
                        None => break,
                    }
                }
            }
        };
        let req = reorder_reader_loads(req, &mut rx, &mut backlog);

        if matches!(req, MailRequest::Reconnect) {
            session = connect_and_list(account_id, &account, cache.as_ref(), &emit).await;
            sweep_due = true;
            continue;
        }
        // Answered offline too: a move that could not run has still settled.
        if let MailRequest::Settle { path, uids } = req {
            emit(WorkerEvent::MovesSettled { path, uids });
            continue;
        }

        // Bodies of a `LoadBodies` batch that the disk cache couldn't answer, so
        // the network arm below fetches only those.
        let mut pending_bodies: Vec<(u32, u32)> = Vec::new();

        // Serve from cache first so mail appears instantly (and offline).
        match &req {
            MailRequest::LoadMessages { folder_id, path } => {
                if let Some(c) = cache.as_ref() {
                    let cached = c.load_messages(account_id, path, *folder_id);
                    if !cached.is_empty() {
                        emit(WorkerEvent::Messages {
                            folder_id: *folder_id,
                            messages: cached,
                        });
                    }
                }
            }
            MailRequest::LoadRelated { message_id, ids } => {
                let messages = cache
                    .as_ref()
                    .map(|c| related_from_cache(c, account_id, ids))
                    .unwrap_or_default();
                emit(WorkerEvent::Related {
                    message_id: *message_id,
                    messages,
                });
                continue; // cache-only, never hits the network
            }
            MailRequest::LoadThreadSummaries { groups } => {
                let summaries = cache
                    .as_ref()
                    .map(|c| thread_summaries_with_members(c, account_id, groups))
                    .unwrap_or_default();
                emit(WorkerEvent::ThreadSummaries { summaries });
                continue; // cache-only, never hits the network
            }
            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => {
                if cache.as_ref().is_some_and(|c| {
                    serve_cached_body(c, account_id, path, *uid, *message_id, &emit)
                }) {
                    continue; // already cached; no network needed
                }
            }
            MailRequest::LoadBodies { items, path } => {
                // Serve every member the cache already has, and leave the rest
                // for one fetch. A conversation reopened after its bodies were
                // cached costs no network at all.
                for (message_id, uid) in items {
                    if !cache.as_ref().is_some_and(|c| {
                        serve_cached_body(c, account_id, path, *uid, *message_id, &emit)
                    }) {
                        pending_bodies.push((*message_id, *uid));
                    }
                }
                if pending_bodies.is_empty() {
                    continue; // whole conversation was cached; no network needed
                }
            }
            MailRequest::LoadAttachments {
                message_id,
                path,
                uid,
                download,
            } => {
                if let Some(c) = cache.as_ref() {
                    let items = c.load_attachments(account_id, path, *uid);
                    if !items.is_empty() {
                        emit(WorkerEvent::Attachments {
                            path: path.to_string(),
                            message_id: *message_id,
                            items,
                        });
                        continue; // already cached; no network needed
                    }
                }
                // Not cached and the user hasn't asked to download — tell the UI
                // so it can offer a "Load attachments" button instead of fetching.
                if !download {
                    emit(WorkerEvent::AttachmentsPending {
                        path: path.to_string(),
                        message_id: *message_id,
                    });
                    continue;
                }
            }
            _ => {}
        }

        // Everything below needs a live session. One left from before the
        // network went is dead, and a request on it would wait out the
        // socket's timeout (#442).
        if session.is_some() && !crate::power::reaches(&account.imap_host) {
            session = None;
        }
        if session.is_none() {
            session = connect_and_list(account_id, &account, cache.as_ref(), &emit).await;
            if session.is_none() {
                // Still offline; cached data (if any) was already sent. Close
                // the folder's index anyway (#218): "we tried and could not
                // reach the server" has to end the list's "Loading more…"
                // spinner the same way success does, or an unreachable account
                // is indistinguishable from one that is still streaming — and
                // in a unified view one such account pins the spinner for
                // every account's mail.
                if let MailRequest::LoadMessages { folder_id, .. }
                | MailRequest::SyncFolder { folder_id, .. } = &req
                {
                    emit(WorkerEvent::BackfillDone {
                        folder_id: *folder_id,
                    });
                }
                // A message sent or scheduled with the server out of reach
                // waits in the Outbox; dropped here it was lost (#442).
                if let MailRequest::Send {
                    mut message,
                    sent_path,
                    ticket,
                } = req
                {
                    restore_msgid_case(cache.as_ref(), &mut message);
                    match message.send_at.filter(|t| *t > crate::datefmt::now()) {
                        Some(at) => schedule_send(
                            cache.as_ref(),
                            account_id,
                            &account,
                            &message,
                            sent_path.as_deref(),
                            at,
                            &emit,
                        ),
                        None => {
                            let e = if crate::power::reaches(&account.imap_host) {
                                i18n("could not reach the server")
                            } else {
                                offline_text()
                            };
                            send_failed(
                                cache.as_ref(),
                                account_id,
                                &account,
                                &message,
                                sent_path.as_deref(),
                                &e,
                                false,
                                &emit,
                            );
                        }
                    }
                    emit(WorkerEvent::SendDone {
                        ticket,
                        sent: false,
                    });
                }
                continue;
            }
        }
        // On a connection-shaped failure we drop the session to force a reconnect.
        let mut lost = false;
        // A background resync keeps quiet and leaves IDLE/watching alone.
        let background = matches!(req, MailRequest::SyncFolder { .. });
        let invalidates_selection = matches!(
            &req,
            MailRequest::RenameFolder { .. }
                | MailRequest::DeleteFolder { .. }
                | MailRequest::DeleteAttachment { .. }
        );
        if invalidates_selection {
            if let Some(sess) = session.as_mut() {
                sess.as_mut().set_selected_folder(None);
            }
        }

        match req {
            // Served from cache before this network match; never reached here.
            MailRequest::LoadRelated { .. } | MailRequest::LoadThreadSummaries { .. } => {}
            MailRequest::LoadMessages { folder_id, path }
            | MailRequest::SyncFolder { folder_id, path } => {
                if !background {
                    emit(WorkerEvent::Status(i18n("Syncing…")));
                }
                // Fast first page (or a recent-window refresh over the cached
                // index); the background backfill indexes the rest of the folder.
                // Reads retry once across a reconnect, so an idle-dropped session
                // recovers transparently instead of surfacing an EOF.
                let result = interrupt_read_for_reader(
                    load_messages_retry(
                        account_id,
                        &mut session,
                        &account,
                        folder_id,
                        &path,
                        &mut use_envelope,
                        cache.as_ref(),
                    ),
                    &mut rx,
                    &mut backlog,
                )
                .await;
                let Some(result) = result else {
                    tracing::debug!("folder sync yielded to the reader: {path}");
                    backlog.push_back(if background {
                        MailRequest::SyncFolder { folder_id, path }
                    } else {
                        MailRequest::LoadMessages { folder_id, path }
                    });
                    continue;
                };
                match result {
                    Ok(messages) => {
                        if let Some(c) = cache.as_ref() {
                            // Upsert (not replace) so the background-indexed tail
                            // isn't wiped by a fast first-page load.
                            c.upsert_messages(account_id, &path, &messages);
                        }
                        // Ensure this folder gets fully indexed (if not already
                        // queued this session).
                        if backfill_seen.insert(path.clone()) {
                            backfill.push_back(Backfill {
                                folder_id,
                                gallery: folder_is_gallery(cache.as_ref(), account_id, &path),
                                path: path.clone(),
                                remaining: None,
                            });
                        }
                        // Pre-download recent bodies so opening them is instant.
                        queue_body_prefetch(&mut body_prefetch, &path, &messages, &body_emitted);
                        // Queue background attachment pre-downloads for recent
                        // messages that have them — older ones download on demand.
                        queue_attachment_prefetch(
                            &mut prefetch,
                            &path,
                            &messages,
                            cache.as_ref(),
                            account_id,
                        );
                        if !background {
                            idle_folder = Some((folder_id, path.clone()));
                            // Opening a folder is "use": keep it push-watched
                            // for the next lease hour, so changes still land
                            // instantly after the user moves on to another
                            // folder.
                            let kind = cache.as_ref().and_then(|c| {
                                c.load_folders(account_id)
                                    .into_iter()
                                    .find(|f| f.path == path)
                                    .map(|f| f.kind)
                            });
                            if kind.is_some_and(watchable) {
                                watch_active_folder(&mut watchers, &account, &path, 0, &emit);
                            }
                        }
                        emit_body_hits(
                            &mut session,
                            &account,
                            account_id,
                            folder_id,
                            &path,
                            &messages,
                            cache.as_ref(),
                            &emit,
                        )
                        .await;
                        // A load that came back with less than a full first
                        // page *is* the whole folder — an uncached folder asks
                        // for the newest FIRST_PAGE of however many the server
                        // holds, and a cached one is reconciled against the
                        // server's complete UID set — so its index is already
                        // complete and the background backfill has nothing to
                        // add. Saying so now (#218) instead of waiting for that
                        // job's turn matters because the job sits behind every
                        // other folder in the mailbox, and behind the body
                        // prefetch, unread sweep and attachment prefetch ahead
                        // of it in the idle ladder: a small inbox could wait
                        // hours for a "Loading more…" spinner that had nothing
                        // left to load. Larger folders are unaffected — the
                        // list stops showing the spinner once it holds a full
                        // page anyway.
                        let complete = messages.len() < FIRST_PAGE as usize;
                        emit(WorkerEvent::Messages {
                            folder_id,
                            messages,
                        });
                        if complete {
                            emit(WorkerEvent::BackfillDone { folder_id });
                        }
                        // Refresh the true unread count (catches new mail and
                        // reads from other clients beyond the loaded window).
                        if let Some(sess) = session.as_mut() {
                            let kind = cached_folder_kind(cache.as_ref(), account_id, &path);
                            if let Some(unread) = selected_chip_count(sess, kind).await {
                                emit(WorkerEvent::FolderUnread { folder_id, unread });
                            }
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::net_error(i18n_f(
                            "Could not load {path}: {e}",
                            &[("path", &(path).to_string()), ("e", &(e).to_string())],
                        )));
                        // The load failed, so nothing more is on its way for
                        // this folder: end its index (#218) rather than leave
                        // the list spinning on a backfill that will not run.
                        emit(WorkerEvent::BackfillDone { folder_id });
                    }
                }
                // However it went, this folder has been to the server (#198).
                emit(WorkerEvent::FolderSynced { folder_id });
                emit(WorkerEvent::Status(prefetch_status(prefetch.len())));
            }

            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => match load_body_retry(&mut session, &account, &path, uid).await {
                Ok((body, check, has_attachments)) => {
                    if let Some(c) = cache.as_ref() {
                        if body_cacheable(&check) {
                            c.save_body(account_id, &path, uid, &body);
                        }
                        c.save_sender_check(account_id, &path, uid, &check);
                        c.set_has_attachment(account_id, &path, uid, has_attachments);
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
                    emit(if has_attachments {
                        WorkerEvent::HasAttachments {
                            path: path.to_string(),
                            message_id,
                        }
                    } else {
                        WorkerEvent::NoAttachments {
                            path: path.to_string(),
                            message_id,
                        }
                    });
                }
                Err(e) => {
                    emit(WorkerEvent::net_error(i18n_f(
                        "Could not load message: {e}",
                        &[("e", &(e).to_string())],
                    )));
                }
            },

            MailRequest::LoadBodies { path, .. } => {
                for group in pending_bodies.chunks(BODY_FETCH_BATCH) {
                    // A failed batch leaves no session behind (the retry only
                    // hands one back on success), so the rest of the conversation
                    // needs one before it can go on.
                    if session.is_none() {
                        session = connect(&account).await.ok();
                    }
                    if session.is_none() {
                        break; // still offline; what's left loads on reopen
                    }
                    let uids: Vec<u32> = group.iter().map(|(_, uid)| *uid).collect();
                    match load_bodies_retry(&mut session, &account, &path, &uids).await {
                        Ok(mut fetched) => {
                            // The UIDs the server answered without a body.
                            let mut missing: Vec<(u32, u32)> = Vec::new();
                            for (message_id, uid) in group {
                                let Some((body, check, has_attachments)) = fetched.remove(uid)
                                else {
                                    missing.push((*message_id, *uid));
                                    continue;
                                };
                                if let Some(c) = cache.as_ref() {
                                    if body_cacheable(&check) {
                                        c.save_body(account_id, &path, *uid, &body);
                                    }
                                    c.save_sender_check(account_id, &path, *uid, &check);
                                    c.set_has_attachment(account_id, &path, *uid, has_attachments);
                                }
                                emit(WorkerEvent::Body {
                                    message_id: *message_id,
                                    path: path.clone(),
                                    body,
                                });
                                emit(WorkerEvent::SenderChecked {
                                    path: path.to_string(),
                                    message_id: *message_id,
                                    check,
                                });
                                emit(if has_attachments {
                                    WorkerEvent::HasAttachments {
                                        path: path.to_string(),
                                        message_id: *message_id,
                                    }
                                } else {
                                    WorkerEvent::NoAttachments {
                                        path: path.to_string(),
                                        message_id: *message_id,
                                    }
                                });
                            }
                            if !missing.is_empty() {
                                settle_missing_bodies(
                                    &mut session,
                                    account_id,
                                    &path,
                                    missing,
                                    cache.as_ref(),
                                    &emit,
                                )
                                .await;
                            }
                        }
                        Err(e) => {
                            emit(WorkerEvent::net_error(i18n_f(
                                "Could not load conversation: {e}",
                                &[("e", &(e).to_string())],
                            )));
                        }
                    }
                }
            }

            MailRequest::LoadSource { path, uid, .. } => {
                match load_source_retry(&mut session, &account, &path, uid).await {
                    Ok(text) => emit(WorkerEvent::Source { text }),
                    Err(e) => {
                        emit(WorkerEvent::net_error(i18n_f(
                            "Could not load source: {e}",
                            &[("e", &(e).to_string())],
                        )));
                    }
                }
            }

            MailRequest::LoadAttachments {
                message_id,
                path,
                uid,
                download: _,
            } => match load_raw_retry(&mut session, &account, &path, uid).await {
                Ok(raw) => {
                    let (items, cacheable) = attachments_of(&raw);
                    if let Some(c) = cache.as_ref() {
                        if cacheable {
                            c.save_attachments(account_id, &path, uid, &items);
                            c.mark_attachments_checked(account_id, &path, uid);
                        }
                    }
                    emit(WorkerEvent::Attachments {
                        path: path.to_string(),
                        message_id,
                        items,
                    });
                }
                Err(e) => {
                    emit(WorkerEvent::net_error(i18n_f(
                        "Could not load attachments: {e}",
                        &[("e", &(e).to_string())],
                    )));
                }
            },

            MailRequest::SetSeen { path, uid, seen } => {
                let sess = session.as_mut().unwrap();
                if let Err(e) = store_flag(sess, &path, uid, "\\Seen", seen).await {
                    emit(WorkerEvent::error(i18n_f(
                        "Could not update message: {e}",
                        &[("e", &(e).to_string())],
                    )));
                    lost = true;
                } else if let Some(c) = cache.as_ref() {
                    c.set_unread(account_id, &path, uid, !seen);
                }
                emit(WorkerEvent::SeenSettled { path, uid });
            }

            MailRequest::SetFlagged { path, uid, flagged } => {
                let sess = session.as_mut().unwrap();
                if let Err(e) = store_flag(sess, &path, uid, "\\Flagged", flagged).await {
                    emit(WorkerEvent::error(i18n_f(
                        "Could not change the star: {e}",
                        &[("e", &(e).to_string())],
                    )));
                    lost = true;
                } else if let Some(c) = cache.as_ref() {
                    c.set_starred(account_id, &path, uid, flagged);
                }
            }

            MailRequest::SetKeyword {
                path,
                uid,
                message_id,
                keyword,
                add,
            } => {
                let sess = session.as_mut().unwrap();
                let stored = store_keyword(sess, &path, uid, &keyword, add).await;
                // On Gmail a tag is a label too (#374), so Gmail's own apps
                // show it; the keyword stays for other IMAP clients.
                if stored.is_ok() && gmail_labels_wanted(account_id) {
                    let labels = tag_labels(&keyword, add);
                    match store_gmail_labels(sess, uid, &labels, add).await {
                        Ok(()) => {
                            if let Some(c) = cache.as_ref() {
                                c.set_gmail_label(account_id, &path, uid, &labels, add);
                            }
                            // A new label is a new folder on Gmail.
                            if add {
                                folder_refresh_due = true;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("[account {account_id}] could not set Gmail label: {e}")
                        }
                    }
                }
                match stored {
                    Ok(true) => {
                        if let Some(c) = cache.as_ref() {
                            c.set_keyword(account_id, &path, uid, &keyword, add);
                        }
                    }
                    Ok(false) => {
                        // The mailbox takes no custom keywords: keep the tag
                        // in Hylki, and say so once per session.
                        if let Some(c) = cache.as_ref() {
                            if message_id.is_empty() {
                                c.set_keyword(account_id, &path, uid, &keyword, add);
                            } else {
                                c.set_local_tag(account_id, &message_id, &keyword, add);
                            }
                        }
                        if !local_tags_noticed {
                            local_tags_noticed = true;
                            emit(WorkerEvent::Notice(i18n(
                                "This server does not store tags, so Hylki keeps them on this computer only",
                            )));
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not tag message: {e}",
                            &[("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
                emit(WorkerEvent::TagSettled { path, uid, keyword });
            }

            MailRequest::MarkAllRead { folder_id, path } => {
                let sess = session.as_mut().unwrap();
                match mark_all_read(sess, &path).await {
                    Ok(()) => {
                        if let Some(c) = cache.as_ref() {
                            c.mark_folder_read(account_id, &path);
                        }
                        emit(WorkerEvent::FolderUnread {
                            folder_id,
                            unread: 0,
                        });
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not mark folder read: {e}",
                            &[("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
            }

            MailRequest::RefreshUnread => {
                // reorder_reader_loads has moved queued requests into backlog,
                // so rx alone cannot tell a sweep that the reader is waiting.
                // Defer both the listing and counts to the idle chain, which
                // runs only after backlog and rx have drained. Repeated refresh
                // ticks coalesce instead of queuing another mailbox-wide walk.
                folder_refresh_due = true;
                sweep_due = true;
            }

            MailRequest::MarkSpam { path, uid, dest } => {
                let sess = session.as_mut().unwrap();
                match mark_spam(sess, &path, uid, &dest).await {
                    Ok(created) => {
                        if let Some(c) = cache.as_ref() {
                            drop_gmail_label_copies(c, account_id, &account, &path, &[uid], &dest);
                            c.delete_message(account_id, &path, uid);
                        }
                        if created {
                            refresh_folders(account_id, &account, sess, cache.as_ref(), &emit)
                                .await;
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not mark as spam: {e}",
                            &[("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
            }

            MailRequest::MarkHam { path, uid, dest } => {
                let sess = session.as_mut().unwrap();
                match mark_ham(sess, &path, &[uid], &dest).await {
                    Ok(created) => {
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, &path, uid);
                        }
                        if created {
                            refresh_folders(account_id, &account, sess, cache.as_ref(), &emit)
                                .await;
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not mark as not spam: {e}",
                            &[("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
            }

            MailRequest::MarkHamMany { path, uids, dest } => {
                let sess = session.as_mut().unwrap();
                match mark_ham(sess, &path, &uids, &dest).await {
                    Ok(created) => {
                        if let Some(c) = cache.as_ref() {
                            for uid in &uids {
                                c.delete_message(account_id, &path, *uid);
                            }
                        }
                        if created {
                            refresh_folders(account_id, &account, sess, cache.as_ref(), &emit)
                                .await;
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not mark {len} messages as not spam: {e}",
                            &[("len", &(uids.len()).to_string()), ("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::ExportRaw {
                token, path, uid, ..
            } => {
                let raw = load_raw_retry(&mut session, &account, &path, uid)
                    .await
                    .map_err(|e| e.to_string());
                emit(WorkerEvent::RawExported { token, raw });
            }

            MailRequest::ImportRaw {
                token,
                path,
                raw,
                seen,
                flagged,
            } => {
                let sess = session.as_mut().unwrap();
                let flags = import_flags(seen, flagged);
                let result = append_msg(sess, &path, flags.as_deref(), &raw)
                    .await
                    .map_err(|e| e.to_string());
                emit(WorkerEvent::RawImported { token, result });
            }

            MailRequest::DeleteAttachment {
                message_id,
                path,
                uid,
                name,
                size,
            } => {
                let folders = cache
                    .as_ref()
                    .map(|c| c.load_folders(account_id))
                    .unwrap_or_default();
                // Gmail keeps every message in All Mail as well as under the
                // label it is shown in: the original would stay there, file
                // and all, beside the copy.
                if has_gmail_labels(&account, &folders) {
                    let why = i18n("Gmail keeps a copy of every message in All Mail, so Hylki cannot remove an attachment there");
                    strip_refused(&emit, why, message_id, name, size);
                    continue;
                }
                emit(WorkerEvent::Status(i18n("Removing the attachment…")));
                let result =
                    strip_imap_attachment(&mut session, &account, &path, uid, &name, size).await;
                emit(WorkerEvent::Status(String::new()));
                match result {
                    Ok((new_uid, raw)) => {
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, &path, uid);
                        }
                        if let Some(folder_id) =
                            folders.iter().find(|f| f.path == path).map(|f| f.id)
                        {
                            if let Ok(messages) = load_messages_retry(
                                account_id,
                                &mut session,
                                &account,
                                folder_id,
                                &path,
                                &mut use_envelope,
                                cache.as_ref(),
                            )
                            .await
                            {
                                if let Some(c) = cache.as_ref() {
                                    c.upsert_messages(account_id, &path, &messages);
                                }
                                emit(WorkerEvent::Messages {
                                    folder_id,
                                    messages,
                                });
                            }
                        }
                        if let Some(c) = cache.as_ref() {
                            c.save_attachment_meta(account_id, &path, uid, &[]);
                            if let Some(new_uid) = new_uid {
                                cache_rewritten(c, account_id, &path, new_uid, &raw);
                            }
                        }
                        emit(WorkerEvent::AttachmentDeleted {
                            message_id,
                            path,
                            uid,
                            new_uid,
                            name,
                            size,
                        });
                    }
                    Err(e) => {
                        let why = i18n_f("Could not remove the attachment: {e}", &[("e", &e.text)]);
                        strip_refused(&emit, why, message_id, name, size);
                        lost |= e.connection;
                    }
                }
            }

            MailRequest::MoveMessage { path, uid, dest } => {
                let sess = session.as_mut().unwrap();
                match move_message(sess, &path, uid, &dest).await {
                    Ok(created) => {
                        if let Some(c) = cache.as_ref() {
                            drop_gmail_label_copies(c, account_id, &account, &path, &[uid], &dest);
                            c.delete_message(account_id, &path, uid);
                        }
                        if created {
                            refresh_folders(account_id, &account, sess, cache.as_ref(), &emit)
                                .await;
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not move message: {e}",
                            &[("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
            }

            MailRequest::MoveMessages { path, uids, dest } => {
                let sess = session.as_mut().unwrap();
                match move_messages(sess, &path, &uids, &dest).await {
                    Ok(created) => {
                        if let Some(c) = cache.as_ref() {
                            drop_gmail_label_copies(c, account_id, &account, &path, &uids, &dest);
                            for uid in &uids {
                                c.delete_message(account_id, &path, *uid);
                            }
                        }
                        if created {
                            refresh_folders(account_id, &account, sess, cache.as_ref(), &emit)
                                .await;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "move failed: {path:?} -> {dest:?} set={} ({e})",
                            uid_set(&uids)
                        );
                        emit(WorkerEvent::error(i18n_f(
                            "Could not move {len} messages: {e}",
                            &[("len", &(uids.len()).to_string()), ("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
                // Always signal completion so the UI's bulk spinner clears.
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::Locate { message_id } => {
                // A folder at a time: SELECT, then a HEADER search. Servers
                // whose header index lags (see UndoMove below) are the reason
                // the cache is asked first; this is the fallback for a message
                // Hylki has never listed.
                let sess = session.as_mut().unwrap();
                let mut hit: Option<(String, u32)> = None;
                match list_folders(
                    account_id,
                    sess,
                    &account.folder_roles,
                    &account.hidden_folders,
                )
                .await
                {
                    Ok(folders) => {
                        for f in folders {
                            if sel(sess, &f.path).await.is_err() {
                                continue;
                            }
                            let Ok(set) =
                                search_uids(sess, format!("HEADER Message-ID \"{message_id}\""))
                                    .await
                            else {
                                continue;
                            };
                            if let Some(uid) = set.into_iter().max() {
                                hit = Some((f.path.clone(), uid));
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("[account {account_id}] locate: listing folders failed: {e}")
                    }
                }
                emit(WorkerEvent::Located {
                    message_id: message_id.clone(),
                    hit,
                });
            }

            MailRequest::ScanAttachments { folder_path } => {
                // Ask the server what a batch of messages holds, without
                // downloading any of it. BODYSTRUCTURE is a few hundred bytes
                // per message, so a whole archive costs single-digit megabytes
                // — the difference between a gallery that reaches back two
                // decades and one that shows only what has been opened.
                let Some(c) = cache.as_ref() else { continue };
                let uids =
                    c.unscanned_attachment_uids(account_id, folder_path.as_str(), SCAN_CHUNK);
                if uids.is_empty() {
                    emit(WorkerEvent::AttachmentsScanned {
                        folder_path: folder_path.clone(),
                        added: 0,
                        remaining: 0,
                    });
                    continue;
                }
                let sess = session.as_mut().unwrap();
                if sel(sess, folder_path.as_str()).await.is_err() {
                    continue;
                }

                // One bad response must not cost the whole batch. A batch that
                // will not parse is halved and retried, down to the single
                // message responsible: its neighbours are described normally
                // and only it is written off. Smaller responses also parse more
                // often, so the split usually resolves the failure outright.
                let mut added = 0u32;
                let mut pending: Vec<Vec<u32>> = vec![uids];
                let mut budget = SCAN_SPLIT_BUDGET;
                while let Some(batch) = pending.pop() {
                    if budget == 0 {
                        break; // the rest keeps its place in the queue
                    }
                    budget -= 1;
                    let set = uid_set(&batch);
                    // BODYSTRUCTURE on its own, not beside ENVELOPE: the
                    // servers that make us fall back to raw headers (iCloud)
                    // usually choke on the ENVELOPE half — an unescaped quote
                    // in a Message-ID — so asking for the structure alone
                    // often parses where the combined fetch did not.
                    let fetched: Result<Vec<Fetch>, _> =
                        match fetch_uids(sess, &set, "BODYSTRUCTURE").await {
                            Ok(stream) => stream.try_collect().await,
                            Err(e) => Err(e),
                        };
                    match fetched {
                        Ok(fetches) => {
                            // Every UID asked about is answered, including the
                            // ones holding nothing: an empty list is still an
                            // answer, and recording it stops the message being
                            // asked about on every pass forever.
                            let mut seen: std::collections::HashSet<u32> =
                                std::collections::HashSet::new();
                            for f in &fetches {
                                let Some(uid) = f.uid else { continue };
                                seen.insert(uid);
                                let metas = f
                                    .bodystructure()
                                    .map(structure_attachments)
                                    .unwrap_or_default();
                                added += metas.len() as u32;
                                c.save_attachment_meta(
                                    account_id,
                                    folder_path.as_str(),
                                    uid,
                                    &metas,
                                );
                            }
                            for uid in batch.iter().filter(|u| !seen.contains(u)) {
                                c.save_attachment_meta(account_id, folder_path.as_str(), *uid, &[]);
                            }
                        }
                        // A dropped connection says nothing about these
                        // messages. Leave them unscanned and let the next pass
                        // have them, rather than recording an answer the server
                        // never gave.
                        Err(async_imap::error::Error::ConnectionLost) => {
                            pending.clear();
                            session.take();
                            break;
                        }
                        Err(e) if batch.len() > 1 => {
                            let mid = batch.len() / 2;
                            pending.push(batch[mid..].to_vec());
                            pending.push(batch[..mid].to_vec());
                            tracing::debug!(
                                "attachment scan of {folder_path}: splitting {} after {e}",
                                batch.len()
                            );
                        }
                        Err(e) => {
                            // One message whose structure our parser will not
                            // take. Record it as holding nothing rather than
                            // retrying it forever; opening it still fetches the
                            // message in full and lists what it really holds.
                            tracing::warn!(
                                "attachment scan of {folder_path} uid {}: {e}",
                                batch[0]
                            );
                            c.save_attachment_meta(account_id, folder_path.as_str(), batch[0], &[]);
                        }
                    }
                }

                emit(WorkerEvent::AttachmentsScanned {
                    folder_path: folder_path.clone(),
                    added,
                    remaining: c.unscanned_attachment_count(account_id, folder_path.as_str()),
                });
            }
            MailRequest::FindKeywords => {
                // Every folder, read-only: EXAMINE answers with the mailbox's
                // FLAGS line, which lists the keywords in use there, and a
                // KEYWORD search per candidate says how many messages carry
                // it (some servers list every keyword ever defined, so an
                // unused one is dropped here).
                let sess = session.as_mut().unwrap();
                let mut found: Vec<KeywordFinding> = Vec::new();
                match list_folders(
                    account_id,
                    sess,
                    &account.folder_roles,
                    &account.hidden_folders,
                )
                .await
                {
                    Ok(folders) => {
                        for f in &folders {
                            let Ok(mb) = exam(sess, &f.path).await else {
                                continue;
                            };
                            let candidates: Vec<String> = mb
                                .flags
                                .iter()
                                .filter_map(|fl| match fl {
                                    Flag::Custom(k) if !is_system_keyword(k) => Some(k.to_string()),
                                    _ => None,
                                })
                                .collect();
                            for kw in candidates {
                                let n = match search_uids(sess, format!("KEYWORD {kw}")).await {
                                    Ok(set) => set.len(),
                                    Err(_) => 0,
                                };
                                if n == 0 {
                                    continue;
                                }
                                match found
                                    .iter_mut()
                                    .find(|x| x.keyword.eq_ignore_ascii_case(&kw))
                                {
                                    Some(x) => {
                                        x.count += n;
                                        if !x.folders.contains(&f.name) {
                                            x.folders.push(f.name.clone());
                                        }
                                    }
                                    None => found.push(KeywordFinding {
                                        keyword: kw,
                                        name: None,
                                        color: None,
                                        count: n,
                                        folders: vec![f.name.clone()],
                                    }),
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!(
                        "[account {account_id}] find tags: listing folders failed: {e}"
                    ),
                }
                emit(WorkerEvent::KeywordsFound(found));
            }

            MailRequest::Quota => {
                // RFC 2087/9208: the quota root the inbox belongs to is the
                // account's own, and its STORAGE resource is the mailbox size.
                let sess = session.as_mut().unwrap();
                let offered = sess
                    .capabilities()
                    .await
                    .is_ok_and(|caps| caps.has_str("QUOTA"));
                let quota = if offered {
                    match sess.get_quota_root("INBOX").await {
                        Ok((_, quotas)) => imap_storage_quota(&quotas),
                        Err(e) => {
                            tracing::warn!(
                                "[account {account_id}] quota: GETQUOTAROOT failed: {e}"
                            );
                            None
                        }
                    }
                } else {
                    None
                };
                tracing::debug!(target: "hylki::imap", "quota: {quota:?} (server {} QUOTA)", if offered { "offers" } else { "lacks" });
                emit(WorkerEvent::Quota(quota));
            }
            MailRequest::RefreshKeywords { keywords } => {
                let sess = session.as_mut().unwrap();
                let paths = refresh_keywords(account_id, sess, cache.as_ref(), &keywords).await;
                emit(WorkerEvent::KeywordsSynced { paths });
            }

            MailRequest::UndoMove {
                path,
                dest,
                dest_folder_id,
                message_ids,
            } => {
                // Find the moved messages where the move landed them. UIDs
                // changed in transit; Message-IDs did not.
                let mut uids: Vec<u32> = Vec::new();
                let mut failed: Option<String> = None;
                {
                    let sess = session.as_mut().unwrap();
                    let mut exists = 0u32;
                    match sel(sess, &path).await {
                        Err(e) => failed = Some(e.to_string()),
                        Ok(mb) => {
                            exists = mb.exists;
                            for id in &message_ids {
                                match search_uids(sess, format!("HEADER Message-ID \"{id}\"")).await
                                {
                                    Ok(set) => uids.extend(set),
                                    Err(e) => {
                                        failed = Some(e.to_string());
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    uids.sort_unstable();
                    uids.dedup();
                    // Some servers (iCloud among them) can't be trusted with
                    // HEADER searches: the index lags behind a just-landed
                    // move, and our stored ids are lowercased while the
                    // server's match may be case-sensitive. FETCH has neither
                    // problem — before declaring the messages gone, read the
                    // newest headers (a move lands at the top of the UID
                    // space) and match Message-IDs locally.
                    if failed.is_none() && uids.is_empty() && exists > 0 {
                        let wanted: std::collections::HashSet<&str> =
                            message_ids.iter().map(String::as_str).collect();
                        let lo = exists.saturating_sub(199).max(1);
                        let scanned: Result<Vec<Fetch>, _> = async {
                            sess.fetch(
                                format!("{lo}:{exists}"),
                                "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])",
                            )
                            .await?
                            .try_collect()
                            .await
                        }
                        .await;
                        match scanned {
                            Ok(fetches) => uids.extend(fetches.iter().filter_map(|f| {
                                f.uid.filter(|_| wanted.contains(message_id_of(f).as_str()))
                            })),
                            Err(e) => failed = Some(e.to_string()),
                        }
                    }
                    if failed.is_none() && !uids.is_empty() {
                        if let Err(e) = move_messages(sess, &path, &uids, &dest).await {
                            failed = Some(e.to_string());
                        } else if let Some(c) = cache.as_ref() {
                            for uid in &uids {
                                c.delete_message(account_id, &path, *uid);
                            }
                        }
                    }
                }
                if let Some(e) = failed {
                    emit(WorkerEvent::error(i18n_f(
                        "Undo failed: {e}",
                        &[("e", &(e).to_string())],
                    )));
                } else if uids.is_empty() {
                    // Not an error the reader needs interrupting for: the
                    // step is simply spent (#200).
                    tracing::info!("undo: the messages are no longer where that move put them");
                } else {
                    // Reload the restored folder so the messages reappear in
                    // the list (the app can't restore them optimistically —
                    // it never learns their new UIDs).
                    if let Ok(messages) = load_messages_retry(
                        account_id,
                        &mut session,
                        &account,
                        dest_folder_id,
                        &dest,
                        &mut use_envelope,
                        cache.as_ref(),
                    )
                    .await
                    {
                        if let Some(c) = cache.as_ref() {
                            c.upsert_messages(account_id, &dest, &messages);
                        }
                        emit(WorkerEvent::Messages {
                            folder_id: dest_folder_id,
                            messages,
                        });
                        emit(WorkerEvent::Restored {
                            folder_id: dest_folder_id,
                            message_ids: message_ids.clone(),
                        });
                    }
                }
                // Undoing something is not news: the messages reappearing in
                // the list is the whole report (#200).
                // Always signal completion — the app spins the refresh
                // indicator while the undo's server work is in flight.
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::PurgeMessages { path, uids } => {
                let sess = session.as_mut().unwrap();
                // Gmail only takes a label off mail expunged outside Trash,
                // so the mail would sit on in All Mail (#471).
                let trash =
                    gmail_trash(cache.as_ref(), account_id, &account).filter(|t| *t != path);
                let result = match &trash {
                    Some(trash) => gmail_erase(sess, &path, &uids, trash).await,
                    None => purge_messages(sess, &path, &uids).await,
                };
                match result {
                    Ok(()) => {
                        if let Some(c) = cache.as_ref() {
                            if let Some(trash) = &trash {
                                c.drop_label_copies(account_id, &path, &uids, trash);
                            }
                            for uid in &uids {
                                c.delete_message(account_id, &path, *uid);
                            }
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not delete permanently: {e}",
                            &[("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
                // Always signal completion so the UI's bulk spinner clears.
                emit(WorkerEvent::BulkComplete);
            }

            MailRequest::EmptyFolder { folder_id, path } => {
                let sess = session.as_mut().unwrap();
                let found = match sel(sess, &path).await {
                    Ok(_) => search_uids(sess, "ALL").await,
                    Err(e) => Err(e),
                };
                let result = match found {
                    Ok(set) => {
                        let uids: Vec<u32> = set.into_iter().collect();
                        purge_messages(sess, &path, &uids).await.map(|()| uids)
                    }
                    Err(e) => Err(e),
                };
                match result {
                    Ok(uids) => {
                        if let Some(c) = cache.as_ref() {
                            for uid in &uids {
                                c.delete_message(account_id, &path, *uid);
                            }
                        }
                        tracing::info!("emptied {path}: {} message(s) erased", uids.len());
                        emit(WorkerEvent::Messages {
                            folder_id,
                            messages: Vec::new(),
                        });
                        emit(WorkerEvent::FolderUnread {
                            folder_id,
                            unread: 0,
                        });
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not empty the folder: {e}",
                            &[("e", &e.to_string())],
                        )));
                        lost = true;
                    }
                }
            }

            MailRequest::CreateFolder { path } => {
                let sess = session.as_mut().unwrap();
                match create_folder(sess, &path).await {
                    Ok(()) => {
                        refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not create folder: {e}",
                            &[("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
            }

            MailRequest::RenameFolder { old_path, new_path } => {
                let sess = session.as_mut().unwrap();
                match rename_folder(sess, &old_path, &new_path).await {
                    Ok(()) => {
                        refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not move folder: {e}",
                            &[("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
            }

            MailRequest::SetHiddenFolders { paths } => {
                account.hidden_folders = paths;
                let sess = session.as_mut().unwrap();
                refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await;
            }

            MailRequest::DeleteFolder { path, trash } => {
                let sess = session.as_mut().unwrap();
                match delete_folder(sess, &path, trash.as_deref()).await {
                    Ok(()) => {
                        refresh_folders(account_id, &account, sess, cache.as_ref(), &emit).await
                    }
                    Err(e) => {
                        emit(WorkerEvent::error(i18n_f(
                            "Could not delete folder: {e}",
                            &[("e", &(e).to_string())],
                        )));
                        lost = true;
                    }
                }
            }

            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send {
                mut message,
                sent_path,
                ticket,
            } if message.send_at.is_some_and(|t| t > crate::datefmt::now()) => {
                let at = message.send_at.unwrap_or_default();
                restore_msgid_case(cache.as_ref(), &mut message);
                schedule_send(
                    cache.as_ref(),
                    account_id,
                    &account,
                    &message,
                    sent_path.as_deref(),
                    at,
                    &emit,
                );
                emit(WorkerEvent::SendDone {
                    ticket,
                    sent: false,
                });
                // The draft it was opened from is superseded by the queued copy.
                if let Some(o) = message.draft_origin.clone() {
                    if o.account_id == account_id {
                        if let Some(sess) = session.as_mut() {
                            let _ = delete_draft(sess, &o.path, o.uid).await;
                        }
                        if let Some(c) = cache.as_ref() {
                            c.delete_message(account_id, &o.path, o.uid);
                        }
                        if let Ok(messages) = load_messages_retry(
                            account_id,
                            &mut session,
                            &account,
                            o.folder_id,
                            &o.path,
                            &mut use_envelope,
                            cache.as_ref(),
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

            MailRequest::Send {
                mut message,
                sent_path,
                ticket,
            } => {
                emit(WorkerEvent::Status(i18n("Sending…")));
                restore_msgid_case(cache.as_ref(), &mut message);
                match send_smtp(&account, &message).await {
                    Ok(raw) => {
                        emit(WorkerEvent::Status(String::new()));
                        mark_answered(
                            &mut session,
                            cache.as_ref(),
                            account_id,
                            &message.in_reply_to,
                            &message.forwards,
                            &emit,
                        )
                        .await;
                        // Save a copy to the Sent folder, or beside the
                        // original for a reply (#468); sending still counts
                        // as success even if this part fails.
                        let reply = reply_copy_path(
                            &account,
                            account_id,
                            cache.as_ref(),
                            &message.in_reply_to,
                        );
                        let sess = session.as_mut().unwrap();
                        match file_sent_copy(sess, reply.as_deref(), sent_path.as_deref(), &raw)
                            .await
                        {
                            Ok(Some(path)) => {
                                index_sent_copy(
                                    account_id,
                                    &mut session,
                                    &account,
                                    &path,
                                    &mut use_envelope,
                                    cache.as_ref(),
                                    &emit,
                                )
                                .await;
                            }
                            Ok(None) => {}
                            Err(e) => emit(WorkerEvent::error(i18n_f(
                                "Message sent, but saving to Sent failed: {e}",
                                &[("e", &(e).to_string())],
                            ))),
                        }
                        // If sending an edited draft (from this account), remove the
                        // now-obsolete draft and refresh the Drafts folder.
                        if let Some(o) = message.draft_origin.clone() {
                            if o.account_id == account_id {
                                {
                                    let sess = session.as_mut().unwrap();
                                    let _ = delete_draft(sess, &o.path, o.uid).await;
                                }
                                if let Some(c) = cache.as_ref() {
                                    c.delete_message(account_id, &o.path, o.uid);
                                }
                                if let Ok(messages) = load_messages_retry(
                                    account_id,
                                    &mut session,
                                    &account,
                                    o.folder_id,
                                    &o.path,
                                    &mut use_envelope,
                                    cache.as_ref(),
                                )
                                .await
                                {
                                    if let Some(c) = cache.as_ref() {
                                        c.upsert_messages(account_id, &o.path, &messages);
                                    }
                                    emit(WorkerEvent::Messages {
                                        folder_id: o.folder_id,
                                        messages,
                                    });
                                }
                            }
                        }
                        // This version replaces the queued one it was edited from.
                        drop_superseded_outbox(cache.as_ref(), account_id, &message, &emit);
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
                            sent_path.as_deref(),
                            &e.to_string(),
                            smtp_refused(&e),
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
                flush_outbox(
                    cache.as_ref(),
                    account_id,
                    &account,
                    id,
                    &mut session,
                    &mut use_envelope,
                    &emit,
                    true,
                )
                .await;
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
                // A draft is kept as written: signing and encrypting happen
                // at send time (#133).
                let mut message = OutgoingMessage {
                    sign: false,
                    encrypt: false,
                    ..*message
                };
                restore_msgid_case(cache.as_ref(), &mut message);
                match build_draft(&account, &message) {
                    Ok(email) => {
                        let message_id = draft_message_id(&email);
                        let raw = email.formatted();
                        let append_res = {
                            let sess = session.as_mut().unwrap();
                            let r = append_draft(sess, &path, &raw, template).await;
                            // Replace the previous version of this draft (same account).
                            if r.is_ok() {
                                if let Some(o) = &message.draft_origin {
                                    if o.account_id == account_id {
                                        let _ = delete_draft(sess, &o.path, o.uid).await;
                                    }
                                }
                            }
                            r
                        };
                        match append_res {
                            Ok(()) => {
                                if let Some(o) = &message.draft_origin {
                                    if o.account_id == account_id {
                                        if let Some(c) = cache.as_ref() {
                                            c.delete_message(account_id, &o.path, o.uid);
                                        }
                                    }
                                }
                                // Reload Drafts so the saved draft appears.
                                if let Ok(messages) = load_messages_retry(
                                    account_id,
                                    &mut session,
                                    &account,
                                    folder_id,
                                    &path,
                                    &mut use_envelope,
                                    cache.as_ref(),
                                )
                                .await
                                {
                                    if let Some(c) = cache.as_ref() {
                                        c.upsert_messages(account_id, &path, &messages);
                                    }
                                    emit(WorkerEvent::Messages {
                                        folder_id,
                                        messages,
                                    });
                                }
                                // Surface a newly-created Drafts folder in the sidebar.
                                if let Some(sess) = session.as_mut() {
                                    refresh_folders(
                                        account_id,
                                        &account,
                                        sess,
                                        cache.as_ref(),
                                        &emit,
                                    )
                                    .await;
                                }
                                // Saved as a draft instead of sent: the queued
                                // copy it was edited from is now superseded.
                                // A template is a copy, and supersedes nothing.
                                if !template {
                                    drop_superseded_outbox(
                                        cache.as_ref(),
                                        account_id,
                                        &message,
                                        &emit,
                                    );
                                }
                                emit(WorkerEvent::Status(String::new()));
                                emit(WorkerEvent::DraftSaved {
                                    autosave,
                                    message_id: Some(message_id),
                                });
                            }
                            Err(e) => {
                                emit(WorkerEvent::Status(String::new()));
                                draft_not_saved(
                                    autosave,
                                    template,
                                    e.to_string(),
                                    message.clone(),
                                    &emit,
                                );
                                lost = true;
                            }
                        }
                    }
                    Err(e) => {
                        emit(WorkerEvent::Status(String::new()));
                        draft_not_saved(autosave, template, e.to_string(), message, &emit);
                    }
                }
            }

            MailRequest::Reconnect | MailRequest::Settle { .. } => unreachable!("handled above"),
        }

        if invalidates_selection {
            if let Some(sess) = session.as_mut() {
                sess.as_mut().set_selected_folder(None);
            }
        }
        if lost {
            session = None;
        }
    }

    if let Some(mut session) = session {
        let _ = session.logout().await;
    }
}
