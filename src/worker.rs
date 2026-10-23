//! Background mail worker.
//!
//! IMAP is async and its sessions are stateful, while the relm4 UI is driven
//! synchronously on the GTK main thread. To bridge the two, [`spawn`] starts a
//! dedicated OS thread running a tokio runtime that owns the IMAP session. The
//! UI sends [`MailRequest`]s over an unbounded channel; the worker performs the
//! network I/O and pushes [`WorkerEvent`]s back via a caller-supplied callback
//! (in practice, the component's input sender). The mock path implements the
//! exact same protocol so the app behaves identically offline.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use async_imap::types::{Fetch, Flag, NameAttribute};
use async_imap::Session;
use async_native_tls::TlsStream;
use futures::TryStreamExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::models::{Account, Folder, FolderKind, KeywordFinding, MailboxQuota, Message};

mod api;
mod auth;
mod chores;
mod compose;
mod conn_test;
mod drafts;
mod folders;
mod graph;
mod headers;
mod ids;
mod imap_io;
mod jmap;
mod lane;
mod mock;
mod outbox;
mod pop3;
mod preempt;
mod preview;
mod reconcile;
mod render;
mod smtp;
mod strip;
mod summary;
mod tls;

pub use api::{CalendarPart, MailRequest, OutgoingMessage, WorkerEvent};
use auth::{fetch_oauth_token, oauth_user, sign_in_password};
pub(crate) use auth::{forget_access_token, oauth_token_blocking};
use chores::{
    auto_empty_due, auto_empty_roles, clean_lists_due, clean_lists_query, imap_before_date,
    role_folder_path,
};
pub(crate) use compose::build::sent_body_preview;
use compose::build::{build_draft, draft_message_id, restore_msgid_case};
pub use compose::editable::editable_from_raw;
use compose::editable::queued_with_bcc;
pub use compose::recipients::{
    clean_recipient_field, format_recipient, invalid_recipient, parse_recipients,
};
pub use conn_test::{test_alias_smtp_blocking, test_connection_blocking, ConnTest};
use drafts::{draft_not_saved, saving_text};
pub use folders::chip_counts_all;
pub(crate) use folders::folder_order;
use folders::{classify_with_source, display_name};
use graph::*;
use headers::{bytes_to_string, format_date, format_timestamp, unescape_quoted};
pub(crate) use headers::{clean_display_name, decode_header, unescape_quoted_str};
use ids::{merge_msgids, normalize_msgid, normalize_msgids};
use imap_io::ImapIo;
use lane::{cache_lane, related_from_cache, serve_cached_body, thread_summaries_with_members};
use mock::run_mock;
pub(crate) use outbox::{answered_copies, park_for_next_launch, reply_copy_path};
use outbox::{
    current_sent_copy_path, delete_queued, drop_superseded_outbox, emit_outbox, outbox_envelope,
    raw_in_reply_to, schedule_send, send_failed,
};
use pop3::run_pop3;
use preempt::{interrupt_read_for_reader, reorder_reader_loads};
use preview::{
    charset_param, finish_preview, mime_header, preview_from_part, text_in_multipart, text_like,
};
pub use render::attachments::extract_attachments_of;
use render::attachments::{attachments_of, extract_attachments, guess_mime, INLINE_ATTACHMENT_MIN};
pub use render::body::extract_body;
pub(crate) use render::body::wrap_plain;
use render::body_cacheable;
pub(crate) use render::render_raw;
use smtp::{send_raw_smtp, send_smtp, smtp_refused};
pub(crate) use summary::summary_from_raw;
use summary::{mp_importance, mp_thread_ids};
pub use tls::is_hostname_mismatch;
use tls::{is_loopback_host, tls_connector};

/// Number of most-recent messages to fetch attachment info (BODYSTRUCTURE) for;
/// older messages get an envelope-only index row and resolve attachments on open.
const PAGE_SIZE: u32 = 50;

/// Number of most-recent messages shown instantly when a folder is first opened
/// (never synced before). The rest of the folder is indexed in the background so
/// browsing is immediate and search fills in shortly after — like Apple Mail.
const FIRST_PAGE: u32 = 200;

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
const REFS_FETCH_ITEM: &str =
    " BODY.PEEK[HEADER.FIELDS (REFERENCES X-PRIORITY IMPORTANCE PRIORITY)]";

/// Background index backfill: how many messages to fetch per idle drain step.
/// Bigger = fewer round-trips; smaller = more responsive to interleaved requests.
const BACKFILL_CHUNK: usize = 1_000;

/// Pre-download attachments only for this many of the most recent messages;
/// older attachment messages download on demand (with a spinner) when opened.
const PREFETCH_LIMIT: usize = 25;

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
const PREFETCH_BODY_LIMIT: usize = 50;

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

fn attachment_prefetch_tried(account_id: u32, path: &str, uid: u32) -> bool {
    ATTACHMENT_PREFETCH_TRIED
        .lock()
        .map(|s| s.contains(&(account_id, path.to_string(), uid)))
        .unwrap_or(false)
}

fn note_attachment_prefetch_tried(account_id: u32, path: &str, uid: u32) {
    if let Ok(mut s) = ATTACHMENT_PREFETCH_TRIED.lock() {
        s.insert((account_id, path.to_string(), uid));
    }
}

/// What the BODY[TEXT] retry found for a row, as (account, folder id, uid) →
/// preview, kept for the session. The summary fetch comes back empty for
/// the same rows on every sync (iCloud's re-appended messages, file-first
/// messages), so without this the retry ran again each time — three rows
/// cost ~100 KB per sync in one user's log. An empty string is a message
/// with no text to show at all — an encrypted one, a photo — which is then
/// not asked about again either.
static PREVIEW_RETRIED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(u32, u32, u32), String>>,
> = std::sync::LazyLock::new(Default::default);

/// Accounts whose server answers the summary fetch with something our IMAP
/// parser cannot read once the list-preview items (`BODY.PEEK[1]<0.N>` and
/// `BODY.PEEK[1.MIME]`) ride along (#226, Mailfence). The summary fetch is
/// retried without them (see [`step_fetch_mode`]) and, once it works that
/// way, every preview read for the account is skipped for the session: a
/// reply the parser rejects leaves the connection unusable, and the repair
/// reads ask for the same items.
static PREVIEW_ITEMS_REJECTED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<u32>>,
> = std::sync::LazyLock::new(Default::default);

/// Gmail accounts whose labels are read (#374): `true` while they are,
/// `false` once the server has refused `X-GM-LABELS` or answered it with
/// something the parser cannot read, for the rest of the session. A server
/// that only looks like Gmail by its folder names (a mailbox migrated with
/// its `[Gmail]/` folders) refuses the item, and an unreadable reply would
/// otherwise fail every sync after it.
static GMAIL_LABELS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<u32, bool>>> =
    std::sync::LazyLock::new(Default::default);

/// Start reading the account's Gmail labels if it is Gmail, unless the
/// server has already refused them this session.
fn note_gmail_account(account_id: u32, account: &AccountConfig, folders: &[Folder]) {
    if has_gmail_labels(account, folders) {
        if let Ok(mut g) = GMAIL_LABELS.lock() {
            g.entry(account_id).or_insert(true);
        }
    }
}

fn gmail_labels_wanted(account_id: u32) -> bool {
    GMAIL_LABELS
        .lock()
        .map(|g| g.get(&account_id) == Some(&true))
        .unwrap_or(false)
}

fn refuse_gmail_labels(account_id: u32) {
    if let Ok(mut g) = GMAIL_LABELS.lock() {
        g.insert(account_id, false);
    }
}

/// Whether the server has rejected the preview items for this account.
fn previews_rejected(account_id: u32) -> bool {
    PREVIEW_ITEMS_REJECTED
        .lock()
        .map(|r| r.contains(&account_id))
        .unwrap_or(false)
}

/// Whether the account's fetches should read list previews at all: the
/// setting is on and the server has not rejected the preview items.
fn inline_previews_wanted(account_id: u32) -> bool {
    (cfg!(test) || crate::config::load_preview_lines() > 0) && !previews_rejected(account_id)
}

/// Start the account at the summary-fetch mode that worked last time
/// (#226), so a server that rejects the default is not failed against on
/// every launch: two unreadable fetches and two reconnects before the list
/// appeared, in the Mailfence log. Returns the `use_envelope` to begin with.
fn seed_fetch_mode(account_id: u32, email: &str) -> bool {
    let (use_envelope, rejected) = crate::config::load_fetch_mode(email);
    if rejected {
        if let Ok(mut r) = PREVIEW_ITEMS_REJECTED.lock() {
            r.insert(account_id);
        }
    }
    if !use_envelope || rejected {
        tracing::info!(
            target: "hylki::imap",
            "summary fetch: starting with envelope={use_envelope} previews={} (remembered)",
            !rejected
        );
    }
    use_envelope
}

/// A parser error with its byte-array dump cut out: the same reply follows
/// as text, which is the readable half, and the numbers were most of a
/// 160 KB log for one user (#226).
pub fn compact_parse_error(text: &str) -> String {
    let Some(start) = text.find("input: [") else {
        return text.to_string();
    };
    let Some(end) = text[start..].find(']').map(|i| start + i + 1) else {
        return text.to_string();
    };
    format!(
        "{}input: [{} bytes]{}",
        &text[..start],
        text[start + 8..end - 1].split(',').count(),
        &text[end..]
    )
}

/// Whether the error is the IMAP parser giving up on a server's reply, as
/// opposed to the connection dropping or the server saying NO.
fn is_parse_error(e: &async_imap::error::Error) -> bool {
    match e {
        async_imap::error::Error::Parse(_) => true,
        async_imap::error::Error::Io(io) => io.to_string().contains(" during parsing of "),
        _ => false,
    }
}

/// After a summary fetch whose reply the parser rejected, move to the next
/// way of asking for the same summaries. Returns false once every way has
/// been tried. The order keeps the structured ENVELOPE as long as possible
/// and previews as long as possible:
///
/// 1. ENVELOPE + BODYSTRUCTURE with previews (the default)
/// 2. raw headers with previews (iCloud's non-compliant ENVELOPE)
/// 3. ENVELOPE + BODYSTRUCTURE without previews (Mailfence, #226: the
///    preview items make its whole FETCH reply unreadable)
/// 4. raw headers without previews
///
/// With previews switched off in Settings, steps 2 and 3 collapse into one.
fn step_fetch_mode(account_id: u32, use_envelope: &mut bool) -> bool {
    let previews = inline_previews_wanted(account_id);
    match (*use_envelope, previews) {
        (true, true) => {
            *use_envelope = false;
        }
        (false, true) => {
            if let Ok(mut r) = PREVIEW_ITEMS_REJECTED.lock() {
                r.insert(account_id);
            }
            *use_envelope = true;
        }
        (true, false) => {
            *use_envelope = false;
        }
        (false, false) => return false,
    }
    tracing::info!(
        target: "hylki::imap",
        "summary fetch: reply unreadable; retrying with envelope={} previews={}",
        *use_envelope,
        inline_previews_wanted(account_id)
    );
    true
}

/// A worker error with a parser dump made readable. When a server's reply
/// cannot be read, the IMAP parser's error carries the whole reply twice,
/// as a byte array and as text: ten thousand numbers for one message's
/// headers, filling the status bar (#226). The log keeps all of it (see
/// `AppMsg::Error`); this is what the person sees instead.
pub fn readable_error(text: &str) -> String {
    const MARK: &str = " during parsing of \"";
    let Some(start) = text.find("Error(Error { input: [") else {
        return text.to_string();
    };
    let Some(reply) = text[start..].find(MARK).map(|i| start + i + MARK.len()) else {
        return text.to_string();
    };
    let head = text[..start].trim_end().trim_end_matches("io:").trim_end();
    let reply = &text[reply..];
    // The reply is the parser's `{:?}` of it, so line ends are the two
    // characters `\r\n`.
    let first_line = reply.split("\\r\\n").next().unwrap_or(reply);
    let mut excerpt: String = first_line.chars().take(96).collect();
    if excerpt.len() < first_line.len() {
        excerpt.push('…');
    }
    let what = i18n_f(
        "the server's reply could not be read (it began: {reply})",
        &[("reply", &excerpt)],
    );
    if head.is_empty() {
        what
    } else {
        format!("{head} {what}")
    }
}

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
const WATCHER_LIMIT: usize = 4;

/// How long one burst of activity keeps a folder's watcher alive. Every touch
/// — the sweep noticing the folder changed, the watcher itself seeing server
/// activity, the user opening the folder — restarts the clock; a folder left
/// quiet for the whole hour logs its watcher out and falls back to sweep
/// coverage until it changes again.
const WATCH_LEASE: Duration = Duration::from_secs(3600);

/// How long a watcher lets IDLE sit before verifying the count itself. IDLE
/// still wakes it instantly where servers announce changes, but a flag flipped
/// from another client is exactly what some servers (iCloud, observed
/// 2026-08-28) never announce to an idling session — so the watcher recounts
/// every minute regardless and reports only when the number moved. One UID
/// SEARCH per watcher per minute; the ceiling on a watched folder's staleness
/// whatever the server chooses to say.
const WATCH_VERIFY: Duration = Duration::from_secs(60);
/// How often a watcher waiting out a network outage looks again: no network
/// traffic, only a flag read, so it can be short (#442).
const NETWORK_WAIT: Duration = Duration::from_secs(5);

/// How long ending an IDLE (the DONE handshake) may take before the connection
/// is declared wedged and dropped (#91). Unbounded, a server that never answers
/// DONE — or a middlebox that silently killed the idle connection — swallowed
/// the folder click that interrupted the IDLE, forever.
const IDLE_DONE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on the SELECT / IDLE-init round trips entering an IDLE, for the same
/// wedged-connection reason as [`IDLE_DONE_TIMEOUT`].
const IDLE_START_TIMEOUT: Duration = Duration::from_secs(10);

/// What a connection that was never tried, because the network is down,
/// reports (#442).
pub(crate) fn offline_text() -> String {
    i18n("no network connection")
}

type ImapSession = Session<ImapIo<TlsStream<TcpStream>>>;

/// Run blocking work (an HTTP request, a cache query) off the async
/// threads; a task that panicked reads as an error.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| Err("task failed".into()))
}

/// A distinct accent color per account (cycles through a small palette).
pub(crate) fn accent_for(account_id: u32) -> &'static str {
    const PALETTE: [&str; 6] = [
        "#3584e4", "#2ec27e", "#e5a50a", "#e66100", "#9141ac", "#c01c28",
    ];
    PALETTE[(account_id.saturating_sub(1) as usize) % PALETTE.len()]
}

/// Start a worker thread for one account (`Some`) or the offline sample data
/// (`None`). Returns the sender used to issue requests. `account_id` stamps all
/// emitted folders/messages and keys the cache.
pub fn spawn(
    account_id: u32,
    account: Option<AccountConfig>,
    emit: impl Fn(WorkerEvent) + Clone + Send + 'static,
) -> mpsc::UnboundedSender<MailRequest> {
    let (tx, rx) = mpsc::unbounded_channel();

    // The UI talks to the cache lane, which answers what the disk cache can
    // and hands the rest to the worker (`tx`), so a cached message never
    // waits behind a sync the worker is busy with. See [`cache_lane`].
    let (ui_tx, lane_rx) = mpsc::unbounded_channel();
    {
        let protocol = account.as_ref().map(|a| a.protocol);
        let emit = emit.clone();
        std::thread::Builder::new()
            .name(format!("hylki-cache-{account_id}"))
            .spawn(move || cache_lane(account_id, protocol, lane_rx, tx, emit))
            .expect("failed to spawn cache lane thread");
    }

    std::thread::Builder::new()
        .name(format!("hylki-mail-{account_id}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    emit(WorkerEvent::error(i18n_f(
                        "runtime error: {e}",
                        &[("e", &(e).to_string())],
                    )));
                    return;
                }
            };
            // A LocalSet so the per-folder IDLE watchers ([`watch_folder`]) can
            // be spawned as (non-Send) local tasks sharing this thread; they
            // are cancelled with it when the worker winds down.
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, run(account_id, account, rx, emit));
        })
        .expect("failed to spawn mail worker thread");

    ui_tx
}

async fn run(
    account_id: u32,
    account: Option<AccountConfig>,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent) + Clone + Send + 'static,
) {
    match account {
        Some(account) if account.protocol == crate::config::Protocol::Pop3 => {
            run_pop3(account_id, account, rx, emit).await
        }
        Some(account) if account.protocol == crate::config::Protocol::Graph => {
            run_graph(account_id, account, rx, emit).await
        }
        Some(account) if account.protocol == crate::config::Protocol::Jmap => {
            jmap::run_jmap(account_id, account, rx, emit).await
        }
        Some(account) => run_imap(account_id, account, rx, emit).await,
        None => run_mock(account_id, rx, emit).await,
    }
}

// ---------------------------------------------------------------------------
// IMAP path
// ---------------------------------------------------------------------------

async fn run_imap(
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

/// Connect, announce the account, and list folders, updating the cache. Emits a
/// fresh `Folders` event only when the listing differs from the cache (so an
/// unchanged list doesn't trigger a redundant UI rebuild). Returns `None` (after
/// emitting an error) if the connection could not be established.
async fn connect_and_list(
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Option<ImapSession> {
    // The Refresh button already says the network is down; an error per
    // account and per attempt would only repeat it (#442).
    if !crate::power::reaches(&account.imap_host) {
        return None;
    }
    emit(WorkerEvent::Status(i18n_f(
        "Connecting to {host}…",
        &[("host", &account.imap_host)],
    )));

    let result = match connect(account).await {
        Ok(mut session) => {
            emit(WorkerEvent::Account(Account {
                id: account_id,
                name: account.name.clone(),
                email: account.email.clone(),
                label: account.display_label(),
                accent: accent_for(account_id).into(),
            }));
            let cached = cache
                .map(|c| c.load_folders(account_id))
                .unwrap_or_default();
            match list_folder_metadata(
                account_id,
                &mut session,
                &account.folder_roles,
                &account.hidden_folders,
                Some(&cached),
            )
            .await
            {
                // An empty LIST can't be right — INBOX always exists (RFC
                // 3501). Keep whatever the cache has instead of wiping it.
                Ok(folders) if folders.is_empty() => {}
                Ok(folders) => {
                    note_gmail_account(account_id, account, &folders);
                    let changed = cache
                        .map(|c| {
                            !crate::cache::folders_equal(&c.load_folders(account_id), &folders)
                        })
                        .unwrap_or(true);
                    if let Some(c) = cache {
                        c.save_folders(account_id, &folders);
                    }
                    if changed {
                        emit(WorkerEvent::Folders(folders));
                    }
                }
                Err(e) => emit(WorkerEvent::net_error(i18n_f(
                    "Could not list folders: {e}",
                    &[("e", &(e).to_string())],
                ))),
            }
            Some(session)
        }
        Err(e) => {
            emit(WorkerEvent::net_error(i18n_f(
                "Connection failed: {e}",
                &[("e", &(e).to_string())],
            )));
            None
        }
    };

    emit(WorkerEvent::Status(String::new()));
    result
}

/// Run `load_messages`, retrying once over a fresh login if the first attempt
/// fails (typically a server-dropped idle connection: "unexpected EOF"). On
/// success the (possibly new) session is stored back; on failure it is dropped.
async fn load_messages_retry(
    account_id: u32,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    folder_id: u32,
    path: &str,
    use_envelope: &mut bool,
    cache: Option<&Cache>,
) -> Result<Vec<Message>, async_imap::error::Error> {
    let mut s = session.take().expect("session ensured before call");
    let mut result = load_messages(account_id, &mut s, folder_id, path, *use_envelope, cache).await;

    // A non-empty success is trustworthy — keep the session and return it.
    if matches!(&result, Ok(msgs) if !msgs.is_empty()) {
        *session = Some(s);
        return result;
    }

    // Otherwise the result is an error (stale connection → EOF, or a reply
    // our parser rejected) or an empty mailbox (which a stale session can
    // return without erroring). Re-verify on a fresh login. An *unverified*
    // empty result is treated as a failure so it can never wipe cached mail.
    //
    // A reply the parser rejected means the server answers this way of
    // asking with something non-compliant — iCloud's ENVELOPE, Mailfence's
    // FETCH once the preview items are in it (#226) — so each such failure
    // moves the account one step down the [`step_fetch_mode`] ladder for the
    // rest of the session, until a way works or the ladder runs out. Any
    // other error is retried once as it was.
    let mut stepped = false;
    for _ in 0..4 {
        if let Err(e) = &result {
            if is_parse_error(e) && !step_fetch_mode(account_id, use_envelope) {
                return result;
            }
            stepped |= is_parse_error(e);
        }
        match connect(account).await {
            Ok(fresh) => s = fresh,
            Err(_) => {
                return match result {
                    Ok(_) => Err(async_imap::error::Error::ConnectionLost),
                    Err(e) => Err(e),
                }
            }
        }
        result = load_messages(account_id, &mut s, folder_id, path, *use_envelope, cache).await;
        match &result {
            Ok(_) => {
                *session = Some(s);
                // A mode reached by stepping has now loaded a folder: the
                // next launch starts here.
                if stepped {
                    crate::config::save_fetch_mode(
                        &account.email,
                        *use_envelope,
                        previews_rejected(account_id),
                    );
                }
                return result;
            }
            Err(e) if is_parse_error(e) => continue,
            Err(_) => return result,
        }
    }
    result
}

/// Like [`load_messages_retry`], but for a single body.
async fn load_body_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uid: u32,
) -> Result<(String, crate::models::SenderCheck, bool), async_imap::error::Error> {
    let mut s = session.take().expect("session ensured before call");
    let mut res = load_body(&mut s, path, uid).await;
    if res.is_err() {
        if let Ok(fresh) = connect(account).await {
            s = fresh;
            res = load_body(&mut s, path, uid).await;
        }
    }
    if res.is_ok() {
        *session = Some(s);
    }
    res
}

/// [`load_bodies`] with [`load_body_retry`]'s one reconnect-and-retry.
async fn load_bodies_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uids: &[u32],
) -> Result<
    std::collections::HashMap<u32, (String, crate::models::SenderCheck, bool)>,
    async_imap::error::Error,
> {
    let mut s = session.take().expect("session ensured before call");
    let mut res = load_bodies(&mut s, path, uids).await;
    if res.is_err() {
        if let Ok(fresh) = connect(account).await {
            s = fresh;
            res = load_bodies(&mut s, path, uids).await;
        }
    }
    if res.is_ok() {
        *session = Some(s);
    }
    res
}

/// Whether the account is Gmail, where a folder is a label and one message
/// sits in several of them at once.
pub(crate) fn has_gmail_labels(account: &AccountConfig, folders: &[Folder]) -> bool {
    let host = account.imap_host.to_ascii_lowercase();
    host.ends_with("gmail.com")
        || host.ends_with("googlemail.com")
        || folders
            .iter()
            .any(|f| f.path.starts_with("[Gmail]/") || f.path.starts_with("[Google Mail]/"))
}

/// After a move into Trash or Spam on Gmail, drop the moved mail's copies
/// under its other labels from the cache: Gmail removed them with the move
/// (see [`Cache::drop_label_copies`]). Any other server keeps a copy in
/// another folder as a message of its own, so nothing is dropped there.
fn drop_gmail_label_copies(
    cache: &Cache,
    account_id: u32,
    account: &AccountConfig,
    from: &str,
    uids: &[u32],
    dest: &str,
) {
    let folders = cache.load_folders(account_id);
    let labels = has_gmail_labels(account, &folders);
    let binned = folders
        .iter()
        .any(|f| f.path == dest && matches!(f.kind, FolderKind::Trash | FolderKind::Junk));
    if labels && binned {
        let n = cache.drop_label_copies(account_id, from, uids, dest);
        if n > 0 {
            tracing::debug!("dropped {n} cached label copies of mail moved to {dest}");
        }
    }
}

/// Account for conversation members a body fetch came back without.
///
/// Either the message is no longer in the folder, or the server lists it and
/// will not hand it over (#226). The cache cannot tell: Gmail takes every
/// label off mail moved to Trash, so the copies cached under All Mail and
/// the other labels outlive it there, and a reply to a deleted conversation
/// brought them all back as blank cards (#257). The server is asked which
/// of the UIDs it still has. The ones it does not are dropped from the
/// cache and the reader is told they are gone; the rest keep the
/// placeholder, which says in place that the server gave nothing.
async fn settle_missing_bodies(
    session: &mut Option<ImapSession>,
    account_id: u32,
    path: &str,
    missing: Vec<(u32, u32)>,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let uids: Vec<u32> = missing.iter().map(|(_, uid)| *uid).collect();
    // The folder is still selected from the fetch.
    let present = match session.as_mut() {
        Some(sess) => search_uids(sess, format!("UID {}", uid_set(&uids)))
            .await
            .ok(),
        None => None,
    };
    for (message_id, uid) in missing {
        if present.as_ref().is_some_and(|p| !p.contains(&uid)) {
            if let Some(c) = cache {
                c.delete_message(account_id, path, uid);
            }
            emit(WorkerEvent::Gone {
                message_id,
                path: path.to_string(),
                uid,
            });
        } else {
            emit(WorkerEvent::Body {
                message_id,
                path: path.to_string(),
                body: "(empty message)".to_string(),
            });
        }
    }
}

async fn load_source_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uid: u32,
) -> Result<String, async_imap::error::Error> {
    let mut s = session.take().expect("session ensured before call");
    let mut res = load_source(&mut s, path, uid).await;
    if res.is_err() {
        if let Ok(fresh) = connect(account).await {
            s = fresh;
            res = load_source(&mut s, path, uid).await;
        }
    }
    if res.is_ok() {
        *session = Some(s);
    }
    res
}

/// Fetch the raw RFC 822 source (headers + body) of a message, undecoded.
async fn load_source(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<String, async_imap::error::Error> {
    select_for_read(session, path).await?;
    let raw = fetch_raw_message(session, uid)
        .await?
        .ok_or_else(|| no_content_error(uid))?;
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// Result of an IDLE wait: a request to handle, a folder that was refreshed
/// (new mail arrived), or the channel closing.
enum IdleOutcome {
    Request(MailRequest),
    Refreshed,
    Quiet,
    Closed,
}

/// Download and cache one queued attachment (body + attachments), if the session
/// is live. Connects on demand; clears the queue if offline.
async fn run_one_prefetch(
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
fn queue_body_prefetch(
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

/// Queue the newest messages that have attachments (not already cached) for
/// background attachment pre-download, so new mail's attachments are ready too.
/// Whether a folder's attachments feed the gallery (so its mail is worth
/// prefetching): everything except Trash, Junk and Drafts.
fn gallery_folder(kind: crate::models::FolderKind) -> bool {
    use crate::models::FolderKind::*;
    !matches!(kind, Trash | Junk | Drafts)
}

/// Gallery eligibility of a folder by path, looked up from the cached folder list.
/// Unknown folders default to eligible.
fn folder_is_gallery(cache: Option<&Cache>, account_id: u32, path: &str) -> bool {
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

fn queue_attachment_prefetch(
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
async fn run_one_body_prefetch(
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

/// Block for the next request (used as the IDLE fallback).
async fn recv_one(rx: &mut mpsc::UnboundedReceiver<MailRequest>) -> IdleOutcome {
    match rx.recv().await {
        Some(req) => IdleOutcome::Request(req),
        None => IdleOutcome::Closed,
    }
}

/// Enter IMAP IDLE on `path` for up to `timeout_secs` and wait for new mail or
/// an incoming request. Returns `Refreshed` (after re-syncing) only when the
/// server actually reports new data; `Quiet` on timeout (so the caller can do a
/// prefetch pass and re-IDLE); `Request` to be handled normally; `Closed` when
/// the channel ends. Any IDLE error falls back to a plain receive.
#[allow(clippy::too_many_arguments)]
/// End an IDLE and recover the session, giving the DONE handshake
/// [`IDLE_DONE_TIMEOUT`] to complete. `None` on timeout or error — the future
/// (and with it the wedged connection) is simply dropped, and the caller's
/// next request reconnects (#91: an unbounded await here swallowed the very
/// request that interrupted the IDLE).
async fn idle_done(
    mut handle: async_imap::extensions::idle::Handle<ImapIo<TlsStream<TcpStream>>>,
) -> Option<ImapSession> {
    handle.as_mut().set_idle(false);
    match tokio::time::timeout(IDLE_DONE_TIMEOUT, handle.done()).await {
        Ok(res) => res.ok(),
        Err(_) => {
            tracing::info!("IDLE DONE went unanswered; dropping the connection");
            None
        }
    }
}

async fn idle_wait(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    folder_id: u32,
    path: &str,
    rx: &mut mpsc::UnboundedReceiver<MailRequest>,
    cache: Option<&Cache>,
    use_envelope: &mut bool,
    body_prefetch: &mut std::collections::VecDeque<(String, u32)>,
    att_prefetch: &mut std::collections::VecDeque<(String, u32)>,
    body_emitted: &std::collections::HashSet<(String, u32)>,
    emit: &impl Fn(WorkerEvent),
    timeout_secs: u64,
) -> IdleOutcome {
    let Some(mut sess) = session.take() else {
        return recv_one(rx).await;
    };
    // Stale or wedged connection — drop it so the next request reconnects.
    // The timeout matters as much as the error: a dead-but-open connection
    // (silently dropped by a NAT during a long IDLE) answers nothing at all.
    let missed = match tokio::time::timeout(IDLE_START_TIMEOUT, sel(&mut sess, path)).await {
        Ok(Ok(mailbox)) => {
            let missed = missed_new_mail(account_id, path, mailbox.uid_next);
            // Taken as seen now: the sync below records it again, and one
            // that fails must not send every later wait round this branch.
            if missed {
                note_synced_uid_next(account_id, path, mailbox.uid_next);
            }
            missed
        }
        _ => return recv_one(rx).await,
    };

    enum Wake {
        Idle(async_imap::error::Result<async_imap::extensions::idle::IdleResponse>),
        Request(Option<MailRequest>),
        /// Mail arrived while the connection was busy; the SELECT just
        /// made reported it, and an IDLE started now never would.
        Missed,
    }
    let wake = if missed {
        tracing::info!("idle: new mail in {path} arrived between commands, syncing");
        *session = Some(sess);
        Wake::Missed
    } else {
        tracing::debug!(target: "hylki::imap", "> IDLE");
        let mut handle = sess.idle();
        match tokio::time::timeout(IDLE_START_TIMEOUT, handle.init()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                *session = idle_done(handle).await;
                return recv_one(rx).await;
            }
            // No answer: the handle (and its connection) is dropped, not reused.
            Err(_) => return recv_one(rx).await,
        }
        let wake = {
            handle.as_mut().set_idle(true);
            let (idle_fut, stop) = handle.wait_with_timeout(Duration::from_secs(timeout_secs));
            tokio::select! {
                r = idle_fut => Wake::Idle(r),
                req = rx.recv() => { drop(stop); Wake::Request(req) }
            }
        };
        *session = idle_done(handle).await;
        wake
    };

    match wake {
        Wake::Request(Some(req)) => IdleOutcome::Request(req),
        Wake::Request(None) => IdleOutcome::Closed,
        // Only re-sync on actual new data; a plain timeout is Quiet.
        Wake::Missed | Wake::Idle(Ok(async_imap::extensions::idle::IdleResponse::NewData(_))) => {
            if session.is_some() {
                if let Ok(messages) = load_messages_retry(
                    account_id,
                    session,
                    account,
                    folder_id,
                    path,
                    use_envelope,
                    cache,
                )
                .await
                {
                    if let Some(c) = cache {
                        c.upsert_messages(account_id, path, &messages);
                    }
                    // Pre-download the new mail's body (and any attachments) so
                    // opening it is instant.
                    queue_body_prefetch(body_prefetch, path, &messages, body_emitted);
                    queue_attachment_prefetch(att_prefetch, path, &messages, cache, account_id);
                    emit_body_hits(
                        session, account, account_id, folder_id, path, &messages, cache, emit,
                    )
                    .await;
                    emit(WorkerEvent::Messages {
                        folder_id,
                        messages,
                    });
                    // Refresh the true unread count too. IDLE only re-synced the
                    // message list; without this the sidebar chip never moves when
                    // new mail lands in a background (unfocused) inbox — it would
                    // take an explicit reload / "All Inboxes" refresh to appear.
                    if let Some(sess) = session.as_mut() {
                        let kind = cached_folder_kind(cache, account_id, path);
                        if let Some(unread) = selected_chip_count(sess, kind).await {
                            emit(WorkerEvent::FolderUnread { folder_id, unread });
                        }
                    }
                }
            }
            IdleOutcome::Refreshed
        }
        Wake::Idle(_) => IdleOutcome::Quiet,
    }
}

/// A leased per-folder IDLE watcher: its task and the shared last-touch clock
/// that keeps it alive (see [`watch_folder`]).
struct FolderWatch {
    handle: tokio::task::JoinHandle<()>,
    lease: Rc<Cell<std::time::Instant>>,
}

/// Whether a folder may hold a dynamic watcher slot. The Inbox is out because
/// it has a permanent watcher; Sent, Drafts and Templates because changes
/// there are the user's own doing; Trash and Junk because nobody needs
/// to-the-second unread counts on either — the periodic sweep keeps them
/// honest.
fn watchable(kind: FolderKind) -> bool {
    !matches!(
        kind,
        FolderKind::Inbox
            | FolderKind::Sent
            | FolderKind::Drafts
            | FolderKind::Templates
            | FolderKind::Trash
            | FolderKind::Junk
    )
}

/// Give every [`watchable`] folder the sweep just saw change a watcher (or a
/// fresh lease on the one it has), staggering the connects of whatever
/// actually spawns — several logins in the same instant look like an attack to
/// providers that rate-limit authentication (iCloud does).
fn watch_changed_folders(
    watchers: &mut std::collections::HashMap<String, FolderWatch>,
    account: &AccountConfig,
    changed: &[(FolderKind, String)],
    emit: &(impl Fn(WorkerEvent) + Clone + 'static),
) {
    // A sweep claiming more folders moved than the pool could ever hold says
    // nothing about which are hot — that shape is a bulk event (a client
    // reorganizing, a suspect baseline), and admitting them would churn every
    // standing watcher out for nothing. Let the next sweep pick the real ones.
    if changed.iter().filter(|(k, _)| watchable(*k)).count() > WATCHER_LIMIT {
        tracing::info!(
            "sweep: bulk change ({} folders), not adjusting watchers",
            changed.len()
        );
        return;
    }
    let mut stagger = 0;
    for (kind, path) in changed {
        if watchable(*kind) && watch_active_folder(watchers, account, path, stagger, emit) {
            stagger += 1;
        }
    }
}

/// Put `path` on the dynamic watch list: renew its lease if its watcher is
/// still running, otherwise spawn one — evicting the stalest lease when the
/// pool is at [`WATCHER_LIMIT`], so fresh activity always wins a slot. Returns
/// whether a new watcher was spawned (callers stagger connects with this).
/// Callers are responsible for the [`watchable`] check.
fn watch_active_folder(
    watchers: &mut std::collections::HashMap<String, FolderWatch>,
    account: &AccountConfig,
    path: &str,
    stagger: usize,
    emit: &(impl Fn(WorkerEvent) + Clone + 'static),
) -> bool {
    if let Some(w) = watchers.get(path) {
        if !w.handle.is_finished() {
            w.lease.set(std::time::Instant::now());
            return false;
        }
    }
    // A metered connection keeps to the Inbox's own watcher; the sweep still
    // covers the rest (#442).
    if crate::power::saving_data() {
        return false;
    }
    // Lapsed watchers exited on their own; drop their leftover entries.
    watchers.retain(|_, w| !w.handle.is_finished());
    while watchers.len() >= WATCHER_LIMIT {
        // The candidate was touched just now, so it is by definition fresher
        // than the stalest sitting lease. Aborting drops that connection
        // without a LOGOUT; the server reaps it.
        let stalest = watchers
            .iter()
            .min_by_key(|(_, w)| w.lease.get())
            .map(|(p, _)| p.clone());
        let Some(stalest) = stalest else { break };
        if let Some(w) = watchers.remove(&stalest) {
            w.handle.abort();
            tracing::info!("unwatching {stalest}: slot needed for {path}");
        }
    }
    tracing::info!("watching {path} (recent activity)");
    let lease = Rc::new(Cell::new(std::time::Instant::now()));
    let handle = tokio::task::spawn_local(watch_folder(
        account.clone(),
        path.to_string(),
        Duration::from_secs(2 * stagger as u64),
        Some(lease.clone()),
        emit.clone(),
    ));
    watchers.insert(path.to_string(), FolderWatch { handle, lease });
    true
}

/// One extra IMAP connection that does nothing but sit in IDLE on a single
/// folder, so a change made anywhere else — rule-filed new mail, a message
/// read on another device — moves that folder's unread chip within seconds
/// instead of at the next sweep. This is how Apple Mail keeps folders live:
/// IDLE only reports on the selected mailbox, so watching N folders takes N
/// connections.
///
/// With a `lease` (the dynamic pool), the watcher lives on recency: every wake
/// the server reports renews the shared clock, as do the sweep and the user
/// opening the folder (via [`watch_active_folder`]); once the clock goes a
/// whole [`WATCH_LEASE`] untouched, the watcher logs out and the folder falls
/// back to sweep coverage until it changes again. Without one (the Inbox), it
/// watches for the life of the worker.
///
/// EXAMINE (never SELECT) keeps the watcher read-only, and it fetches no
/// message content — only a UID SEARCH for the unseen count. Failures are
/// quiet by design: a watcher is a freshness bonus on top of the sweep, so it
/// reconnects with backoff rather than surfacing errors, and retires for good
/// when a healthy session refuses the folder (deleted or renamed — spinning on
/// a mailbox that no longer exists helps no one).
async fn watch_folder(
    account: AccountConfig,
    path: String,
    start_delay: Duration,
    lease: Option<Rc<Cell<std::time::Instant>>>,
    emit: impl Fn(WorkerEvent),
) {
    let lapsed = |lease: &Option<Rc<Cell<std::time::Instant>>>| {
        lease
            .as_ref()
            .is_some_and(|l| l.get().elapsed() >= WATCH_LEASE)
    };
    let mut backoff = start_delay;
    loop {
        tokio::time::sleep(backoff).await;
        if lapsed(&lease) {
            // The lease ran out while offline or backing off; don't reconnect
            // just to watch a folder nothing has touched for an hour.
            return;
        }
        // Offline: wait for the network, neither dialing nor growing the
        // backoff, so the watcher is back within moments of it (#442).
        while !crate::power::reaches(&account.imap_host) {
            tokio::time::sleep(NETWORK_WAIT).await;
            if lapsed(&lease) {
                return;
            }
        }
        if lease.is_some() && crate::power::saving_data() {
            tracing::info!("unwatching {path}: the connection is metered");
            return;
        }
        let Ok(mut sess) = connect(&account).await else {
            // Offline, or the server's connection cap is spent (the main
            // session always connects first and is never contended by this).
            tracing::debug!("watcher for {path}: connect failed, backing off");
            backoff = if backoff < Duration::from_secs(60) {
                Duration::from_secs(60)
            } else {
                (backoff * 2).min(Duration::from_secs(900))
            };
            continue;
        };
        backoff = Duration::from_secs(60);
        match tokio::time::timeout(IDLE_START_TIMEOUT, exam(&mut sess, &path)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                tracing::info!("unwatching {path}: no longer selectable");
                return;
            }
            Err(_) => continue, // wedged straight away; reconnect
        }
        // Catch up on whatever happened while unwatched (startup, or the gap a
        // dropped connection left), then settle into the IDLE/verify cycle.
        let mut last_count = selected_unseen(&mut sess).await;
        match last_count {
            Some(unread) => emit(WorkerEvent::FolderUnreadByPath {
                path: path.clone(),
                unread,
            }),
            None => continue, // wedged straight out of the gate; reconnect
        }
        loop {
            if lapsed(&lease) {
                tracing::info!("unwatching {path}: quiet for the whole lease");
                let _ = sess.logout().await;
                return;
            }
            if lease.is_some() && crate::power::saving_data() {
                tracing::info!("unwatching {path}: the connection is metered");
                let _ = sess.logout().await;
                return;
            }
            if !crate::power::reaches(&account.imap_host) {
                break; // the socket went with the network
            }
            // A short IDLE round: woken instantly where the server announces
            // changes, and capped at [`WATCH_VERIFY`] so the recount below
            // covers the servers that stay silent — never past the lease's
            // own deadline, so a lapsed watcher frees its connection promptly.
            let timeout = lease.as_ref().map_or(WATCH_VERIFY.as_secs(), |l| {
                WATCH_LEASE
                    .saturating_sub(l.get().elapsed())
                    .as_secs()
                    .clamp(1, WATCH_VERIFY.as_secs())
            });
            tracing::debug!(target: "hylki::imap", "> IDLE");
            let mut handle = sess.idle();
            match tokio::time::timeout(IDLE_START_TIMEOUT, handle.init()).await {
                Ok(Ok(())) => {}
                _ => break, // error or wedged; drop the connection and redial
            }
            let woke = {
                handle.as_mut().set_idle(true);
                let (idle_fut, _stop) = handle.wait_with_timeout(Duration::from_secs(timeout));
                matches!(
                    idle_fut.await,
                    Ok(async_imap::extensions::idle::IdleResponse::NewData(_))
                )
            };
            match idle_done(handle).await {
                Some(s) => sess = s,
                None => break,
            }
            // Recount whether pushed awake or on the verify tick; a changed
            // number (or any pushed wake) is activity that renews the lease,
            // and only a changed number is worth an event.
            match selected_unseen(&mut sess).await {
                Some(unread) => {
                    let moved = last_count != Some(unread);
                    if woke || moved {
                        if let Some(l) = &lease {
                            l.set(std::time::Instant::now());
                        }
                    }
                    if moved {
                        tracing::debug!("watcher for {path}: unseen now {unread}");
                        last_count = Some(unread);
                        emit(WorkerEvent::FolderUnreadByPath {
                            path: path.clone(),
                            unread,
                        });
                    }
                }
                None => break, // count unavailable = session gone; reconnect
            }
        }
    }
}

async fn load_raw_retry(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    uid: u32,
) -> Result<Vec<u8>, async_imap::error::Error> {
    let mut s = session.take().expect("session ensured before call");
    let mut res = load_raw(&mut s, path, uid).await;
    if res.is_err() {
        if let Ok(fresh) = connect(account).await {
            s = fresh;
            res = load_raw(&mut s, path, uid).await;
        }
    }
    if res.is_ok() {
        *session = Some(s);
    }
    res
}

/// Fetch the raw RFC 822 bytes of a message (binary-safe, for attachments).
/// The whole message out of one FETCH item, however the server labelled
/// it: `BODY[]` or `RFC822` as asked, or the header and text as two items,
/// which is what a server that will not hand over `BODY[]` (#226) can
/// still be asked for, joined back into one message.
fn raw_of(f: &Fetch) -> Option<Vec<u8>> {
    if let Some(b) = f.body() {
        return Some(b.to_vec());
    }
    match (f.header(), f.text()) {
        (Some(h), Some(t)) => {
            let mut raw = h.to_vec();
            if !raw.ends_with(b"\r\n\r\n") {
                if raw.ends_with(b"\r\n") {
                    raw.extend_from_slice(b"\r\n");
                } else {
                    raw.extend_from_slice(b"\r\n\r\n");
                }
            }
            raw.extend_from_slice(t);
            Some(raw)
        }
        (None, Some(t)) => Some(t.to_vec()),
        _ => None,
    }
}

/// Fetch one message's raw bytes, asking three ways before giving up: the
/// usual `BODY.PEEK[]`, then `RFC822`, then the header and text as two
/// items. Mailfence answered the first with a FETCH that carried no
/// message at all (#226), which read as an empty message and was shown,
/// and cached, as one. `None` means the server would not hand it over any
/// way; the reply's shape is logged so the export says what came back.
async fn fetch_raw_message(
    session: &mut ImapSession,
    uid: u32,
) -> Result<Option<Vec<u8>>, async_imap::error::Error> {
    for (n, items) in [
        "(BODY.PEEK[])",
        "(RFC822)",
        "(BODY.PEEK[HEADER] BODY.PEEK[TEXT])",
    ]
    .iter()
    .enumerate()
    {
        let fetches: Vec<Fetch> = fetch_uids(session, uid.to_string(), items)
            .await?
            .try_collect()
            .await?;
        if let Some(raw) = fetches.iter().find_map(raw_of) {
            if n > 0 {
                tracing::info!(target: "hylki::imap", "message {uid}: content came with {items}");
            }
            return Ok(Some(raw));
        }
        tracing::warn!(
            target: "hylki::imap",
            "message {uid}: {} FETCH item(s) came back for {items}, none carrying the message: {}",
            fetches.len(),
            fetches.iter().map(|f| format!("{f:?}")).collect::<Vec<_>>().join(" | ").chars().take(600).collect::<String>()
        );
    }
    Ok(None)
}

/// The error for a message the server would not hand over (#226): surfaced
/// rather than shown as empty, and never cached.
fn no_content_error(uid: u32) -> async_imap::error::Error {
    async_imap::error::Error::Bad(format!(
        "the server sent no content for message {uid} (asked as BODY.PEEK[], RFC822, and header plus text)"
    ))
}

async fn load_raw(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<Vec<u8>, async_imap::error::Error> {
    select_for_read(session, path).await?;
    fetch_raw_message(session, uid)
        .await?
        .ok_or_else(|| no_content_error(uid))
}

/// Status text for the attachment pre-download queue (empty = idle).
fn prefetch_status(remaining: usize) -> String {
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

// ---------------------------------------------------------------------------
// Outbox
// ---------------------------------------------------------------------------

/// Mark what a sent message answered (#454): `\Answered` on the original
/// of a reply, `$Forwarded` on that of a forward. A mailbox that takes no
/// keywords has nowhere to keep `$Forwarded`, and the arrow then lasts until
/// the folder syncs. A failure is only logged: the message went, and that
/// is what the user asked about.
async fn mark_answered(
    session: &mut Option<ImapSession>,
    cache: Option<&Cache>,
    account_id: u32,
    in_reply_to: &str,
    forwards: &str,
    emit: &impl Fn(WorkerEvent),
) {
    for (path, uid, forwarded) in answered_copies(cache, account_id, in_reply_to, forwards) {
        if let Some(sess) = session.as_mut() {
            let stored = if forwarded {
                store_keyword(sess, &path, uid, FORWARDED_KEYWORD, true)
                    .await
                    .map(|_| ())
            } else {
                store_flag(sess, &path, uid, "\\Answered", true).await
            };
            if let Err(e) = stored {
                tracing::warn!("[account {account_id}] could not mark {path}/{uid} answered: {e}");
            }
        }
        if let Some(c) = cache {
            c.set_answered(account_id, &path, uid, forwarded);
        }
        emit(WorkerEvent::Answered {
            path,
            uid,
            forwarded,
        });
    }
}

/// File a sent copy: beside the original when `reply` names a folder, and
/// in `sent` when there is none or that folder will not take it (one the
/// user may only read). The folder that took it, or why none did.
async fn file_sent_copy(
    session: &mut ImapSession,
    reply: Option<&str>,
    sent: Option<&str>,
    raw: &[u8],
) -> Result<Option<String>, async_imap::error::Error> {
    if let Some(path) = reply {
        match append_to_sent(session, path, raw).await {
            Ok(()) => return Ok(Some(path.to_string())),
            Err(e) => tracing::warn!("could not file the reply's copy in {path}, using Sent: {e}"),
        }
    }
    let Some(path) = sent else { return Ok(None) };
    append_to_sent(session, path, raw)
        .await
        .map(|()| Some(path.to_string()))
}

/// Try the queue: one message when `id` is given, otherwise all of them, oldest
/// first. A message that goes out is copied to Sent and dropped from the queue;
/// one that fails again keeps its place with the new reason recorded. `loud`
/// reports failures to the UI — background sweeps stay quiet, since the user did
/// not ask for anything and already knows the message is waiting.
async fn flush_outbox(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    id: Option<u32>,
    session: &mut Option<ImapSession>,
    use_envelope: &mut bool,
    emit: &impl Fn(WorkerEvent),
    loud: bool,
) {
    let Some(cache) = cache else { return };
    // A scheduled message waits for its time; asking for it by id (Send Now,
    // or the app's clock) sends it regardless.
    let now = crate::datefmt::now();
    let items: Vec<crate::models::OutboxItem> = cache
        .outbox_items(account_id)
        .into_iter()
        .filter(|item| match id {
            Some(wanted) => wanted == item.id,
            None => item.send_at.is_none_or(|t| t <= now),
        })
        .collect();
    if items.is_empty() {
        return;
    }

    if loud {
        emit(WorkerEvent::Status(i18n("Sending…")));
    }
    let mut sent_any = false;
    let mut sent = 0usize;
    for item in items {
        let envelope = match outbox_envelope(&item) {
            Some(envelope) => envelope,
            None => {
                // Unsendable as stored: keep it, but say so rather than retrying
                // it forever in silence.
                cache.record_outbox_failure(
                    item.id,
                    "the stored recipients are not valid addresses",
                );
                continue;
            }
        };
        match send_raw_smtp(account, &envelope, &item.raw).await {
            Ok(()) => {
                sent_any = true;
                sent += 1;
                cache.delete_outbox(item.id);
                mark_answered(
                    session,
                    Some(cache),
                    account_id,
                    &raw_in_reply_to(&item.raw),
                    &item.forwards,
                    emit,
                )
                .await;
                let dest = current_sent_copy_path(
                    account,
                    account_id,
                    Some(cache),
                    item.sent_path.as_deref(),
                );
                let reply = reply_copy_path(
                    account,
                    account_id,
                    Some(cache),
                    &raw_in_reply_to(&item.raw),
                );
                if let Some(sess) = session.as_mut() {
                    let copy = queued_with_bcc(&item.raw, &item.rcpts);
                    match file_sent_copy(sess, reply.as_deref(), dest.as_deref(), &copy).await {
                        Ok(Some(path)) => {
                            index_sent_copy(
                                account_id,
                                session,
                                account,
                                &path,
                                use_envelope,
                                Some(cache),
                                emit,
                            )
                            .await;
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!("outbox: sent, but saving the copy failed: {e}"),
                    }
                }
            }
            Err(e) => {
                cache.record_outbox_failure(item.id, &e.to_string());
                if loud {
                    emit(WorkerEvent::error(i18n_f(
                        "Still could not send “{subject}”: {e}",
                        &[
                            ("subject", &item.subject.to_string()),
                            ("e", &e.to_string()),
                        ],
                    )));
                }
                // A failure now will almost certainly repeat for the rest of the
                // queue (the connection is down), so stop rather than hammering.
                break;
            }
        }
    }
    if loud {
        emit(WorkerEvent::Status(String::new()));
    }
    if sent_any {
        // A background flush is silent up to here, but the user was last told the
        // message had *failed* to send. They have to learn that it since went.
        if !loud {
            emit(WorkerEvent::Notice(match sent {
                1 => "A message waiting in the Outbox has been sent".to_string(),
                n => format!("{n} messages waiting in the Outbox have been sent"),
            }));
        }
        emit(WorkerEvent::Sent);
    }
    emit_outbox(Some(cache), account_id, emit);
}

/// XOAUTH2 SASL authenticator for async-imap.
struct XOAuth2 {
    user: String,
    token: String,
    step: u8,
}

impl async_imap::Authenticator for XOAuth2 {
    type Response = Vec<u8>;
    fn process(&mut self, challenge: &[u8]) -> Self::Response {
        self.step += 1;
        if self.step == 1 {
            // Initial SASL response.
            format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token).into_bytes()
        } else {
            // The server rejected auth and sent an error challenge; XOAUTH2 requires
            // an empty response so the server then sends the tagged error (rather
            // than the exchange deadlocking).
            let _ = challenge;
            Vec::new()
        }
    }
}

async fn append_to_sent(
    session: &mut ImapSession,
    path: &str,
    raw: &[u8],
) -> Result<(), async_imap::error::Error> {
    // Mark the saved copy as already read.
    append_msg(session, path, Some("(\\Seen)"), raw).await
}

/// The copy of a message just sent was APPENDed to `path`: list that folder
/// again so the copy is in the cache now. A conversation is assembled from
/// the cache (`related_from_cache`), so until the copy is indexed the reply
/// cannot join the thread it answers, and the next visit to the Sent folder
/// was the only thing that indexed it (#199). The folder's listing goes out
/// too, so a list showing that folder (copies filed into the Inbox) is
/// current as well.
async fn index_sent_copy(
    account_id: u32,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    path: &str,
    use_envelope: &mut bool,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(c) = cache else { return };
    let Some(folder_id) = c
        .load_folders(account_id)
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.id)
    else {
        return;
    };
    match load_messages_retry(
        account_id,
        session,
        account,
        folder_id,
        path,
        use_envelope,
        cache,
    )
    .await
    {
        Ok(messages) => {
            c.upsert_messages(account_id, path, &messages);
            emit(WorkerEvent::Messages {
                folder_id,
                messages,
            });
        }
        Err(e) => tracing::warn!("could not list {path} after saving the sent copy: {e}"),
    }
}

/// APPEND a draft to the Drafts folder (flagged `\Draft \Seen`), creating the
/// mailbox first if the server doesn't have one yet.
async fn append_draft(
    session: &mut ImapSession,
    path: &str,
    raw: &[u8],
    template: bool,
) -> Result<(), async_imap::error::Error> {
    // A template (#360) is no draft: another client would offer to finish
    // and send it rather than start a message from it.
    let flags = if template {
        "(\\Seen)"
    } else {
        "(\\Draft \\Seen)"
    };
    if append_msg(session, path, Some(flags), raw).await.is_ok() {
        return Ok(());
    }
    // Folder likely doesn't exist — create it and retry.
    let _ = create_box(session, path).await;
    let _ = subscribe_box(session, path).await;
    append_msg(session, path, Some(flags), raw).await
}

/// Delete a superseded draft (the previous version being replaced or sent):
/// flag it `\Deleted` and expunge it from the Drafts folder.
async fn delete_draft(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<(), async_imap::error::Error> {
    sel(session, path).await?;
    let _: Result<Vec<Fetch>, _> = store_uids(session, uid.to_string(), "+FLAGS (\\Deleted)")
        .await?
        .try_collect()
        .await;
    let _: Vec<u32> = expunge_all(session).await?.try_collect().await?;
    Ok(())
}

/// Permanently erase messages from a mailbox: flag them `\Deleted`, then expunge.
///
/// `UID EXPUNGE` (RFC 4315) removes only the UIDs we asked for, leaving alone
/// anything another client flagged `\Deleted` in the meantime. Servers without
/// UIDPLUS reject it, so fall back to a plain `EXPUNGE`.
async fn purge_messages(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
) -> Result<(), async_imap::error::Error> {
    if uids.is_empty() {
        return Ok(());
    }
    let set = uid_set(uids);
    sel(session, path).await?;
    flag_deleted_and_expunge(session, &set).await
}

/// A Gmail account's Trash, where an erase has to happen: expunged from
/// any other folder, Gmail mail only loses that label. `None` elsewhere.
fn gmail_trash(cache: Option<&Cache>, account_id: u32, account: &AccountConfig) -> Option<String> {
    let folders = cache?.load_folders(account_id);
    if !has_gmail_labels(account, &folders) {
        return None;
    }
    folders
        .into_iter()
        .find(|f| f.kind == FolderKind::Trash)
        .map(|f| f.path)
}

/// Erase Gmail mail from outside Trash, the way Gmail's own "Delete
/// forever" goes: into Trash, then expunged there. The move gives the mail
/// new UIDs, so it is found again in Trash by `X-GM-MSGID`, which stays the
/// same under every label. A server that does not know the item (a mailbox
/// that only looks like Gmail by its folder names) erases where it is.
async fn gmail_erase(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
    trash: &str,
) -> Result<(), async_imap::error::Error> {
    if uids.is_empty() {
        return Ok(());
    }
    sel(session, path).await?;
    let Some(ids) = fetch_gmail_msgids(session, uids).await? else {
        return flag_deleted_and_expunge(session, &uid_set(uids)).await;
    };
    for chunk in uids.chunks(300) {
        uid_move(session, &uid_set(chunk), trash).await?;
    }
    sel(session, trash).await?;
    let mut binned = std::collections::HashSet::new();
    for chunk in ids.chunks(100) {
        binned.extend(search_uids(session, gmail_msgid_query(chunk)).await?);
    }
    if binned.is_empty() {
        return Ok(());
    }
    let binned: Vec<u32> = binned.into_iter().collect();
    flag_deleted_and_expunge(session, &uid_set(&binned)).await
}

/// `X-GM-MSGID a`, or `OR X-GM-MSGID a OR X-GM-MSGID b X-GM-MSGID c` for
/// several: IMAP's OR takes two keys.
fn gmail_msgid_query(ids: &[u64]) -> String {
    let mut q = "OR ".repeat(ids.len().saturating_sub(1));
    q.push_str(
        &ids.iter()
            .map(|id| format!("X-GM-MSGID {id}"))
            .collect::<Vec<_>>()
            .join(" "),
    );
    q
}

/// The `X-GM-MSGID` of each message at `uids` in the selected folder, or
/// `None` when the server answers `BAD` (it is not Gmail). Run raw for the
/// same reason as [`fetch_gmail_labels`].
async fn fetch_gmail_msgids<T>(
    session: &mut Session<T>,
    uids: &[u32],
) -> Result<Option<Vec<u64>>, async_imap::error::Error>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    use async_imap::imap_proto::{AttributeValue, Response, Status};
    let cmd = format!("UID FETCH {} (UID X-GM-MSGID)", uid_set(uids));
    wire(&cmd);
    let id = session.run_command(&cmd).await?;
    let mut out = Vec::new();
    loop {
        let reply = match session.read_response().await {
            Some(Ok(reply)) => reply,
            Some(Err(e)) => {
                let r: Result<(), _> = Err(async_imap::error::Error::Io(e));
                wired(&cmd, &r);
                return Err(r.unwrap_err());
            }
            None => {
                wired::<()>(&cmd, &Err(async_imap::error::Error::ConnectionLost));
                return Err(async_imap::error::Error::ConnectionLost);
            }
        };
        match reply.parsed() {
            Response::Fetch(_, attrs) => {
                out.extend(attrs.iter().find_map(|a| match a {
                    AttributeValue::GmailMsgId(id) => Some(*id),
                    _ => None,
                }));
            }
            Response::Done {
                tag,
                status,
                information,
                ..
            } if *tag == id => {
                let why = information.as_deref().unwrap_or("");
                return match status {
                    Status::Ok => {
                        tracing::debug!(target: "hylki::imap", "< OK ({})", cmd_head(&cmd));
                        Ok(Some(out))
                    }
                    Status::Bad => {
                        tracing::warn!(target: "hylki::imap", "< BAD {why} ({cmd})");
                        Ok(None)
                    }
                    _ => {
                        tracing::warn!(target: "hylki::imap", "< {status:?} {why} ({cmd})");
                        Err(async_imap::error::Error::No(why.to_string()))
                    }
                };
            }
            _ => {}
        }
    }
}

/// Flag a UID set `\Deleted` and expunge it: `UID EXPUNGE` where the server
/// has UIDPLUS, a plain `EXPUNGE` otherwise.
async fn flag_deleted_and_expunge(
    session: &mut ImapSession,
    set: &str,
) -> Result<(), async_imap::error::Error> {
    let _: Vec<Fetch> = store_uids(session, set, "+FLAGS (\\Deleted)")
        .await?
        .try_collect()
        .await?;
    // A server without UIDPLUS answers `BAD`, which surfaces while draining the
    // response stream rather than from the call itself — so judge it on the
    // collected result, not with `?`.
    let uid_expunged = match expunge_uids(session, set).await {
        Ok(stream) => stream.try_collect::<Vec<u32>>().await.is_ok(),
        Err(_) => false,
    };
    if !uid_expunged {
        let _: Vec<u32> = expunge_all(session).await?.try_collect().await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The IMAP conversation, for the console (#132 and every "it failed on my
// server" report): each command Hylki sends is logged as `> …` and the
// server's verdict as `< OK` or `< <error>`, under the `hylki::imap`
// target — commands and results only, never a message body or a password.
// The console keeps it whether or not console mode is on, so "Export log"
// carries it after the fact; stderr sees only the failures.
fn wire(cmd: &str) {
    tracing::debug!(target: "hylki::imap", "> {cmd}");
}

fn wired<T>(cmd: &str, r: &Result<T, async_imap::error::Error>) {
    match r {
        Ok(_) => tracing::debug!(target: "hylki::imap", "< OK ({})", cmd_head(cmd)),
        Err(e) => {
            tracing::warn!(target: "hylki::imap", "< {} ({cmd})", compact_parse_error(&e.to_string()))
        }
    }
}

/// The command's verb and first argument, for the terse OK line.
fn cmd_head(cmd: &str) -> String {
    cmd.split_whitespace().take(3).collect::<Vec<_>>().join(" ")
}

/// Keywords that are a server's or another client's bookkeeping, not tags a
/// person set: left out of the tag finder's report. System flags (`\Seen`…)
/// never reach here as keywords, but a leading backslash is rejected too.
pub fn is_system_keyword(keyword: &str) -> bool {
    let k = keyword.to_ascii_lowercase();
    if k.starts_with('\\') {
        return true;
    }
    const SYSTEM: &[&str] = &[
        "$junk",
        "$notjunk",
        "junk",
        "nonjunk",
        "notjunk",
        "$autojunk",
        "$forwarded",
        "$mdnsent",
        "$phishing",
        "$submitted",
        "$submitpending",
        "$hasattachment",
        "$hasnoattachment",
        "$signed",
        "$encrypted",
        "$recent",
        "$isnotification",
        "$ismailinglist",
        "$istrusted",
        "$notification",
    ];
    SYSTEM.contains(&k.as_str())
        // Apple Mail's flag colors and Fastmail's internal annotations.
        || k.starts_with("$mailflagbit")
        || k.starts_with("$x-me-")
}

async fn sel(
    session: &mut ImapSession,
    path: &str,
) -> Result<async_imap::types::Mailbox, async_imap::error::Error> {
    let cmd = format!("SELECT {}", quote_mailbox(path));
    wire(&cmd);
    session.as_mut().set_selected_folder(None);
    let r = session.select(path).await;
    if r.is_ok() {
        session.as_mut().set_selected_folder(Some(path));
    }
    wired(&cmd, &r);
    r
}

async fn exam(
    session: &mut ImapSession,
    path: &str,
) -> Result<async_imap::types::Mailbox, async_imap::error::Error> {
    let cmd = format!("EXAMINE {}", quote_mailbox(path));
    wire(&cmd);
    session.as_mut().set_selected_folder(None);
    let r = session.examine(path).await;
    if r.is_ok() {
        session.as_mut().set_selected_folder(Some(path));
    }
    wired(&cmd, &r);
    r
}

/// PEEK reads work in either SELECT or EXAMINE. Reusing the mailbox already
/// selected on this connection avoids a round trip for every prefetched body.
/// List syncs and mutations still select explicitly to get current metadata
/// and the write mode they need. A new connection starts unselected.
async fn select_for_read(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    if !session.as_mut().is_selected(path) {
        sel(session, path).await?;
    }
    Ok(())
}

async fn fetch_uids<'a, S1: AsRef<str>, S2: AsRef<str>>(
    session: &'a mut ImapSession,
    set: S1,
    items: S2,
) -> Result<
    impl futures::Stream<Item = Result<Fetch, async_imap::error::Error>> + 'a + Send + Unpin,
    async_imap::error::Error,
> {
    let (set, items) = (set.as_ref().to_string(), items.as_ref().to_string());
    let cmd = format!("UID FETCH {set} {items}");
    wire(&cmd);
    let r = session.uid_fetch(set, items).await;
    wired(&cmd, &r);
    r
}

async fn store_uids<'a, S1: AsRef<str>, S2: AsRef<str>>(
    session: &'a mut ImapSession,
    set: S1,
    query: S2,
) -> Result<
    impl futures::Stream<Item = Result<Fetch, async_imap::error::Error>> + 'a + Send + Unpin,
    async_imap::error::Error,
> {
    let (set, query) = (set.as_ref().to_string(), query.as_ref().to_string());
    let cmd = format!("UID STORE {set} {query}");
    wire(&cmd);
    let r = session.uid_store(set, query).await;
    wired(&cmd, &r);
    r
}

async fn copy_uids(
    session: &mut ImapSession,
    set: &str,
    dest: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("UID COPY {set} {}", quote_mailbox(dest));
    wire(&cmd);
    let r = session.uid_copy(set, quote_mailbox(dest)).await;
    wired(&cmd, &r);
    r
}

async fn mv_uids(
    session: &mut ImapSession,
    set: &str,
    dest: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("UID MOVE {set} {}", quote_mailbox(dest));
    wire(&cmd);
    let r = session.uid_mv(set, dest).await;
    wired(&cmd, &r);
    r
}

async fn expunge_uids<'a, S: AsRef<str>>(
    session: &'a mut ImapSession,
    set: S,
) -> Result<
    impl futures::Stream<Item = Result<u32, async_imap::error::Error>> + 'a + Send,
    async_imap::error::Error,
> {
    let set = set.as_ref().to_string();
    let cmd = format!("UID EXPUNGE {set}");
    wire(&cmd);
    let r = session.uid_expunge(set).await;
    wired(&cmd, &r);
    r
}

async fn expunge_all<'a>(
    session: &'a mut ImapSession,
) -> Result<
    impl futures::Stream<Item = Result<u32, async_imap::error::Error>> + 'a + Send,
    async_imap::error::Error,
> {
    let cmd = "EXPUNGE";
    wire(cmd);
    let r = session.expunge().await;
    wired(cmd, &r);
    r
}

async fn search_uids<S: AsRef<str>>(
    session: &mut ImapSession,
    query: S,
) -> Result<std::collections::HashSet<u32>, async_imap::error::Error> {
    let cmd = format!("UID SEARCH {}", query.as_ref());
    wire(&cmd);
    let r = session.uid_search(query.as_ref()).await;
    wired(&cmd, &r);
    r
}

async fn create_box(session: &mut ImapSession, path: &str) -> Result<(), async_imap::error::Error> {
    let cmd = format!("CREATE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.create(path).await;
    wired(&cmd, &r);
    r
}

async fn subscribe_box(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("SUBSCRIBE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.subscribe(path).await;
    wired(&cmd, &r);
    r
}

async fn unsubscribe_box(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("UNSUBSCRIBE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.unsubscribe(path).await;
    wired(&cmd, &r);
    r
}

async fn rename_box(
    session: &mut ImapSession,
    from: &str,
    to: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("RENAME {} {}", quote_mailbox(from), quote_mailbox(to));
    wire(&cmd);
    let r = session.rename(from, to).await;
    wired(&cmd, &r);
    r
}

async fn delete_box(session: &mut ImapSession, path: &str) -> Result<(), async_imap::error::Error> {
    let cmd = format!("DELETE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.delete(path).await;
    wired(&cmd, &r);
    r
}

async fn box_status(
    session: &mut ImapSession,
    path: &str,
    items: &str,
) -> Result<async_imap::types::Mailbox, async_imap::error::Error> {
    let cmd = format!("STATUS {} {items}", quote_mailbox(path));
    wire(&cmd);
    let r = session.status(path, items).await;
    wired(&cmd, &r);
    r
}

/// The IMAP flags a message moved in from another account (#265) keeps.
fn import_flags(seen: bool, flagged: bool) -> Option<String> {
    let flags: Vec<&str> = [(seen, "\\Seen"), (flagged, "\\Flagged")]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, f)| f)
        .collect();
    (!flags.is_empty()).then(|| format!("({})", flags.join(" ")))
}

async fn append_msg(
    session: &mut ImapSession,
    path: &str,
    flags: Option<&str>,
    raw: &[u8],
) -> Result<(), async_imap::error::Error> {
    let cmd = format!(
        "APPEND {} {} ({} bytes)",
        quote_mailbox(path),
        flags.unwrap_or(""),
        raw.len()
    );
    wire(&cmd);
    let r = session.append(path, flags, None, raw).await;
    wired(&cmd, &r);
    r
}

/// Put what a message holds after an attachment was removed (#289) into
/// the cache: the files, the paperclip, and the gallery's index of them.
fn cache_rewritten(c: &Cache, account_id: u32, path: &str, uid: u32, raw: &[u8]) {
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
fn strip_refused(
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
struct StripFailure {
    text: String,
    connection: bool,
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
async fn strip_imap_attachment(
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
async fn find_appended(
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

/// The flags a rewritten message carries over: all but the ones the server
/// sets itself or that would delete it.
fn kept_flags<'a>(flags: impl Iterator<Item = Flag<'a>>) -> Vec<String> {
    flags
        .filter_map(|f| match f {
            Flag::Seen => Some("\\Seen".to_string()),
            Flag::Answered => Some("\\Answered".to_string()),
            Flag::Flagged => Some("\\Flagged".to_string()),
            Flag::Draft => Some("\\Draft".to_string()),
            Flag::Custom(k) if !k.starts_with('\\') => Some(k.to_string()),
            _ => None,
        })
        .collect()
}

async fn append_dated(
    session: &mut ImapSession,
    path: &str,
    flags: Option<&str>,
    date: Option<&str>,
    raw: &[u8],
) -> Result<(), async_imap::error::Error> {
    let cmd = format!(
        "APPEND {} {} {} ({} bytes)",
        quote_mailbox(path),
        flags.unwrap_or(""),
        date.unwrap_or(""),
        raw.len()
    );
    wire(&cmd);
    let r = session.append(path, flags, date, raw).await;
    wired(&cmd, &r);
    r
}

/// `UID MOVE` where the server offers it (RFC 6851), else what every client
/// did before there was a MOVE: `UID COPY`, flag the originals `\Deleted`,
/// expunge (issue #128 — a server without MOVE answers `UID MOVE` with
/// "command not permitted with UID", and every move, deleting included,
/// failed on it). The capability is asked for each time: it is one short
/// round trip, and a session's answer can change after login on some
/// servers, so it is not worth caching wrong.
async fn uid_move(
    session: &mut ImapSession,
    set: &str,
    dest: &str,
) -> Result<(), async_imap::error::Error> {
    let can_move = match session.capabilities().await {
        Ok(caps) => caps.has_str("MOVE"),
        // If even CAPABILITY fails, let MOVE produce the real error.
        Err(_) => true,
    };
    tracing::debug!(target: "hylki::imap", "server {} MOVE", if can_move { "offers" } else { "lacks" });
    if can_move {
        return mv_uids(session, set, dest).await;
    }
    // `uid_copy` sends the mailbox name as given (unlike `uid_mv`, which
    // quotes it), so a name with a space — iCloud's "Deleted Messages", the
    // one server this path actually serves — went out bare and every move
    // and delete there answered "BAD Parse Error" (1.22.0 to 1.23.0).
    // `copy_uids` quotes it.
    copy_uids(session, set, dest).await?;
    flag_deleted_and_expunge(session, set).await
}

/// The mailbox size out of a GETQUOTAROOT answer: the first quota with a
/// STORAGE resource and a limit, whose figures are in units of 1024 octets.
fn imap_storage_quota(quotas: &[async_imap::types::Quota]) -> Option<MailboxQuota> {
    use async_imap::types::QuotaResourceName;
    quotas
        .iter()
        .flat_map(|q| &q.resources)
        .find(|r| r.name == QuotaResourceName::Storage && r.limit > 0)
        .map(|r| MailboxQuota {
            used: r.usage.saturating_mul(1024),
            limit: r.limit.saturating_mul(1024),
        })
}

/// A mailbox name as an IMAP quoted string: backslashes and quotes escaped,
/// the whole wrapped in quotes (RFC 3501 §4.3).
fn quote_mailbox(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push('"');
    for c in name.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// A LIST or LSUB name as the server meant it. imap-proto hands a quoted
/// string back with its escapes still in, so `"\\All"` arrives as `\\All`
/// and quote_mailbox would escape it a second time (#514). Atoms cannot hold
/// a backslash, so only a literal could be wrongly unescaped, and only one
/// that holds `\\` or `\"`.
fn imap_unquote(raw: &str) -> std::borrow::Cow<'_, str> {
    if !raw.contains('\\') {
        return std::borrow::Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next @ ('\\' | '"')) => out.push(next),
                Some(next) => {
                    out.push(c);
                    out.push(next);
                }
                None => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

async fn store_flag(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    flag: &str,
    add: bool,
) -> Result<(), async_imap::error::Error> {
    sel(session, path).await?;
    let op = if add { "+FLAGS" } else { "-FLAGS" };
    let query = format!("{op} ({flag})");
    // Drain the resulting FETCH stream so the command completes.
    let _: Vec<Fetch> = store_uids(session, uid.to_string(), query)
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// Add or remove a user-defined keyword (a tag, #71). Answers whether the
/// server took it: `Ok(false)` means the mailbox's PERMANENTFLAGS lists no
/// `\*` (and not this keyword either), so a STORE would be refused or
/// forgotten at the end of the session — the caller keeps the tag locally.
/// A server that sends no PERMANENTFLAGS at all keeps every flag (RFC 3501).
async fn store_keyword(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    keyword: &str,
    add: bool,
) -> Result<bool, async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    let allowed = mailbox.permanent_flags.is_empty()
        || mailbox.permanent_flags.iter().any(|f| match f {
            Flag::MayCreate => true,
            Flag::Custom(k) => k.eq_ignore_ascii_case(keyword),
            _ => false,
        });
    if !allowed {
        return Ok(false);
    }
    let op = if add { "+FLAGS" } else { "-FLAGS" };
    let query = format!("{op} ({keyword})");
    let _: Vec<Fetch> = store_uids(session, uid.to_string(), query)
        .await?
        .try_collect()
        .await?;
    Ok(true)
}

/// The Gmail labels a tag means (#374), as the server writes label names:
/// the tag's name when it is put on; when it comes off, every label that
/// stands for it, its name's and its keyword's.
fn tag_labels(keyword: &str, add: bool) -> Vec<String> {
    let name = crate::config::load_tags()
        .into_iter()
        .find(|t| t.keyword.eq_ignore_ascii_case(keyword))
        .map(|t| t.name)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| keyword.to_string());
    let mut out = vec![crate::mutf7::encode(&name)];
    if !add && !name.eq_ignore_ascii_case(keyword) {
        out.push(crate::mutf7::encode(keyword));
    }
    out
}

/// Put Gmail labels on a message in the selected folder, or take them off.
async fn store_gmail_labels(
    session: &mut ImapSession,
    uid: u32,
    labels: &[String],
    add: bool,
) -> Result<(), async_imap::error::Error> {
    let quoted: Vec<String> = labels
        .iter()
        .map(|l| format!("\"{}\"", l.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    let op = if add { "+X-GM-LABELS" } else { "-X-GM-LABELS" };
    let _: Vec<Fetch> = store_uids(
        session,
        uid.to_string(),
        format!("{op} ({})", quoted.join(" ")),
    )
    .await?
    .try_collect()
    .await?;
    Ok(())
}

/// Move a message to `dest`, creating (and subscribing to) the destination
/// mailbox first if it doesn't exist yet. Returns whether a folder was created,
/// so the caller can refresh the folder list.
async fn move_or_create(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    if uid_move(session, &uid.to_string(), dest).await.is_ok() {
        return Ok(false);
    }
    // The move failed — most likely the destination mailbox doesn't exist (the
    // account has no Archive/Junk/… folder yet). Create it, subscribe, and retry;
    // if it still fails, surface that error.
    let created = create_box(session, dest).await.is_ok();
    let _ = subscribe_box(session, dest).await;
    sel(session, path).await?;
    uid_move(session, &uid.to_string(), dest).await?;
    Ok(created)
}

async fn move_message(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    move_or_create(session, path, uid, dest).await
}

/// Move many messages from `path` to `dest` with as few IMAP commands as
/// possible: one SELECT, then a UID MOVE per chunk of the UID set (chunked so a
/// huge selection never overflows the server's command-length limit). Creates and
/// subscribes to `dest` on demand. Returns whether the destination was created.
async fn move_messages(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    let mut created = false;
    let mut ensured_dest = false;
    for chunk in uids.chunks(300) {
        let set = uid_set(chunk);
        if uid_move(session, &set, dest).await.is_ok() {
            continue;
        }
        // First failure is most likely a missing destination mailbox — create,
        // subscribe, re-select the source, and retry this chunk (then the rest).
        if !ensured_dest {
            created = create_box(session, dest).await.is_ok();
            let _ = subscribe_box(session, dest).await;
            ensured_dest = true;
            sel(session, path).await?;
        }
        uid_move(session, &set, dest).await?;
    }
    Ok(created)
}

/// Create (and subscribe to) a new mailbox.
async fn create_folder(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    create_box(session, path).await?;
    let _ = subscribe_box(session, path).await;
    Ok(())
}

/// RENAME a mailbox — the server carries any inferior hierarchy along
/// (RFC 3501 §6.3.5) — and follow the subscription to the new name.
async fn rename_folder(
    session: &mut ImapSession,
    old_path: &str,
    new_path: &str,
) -> Result<(), async_imap::error::Error> {
    // Sub-folders go along with the RENAME, but their subscriptions are the
    // client's to move: Dovecot leaves them at the old names, and a client
    // that shows subscribed folders only (Thunderbird's default) would lose
    // them. So they are listed first, and each one's subscription is moved too.
    // async-imap sends the pattern as given: quoted here, names have spaces.
    let pattern = quote_mailbox(&format!("{old_path}*"));
    let children: Vec<String> = match session.list(Some(""), Some(&pattern)).await {
        Ok(stream) => stream
            .try_collect::<Vec<async_imap::types::Name>>()
            .await
            .unwrap_or_default()
            .iter()
            .map(|n| imap_unquote(n.name()).into_owned())
            .filter(|p| {
                p.len() > old_path.len() + 1
                    && p.starts_with(old_path)
                    && matches!(p.as_bytes()[old_path.len()], b'/' | b'.' | b'\\')
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    rename_box(session, old_path, new_path).await?;
    let _ = unsubscribe_box(session, old_path).await;
    let _ = subscribe_box(session, new_path).await;
    for child in children {
        let moved = format!("{new_path}{}", &child[old_path.len()..]);
        let _ = unsubscribe_box(session, &child).await;
        let _ = subscribe_box(session, &moved).await;
    }
    Ok(())
}

/// Delete a mailbox after moving all of its messages to `trash` (creating the
/// trash mailbox if needed). With no trash target the contents are discarded.
async fn delete_folder(
    session: &mut ImapSession,
    path: &str,
    trash: Option<&str>,
) -> Result<(), async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    if mailbox.exists > 0 {
        if let Some(trash) = trash {
            if !trash.eq_ignore_ascii_case(path) {
                // Move everything to Trash; create it first if the move fails.
                if uid_move(session, "1:*", trash).await.is_err() {
                    let _ = create_box(session, trash).await;
                    let _ = subscribe_box(session, trash).await;
                    sel(session, path).await?;
                    uid_move(session, "1:*", trash).await?;
                }
            }
        }
    }
    // A mailbox can't be deleted while selected — close it first.
    let _ = session.close().await;
    delete_box(session, path).await?;
    Ok(())
}

/// Mark every message in a folder as read (`\Seen`) in one STORE.
async fn mark_all_read(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    if mailbox.exists == 0 {
        return Ok(());
    }
    let _: Vec<Fetch> = store_uids(session, "1:*", "+FLAGS (\\Seen)")
        .await?
        .try_collect()
        .await?;
    Ok(())
}

/// Mark a message as spam following common conventions: set the `$Junk` keyword
/// and clear `$NotJunk` (so server-side filters / other clients can learn), then
/// move it to the Junk folder. Keyword stores are best-effort — some servers
/// reject custom keywords — but the move is authoritative.
async fn mark_spam(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    for query in ["+FLAGS ($Junk)", "-FLAGS ($NotJunk)"] {
        if let Ok(stream) = store_uids(session, uid.to_string(), query).await {
            let _: Result<Vec<Fetch>, _> = stream.try_collect().await;
        }
    }
    move_or_create(session, path, uid, dest).await
}

/// The reverse of [`mark_spam`] (#168): set `$NotJunk` and clear `$Junk` on
/// every uid (best-effort, as above), then move them back to `dest`.
async fn mark_ham(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
    dest: &str,
) -> Result<bool, async_imap::error::Error> {
    sel(session, path).await?;
    let set = uid_set(uids);
    for query in ["+FLAGS ($NotJunk)", "-FLAGS ($Junk)"] {
        if let Ok(stream) = store_uids(session, set.clone(), query).await {
            let _: Result<Vec<Fetch>, _> = stream.try_collect().await;
        }
    }
    move_messages(session, path, uids, dest).await
}

/// Whether the IMAP connection opens in plaintext and upgrades with STARTTLS
/// instead of negotiating TLS from the first byte. 993 is always implicit TLS
/// and 143 is the conventional STARTTLS port; a local bridge listens on a port
/// of its own (Proton Bridge defaults to 1143) and speaks STARTTLS there.
/// GNOME Online Accounts records which it is, and is followed (#254).
fn imap_uses_starttls(account: &AccountConfig) -> bool {
    if let Some(sec) = &account.security {
        return sec.imap_starttls;
    }
    account.imap_port != 993 && (account.imap_port == 143 || is_loopback_host(&account.imap_host))
}

/// Hard ceiling on one IMAP connection attempt — TCP, TLS, and authentication
/// together. A server (or middlebox) that accepts the socket and then stalls
/// would otherwise hang the worker with no error ever surfacing.
const IMAP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn connect(account: &AccountConfig) -> Result<ImapSession, Box<dyn std::error::Error>> {
    if !crate::power::reaches(&account.imap_host) {
        return Err(offline_text().into());
    }
    // Read before the connect timeout starts: the keyring's own wait is not
    // the server's, and must not be reported as one.
    let password = sign_in_password(account, false).await?;
    match tokio::time::timeout(IMAP_CONNECT_TIMEOUT, connect_inner(account, &password)).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "connecting to {} timed out after {} seconds",
            account.imap_host,
            IMAP_CONNECT_TIMEOUT.as_secs()
        )
        .into()),
    }
}

async fn connect_inner(
    account: &AccountConfig,
    password: &str,
) -> Result<ImapSession, Box<dyn std::error::Error>> {
    // An Exchange (EWS) account imported from Online Accounts before #316
    // has no IMAP server, and the resolver's answer for an empty name says
    // nothing a user could act on.
    if account.imap_host.trim().is_empty() {
        return Err(i18n(
            "This account has no IMAP server. Hylki can't use Exchange accounts from Online Accounts: remove it and add it as an IMAP or Microsoft 365 account.",
        )
        .into());
    }
    let tcp = TcpStream::connect((account.imap_host.as_str(), account.imap_port)).await?;
    let accept_invalid = account
        .security
        .as_ref()
        .is_some_and(|s| s.imap_accept_invalid_certs);
    let tls = tls_connector(
        &account.imap_host,
        account.tls_accept_hostname_mismatch,
        accept_invalid,
    );
    let client = if imap_uses_starttls(account) {
        let mut plain = async_imap::Client::new(tcp);
        // Consume the plaintext greeting before issuing STARTTLS. Nothing secret
        // has been sent at this point; the credentials go out below, after the
        // socket has been upgraded.
        let _ = plain.read_response().await;
        plain.run_command_and_check_ok("STARTTLS", None).await?;
        let stream = tls
            .connect(account.imap_host.as_str(), plain.into_inner())
            .await?;
        // A server that accepted STARTTLS does not send a second greeting.
        async_imap::Client::new(ImapIo::new(stream))
    } else {
        let stream = tls.connect(account.imap_host.as_str(), tcp).await?;
        let mut client = async_imap::Client::new(ImapIo::new(stream));
        // Consume the server greeting before issuing commands. LOGIN tolerates an
        // unread greeting, but the AUTHENTICATE handshake reads it as the command
        // reply and deadlocks — so read it explicitly here.
        let _ = client.read_response().await;
        client
    };
    // The sign-in for the console: mechanism and user, never the secret.
    let session = if account.oauth {
        // XOAUTH2 with a fresh access token (from GOA or a native refresh token).
        tracing::debug!(target: "hylki::imap", "> AUTHENTICATE XOAUTH2 {}", oauth_user(account));
        let token = match fetch_oauth_token(account).await {
            Some(t) => t,
            None => {
                tracing::warn!(target: "hylki::imap", "< no OAuth token (AUTHENTICATE XOAUTH2)");
                return Err("could not get an OAuth token".into());
            }
        };
        let auth = XOAuth2 {
            user: oauth_user(account),
            token,
            step: 0,
        };
        let r = client
            .authenticate("XOAUTH2", auth)
            .await
            .map_err(|(e, _client)| e);
        match &r {
            Ok(_) => tracing::debug!(target: "hylki::imap", "< OK (AUTHENTICATE XOAUTH2)"),
            Err(e) => {
                tracing::warn!(target: "hylki::imap", "< {e} (AUTHENTICATE XOAUTH2)");
                forget_access_token(account);
            }
        }
        r?
    } else {
        tracing::debug!(target: "hylki::imap", "> LOGIN {} ****", account.username);
        let r = client
            .login(&account.username, password)
            .await
            .map_err(|(e, _client)| e);
        match &r {
            Ok(_) => tracing::debug!(target: "hylki::imap", "< OK (LOGIN)"),
            Err(e) => tracing::warn!(target: "hylki::imap", "< {e} (LOGIN)"),
        }
        r?
    };
    Ok(session)
}

async fn list_folders(
    account_id: u32,
    session: &mut ImapSession,
    roles: &std::collections::BTreeMap<String, String>,
    hidden: &[String],
) -> Result<Vec<Folder>, async_imap::error::Error> {
    list_folder_metadata(account_id, session, roles, hidden, None).await
}

/// Discover folders without a serial STATUS round trip for every folder when
/// cached counts are supplied. The interruptible idle sweep refreshes counts.
async fn list_folder_metadata(
    account_id: u32,
    session: &mut ImapSession,
    roles: &std::collections::BTreeMap<String, String>,
    hidden: &[String],
    cached_counts: Option<&[Folder]>,
) -> Result<Vec<Folder>, async_imap::error::Error> {
    let names: Vec<async_imap::types::Name> = session
        .list(Some(""), Some("*"))
        .await?
        .try_collect()
        .await?;

    let mut folders = Vec::new();
    let mut special_use = Vec::new(); // parallel: kind came from a SPECIAL-USE attr
    for name in names.iter() {
        // Skip containers that cannot hold messages.
        if name.attributes().contains(&NameAttribute::NoSelect) {
            continue;
        }
        let path = imap_unquote(name.name()).into_owned();
        let delimiter = name.delimiter().map(imap_unquote);
        let delimiter = delimiter.as_deref();
        // A hidden folder (#239) is left out here, ahead of everything
        // that walks the listing: the sweep, the watchers, the counts.
        if crate::models::folder_is_hidden(&path, delimiter, hidden) {
            continue;
        }
        let (kind, by_special_use) = classify_with_source(&path, delimiter, name.attributes());
        special_use.push(by_special_use);
        folders.push(Folder {
            id: 0, // assigned by order below
            account_id,
            name: display_name(&path, delimiter),
            path,
            kind,
            unread: 0,
        });
    }

    // When a role has a real SPECIAL-USE folder, demote any name-matched impostors
    // of that role to Custom — otherwise a stray folder like a plain "Trash" label
    // can shadow the server's actual [Gmail]/Trash and mail "moved" there never
    // really leaves (it just gains a label).
    for role in [
        FolderKind::Sent,
        FolderKind::Drafts,
        FolderKind::Trash,
        FolderKind::Junk,
        FolderKind::Archive,
        FolderKind::Starred,
    ] {
        let has_special = folders
            .iter()
            .zip(&special_use)
            .any(|(f, su)| f.kind == role && *su);
        if has_special {
            for (f, su) in folders.iter_mut().zip(&special_use) {
                if f.kind == role && !*su {
                    f.kind = FolderKind::Custom;
                }
            }
        }
    }

    // Essential folders first in their fixed order; custom folders sorted by
    // their full hierarchical path (case-insensitively), so sub-folders sit
    // grouped under their parents in a stable, server-independent order rather
    // than in whatever order the server's LIST happened to return them.
    folders.sort_by(|a, b| {
        folder_order(a.kind)
            .cmp(&folder_order(b.kind))
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
    for (i, f) in folders.iter_mut().enumerate() {
        f.id = i as u32 + 1;
    }
    // Manual Special Folders assignments (#82) ride over the detection
    // before anything is counted: a folder assigned as Drafts must count
    // every draft, and the cached kind drives every later re-count (the
    // sweep, the on-open refresh) too. Ids and order stay as numbered —
    // the app re-sorts its own copy.
    crate::models::assign_folder_roles(roles, &mut folders);

    if let Some(cached) = cached_counts {
        for folder in &mut folders {
            folder.unread = cached
                .iter()
                .find(|f| f.path == folder.path)
                .map_or(0, |f| f.unread);
        }
        return Ok(folders);
    }

    // Ask the server for each folder's true unread count. STATUS is cheap and
    // downloads no message content, so this stays fast even for huge mailboxes.
    for f in folders.iter_mut() {
        if let Ok(mb) = box_status(session, &f.path, "(UNSEEN MESSAGES)").await {
            f.unread = if chip_counts_all(f.kind) {
                mb.exists
            } else {
                mb.unseen.unwrap_or(0)
            };
        }
    }

    // STATUS is unreliable on some servers (notably iCloud), which leaves stale
    // inbox chips in the sidebar until the folder is opened. The inbox is the only
    // folder whose unread count is shown, so refine just that one up front with the
    // accurate EXAMINE + SEARCH UNSEEN (read-only; subsequent mutations must
    // re-select the mailbox in write mode).
    if let Some(inbox) = folders.iter_mut().find(|f| f.kind == FolderKind::Inbox) {
        if exam(session, &inbox.path).await.is_ok() {
            if let Some(n) = selected_unseen(session).await {
                inbox.unread = n;
            }
        }
    }
    Ok(folders)
}

/// Auto-empty (#140) for an IMAP account: delete for good whatever in the
/// Junk / Trash folder is older than the account's chosen age. Rides the
/// unread sweep, at most every [`AUTO_EMPTY_MIN`]. Age is the message's
/// internal date, the day it reached the server, which is how the other
/// clients count too. A failure is logged and tried again next time; the
/// sweep never surfaces an error for a chore nobody is waiting on.
async fn auto_empty_imap(
    account_id: u32,
    account: &AccountConfig,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    last: &mut Option<std::time::Instant>,
    emit: &impl Fn(WorkerEvent),
) {
    if !auto_empty_due(account, last) {
        return;
    }
    let folders = cache
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    let mut purged_any = false;
    for (kind, role, days) in auto_empty_roles(account) {
        let Some(path) = role_folder_path(account, &folders, kind, role) else {
            tracing::info!("auto-empty: account {account_id} has no {role} folder");
            continue;
        };
        let before = imap_before_date(days);
        let found = match sel(session, &path).await {
            Ok(_) => search_uids(session, format!("BEFORE {before}")).await,
            Err(e) => Err(e),
        };
        let uids: Vec<u32> = match found {
            Ok(set) => set.into_iter().collect(),
            Err(e) => {
                tracing::warn!("auto-empty: could not search {path} on account {account_id}: {e}");
                continue;
            }
        };
        if uids.is_empty() {
            continue;
        }
        match purge_messages(session, &path, &uids).await {
            Ok(()) => {
                if let Some(c) = cache {
                    for uid in &uids {
                        c.delete_message(account_id, &path, *uid);
                    }
                }
                tracing::info!(
                    "auto-empty: deleted {} message(s) older than {days} days from {path} on account {account_id}",
                    uids.len()
                );
                purged_any = true;
            }
            Err(e) => {
                tracing::warn!(
                    "auto-empty: could not delete from {path} on account {account_id}: {e}"
                );
            }
        }
    }
    if purged_any {
        refresh_folders(account_id, account, session, cache, emit).await;
    }
}

/// Which of `uids` (in the selected folder) carry a List-Unsubscribe
/// header, read 500 at a time.
async fn with_list_header(
    session: &mut ImapSession,
    uids: &[u32],
) -> Result<Vec<u32>, async_imap::error::Error> {
    let mut out = Vec::new();
    for chunk in uids.chunks(500) {
        let fetches: Vec<Fetch> = fetch_uids(
            session,
            uid_set(chunk),
            "(UID BODY.PEEK[HEADER.FIELDS (LIST-UNSUBSCRIBE)])",
        )
        .await?
        .try_collect()
        .await?;
        out.extend(fetches.iter().filter_map(|f| {
            let listed = fetched_fields(f).iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("list-unsubscribe") && !value.is_empty()
            });
            listed.then_some(f.uid?)
        }));
    }
    Ok(out)
}

/// Move the Inbox's list mail older than the account's chosen age to the
/// Trash (#407), searched for on the server, so mail never opened counts
/// too. It rides the unread sweep like auto-empty, and a failure is logged
/// and tried again next time. The Inbox list is redrawn from the cache.
async fn clean_lists_imap(
    account_id: u32,
    account: &AccountConfig,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    last: &mut Option<std::time::Instant>,
    emit: &impl Fn(WorkerEvent),
) {
    if !clean_lists_due(account, last) {
        return;
    }
    let days = account.clean_lists_days;
    let folders = cache
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    let Some(inbox) = folders
        .iter()
        .find(|f| f.kind == FolderKind::Inbox)
        .cloned()
    else {
        return;
    };
    let Some(trash) = role_folder_path(account, &folders, FolderKind::Trash, "trash") else {
        tracing::info!("list clean-up: account {account_id} has no Trash folder");
        return;
    };
    let found = match sel(session, &inbox.path).await {
        Ok(_) => search_uids(session, clean_lists_query(days)).await,
        Err(e) => Err(e),
    };
    let found = match found {
        Ok(set) => {
            let mut candidates: Vec<u32> = set.into_iter().collect();
            candidates.sort_unstable();
            with_list_header(session, &candidates).await
        }
        Err(e) => Err(e),
    };
    let uids: Vec<u32> = match found {
        Ok(uids) => uids,
        Err(e) => {
            tracing::warn!(
                "list clean-up: could not search {} on account {account_id}: {e}",
                inbox.path
            );
            return;
        }
    };
    if uids.is_empty() {
        return;
    }
    match move_messages(session, &inbox.path, &uids, &trash).await {
        Ok(_) => {
            tracing::info!(
                "list clean-up: moved {} list message(s) older than {days} days to {trash} on account {account_id}",
                uids.len()
            );
            if let Some(c) = cache {
                for uid in &uids {
                    c.delete_message(account_id, &inbox.path, *uid);
                }
                emit(WorkerEvent::Messages {
                    folder_id: inbox.id,
                    messages: c.load_messages(account_id, &inbox.path, inbox.id),
                });
            }
        }
        Err(e) => {
            tracing::warn!("list clean-up: could not move list mail on account {account_id}: {e}")
        }
    }
}

async fn refresh_folder_names(
    account_id: u32,
    account: &AccountConfig,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let cached = cache
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    if let Ok(folders) = list_folder_metadata(
        account_id,
        session,
        &account.folder_roles,
        &account.hidden_folders,
        Some(&cached),
    )
    .await
    {
        if folders.is_empty() || crate::cache::folders_equal(&cached, &folders) {
            return;
        }
        if let Some(c) = cache {
            c.save_folders(account_id, &folders);
        }
        emit(WorkerEvent::Folders(folders));
    }
}

/// Re-list folders (e.g. after auto-creating one) and push them to the UI.
async fn refresh_folders(
    account_id: u32,
    account: &AccountConfig,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    if let Ok(folders) = list_folders(
        account_id,
        session,
        &account.folder_roles,
        &account.hidden_folders,
    )
    .await
    {
        // A mailbox always has at least INBOX (RFC 3501): an empty LIST is a
        // wedged or throttled session answering nonsense, not the truth.
        // Trusting one once wiped an account's whole folder list — cached,
        // so it stayed wiped across restarts (seen on iCloud after a burst
        // of RENAMEs).
        if folders.is_empty() {
            return;
        }
        if let Some(c) = cache {
            c.save_folders(account_id, &folders);
        }
        emit(WorkerEvent::Folders(folders));
    }
}

/// Sweep every known folder for its true unread count and emit a
/// [`WorkerEvent::FolderUnread`] for each — the per-folder event is the one
/// path allowed to assert a genuine zero (the merged Folders path deliberately
/// won't, see the app's SetFolders). A folder that can't be read emits
/// nothing, so a wedged answer can't wipe a good chip.
///
/// EXAMINE + UID SEARCH, not STATUS: STATUS answers from a cache that lags
/// minutes behind on some servers (iCloud, see the inbox refinement in
/// [`list_folders`]) — stale numbers left chips unmoved and, worse, blinded
/// the change detection below. The read-only EXAMINE walk costs a second
/// round trip per folder and leaves the session's selection wherever it ends;
/// subsequent reads check that selection and mutations re-select explicitly.
///
/// `selected` (the mailbox the main loop is working) is skipped: its count is
/// already refreshed by every load and IDLE pass, and examining it out from
/// under the loop would be rude.
///
/// Doubles as the activity detector for the dynamic watchers: each folder's
/// (unseen, UIDNEXT) pair is compared against `baseline` — the same pair from
/// the previous sweep — and the folders that moved are returned so the caller
/// can put them on the watch list. UIDNEXT (from the EXAMINE response)
/// catches deliveries of already-read mail that the unseen count alone would
/// miss. A folder with no baseline yet is recorded, not reported: everything
/// that happened while Hylki was closed would otherwise look like fresh
/// activity.
/// The keyword half of a refresh with a tag view open (#166): every folder
/// is examined and, for each configured tag, the server's KEYWORD search is
/// set against the cached keywords. Tags set or cleared from another client
/// land in the index here — a folder's own sync only sees them once that
/// folder is opened. Returns the folders whose rows changed.
async fn refresh_keywords(
    account_id: u32,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    keywords: &[String],
) -> Vec<String> {
    let mut changed = Vec::new();
    let Some(cache) = cache else { return changed };
    if keywords.is_empty() {
        return changed;
    }
    for f in cache.load_folders(account_id) {
        if exam(session, &f.path).await.is_err() {
            continue;
        }
        // Where the search and the index disagree, the message's own flags
        // decide. iCloud answers SEARCH from an index that lags behind STORE
        // and FETCH, so trusting the search took a tag just set off again
        // until the folder's next sync put it back, and round again: tags
        // came and went in the tag view.
        let mut disputed: Vec<(u32, &str, bool)> = Vec::new();
        for kw in keywords {
            let Ok(server) = search_uids(session, format!("KEYWORD {kw}")).await else {
                continue;
            };
            let cached: std::collections::HashSet<u32> = cache
                .uids_with_keyword(account_id, &f.path, kw)
                .into_iter()
                .collect();
            disputed.extend(
                server
                    .symmetric_difference(&cached)
                    .map(|uid| (*uid, kw.as_str(), cached.contains(uid))),
            );
        }
        if disputed.is_empty() {
            continue;
        }
        let uids: Vec<u32> = disputed.iter().map(|(uid, _, _)| *uid).collect();
        let flags: std::collections::HashMap<u32, Vec<String>> =
            match fetch_uids(session, uid_set(&uids), "(UID FLAGS)").await {
                Ok(stream) => match stream.try_collect::<Vec<Fetch>>().await {
                    Ok(fetches) => fetches
                        .iter()
                        .filter_map(|f| {
                            Some((f.uid?, custom_flags(&f.flags().collect::<Vec<_>>())))
                        })
                        .collect(),
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
        let mut touched = false;
        for (uid, kw, indexed) in disputed {
            // A uid the server did not return is gone, or new mail the index
            // does not hold yet: the folder's own sync deals with both.
            let Some(has) = flags.get(&uid) else { continue };
            let on = has.iter().any(|k| k.eq_ignore_ascii_case(kw));
            if on != indexed {
                touched |= cache.set_keyword(account_id, &f.path, uid, kw, on);
            }
        }
        if touched {
            changed.push(f.path.clone());
        }
    }
    if !changed.is_empty() {
        tracing::info!("keywords: changed {changed:?}");
    }
    changed
}

/// Re-count every folder's unread chip (EXAMINE + SEARCH each), one folder
/// at a time — and only while nothing is waiting on the worker. Each folder
/// costs two round trips, so a dozen folders on a slow link hold the
/// session for seconds; a message the user just opened would wait behind
/// the whole walk (a notification click did exactly that). `pending` is the
/// walk's place: empty starts a fresh pass over the cached folder list, and
/// what is left when a request interrupts is resumed by the next call.
/// Returns the folders whose counts moved and whether the pass finished.
async fn refresh_unread_counts(
    account_id: u32,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    selected: Option<&str>,
    baseline: &mut std::collections::HashMap<String, (u32, Option<u32>)>,
    pending: &mut Vec<crate::models::Folder>,
    rx: &mpsc::UnboundedReceiver<MailRequest>,
    emit: &impl Fn(WorkerEvent),
) -> (Vec<(FolderKind, String)>, bool) {
    let mut changed = Vec::new();
    if pending.is_empty() {
        *pending = cache
            .map(|c| c.load_folders(account_id))
            .unwrap_or_default();
        pending.reverse(); // popped from the back, so walked in list order
    }
    while let Some(f) = pending.last().cloned() {
        if !rx.is_empty() {
            tracing::debug!("sweep: paused with {} folders left", pending.len());
            return (changed, false);
        }
        pending.pop();
        if Some(f.path.as_str()) == selected {
            continue;
        }
        let Ok(mb) = exam(session, &f.path).await else {
            tracing::debug!("sweep: cannot examine {}", f.path);
            continue;
        };
        let Some(unread) = selected_chip_count(session, Some(f.kind)).await else {
            continue;
        };
        emit(WorkerEvent::FolderUnread {
            folder_id: f.id,
            unread,
        });
        let now = (unread, mb.uid_next);
        match baseline.insert(f.path.clone(), now) {
            Some(prev) if prev != now => {
                tracing::debug!("sweep: {} moved {:?} -> {:?}", f.path, prev, now);
                changed.push((f.kind, f.path.clone()));
            }
            _ => {}
        }
    }
    if !changed.is_empty() {
        tracing::info!(
            "sweep: changed {:?}",
            changed.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>()
        );
    }
    (changed, true)
}

/// The chip number for the currently-selected mailbox: unseen, or every
/// message for a kind that counts them all. `None` for an unknown kind
/// counts unseen.
async fn selected_chip_count(session: &mut ImapSession, kind: Option<FolderKind>) -> Option<u32> {
    if kind.is_some_and(chip_counts_all) {
        search_uids(session, "ALL")
            .await
            .ok()
            .map(|uids| uids.len() as u32)
    } else {
        selected_unseen(session).await
    }
}

/// A folder's kind as the cache last saw it, by path.
/// Answer the "Message body" filter conditions (#191) for an inbox sync on
/// the server: one `UID SEARCH … BODY` per alternative over the listed
/// uids, so the rules learn what the server finds without a body coming
/// down. The answer goes out as [`WorkerEvent::BodyHits`] just ahead of the
/// messages. Nothing is sent for other folders, without body rules, or for
/// an empty list; a search the server refuses is logged and skipped, and
/// that alternative then works off the list preview alone.
async fn emit_body_hits(
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    folder_id: u32,
    path: &str,
    messages: &[Message],
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    if messages.is_empty() || !is_inbox_path(cache, account_id, path) {
        return;
    }
    let needles = crate::config::filter_body_needles(&account.email);
    if needles.is_empty() {
        return;
    }
    let Some(sess) = session.as_mut() else { return };
    let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
    let listed: std::collections::HashSet<u32> = uids.iter().copied().collect();
    let set = uid_set(&uids);
    let mut hits: std::collections::HashMap<u32, Vec<String>> = Default::default();
    for needle in needles {
        let query = body_search_query(&set, &needle);
        match search_uids(sess, &query).await {
            Ok(found) => {
                for uid in found.into_iter().filter(|u| listed.contains(u)) {
                    hits.entry(uid).or_default().push(needle.clone());
                }
            }
            Err(e) => tracing::warn!("filter: body search for {needle:?} failed: {e}"),
        }
    }
    tracing::info!(
        "filter: body search over {} messages hit {}",
        uids.len(),
        hits.len()
    );
    emit(WorkerEvent::BodyHits { folder_id, hits });
}

/// Whether `path` is the account's inbox: by the cached folder list, or by
/// name when there is no cache to ask.
fn is_inbox_path(cache: Option<&Cache>, account_id: u32, path: &str) -> bool {
    match cached_folder_kind(cache, account_id, path) {
        Some(kind) => kind == FolderKind::Inbox,
        None => path.eq_ignore_ascii_case("INBOX"),
    }
}

/// The `UID SEARCH` keys that find `needle` in the bodies of `set`. A
/// needle beyond ASCII needs the CHARSET key in front (RFC 3501 §6.4.4),
/// which must lead the keys; quotes and backslashes are escaped as in any
/// quoted string, and line breaks can never be in a quoted string at all.
fn body_search_query(set: &str, needle: &str) -> String {
    let clean: String = needle
        .chars()
        .filter(|c| !matches!(c, '\r' | '\n'))
        .collect();
    let quoted = clean.replace('\\', "\\\\").replace('"', "\\\"");
    let charset = if clean.is_ascii() {
        ""
    } else {
        "CHARSET UTF-8 "
    };
    format!("{charset}UID {set} BODY \"{quoted}\"")
}

fn cached_folder_kind(cache: Option<&Cache>, account_id: u32, path: &str) -> Option<FolderKind> {
    cache?
        .load_folders(account_id)
        .into_iter()
        .find(|f| f.path == path)
        .map(|f| f.kind)
}

/// Count unseen messages in the currently-selected mailbox via SEARCH (safe on
/// the selected folder, unlike STATUS on some servers). Downloads only ids.
async fn selected_unseen(session: &mut ImapSession) -> Option<u32> {
    search_uids(session, "UNSEEN")
        .await
        .ok()
        .map(|uids| uids.len() as u32)
}

/// The UIDNEXT each folder had when its list was last loaded, by (account,
/// path). Mail that lands while the connection is busy with something else
/// is only told to the SELECT that comes before the next IDLE, never to the
/// IDLE itself, so that SELECT has to be compared against this (#336).
static SYNCED_UID_NEXT: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(u32, String), u32>>,
> = std::sync::LazyLock::new(Default::default);

fn note_synced_uid_next(account_id: u32, path: &str, uid_next: Option<u32>) {
    if let Some(next) = uid_next {
        SYNCED_UID_NEXT
            .lock()
            .unwrap()
            .insert((account_id, path.to_string()), next);
    }
}

/// Whether a folder's UIDNEXT has moved past what its last loaded list saw:
/// mail arrived that no sync has picked up.
fn missed_new_mail(account_id: u32, path: &str, uid_next: Option<u32>) -> bool {
    let seen = SYNCED_UID_NEXT
        .lock()
        .unwrap()
        .get(&(account_id, path.to_string()))
        .copied();
    matches!((seen, uid_next), (Some(seen), Some(now)) if now > seen)
}

/// Load a folder's message index for immediate display.
///
/// Never-synced folder → fetch a fast [`FIRST_PAGE`] of the newest messages so
/// browsing is instant; the background backfill indexes the rest. Already-cached
/// folder → fetch just the recent window and merge it over the existing (possibly
/// whole-mailbox) index, picking up new mail and flag changes without re-pulling
/// thousands of envelopes.
async fn load_messages(
    account_id: u32,
    session: &mut ImapSession,
    folder_id: u32,
    path: &str,
    use_envelope: bool,
    cache: Option<&Cache>,
) -> Result<Vec<Message>, async_imap::error::Error> {
    let mailbox = sel(session, path).await?;
    note_synced_uid_next(account_id, path, mailbox.uid_next);
    let total = mailbox.exists;
    if total == 0 {
        // Folder emptied on the server — drop any cached copies so they don't linger.
        if let Some(c) = cache {
            for uid in c.cached_uids(account_id, path) {
                c.delete_message(account_id, path, uid);
            }
        }
        return Ok(Vec::new());
    }

    let cached = cache
        .map(|c| c.load_messages(account_id, path, folder_id))
        .unwrap_or_default();
    if cached.is_empty() {
        let mut messages = fetch_window(
            account_id,
            session,
            folder_id,
            total,
            FIRST_PAGE,
            use_envelope,
        )
        .await?;
        let uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
        read_gmail_labels(account_id, session, path, &uids, &mut messages).await?;
        reconcile_attachment_flags(cache, account_id, path, &mut messages);
        Ok(messages)
    } else {
        let recent = fetch_window(
            account_id,
            session,
            folder_id,
            total,
            PAGE_SIZE,
            use_envelope,
        )
        .await?;
        let window: Vec<u32> = recent.iter().map(|m| m.uid).collect();
        let mut merged = merge_index(cached, recent);
        read_gmail_labels(account_id, session, path, &window, &mut merged).await?;
        redecode_garbled_previews(session, &mut merged).await;
        // Reconcile deletions/moves made on the server or another device: drop any
        // message whose UID the server no longer lists (a plain merge would keep it
        // forever), and prune it from the cache so it doesn't come back. The full
        // UID set is a cheap server-side search even for large mailboxes.
        let server: std::collections::HashSet<u32> = search_uids(session, "ALL").await?;
        if let Some(c) = cache {
            for m in merged.iter().filter(|m| !server.contains(&m.uid)) {
                c.delete_message(account_id, path, m.uid);
            }
        }
        merged.retain(|m| server.contains(&m.uid));
        reconcile_attachment_flags(cache, account_id, path, &mut merged);
        Ok(merged)
    }
}

/// Clear the "has attachment" flag on freshly-fetched summaries whose bodies we
/// already downloaded and found to contain no real attachments. Server summary
/// flags (especially iCloud's header-only `multipart/mixed` guess) over-report
/// attachments for HTML mail whose only extra parts are inline `cid:` images.
fn reconcile_attachment_flags(
    cache: Option<&Cache>,
    account_id: u32,
    path: &str,
    messages: &mut [Message],
) {
    let Some(c) = cache else { return };
    if messages.iter().all(|m| !m.has_attachment) {
        return;
    }
    let attachmentless = c.attachmentless_uids(account_id, path);
    if attachmentless.is_empty() {
        return;
    }
    for m in messages.iter_mut() {
        if m.has_attachment && attachmentless.contains(&m.uid) {
            m.has_attachment = false;
        }
    }
}

/// Fetch the most-recent `count` messages' summaries (newest first). Includes
/// BODYSTRUCTURE so the attachment indicator is known for every indexed message
/// — still no bodies/attachments are downloaded.
async fn fetch_window(
    account_id: u32,
    session: &mut ImapSession,
    folder_id: u32,
    total: u32,
    count: u32,
    use_envelope: bool,
) -> Result<Vec<Message>, async_imap::error::Error> {
    let start = total.saturating_sub(count - 1).max(1);
    let range = format!("{start}:{total}");

    // Normally we fetch the structured ENVELOPE + BODYSTRUCTURE (compact, and the
    // latter gives the attachment indicator). But some servers (notably iCloud)
    // emit RFC-noncompliant ENVELOPE/BODYSTRUCTURE (e.g. NIL transfer-encodings,
    // unescaped quotes in the Message-ID) that our IMAP parser rejects. For those
    // the caller retries with `use_envelope = false`, and we instead pull the raw
    // header block — opaque to the IMAP parser — and parse it with mail-parser.
    // `BODY.PEEK[1]<0.2048>` rides along for the list preview, with `BODY.PEEK[1.MIME]`
    // — the part's own headers — so the bytes are read in the charset they
    // declare rather than assumed UTF-8 (#159). Section 1 is the
    // first body part of a multipart message and the whole body of a single-part
    // one, so one query covers both without a second round trip per message.
    // When that first part is itself a multipart — `mixed(alternative(text, html),
    // attachment)`, which is what ProtonMail sends — what comes back is the nested
    // MIME rather than any text, and [`preview_from_part`] descends into it.
    //
    // With previews switched off it is left out altogether: the setting exists
    // partly to avoid downloading a slice of every message, so honouring it only
    // in the UI would miss the point. Read per sync, so turning it back on takes
    // effect without a restart.
    let want_preview = inline_previews_wanted(account_id);
    let preview_part = if want_preview {
        preview_fetch_items()
    } else {
        String::new()
    };
    let mut messages: Vec<Message> = if use_envelope {
        let query = format!(
            "(UID ENVELOPE FLAGS BODYSTRUCTURE INTERNALDATE{REFS_FETCH_ITEM}{preview_part})"
        );
        let fetches = logged_fetch(session, &range, &query).await?;
        fetches
            .iter()
            .map(|f| {
                let mut m = build_summary(account_id, f, folder_id);
                m.preview = preview_of(f);
                m
            })
            .collect()
    } else {
        let query = format!("(UID FLAGS BODY.PEEK[HEADER] INTERNALDATE{preview_part})");
        let fetches = logged_fetch(session, &range, &query).await?;
        fetches
            .iter()
            .map(|f| {
                let mut m = summary_from_headers(account_id, f, folder_id);
                m.preview = preview_of(f);
                m
            })
            .collect()
    };
    // Fill in previews the summary fetch came back without (see
    // [`retry_missing_previews`] for who those are). Bounded so body-less
    // messages don't grow the fetch each sync.
    if want_preview {
        let missing: Vec<u32> = messages
            .iter()
            .filter(|m| m.preview.is_empty())
            .take(PREVIEW_REDECODE_ROWS)
            .map(|m| m.uid)
            .collect();
        retry_missing_previews(session, &mut messages, missing).await;
    }
    messages.reverse(); // IMAP returns oldest-first; show newest at the top.
    Ok(messages)
}

/// A sequence-number FETCH through the conversation log, like the UID
/// wrappers: the summary fetch is the one a server rejects (#226), and its
/// command and verdict were the two lines missing from the export.
async fn logged_fetch(
    session: &mut ImapSession,
    range: &str,
    query: &str,
) -> Result<Vec<Fetch>, async_imap::error::Error> {
    let cmd = format!("FETCH {range} {query}");
    wire(&cmd);
    let r: Result<Vec<Fetch>, async_imap::error::Error> =
        async { session.fetch(range, query).await?.try_collect().await }.await;
    wired(&cmd, &r);
    r
}

/// Put each message's Gmail labels on it, for the `uids` asked about, on an
/// account whose labels are read (#374). A message the server said nothing
/// about keeps the labels it has.
async fn read_gmail_labels<T>(
    account_id: u32,
    session: &mut Session<T>,
    path: &str,
    uids: &[u32],
    messages: &mut [Message],
) -> Result<(), async_imap::error::Error>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    if uids.is_empty() || !gmail_labels_wanted(account_id) {
        return Ok(());
    }
    let Some(mut labels) = fetch_gmail_labels(account_id, session, path, uids).await? else {
        return Ok(());
    };
    for m in messages.iter_mut() {
        if let Some(l) = labels.remove(&m.uid) {
            m.labels = l;
        }
    }
    Ok(())
}

/// Each message's shown Gmail labels by UID, or `None` when the server
/// refused to say and the labels already known should stand.
///
/// async-imap's `Fetch` keeps its parsed reply private and has no getter for
/// `X-GM-LABELS`, so the command is run raw and its replies read here. An
/// unreadable reply leaves the rest of it in the stream, so it is an error
/// the caller reconnects after, and labels are left off for the session so
/// the same reply cannot fail the retry too. It is not passed on as a parse
/// error: that would move the summary fetch down its ladder (#226) for a
/// fault that is not the summary's.
async fn fetch_gmail_labels<T>(
    account_id: u32,
    session: &mut Session<T>,
    path: &str,
    uids: &[u32],
) -> Result<Option<std::collections::HashMap<u32, Vec<String>>>, async_imap::error::Error>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    use async_imap::imap_proto::{Response, Status};
    let cmd = format!("UID FETCH {} (UID X-GM-LABELS)", uid_set(uids));
    wire(&cmd);
    let id = session.run_command(&cmd).await?;
    let mut out = std::collections::HashMap::new();
    loop {
        let reply = match session.read_response().await {
            Some(Ok(reply)) => reply,
            Some(Err(e)) => {
                let r: Result<(), _> = Err(async_imap::error::Error::Io(e));
                wired(&cmd, &r);
                let e = r.unwrap_err();
                if !is_parse_error(&e) {
                    return Err(e);
                }
                refuse_gmail_labels(account_id);
                return Err(async_imap::error::Error::Io(std::io::Error::other(
                    "unreadable X-GM-LABELS reply",
                )));
            }
            None => {
                wired::<()>(&cmd, &Err(async_imap::error::Error::ConnectionLost));
                return Err(async_imap::error::Error::ConnectionLost);
            }
        };
        match reply.parsed() {
            Response::Fetch(_, attrs) => {
                if let Some((uid, labels)) = gmail_labels_of(attrs, path) {
                    out.insert(uid, labels);
                }
            }
            Response::Done {
                tag,
                status,
                information,
                ..
            } if *tag == id => {
                let why = information.as_deref().unwrap_or("");
                return Ok(match status {
                    Status::Ok => {
                        tracing::debug!(target: "hylki::imap", "< OK ({})", cmd_head(&cmd));
                        Some(out)
                    }
                    // A server that does not know the item. Gmail itself
                    // answers NO when a message went while it was being
                    // read, and what it did send still holds.
                    Status::Bad => {
                        tracing::warn!(
                            target: "hylki::imap",
                            "< BAD {why} ({cmd}): Gmail labels left off for the session"
                        );
                        refuse_gmail_labels(account_id);
                        None
                    }
                    _ => {
                        tracing::warn!(target: "hylki::imap", "< {status:?} {why} ({cmd})");
                        Some(out)
                    }
                });
            }
            // EXISTS, EXPUNGE and the like: the next sync reads them.
            _ => {}
        }
    }
}

/// The UID and shown labels in one `FETCH` reply's attributes, if it carries
/// `X-GM-LABELS`.
fn gmail_labels_of(
    attrs: &[async_imap::imap_proto::AttributeValue<'_>],
    folder: &str,
) -> Option<(u32, Vec<String>)> {
    use async_imap::imap_proto::AttributeValue;
    let uid = attrs.iter().find_map(|a| match a {
        AttributeValue::Uid(uid) => Some(*uid),
        _ => None,
    })?;
    let labels = attrs.iter().find_map(|a| match a {
        AttributeValue::GmailLabels(labels) => Some(labels),
        _ => None,
    })?;
    Some((
        uid,
        shown_gmail_labels(labels.iter().map(|l| l.as_ref()), folder),
    ))
}

/// The labels worth a pill: not Gmail's system labels (`\Inbox`,
/// `\Important`, `\Starred`…), which the folder list and the star already
/// show, and not the one the folder being read is, which every message in
/// it carries.
fn shown_gmail_labels<'a>(labels: impl Iterator<Item = &'a str>, folder: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in labels {
        if l.is_empty() || l.starts_with('\\') || l.eq_ignore_ascii_case(folder) {
            continue;
        }
        if !out.iter().any(|o| o == l) {
            out.push(l.to_string());
        }
    }
    out
}

/// The preview snippet from a fetch that asked for `BODY.PEEK[1]` together with
/// `BODY.PEEK[1.MIME]`, the part's own headers, which say what the bytes are.
fn preview_of(fetch: &Fetch) -> String {
    use async_imap::imap_proto::types::SectionPath;
    let ctype = section_content_type(fetch);
    // An image or a document as the first part — photo mail, a forwarded
    // file — has no text to show. An empty preview here lets the BODY[TEXT]
    // retry look further into the message for the text part, instead of the
    // row wearing the file's first bytes.
    if ctype.as_deref().is_some_and(|t| !text_like(t)) {
        return String::new();
    }
    let charset = ctype.as_deref().and_then(charset_param);
    let encoding = section_encoding(fetch);
    let bytes = fetch
        .section(&SectionPath::Part(vec![1], None))
        .unwrap_or_default();
    let p = preview_from_part(bytes, charset.as_deref(), encoding.as_deref());
    // A preheader is built to defeat exactly this read: a marketing mail can
    // spend thousands of bytes on invisible padding before its first real
    // sentence, the way an HTML part spends them on <head>. When the slice ran
    // to its limit and still yielded almost nothing to show, report nothing
    // rather than a scrap, and let the deeper repair read — bounded, and rare
    // — go and find the text.
    if bytes.len() >= PREVIEW_FETCH_BYTES && p.chars().count() < PREVIEW_MIN_CHARS {
        return String::new();
    }
    // GTK labels abort on interior NULs, and decoded message text can carry
    // them.
    if p.contains('\0') {
        p.replace('\0', " ")
    } else {
        p
    }
}

/// The lowercased Content-Type section 1 declares, read from the
/// `BODY[1.MIME]` header block that rides along with the preview fetch (#159).
/// `None` when the server sent no MIME headers.
fn section_content_type(fetch: &Fetch) -> Option<String> {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    let mime = fetch.section(&SectionPath::Part(vec![1], Some(MessageSection::Mime)))?;
    mime_header(&String::from_utf8_lossy(mime), "content-type")
}

/// The charset section 1 declares, when its headers name one — a multipart
/// part 1 doesn't, its text parts carry their own.
fn section_charset(fetch: &Fetch) -> Option<String> {
    section_content_type(fetch)
        .as_deref()
        .and_then(charset_param)
}

/// The transfer encoding section 1 declares, from the same `BODY[1.MIME]`
/// block the charset comes from. Reading it beats guessing: a part can be
/// quoted-printable and still be spelled entirely in base64's alphabet.
fn section_encoding(fetch: &Fetch) -> Option<String> {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    let mime = fetch.section(&SectionPath::Part(vec![1], Some(MessageSection::Mime)))?;
    mime_header(&String::from_utf8_lossy(mime), "content-transfer-encoding")
}

/// How much of a message's first body part to fetch for the list preview. Enough
/// for a couple of lines of text after decoding, small enough that syncing a
/// large mailbox doesn't turn into downloading it.
const PREVIEW_FETCH_BYTES: usize = 2048;

/// Below this many characters, a preview read from a full slice has not found
/// the message's text yet — it is still in the preheader padding or the
/// `<head>` — and the repair read is worth the round trip.
const PREVIEW_MIN_CHARS: usize = 30;

/// The bigger slice for the preview *repair* fetch (the BODY[TEXT] retry for
/// messages whose summary fetch produced no preview). An HTML-only message can
/// spend several KB on `<head>` boilerplate before its first visible text —
/// which is exactly how a message ends up on the repair list — so the retry
/// reads deep enough to find it. Only a handful of messages ever qualify.
const PREVIEW_REPAIR_BYTES: usize = 32768;

/// How many cached rows with a garbled preview one sync re-reads.
const PREVIEW_REDECODE_ROWS: usize = 12;

/// The fetch items that read a message's first part for the list preview: a
/// slice of its bytes plus its MIME headers, for the charset.
fn preview_fetch_items() -> String {
    format!(" BODY.PEEK[1]<0.{PREVIEW_FETCH_BYTES}> BODY.PEEK[1.MIME]")
}

/// Re-read the previews of cached rows that still show replacement characters
/// (#159). Rows indexed before the preview fetch learned the part's charset
/// keep their garbled snippet otherwise, since only the recent window is read
/// again on each sync. A few rows per sync.
async fn redecode_garbled_previews(session: &mut ImapSession, messages: &mut [Message]) {
    let Some(account_id) = messages.first().map(|m| m.account_id) else {
        return;
    };
    if !inline_previews_wanted(account_id) {
        return;
    }
    let uids: Vec<u32> = messages
        .iter()
        .filter(|m| m.preview.contains('\u{fffd}'))
        .take(PREVIEW_REDECODE_ROWS)
        .map(|m| m.uid)
        .collect();
    if uids.is_empty() {
        return;
    }
    tracing::info!("previews: re-reading garbled rows uids={uids:?}");
    let fetched: Result<Vec<Fetch>, _> = async {
        fetch_uids(
            session,
            uid_set(&uids),
            format!("(UID{})", preview_fetch_items()),
        )
        .await?
        .try_collect()
        .await
    }
    .await;
    let fetches = match fetched {
        Err(e) => {
            tracing::warn!("previews: garbled-row re-read failed: {e}");
            return;
        }
        Ok(f) => f,
    };
    let mut still_missing = Vec::new();
    for f in &fetches {
        let Some(m) = f
            .uid
            .and_then(|uid| messages.iter_mut().find(|m| m.uid == uid))
        else {
            continue;
        };
        // Whatever this read produced — including nothing, when the part
        // turned out to be a file — beats the garbage it replaces, and an
        // empty row is not asked about again next sync.
        m.preview = preview_of(f);
        if m.preview.is_empty() {
            still_missing.push(m.uid);
        }
    }
    retry_missing_previews(session, messages, still_missing).await;
}

/// Fill in previews for `uids`, rows whose first-part read came back empty.
///
/// iCloud answers BODY[1] with nothing on a message it re-appended (a move-back
/// from an undo, for one) — and keeps doing so on every later sync, so the row
/// would wear a blank summary for good — and a message whose first part is a
/// file has its text further in. The TEXT section survives (the full-body
/// fetch the reader uses works fine), so re-ask with that and run the same
/// MIME-prefix extraction. Callers bound the list: genuinely body-less messages
/// stay empty and shouldn't grow the fetch each sync.
async fn retry_missing_previews(
    session: &mut ImapSession,
    messages: &mut [Message],
    uids: Vec<u32>,
) {
    // Rows the retry already answered this session are served from that
    // answer (see [`PREVIEW_RETRIED`]): the retry runs on every sync of the
    // folder, and it would otherwise cost 32 KB per row per sync for as
    // long as the row stayed among those checked.
    fn key(messages: &[Message], uid: u32) -> Option<(u32, u32, u32)> {
        messages
            .iter()
            .find(|m| m.uid == uid)
            .map(|m| (m.account_id, m.folder_id, uid))
    }
    let uids: Vec<u32> = {
        let retried = PREVIEW_RETRIED.lock().ok();
        uids.into_iter()
            .filter(|&uid| {
                let Some(k) = key(messages, uid) else {
                    return false;
                };
                match retried.as_ref().and_then(|r| r.get(&k)) {
                    None => true,
                    Some(found) => {
                        if let Some(m) = messages.iter_mut().find(|m| m.uid == uid) {
                            m.preview = found.clone();
                        }
                        false
                    }
                }
            })
            .collect()
    };
    if uids.is_empty() {
        return;
    }
    retry_missing_previews_now(session, messages, &uids).await;
    let answers: Vec<((u32, u32, u32), String)> = uids
        .iter()
        .filter_map(|&uid| {
            let k = key(messages, uid)?;
            let preview = messages.iter().find(|m| m.uid == uid)?.preview.clone();
            Some((k, preview))
        })
        .collect();
    if let Ok(mut retried) = PREVIEW_RETRIED.lock() {
        retried.extend(answers);
    }
}

/// The BODY[TEXT] retry itself, for `uids` (non-empty).
async fn retry_missing_previews_now(
    session: &mut ImapSession,
    messages: &mut [Message],
    uids: &[u32],
) {
    tracing::info!("previews: retrying via BODY[TEXT] uids={uids:?}");
    let refetched: Result<Vec<Fetch>, _> = async {
        fetch_uids(
            session,
            uid_set(&uids),
            format!("(UID BODY.PEEK[TEXT]<0.{PREVIEW_REPAIR_BYTES}> BODY.PEEK[1.MIME])"),
        )
        .await?
        .try_collect()
        .await
    }
    .await;
    match refetched {
        Err(e) => tracing::warn!("previews: BODY[TEXT] retry failed: {e}"),
        Ok(fetches) => {
            use async_imap::imap_proto::types::{MessageSection, SectionPath};
            for f in &fetches {
                // For a multipart, `text_in_multipart` reads each part's own
                // headers and these go unused; for a single-part message,
                // section 1 is the body and they describe it exactly.
                let charset = section_charset(f);
                let encoding = section_encoding(f);
                // Section 1 is a file, not text (#241: a DMARC report that is
                // nothing but a zip). The summary fetch blanked the preview
                // for that reason, and this read must not undo it by decoding
                // the file: only a multipart body with a text part inside
                // has anything to show.
                let binary_first = section_content_type(f).is_some_and(|t| !text_like(&t));
                let p = f
                    .section(&SectionPath::Full(MessageSection::Text))
                    .map(|b| {
                        if binary_first {
                            text_in_multipart(b, 0)
                                .map(finish_preview)
                                .unwrap_or_default()
                        } else {
                            preview_from_part(b, charset.as_deref(), encoding.as_deref())
                        }
                    })
                    .unwrap_or_default()
                    .replace('\0', " ");
                if p.is_empty() {
                    continue;
                }
                if let Some(m) = f
                    .uid
                    .and_then(|uid| messages.iter_mut().find(|m| m.uid == uid))
                {
                    m.preview = p;
                }
            }
        }
    }
}

/// Build a message summary from a raw header block (mail-parser), for servers
/// whose structured ENVELOPE our IMAP parser can't handle.
fn summary_from_headers(account_id: u32, fetch: &Fetch, folder_id: u32) -> Message {
    use mail_parser::MessageParser;

    let uid = fetch.uid.unwrap_or(0);
    let flags: Vec<Flag> = fetch.flags().collect();
    let unread = !flags.iter().any(|f| matches!(f, Flag::Seen));
    let starred = flags.iter().any(|f| matches!(f, Flag::Flagged));
    let answered = flags.iter().any(|f| matches!(f, Flag::Answered));
    let keywords = custom_flags(&flags);
    let forwarded = keywords
        .iter()
        .any(|k| k.eq_ignore_ascii_case(FORWARDED_KEYWORD));

    let raw = fetch.header().unwrap_or(&[]);
    let parsed = MessageParser::default().parse(raw);

    let mp_first = |a: Option<&mail_parser::Address>| -> (String, String) {
        a.and_then(|a| a.first())
            .map(|addr| {
                let email = addr.address().unwrap_or_default().to_string();
                let name = addr.name().map(|s| s.to_string()).filter(|s| !s.is_empty());
                (name.unwrap_or_else(|| email.clone()), email)
            })
            .unwrap_or_else(|| ("Unknown".to_string(), String::new()))
    };
    let mp_list = |a: Option<&mail_parser::Address>| -> String {
        a.map(|a| {
            a.iter()
                .filter_map(|addr| addr.address().map(|s| s.to_string()))
                .filter(|e| !e.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
    };

    let (from_name, from_addr) = parsed
        .as_ref()
        .map(|p| mp_first(p.from()))
        .unwrap_or_else(|| ("Unknown".to_string(), String::new()));
    let subject = parsed
        .as_ref()
        .and_then(|p| p.subject())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no subject)".to_string());
    let (date, timestamp) = parsed
        .as_ref()
        .and_then(|p| p.date())
        .map(|d| {
            let ts = d.to_timestamp();
            (format_timestamp(ts), ts)
        })
        .filter(|(_, ts)| *ts > 0)
        .unwrap_or_else(|| internal_date_summary(fetch));
    let to = parsed.as_ref().map(|p| mp_list(p.to())).unwrap_or_default();
    let cc = parsed.as_ref().map(|p| mp_list(p.cc())).unwrap_or_default();
    let bcc = parsed
        .as_ref()
        .map(|p| mp_list(p.bcc()))
        .unwrap_or_default();
    let reply_to = parsed
        .as_ref()
        .map(|p| mp_list(p.reply_to()))
        .unwrap_or_default();
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) {
        String::new()
    } else {
        reply_to
    };

    // Best-effort attachment hint from the top-level Content-Type (BODYSTRUCTURE
    // isn't available on this path). multipart/mixed is the usual attachment case.
    let has_attachment = parsed
        .as_ref()
        .and_then(|p| p.header("Content-Type"))
        .and_then(|h| h.as_content_type())
        .map(|ct| {
            ct.ctype().eq_ignore_ascii_case("multipart")
                && ct
                    .subtype()
                    .is_some_and(|s| s.eq_ignore_ascii_case("mixed"))
        })
        .unwrap_or(false);

    let (message_id, references) = mp_thread_ids(parsed.as_ref());

    let mut msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name,
        from_addr,
        reply_to,
        to,
        cc,
        bcc,
        subject,
        preview: String::new(),
        body: String::new(),
        date,
        timestamp,
        unread,
        starred,
        answered,
        forwarded,
        keywords,
        has_attachment,
        message_id,
        references,
        importance: mp_importance(parsed.as_ref()),
        due: 0,
        labels: Vec::new(),
    };
    msg.scrub_nuls();
    msg
}

/// Overlay a freshly-fetched recent window onto the cached index: recent rows
/// replace their cached versions (updated flags / new mail), the rest are kept.
/// No size cap — the whole folder is searchable once the background backfill has
/// indexed it.
///
/// A recent row that came back without a preview keeps the cached one, as
/// [`Cache::upsert_messages`] does. Otherwise the list this returns never
/// matched the one served from the cache a moment earlier, and every quiet
/// sync looked to the app like new mail (#330).
fn merge_index(cached: Vec<Message>, recent: Vec<Message>) -> Vec<Message> {
    let mut map: std::collections::HashMap<u32, Message> =
        cached.into_iter().map(|m| (m.uid, m)).collect();
    for mut m in recent {
        if let Some(old) = map.get_mut(&m.uid) {
            if m.preview.is_empty() {
                m.preview = std::mem::take(&mut old.preview);
            }
            // The summary fetch carries no Gmail labels: they are read after
            // the merge, and a message they could not be read for keeps the
            // ones it had.
            if m.labels.is_empty() {
                m.labels = std::mem::take(&mut old.labels);
            }
        }
        map.insert(m.uid, m);
    }
    let mut out: Vec<Message> = map.into_values().collect();
    out.sort_by(|a, b| b.uid.cmp(&a.uid)); // newest first
    out
}

/// How many messages one References-repair step asks the server about. Each row
/// costs only its References header, so this can be larger than an envelope
/// chunk without making the connection unresponsive to real requests.
const REFS_REPAIR_CHUNK: usize = 500;

/// One step of the one-time References repair for the folder at the head of the
/// queue (#21, #42).
///
/// Messages indexed before Hylki asked for References hold only In-Reply-To,
/// which names the immediate parent — and for an incoming reply that parent is
/// usually your own message in Sent, so the link points outside the folder and
/// the reply starts a thread of its own. This asks the server for the one header
/// that fixes it, for the replies that can still be improved, newest first.
///
/// The watermark makes it resumable and run exactly once per folder; a repaired
/// row re-threads the next time its folder is read from cache.
async fn run_one_refs_repair(
    queue: &mut std::collections::VecDeque<(u32, String)>,
    session: &mut Option<ImapSession>,
    account_id: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some((folder_id, path)) = queue.pop_front() else {
        return;
    };
    let Some(c) = cache else { return }; // nothing to repair without a cache
    let Some(mut s) = session.take() else {
        queue.push_front((folder_id, path));
        return;
    };

    let (below, _) = c.refs_repair_state(account_id, &path);
    let chunk = c.uids_needing_references(account_id, &path, below, REFS_REPAIR_CHUNK);
    if chunk.is_empty() {
        c.set_refs_repair_state(account_id, &path, 0, true);
        *session = Some(s); // folder done — don't requeue
        return;
    }

    let set = uid_set(&chunk.iter().map(|(uid, _)| *uid).collect::<Vec<_>>());
    let fetched: Result<Vec<Fetch>, _> = async {
        sel(&mut s, &path).await?;
        fetch_uids(&mut s, &set, format!("(UID{REFS_FETCH_ITEM})"))
            .await?
            .try_collect()
            .await
    }
    .await;
    let Ok(fetches) = fetched else {
        // Leave the watermark alone and try this folder again later.
        queue.push_back((folder_id, path));
        *session = Some(s);
        return;
    };

    let found: std::collections::HashMap<u32, String> = fetches
        .iter()
        .filter_map(|f| f.uid.map(|uid| (uid, references_of(f))))
        .collect();
    let mut repaired = 0usize;
    for (uid, existing) in &chunk {
        if let Some(refs) = found.get(uid) {
            let merged = merge_msgids(refs, existing);
            if &merged != existing {
                c.set_references(account_id, &path, *uid, &merged);
                repaired += 1;
            }
        }
    }
    // Walk on from the oldest uid this chunk covered.
    let lowest = chunk.iter().map(|(uid, _)| *uid).min().unwrap_or(0);
    c.set_refs_repair_state(account_id, &path, lowest, false);
    queue.push_back((folder_id, path));
    *session = Some(s);
    if repaired > 0 {
        // The folder's cached rows have changed underneath whatever is on
        // screen; re-emitting lets the list re-thread with what was found.
        emit(WorkerEvent::RefsRepaired { folder_id });
    }
}

/// How many messages one label-fill step asks Gmail about. A label reply is a
/// line per message, smaller than a References one.
const LABEL_FILL_CHUNK: usize = 1_000;

/// One step of the one-time Gmail label fill for the folder at the head of
/// the queue (#374).
///
/// Labels come down with the summaries, and a sync asks again only about its
/// recent window, so mail indexed before Hylki read labels would never get
/// any. This asks about the rest of each folder once, newest first, resuming
/// from a watermark like [`run_one_refs_repair`].
async fn run_one_label_fill(
    queue: &mut std::collections::VecDeque<(u32, String)>,
    session: &mut Option<ImapSession>,
    account_id: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(c) = cache else {
        queue.clear();
        return;
    };
    if !gmail_labels_wanted(account_id) {
        // Refused this session: the fill waits for the next one.
        queue.clear();
        return;
    }
    let Some((folder_id, path)) = queue.pop_front() else {
        return;
    };
    let Some(mut s) = session.take() else {
        queue.push_front((folder_id, path));
        return;
    };
    let (below, _) = c.label_fill_state(account_id, &path);
    let chunk = c.uids_below(account_id, &path, below, LABEL_FILL_CHUNK);
    if chunk.is_empty() {
        c.set_label_fill_state(account_id, &path, 0, true);
        *session = Some(s);
        return;
    }
    let read = async {
        sel(&mut s, &path).await?;
        fetch_gmail_labels(account_id, &mut s, &path, &chunk).await
    }
    .await;
    match read {
        Ok(Some(found)) => {
            let changed = c.set_gmail_labels(account_id, &path, &found);
            let lowest = chunk.iter().copied().min().unwrap_or(0);
            c.set_label_fill_state(account_id, &path, lowest, false);
            queue.push_back((folder_id, path));
            *session = Some(s);
            if changed > 0 {
                emit(WorkerEvent::RefsRepaired { folder_id });
            }
        }
        // Refused; the queue is dropped at the next step.
        Ok(None) => *session = Some(s),
        // The session may be left mid-reply: drop it, and the main loop
        // reconnects when it next needs one.
        Err(_) => queue.push_back((folder_id, path)),
    }
}

/// A background job to index the rest of a folder (everything past the fast first
/// page) so search covers the whole mailbox. `remaining` is the still-to-fetch
/// UIDs (newest first), computed lazily on the first drain.
#[derive(Clone)]
struct Backfill {
    folder_id: u32,
    path: String,
    remaining: Option<Vec<u32>>,
    /// Whether this folder feeds the attachments gallery (not Trash/Junk/Drafts);
    /// if so, its backfilled messages' attachments are prefetched too.
    gallery: bool,
}

// select! may receive a reader before polling the job at all. In that case
// the original is still queued; otherwise restore the snapshot, including any
// chunk the cancelled future drained before the network response arrived.
fn restore_interrupted_backfill(queue: &mut std::collections::VecDeque<Backfill>, job: Backfill) {
    if !queue.iter().any(|queued| queued.path == job.path) {
        queue.push_front(job);
    }
}

/// Determine which UIDs still need indexing: everything on the server not already
/// cached. Also reconciles deletions (cached UIDs the server no longer has).
async fn backfill_worklist(
    session: &mut ImapSession,
    account_id: u32,
    path: &str,
    cache: Option<&Cache>,
) -> Result<Vec<u32>, async_imap::error::Error> {
    sel(session, path).await?;
    let server: std::collections::HashSet<u32> = search_uids(session, "ALL").await?;
    let cached = cache
        .map(|c| c.cached_uids(account_id, path))
        .unwrap_or_default();
    if let Some(c) = cache {
        for uid in cached.iter() {
            if !server.contains(uid) {
                c.delete_message(account_id, path, *uid);
            }
        }
    }
    let mut remaining: Vec<u32> = server.difference(&cached).copied().collect();
    remaining.sort_unstable_by(|a, b| b.cmp(a)); // newest first
    Ok(remaining)
}

/// Fetch message summaries for a specific set of UIDs (used by the backfill).
async fn fetch_summaries_by_uid(
    account_id: u32,
    session: &mut ImapSession,
    folder_id: u32,
    path: &str,
    uids: &[u32],
    use_envelope: bool,
) -> Result<Vec<Message>, async_imap::error::Error> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    sel(session, path).await?;
    let set = uid_set(uids);
    let items: String = if use_envelope {
        format!("(UID ENVELOPE FLAGS BODYSTRUCTURE INTERNALDATE{REFS_FETCH_ITEM})")
    } else {
        "(UID FLAGS BODY.PEEK[HEADER] INTERNALDATE)".to_string()
    };
    let fetches: Vec<Fetch> = fetch_uids(session, set, items).await?.try_collect().await?;
    let mut messages: Vec<Message> = fetches
        .iter()
        .map(|f| {
            if use_envelope {
                build_summary(account_id, f, folder_id)
            } else {
                summary_from_headers(account_id, f, folder_id)
            }
        })
        .collect();
    read_gmail_labels(account_id, session, path, uids, &mut messages).await?;
    Ok(messages)
}

/// Advance one backfill job by a chunk. Fetches the next `BACKFILL_CHUNK` UIDs,
/// upserts them into the cache, and emits them as an append to the UI's index.
/// Requeues the job (at the back) if more remain. Reconnects and, if needed,
/// disables ENVELOPE parsing (iCloud) on a parse failure.
#[allow(clippy::too_many_arguments)]
async fn run_one_backfill(
    queue: &mut std::collections::VecDeque<Backfill>,
    session: &mut Option<ImapSession>,
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    prefetch: &mut std::collections::VecDeque<(String, u32)>,
    use_envelope: &mut bool,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(mut job) = queue.pop_front() else {
        return;
    };
    let Some(mut s) = session.take() else {
        queue.push_front(job);
        return;
    };

    // Compute the worklist on first touch (with one reconnect on failure).
    if job.remaining.is_none() {
        match backfill_worklist(&mut s, account_id, &job.path, cache).await {
            Ok(rem) => job.remaining = Some(rem),
            Err(_) => match connect(account).await {
                Ok(fresh) => {
                    s = fresh;
                    match backfill_worklist(&mut s, account_id, &job.path, cache).await {
                        Ok(rem) => job.remaining = Some(rem),
                        Err(_) => {
                            *session = Some(s);
                            queue.push_back(job);
                            return;
                        }
                    }
                }
                Err(_) => {
                    // Stay offline; retry this job on the next connect.
                    queue.push_back(job);
                    return;
                }
            },
        }
    }

    let rem = job.remaining.as_mut().unwrap();
    if rem.is_empty() {
        emit(WorkerEvent::BackfillDone {
            folder_id: job.folder_id,
        });
        *session = Some(s); // done — don't requeue
        return;
    }
    let take = rem.len().min(BACKFILL_CHUNK);
    let chunk: Vec<u32> = rem.drain(..take).collect();

    match fetch_summaries_by_uid(
        account_id,
        &mut s,
        job.folder_id,
        &job.path,
        &chunk,
        *use_envelope,
    )
    .await
    {
        Ok(msgs) => {
            if let Some(c) = cache {
                c.upsert_messages(account_id, &job.path, &msgs);
            }
            // Gallery folders: queue this chunk's attachments for background
            // download so they appear in the attachments gallery.
            if job.gallery {
                queue_attachment_prefetch(prefetch, &job.path, &msgs, cache, account_id);
            }
            emit(WorkerEvent::MessagesAppend {
                folder_id: job.folder_id,
                messages: msgs,
            });
            *session = Some(s);
        }
        Err(e) => {
            // Put the chunk back and reconnect; a parse error means the server
            // answers this way of asking with something unreadable (iCloud's
            // ENVELOPE, Mailfence with the preview items, #226) — move down
            // the same ladder the first load uses.
            for uid in chunk.into_iter().rev() {
                rem.insert(0, uid);
            }
            if is_parse_error(&e) {
                step_fetch_mode(account_id, use_envelope);
            }
            if let Ok(fresh) = connect(account).await {
                *session = Some(fresh);
            }
            // else: session stays None; the main loop reconnects on next request.
        }
    }

    if job.remaining.as_ref().is_some_and(|r| !r.is_empty()) {
        queue.push_back(job);
    } else {
        emit(WorkerEvent::BackfillDone {
            folder_id: job.folder_id,
        });
    }
}

/// Returns the rendered body, the sender verdict, and whether the message
/// actually carries attachments — all three from the one fetch.
async fn load_body(
    session: &mut ImapSession,
    path: &str,
    uid: u32,
) -> Result<(String, crate::models::SenderCheck, bool), async_imap::error::Error> {
    select_for_read(session, path).await?;

    // Fetch the whole message (PEEK so \Seen isn't set) and extract the body with
    // mail-parser. We deliberately avoid a BODYSTRUCTURE-based "text part only"
    // fast path: some servers (iCloud) return structures our IMAP parser rejects,
    // which would fail the fetch and corrupt the session.
    // The whole message is in hand, so the sender check rides along for free
    // rather than costing a second fetch. The paperclip is guessed from
    // BODYSTRUCTURE (or, on servers whose structure we can't parse, from the
    // top-level Content-Type), and both guesses miss shapes like Apple Mail's
    // inline PDF nested under an alternative (issue #9). The whole message is
    // in hand here, so the guess can be replaced with fact, at no extra
    // network cost. A server that hands nothing over is an error, not an
    // empty message (#226): shown as one, it read as the message being
    // blank, and was cached as such.
    let raw = fetch_raw_message(session, uid)
        .await?
        .ok_or_else(|| no_content_error(uid))?;
    Ok(render_raw(&raw))
}

/// Fetch several messages' bodies from one folder in a single `uid_fetch`.
///
/// The per-message extraction is [`load_body`]'s, applied to each response in
/// the set: body, sender check and the attachment fact all come off the raw
/// message that is already in hand, at no extra network cost. A UID the server
/// doesn't answer is simply absent from the map — the caller decides what to
/// show in its place.
async fn load_bodies(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
) -> Result<
    std::collections::HashMap<u32, (String, crate::models::SenderCheck, bool)>,
    async_imap::error::Error,
> {
    select_for_read(session, path).await?;
    let set = uid_set(uids);
    let fetches: Vec<Fetch> = fetch_uids(session, set, "(BODY.PEEK[])")
        .await?
        .try_collect()
        .await?;
    let mut out = std::collections::HashMap::new();
    let mut last_uid: Option<u32> = None;
    for f in &fetches {
        if let Some(u) = f.uid {
            last_uid = Some(u);
        }
        // A message's response can arrive split across items — [`load_body`]
        // scans every one of them for the body rather than trusting the first,
        // and this has to be as forgiving. An item with no body is not an empty
        // message, just not the part carrying one, so it is skipped: leaving the
        // UID out of the map means the caller shows "(empty message)" without
        // writing that over a real body in the cache.
        let (Some(uid), Some(raw)) = (last_uid, raw_of(f)) else {
            continue;
        };
        out.entry(uid).or_insert_with(|| render_raw(&raw));
    }
    Ok(out)
}

fn build_summary(account_id: u32, fetch: &Fetch, folder_id: u32) -> Message {
    let uid = fetch.uid.unwrap_or(0);
    let flags: Vec<Flag> = fetch.flags().collect();
    let unread = !flags.iter().any(|f| matches!(f, Flag::Seen));
    let starred = flags.iter().any(|f| matches!(f, Flag::Flagged));
    let answered = flags.iter().any(|f| matches!(f, Flag::Answered));
    let keywords = custom_flags(&flags);
    let forwarded = keywords
        .iter()
        .any(|k| k.eq_ignore_ascii_case(FORWARDED_KEYWORD));

    let env = fetch.envelope();
    let (from_name, from_addr) = env
        .and_then(|e| e.from.as_ref())
        .and_then(|v| v.first())
        .map(address_parts)
        .unwrap_or_else(|| ("Unknown".to_string(), String::new()));
    let subject = env
        .and_then(|e| e.subject.as_deref())
        .map(|s| decode_header(&unescape_quoted(s)))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no subject)".to_string());
    let (date, timestamp) = env
        .and_then(|e| e.date.as_deref())
        .map(format_date)
        .filter(|(_, ts)| *ts > 0)
        .unwrap_or_else(|| internal_date_summary(fetch));
    let to = address_list(env.and_then(|e| e.to.as_ref()));
    let cc = address_list(env.and_then(|e| e.cc.as_ref()));
    // Only a sender's own copy carries one (#476).
    let bcc = address_list(env.and_then(|e| e.bcc.as_ref()));
    // Where the sender asked replies to go. Servers echo From here when the
    // header is absent; storing that echo would be noise, so keep it only
    // when it actually differs.
    let reply_to = address_list(env.and_then(|e| e.reply_to.as_ref()));
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) {
        String::new()
    } else {
        reply_to
    };

    let has_attachment = fetch
        .bodystructure()
        .map(structure_has_attachment)
        .unwrap_or(false);

    // Message-ID + References + In-Reply-To drive accurate threading. References
    // isn't in the ENVELOPE, so it rides along as its own header fetch
    // ([`REFS_FETCH_ITEM`]) — without it only the immediate parent is known, and
    // threads break wherever that parent lives in another folder.
    let message_id = env
        .and_then(|e| e.message_id.as_deref())
        .map(normalize_msgid)
        .unwrap_or_default();
    let in_reply_to = env
        .and_then(|e| e.in_reply_to.as_deref())
        .map(normalize_msgids)
        .unwrap_or_default();
    let references = merge_msgids(&references_of(fetch), &in_reply_to);

    let mut msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name,
        from_addr,
        reply_to,
        to,
        cc,
        bcc,
        subject,
        preview: String::new(),
        body: String::new(),
        date,
        timestamp,
        unread,
        starred,
        answered,
        forwarded,
        keywords,
        has_attachment,
        message_id,
        references,
        importance: importance_of(fetch),
        due: 0,
        labels: Vec::new(),
    };
    msg.scrub_nuls();
    msg
}

/// A UID list as an IMAP sequence set: sorted, without repeats, and with
/// runs collapsed to ranges (`3,4,5,9` → `3:5,9`). Every command that takes
/// a set goes through here: iCloud (and Exchange) answer an unsorted set —
/// which a selection taken in list order, newest first, always was — with
/// "Parse Error", so a bulk delete or move there failed outright.
fn uid_set(uids: &[u32]) -> String {
    let mut sorted: Vec<u32> = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = String::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        if !out.is_empty() {
            out.push(',');
        }
        if end > start {
            out.push_str(&format!("{start}:{end}"));
        } else {
            out.push_str(&start.to_string());
        }
        i += 1;
    }
    out
}

/// The keyword RFC 5550 gives a forwarded message; servers and clients
/// differ in its case.
pub const FORWARDED_KEYWORD: &str = "$Forwarded";

/// The user-defined keywords among a message's flags, verbatim.
fn custom_flags(flags: &[Flag]) -> Vec<String> {
    flags
        .iter()
        .filter_map(|f| match f {
            Flag::Custom(k) => Some(k.to_string()),
            _ => None,
        })
        .collect()
}

/// The headers of a `HEADER.FIELDS` block, unfolded, as (name, value).
///
/// What comes back is a small header block (the requested headers that the
/// message has, each folded across lines like any other, terminated by a
/// blank line) or nothing at all.
fn header_fields(raw: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(raw);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = out.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    out
}

/// The headers a fetch that asked for [`REFS_FETCH_ITEM`] brought back.
fn fetched_fields(fetch: &Fetch) -> Vec<(String, String)> {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    fetch
        .section(&SectionPath::Full(MessageSection::Header))
        .map(header_fields)
        .unwrap_or_default()
}

/// The `References:` value from a fetch that asked for [`REFS_FETCH_ITEM`],
/// normalized into the same space-separated form as the ENVELOPE ids.
fn references_of(fetch: &Fetch) -> String {
    fetched_fields(fetch)
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("references"))
        .map(|(_, value)| normalize_msgids(value.as_bytes()))
        .unwrap_or_default()
}

/// The importance the priority headers of a [`REFS_FETCH_ITEM`] fetch give.
fn importance_of(fetch: &Fetch) -> crate::models::Importance {
    let fields = fetched_fields(fetch);
    crate::models::Importance::from_headers(|name| {
        fields
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    })
}

/// The `Message-ID:` value from a fetch that asked for
/// `HEADER.FIELDS (MESSAGE-ID)`, normalized like the ENVELOPE ids so it
/// compares equal to the ids the app stores.
fn message_id_of(fetch: &Fetch) -> String {
    use async_imap::imap_proto::types::{MessageSection, SectionPath};
    let Some(raw) = fetch.section(&SectionPath::Full(MessageSection::Header)) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(raw);
    let Some((name, value)) = text.split_once(':') else {
        return String::new();
    };
    if !name.trim().eq_ignore_ascii_case("message-id") {
        return String::new();
    }
    normalize_msgid(value.as_bytes())
}

/// Whether an IMAP BODYSTRUCTURE contains a part marked as an attachment.
///
/// `Content-Disposition: attachment` is the obvious case, but Apple Mail sends
/// iPhone photos as *inline* parts of a multipart/mixed, so disposition alone
/// misses them. Any non-text part therefore counts, except a small one carrying a
/// Content-ID — see [`INLINE_ATTACHMENT_MIN`].
fn structure_has_attachment(bs: &async_imap::imap_proto::types::BodyStructure) -> bool {
    use async_imap::imap_proto::types::{BodyStructure as Bs, ContentEncoding};

    let is_attachment = |common: &async_imap::imap_proto::types::BodyContentCommon| {
        common
            .disposition
            .as_ref()
            .is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"))
    };
    // BODYSTRUCTURE reports the *encoded* size; base64 inflates by 4/3.
    let decoded_size = |other: &async_imap::imap_proto::types::BodyContentSinglePart| {
        let octets = other.octets as usize;
        match other.transfer_encoding {
            ContentEncoding::Base64 => octets / 4 * 3,
            _ => octets,
        }
    };

    match bs {
        Bs::Multipart { bodies, .. } => bodies.iter().any(structure_has_attachment),
        // A text part is an attachment only when it says so — except a
        // calendar part, which a meeting request sends as a plain
        // alternative with no disposition at all (#223). That part is the
        // meeting: it has to be listed, and the reader has to know the
        // message carries files before it will ask for them.
        Bs::Text { common, .. } => is_attachment(common) || is_calendar(common),
        Bs::Basic { common, other, .. } | Bs::Message { common, other, .. } => {
            is_attachment(common)
                || other.id.is_none()
                || decoded_size(other) >= INLINE_ATTACHMENT_MIN
        }
    }
}

/// Whether a BODYSTRUCTURE part is a `text/calendar` one (#223).
fn is_calendar(common: &async_imap::imap_proto::types::BodyContentCommon) -> bool {
    common.ty.ty.eq_ignore_ascii_case("text") && common.ty.subtype.eq_ignore_ascii_case("calendar")
}

/// Every attachment in a BODYSTRUCTURE, with what it is called, how big it is
/// and which IMAP section holds it — all without downloading a byte of it. This
/// is what lets the gallery reach back through an archive: the server describes
/// the files, and only opening one actually fetches it.
///
/// What counts as an attachment is [`structure_has_attachment`]'s rule, kept
/// deliberately identical so a message the list gives a paperclip to is a
/// message the gallery lists parts for.
fn structure_attachments(
    bs: &async_imap::imap_proto::types::BodyStructure,
) -> Vec<crate::models::AttachmentMeta> {
    use async_imap::imap_proto::types::{BodyStructure as Bs, ContentEncoding};

    /// A parameter's value, matched case-insensitively as RFC 2045 requires.
    fn param(params: &async_imap::imap_proto::types::BodyParams, key: &str) -> Option<String> {
        params.as_ref()?.iter().find_map(|(k, v)| {
            k.eq_ignore_ascii_case(key)
                .then(|| decode_header(v.as_bytes()))
        })
    }

    fn walk(bs: &Bs, section: &str, out: &mut Vec<crate::models::AttachmentMeta>) {
        match bs {
            Bs::Multipart { bodies, .. } => {
                for (i, child) in bodies.iter().enumerate() {
                    let child_section = if section.is_empty() {
                        format!("{}", i + 1)
                    } else {
                        format!("{section}.{}", i + 1)
                    };
                    walk(child, &child_section, out);
                }
            }
            Bs::Text { common, other, .. }
            | Bs::Basic { common, other, .. }
            | Bs::Message { common, other, .. } => {
                let declared = common
                    .disposition
                    .as_ref()
                    .is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"));
                // BODYSTRUCTURE reports the encoded size; base64 inflates by 4/3.
                let size = match other.transfer_encoding {
                    ContentEncoding::Base64 => other.octets as u64 / 4 * 3,
                    _ => other.octets as u64,
                };
                let is_text = matches!(bs, Bs::Text { .. });
                // A text part is only ever an attachment when it says so; for
                // the rest, an inline part with a Content-ID is decoration
                // unless it is big enough to be content (see the constant).
                let counts = if is_text {
                    declared || is_calendar(common)
                } else {
                    declared || other.id.is_none() || size as usize >= INLINE_ATTACHMENT_MIN
                };
                if !counts {
                    return;
                }
                let name = common
                    .disposition
                    .as_ref()
                    .and_then(|d| param(&d.params, "filename"))
                    .or_else(|| param(&common.ty.params, "name"))
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| {
                        // Unnamed, as Apple Mail's inline photos arrive: give it
                        // the extension its type implies so the gallery can sort
                        // and filter it like any other file.
                        let ext = mime_extension(&common.ty.ty, &common.ty.subtype);
                        format!("attachment-{}{ext}", out.len() + 1)
                    });
                out.push(crate::models::AttachmentMeta {
                    idx: out.len() as u32,
                    name,
                    mime: format!(
                        "{}/{}",
                        common.ty.ty.to_lowercase(),
                        common.ty.subtype.to_lowercase()
                    ),
                    size,
                    section: section.to_string(),
                });
            }
        }
    }

    let mut out = Vec::new();
    // A message that is not multipart is one part, section 1.
    walk(
        bs,
        if matches!(bs, Bs::Multipart { .. }) {
            ""
        } else {
            "1"
        },
        &mut out,
    );
    out
}

/// A filename extension for a MIME type, for parts that arrive unnamed. Only
/// the types that actually turn up without a filename are worth listing; the
/// rest get none, and show as a generic file.
fn mime_extension(ty: &str, subtype: &str) -> &'static str {
    // A meeting request's calendar part often arrives with no name at all
    // (Outlook sends one); without the suffix nothing would open it (#223).
    if ty.eq_ignore_ascii_case("text") && subtype.eq_ignore_ascii_case("calendar") {
        return ".ics";
    }
    if !ty.eq_ignore_ascii_case("image") {
        return "";
    }
    match subtype.to_ascii_lowercase().as_str() {
        "jpeg" | "jpg" => ".jpg",
        "png" => ".png",
        "gif" => ".gif",
        "heic" => ".heic",
        "heif" => ".heif",
        "webp" => ".webp",
        "tiff" => ".tif",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// POP3 path
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Mock path (offline fallback)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Join an envelope address list into "a@x.com, b@y.com" (emails only).
fn address_list(addrs: Option<&Vec<async_imap::imap_proto::types::Address>>) -> String {
    addrs
        .map(|v| {
            v.iter()
                .map(|a| address_parts(a).1)
                .filter(|e| !e.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn address_parts(addr: &async_imap::imap_proto::types::Address) -> (String, String) {
    let mailbox = addr
        .mailbox
        .as_deref()
        .map(bytes_to_string)
        .unwrap_or_default();
    let host = addr
        .host
        .as_deref()
        .map(bytes_to_string)
        .unwrap_or_default();
    let email = if host.is_empty() {
        mailbox.clone()
    } else {
        format!("{mailbox}@{host}")
    };
    let name = addr
        .name
        .as_deref()
        .map(|n| clean_display_name(&decode_header(&unescape_quoted(n))))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| email.clone());
    (name, email)
}

/// Fall back to the server's `INTERNALDATE` (the delivery date) when a message
/// carries no parseable `Date:` header. Some senders omit `Date:` entirely,
/// which would otherwise leave the row with a blank date label and a zero sort
/// timestamp (sinking it to the bottom of the list). Returns `("", 0)` if the
/// server didn't supply an INTERNALDATE either.
fn internal_date_summary(fetch: &Fetch) -> (String, i64) {
    match fetch.internal_date() {
        Some(dt) => {
            let ts = dt.timestamp();
            (format_timestamp(ts), ts)
        }
        None => (String::new(), 0),
    }
}

#[cfg(test)]
mod compact_parse_error_tests {
    use super::compact_parse_error;

    #[test]
    fn drops_the_byte_dump_and_keeps_the_text() {
        let e = "io: Error(Error { input: [42, 32, 55, 57, 56], code: Tag }) during parsing of \"* 798 FETCH\"";
        assert_eq!(
            compact_parse_error(e),
            "io: Error(Error { input: [5 bytes], code: Tag }) during parsing of \"* 798 FETCH\""
        );
        assert_eq!(compact_parse_error("connection lost"), "connection lost");
    }
}

/// A plain IMAP account for the tests here and in the child modules.
#[cfg(test)]
pub(super) fn sample_account() -> AccountConfig {
    AccountConfig {
        folder_roles: Default::default(),
        hidden_folders: Vec::new(),
        folders_seeded: false,
        sent_copy_path: None,
        server_saves_sent: false,
        reply_copies: Default::default(),
        empty_junk_days: 0,
        empty_trash_days: 0,
        clean_lists_days: 0,
        pgp_key: None,
        in_unified: true,
        folder_sort: None,
        sign_by_default: false,
        plain_subject: false,
        graph_directory: false,
        graph_directory_only: false,
        graph_label_names: false,
        push: None,
        name: String::new(),
        email: "me@example.com".into(),
        protocol: crate::config::Protocol::Imap,
        imap_host: "imap.example.com".into(),
        imap_port: 993,
        smtp_host: String::new(),
        smtp_port: 587,
        username: "me@example.com".into(),
        password: String::new(),
        smtp_separate: false,
        jmap_token: false,
        jmap_generic: false,
        tls_accept_hostname_mismatch: false,
        security: None,
        smtp_username: String::new(),
        smtp_password: String::new(),
        color: None,
        emoji: None,
        avatar: None,
        gravatar: false,
        signature: None,
        signature_html: false,
        signatures: Vec::new(),
        default_signature: None,
        label: None,
        aliases: Vec::new(),
        enabled: true,
        goa_id: None,
        goa_mail_disabled: false,
        goa_enabled_before_mail_disabled: true,
        oauth: false,
        oauth_settings: None,
        oauth_refresh: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::auth::sign_in_password_blocking;
    use crate::worker::chores::graph_before_date;
    use crate::worker::chores::AUTO_EMPTY_MIN;
    use crate::worker::compose::build::build_email;
    use crate::worker::compose::build::build_message;
    use crate::worker::compose::editable::with_bcc;
    use crate::worker::compose::recipients::mailbox;
    use crate::worker::outbox::pick_queued_sent_copy;
    use crate::worker::outbox::pick_reply_folder;
    use crate::worker::preview::looks_like_base64;
    use crate::worker::preview::pgp_preview;
    use crate::worker::preview::PREVIEW_CHARS;
    use crate::worker::render::body::image_mime;
    use crate::worker::render::body::linkify;
    use crate::worker::smtp::alias_with_own_smtp;
    use crate::worker::smtp::SmtpTls;

    #[test]
    fn only_the_top_level_inbox_is_the_inbox() {
        // #344: an archive keeps per-year Inbox and Sent folders.
        let kind = |p: &str, d: &str| classify_with_source(p, Some(d), &[]).0;
        assert_eq!(kind("INBOX", "/"), FolderKind::Inbox);
        assert_eq!(kind("Inbox", "/"), FolderKind::Inbox);
        assert_eq!(kind("Archiv/2020/Inbox", "/"), FolderKind::Custom);
        assert_eq!(kind("Archiv/2020/Sent", "/"), FolderKind::Custom);
        assert_eq!(kind("INBOX.Archiv.Inbox", "."), FolderKind::Custom);
        // Where servers keep their roles, names still count.
        assert_eq!(kind("Sent", "/"), FolderKind::Sent);
        assert_eq!(kind("INBOX.Sent", "."), FolderKind::Sent);
        assert_eq!(kind("INBOX.Trash", "."), FolderKind::Trash);
        assert_eq!(kind("[Gmail]/Spam", "/"), FolderKind::Junk);
        assert_eq!(kind("Projects/Drafts", "/"), FolderKind::Custom);
        // A dot in a name is not a level when the server's delimiter is /.
        assert_eq!(kind("v1.0/Sent", "/"), FolderKind::Custom);
    }

    #[test]
    fn a_templates_folder_is_found_by_name() {
        // #360: no SPECIAL-USE attribute marks one.
        let kind = |p: &str, d: &str| classify_with_source(p, Some(d), &[]).0;
        assert_eq!(kind("Templates", "/"), FolderKind::Templates);
        assert_eq!(kind("INBOX.Templates", "."), FolderKind::Templates);
        assert_eq!(kind("template", "/"), FolderKind::Templates);
        assert_eq!(kind("Projects/Templates", "/"), FolderKind::Custom);
        // Listed right after Drafts, ahead of Sent.
        assert!(folder_order(FolderKind::Drafts) < folder_order(FolderKind::Templates));
        assert!(folder_order(FolderKind::Templates) < folder_order(FolderKind::Sent));
        assert!(folder_order(FolderKind::Templates) < folder_order(FolderKind::Custom));
        // Nothing waits on it, and no conversation reaches into it.
        assert!(!watchable(FolderKind::Templates));
        assert!(!chip_counts_all(FolderKind::Templates));
    }

    #[test]
    fn envelope_names_lose_the_quotes_a_server_kept() {
        // #312: the name as imap-proto hands it back, escapes and all.
        let name = clean_display_name(&decode_header(&unescape_quoted(br#"\"Sender Name\""#)));
        assert_eq!(name, "Sender Name");
        let inner = clean_display_name(&decode_header(&unescape_quoted(br#"Ann \"Nan\" Lee"#)));
        assert_eq!(inner, r#"Ann "Nan" Lee"#);
        assert_eq!(clean_display_name("Plain Name"), "Plain Name");
        // A backslash that escapes nothing is the name's own.
        assert_eq!(unescape_quoted(br"DOM\user"), br"DOM\user".to_vec());
        assert_eq!(unescape_quoted(br#"a\\\""#), br#"a\""#.to_vec());
        // A name an earlier build stored with the escapes still in it.
        assert_eq!(clean_display_name(r#"\"Sender Name\""#), "Sender Name");
    }

    #[test]
    fn envelope_name_with_the_headers_quotes_parses_clean() {
        use async_imap::imap_proto::{AttributeValue, Response};
        let line = b"* 1 FETCH (ENVELOPE (NIL \"S\" ((\"\\\"Sender Name\\\"\" NIL \"some\" \"mail.tld\")) \
NIL NIL NIL NIL NIL NIL NIL))\r\n";
        let (_, resp) = async_imap::imap_proto::parser::parse_response(line).unwrap();
        let Response::Fetch(_, attrs) = resp else {
            panic!("not a FETCH")
        };
        let Some(AttributeValue::Envelope(env)) = attrs.first() else {
            panic!("no ENVELOPE")
        };
        let from = env.from.as_ref().unwrap();
        assert_eq!(
            address_parts(&from[0]),
            ("Sender Name".to_string(), "some@mail.tld".to_string())
        );
    }

    #[test]
    fn imap_storage_quota_reads_kibibytes() {
        use async_imap::types::{Quota, QuotaResource, QuotaResourceName};
        let quotas = vec![Quota {
            root_name: String::new(),
            resources: vec![
                QuotaResource {
                    name: QuotaResourceName::Message,
                    usage: 12,
                    limit: 1000,
                },
                QuotaResource {
                    name: QuotaResourceName::Storage,
                    usage: 1024,
                    limit: 4096,
                },
            ],
        }];
        assert_eq!(
            imap_storage_quota(&quotas),
            Some(MailboxQuota {
                used: 1024 * 1024,
                limit: 4096 * 1024
            })
        );
        let unlimited = vec![Quota {
            root_name: String::new(),
            resources: vec![QuotaResource {
                name: QuotaResourceName::Storage,
                usage: 5,
                limit: 0,
            }],
        }];
        assert_eq!(imap_storage_quota(&unlimited), None);
    }

    /// A password the worker holds is used as it is, and an OAuth account
    /// needs none, so neither reads the keyring (#375).
    #[test]
    fn sign_in_password_prefers_the_held_one() {
        let held = AccountConfig {
            password: "pw".into(),
            smtp_separate: true,
            smtp_password: "smtp".into(),
            ..sample_account()
        };
        assert_eq!(sign_in_password_blocking(&held, false).as_deref(), Ok("pw"));
        assert_eq!(
            sign_in_password_blocking(&held, true).as_deref(),
            Ok("smtp")
        );
        let oauth = AccountConfig {
            oauth: true,
            password: String::new(),
            ..sample_account()
        };
        assert_eq!(sign_in_password_blocking(&oauth, false).as_deref(), Ok(""));
    }

    /// A keyring that cannot be read at sign-in (#375), with
    /// `IMAP_LIVE=host,port,user,password` and `KEYRING_LIVE=1`. Run it in a
    /// throwaway session bus with scratch `XDG_DATA_HOME` and `XDG_RUNTIME_DIR`
    /// under /tmp (`dbus-run-session -- cargo test ...`): it starts its own
    /// gnome-keyring-daemon and stores the password there.
    /// No sign-in goes out without the password, and once the keyring answers
    /// the next sign-in reads it again.
    #[test]
    #[ignore]
    fn live_keyring_read_again() {
        let (Ok(spec), Ok(_)) = (std::env::var("IMAP_LIVE"), std::env::var("KEYRING_LIVE")) else {
            return;
        };
        let scratch = std::env::var("XDG_DATA_HOME").unwrap_or_default();
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
        assert!(
            scratch.starts_with("/tmp/") && runtime.starts_with("/tmp/"),
            "needs a scratch XDG_DATA_HOME and XDG_RUNTIME_DIR, not the desktop's keyring"
        );
        let p: Vec<&str> = spec.splitn(4, ',').collect();
        let account = AccountConfig {
            email: p[2].into(),
            imap_host: p[0].into(),
            imap_port: p[1].parse().expect("port"),
            username: p[2].into(),
            password: String::new(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                imap_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..sample_account()
        };
        // The login keyring, once there, is the default collection entries go to.
        let keyrings = std::path::Path::new(&scratch).join("keyrings");
        std::fs::create_dir_all(&keyrings).unwrap();
        std::fs::write(keyrings.join("default"), "login").unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(connect(&account))
            .err()
            .expect("no keyring, no sign-in")
            .to_string();
        assert!(err.contains("keyring"), "{err}");

        use std::io::Write;
        let mut daemon = std::process::Command::new("gnome-keyring-daemon")
            .args(["--replace", "--unlock", "--components=secrets"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("gnome-keyring-daemon");
        daemon
            .stdin
            .take()
            .unwrap()
            .write_all(b"scratch\n")
            .unwrap();
        daemon.wait().unwrap();
        let mut stored = crate::config::store_password(&account.email, p[3]);
        for _ in 0..20 {
            if stored.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
            stored = crate::config::store_password(&account.email, p[3]);
        }
        stored.expect("store");
        rt.block_on(connect(&account))
            .expect("signs in with the keyring's password");
    }

    /// Removing an attachment on a real server (#289), with
    /// `IMAP_LIVE=host,port,user,password` (implicit TLS, any certificate).
    /// Stores a message with two files, removes one, and checks the copy
    /// kept the other, the flags and the arrival date; the mailbox is left
    /// as it was found.
    #[test]
    #[ignore]
    fn live_strip_attachment() {
        let Ok(spec) = std::env::var("IMAP_LIVE") else {
            return;
        };
        let p: Vec<&str> = spec.splitn(4, ',').collect();
        let account = AccountConfig {
            imap_host: p[0].into(),
            imap_port: p[1].parse().expect("port"),
            username: p[2].into(),
            password: p[3].into(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                imap_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..sample_account()
        };
        let msgid = format!("strip-{}@hylki.test", crate::datefmt::now());
        let raw = strip::MIXED.replace("r1@example.org", &msgid);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut sess = connect(&account).await.expect("connect");
            let uid_next = sel(&mut sess, "INBOX").await.expect("select").uid_next;
            append_dated(
                &mut sess,
                "INBOX",
                Some("(\\Seen \\Flagged $label1)"),
                Some("\"01-Jan-2020 10:00:00 +0000\""),
                raw.as_bytes(),
            )
            .await
            .expect("append");
            let wanted = normalize_msgid(format!("<{msgid}>").as_bytes());
            let uid = find_appended(&mut sess, 0, uid_next, &wanted)
                .await
                .expect("stored");
            let mut session = Some(sess);
            let (new_uid, _) =
                match strip_imap_attachment(&mut session, &account, "INBOX", uid, "report.pdf", 18)
                    .await
                {
                    Ok(done) => done,
                    Err(e) => panic!("strip: {}", e.text),
                };
            let new_uid = new_uid.expect("the copy is found again");
            assert_ne!(new_uid, uid);
            let sess = session.as_mut().unwrap();
            sel(sess, "INBOX").await.expect("select");
            let left: Vec<Fetch> = fetch_uids(sess, uid.to_string(), "(UID FLAGS)")
                .await
                .expect("fetch")
                .try_collect()
                .await
                .expect("fetch");
            assert!(
                left.iter().all(|f| f.uid != Some(uid)),
                "the original is gone"
            );
            let fetches: Vec<Fetch> = fetch_uids(
                sess,
                new_uid.to_string(),
                "(UID FLAGS INTERNALDATE BODY.PEEK[])",
            )
            .await
            .expect("fetch")
            .try_collect()
            .await
            .expect("fetch");
            let f = fetches
                .iter()
                .find(|f| f.uid == Some(new_uid))
                .expect("the copy");
            let flags = kept_flags(f.flags());
            println!(
                "copy {new_uid}: flags {flags:?} date {:?}",
                f.internal_date()
            );
            assert!(
                flags.contains(&"\\Seen".to_string()) && flags.contains(&"\\Flagged".to_string())
            );
            assert!(flags.contains(&"$label1".to_string()));
            assert_eq!(
                f.internal_date().map(|d| d.timestamp()),
                Some(1_577_872_800)
            );
            let names: Vec<String> = extract_attachments(f.body().expect("body"))
                .into_iter()
                .map(|a| a.name)
                .collect();
            assert_eq!(names, vec!["data.csv"]);
            purge_messages(sess, "INBOX", &[new_uid])
                .await
                .expect("clean up");
        });
    }

    /// A folder moved with a sub-folder keeps both subscribed at their new
    /// names, so a client showing subscribed folders only still sees them.
    /// `IMAP_LIVE=host,port,user,password cargo test --bin hylki
    /// worker::tests::live_rename_moves_subscriptions -- --ignored`
    #[test]
    /// #336 against a real server: mail appended while the connection is
    /// busy is picked up by the next wait, not left until something else
    /// happens to sync the folder.
    /// #349, #350 against a real server: a draft saved from an alias with a
    /// Bcc and a file comes back with all three.
    /// `IMAP_LIVE=host,port,user,password cargo test live_draft -- --ignored`
    #[test]
    #[ignore]
    fn live_draft_comes_back_whole() {
        let Ok(spec) = std::env::var("IMAP_LIVE") else {
            return;
        };
        let p: Vec<&str> = spec.splitn(4, ',').collect();
        let account = AccountConfig {
            imap_host: p[0].into(),
            imap_port: p[1].parse().expect("port"),
            username: p[2].into(),
            password: p[3].into(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                imap_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..sample_account()
        };
        let file = std::env::temp_dir().join(format!("hylki-350-{}.txt", std::process::id()));
        std::fs::write(&file, b"notes").expect("write");
        let subject = format!("hylki-350-{}", std::process::id());
        let msg = OutgoingMessage {
            from_alias: Some("Work <work@example.com>".into()),
            to: "ada@example.com".into(),
            bcc: "hidden@example.com".into(),
            subject: subject.clone(),
            html: "<p>draft text</p>".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            in_reply_to: "orig@example.com".into(),
            ..sample_outgoing()
        };
        let raw = build_draft(&account, &msg).expect("builds").formatted();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut sess = connect(&account).await.expect("connect");
            append_draft(&mut sess, "Drafts", &raw, false)
                .await
                .expect("append");
            let listed = load_messages(7, &mut sess, 1, "Drafts", true, None)
                .await
                .expect("load");
            let uid = listed
                .iter()
                .find(|m| m.subject == subject)
                .expect("listed")
                .uid;
            let back = load_raw(&mut sess, "Drafts", uid).await.expect("raw");
            delete_draft(&mut sess, "Drafts", uid).await.ok();
            let e = editable_from_raw(&back, &[]);
            assert_eq!(e.from, "Work <work@example.com>");
            assert_eq!(e.bcc, "hidden@example.com");
            assert_eq!(e.in_reply_to, "orig@example.com");
            assert!(e.body_html.contains("draft text"), "{}", e.body_html);
            assert_eq!(e.attachments.len(), 1);
        });
        std::fs::remove_file(&file).ok();
    }

    /// #360 against a real server: a template goes into a Templates folder
    /// the save makes, as read mail and no draft.
    /// `IMAP_LIVE=host,port,user,password cargo test live_template -- --ignored`
    #[test]
    #[ignore]
    fn live_template_is_saved_as_read_mail() {
        let Ok(spec) = std::env::var("IMAP_LIVE") else {
            return;
        };
        let p: Vec<&str> = spec.splitn(4, ',').collect();
        let account = AccountConfig {
            imap_host: p[0].into(),
            imap_port: p[1].parse().expect("port"),
            username: p[2].into(),
            password: p[3].into(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                imap_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..sample_account()
        };
        let folder = format!("Templates-{}", std::process::id());
        let subject = format!("hylki-360-{}", std::process::id());
        let msg = OutgoingMessage {
            subject: subject.clone(),
            html: "<p>template text</p>".into(),
            ..sample_outgoing()
        };
        let raw = build_draft(&account, &msg).expect("builds").formatted();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut sess = connect(&account).await.expect("connect");
            append_draft(&mut sess, &folder, &raw, true)
                .await
                .expect("append makes the folder");
            let listed = load_messages(7, &mut sess, 1, &folder, true, None)
                .await
                .expect("load");
            let uid = listed
                .iter()
                .find(|m| m.subject == subject)
                .expect("listed")
                .uid;
            sel(&mut sess, &folder).await.expect("select");
            let fetches: Vec<Fetch> = fetch_uids(&mut sess, uid.to_string(), "(UID FLAGS)")
                .await
                .expect("fetch")
                .try_collect()
                .await
                .expect("fetch");
            let flags: Vec<String> = fetches
                .iter()
                .flat_map(|f| f.flags())
                .map(|f| format!("{f:?}"))
                .collect();
            println!("template {uid}: flags {flags:?}");
            assert!(flags.iter().any(|f| f == "Seen"), "{flags:?}");
            assert!(!flags.iter().any(|f| f == "Draft"), "{flags:?}");
            let _ = sess.close().await;
            delete_box(&mut sess, &folder).await.expect("clean up");
        });
    }

    /// #468 against a real server, with `IMAP_LIVE=host,port,user,password`
    /// and `SMTP_LIVE=host,port` (implicit TLS): a reply goes out, and its
    /// copy is filed in the folder of the message it answers, or in Sent when
    /// that folder will not take it.
    #[test]
    #[ignore]
    fn live_reply_is_filed_beside_its_original() {
        let (Ok(spec), Ok(smtp)) = (std::env::var("IMAP_LIVE"), std::env::var("SMTP_LIVE")) else {
            return;
        };
        let p: Vec<&str> = spec.splitn(4, ',').collect();
        let (smtp_host, smtp_port) = smtp.split_once(',').expect("host,port");
        let account = AccountConfig {
            email: p[2].into(),
            imap_host: p[0].into(),
            imap_port: p[1].parse().expect("port"),
            smtp_host: smtp_host.into(),
            smtp_port: smtp_port.parse().expect("port"),
            username: p[2].into(),
            password: p[3].into(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                smtp_starttls: false,
                imap_accept_invalid_certs: true,
                smtp_accept_invalid_certs: true,
                ..Default::default()
            }),
            reply_copies: crate::config::ReplyCopies::Original,
            ..sample_account()
        };
        let pid = std::process::id();
        let folder = format!("Project-{pid}");
        let parent = format!("hylki-468-{pid}@example.test");
        let original = format!(
            "From: Ada <ada@example.test>\r\nTo: {}\r\nSubject: Plans {pid}\r\nMessage-ID: <{parent}>\r\n\
             Date: Fri, 16 Oct 2026 10:00:00 +0000\r\n\r\nShall we plan?\r\n",
            account.email
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let cache = Cache::in_memory().unwrap();
            let mut sess = connect(&account).await.expect("connect");
            create_box(&mut sess, &folder).await.expect("create");
            append_to_sent(&mut sess, &folder, original.as_bytes())
                .await
                .expect("original filed");
            let folders =
                list_folders(1, &mut sess, &account.folder_roles, &account.hidden_folders)
                    .await
                    .expect("list");
            cache.save_folders(1, &folders);
            let folder_id = folders
                .iter()
                .find(|f| f.path == folder)
                .expect("listed")
                .id;
            let sent = folders
                .iter()
                .find(|f| f.kind == FolderKind::Sent)
                .expect("a Sent folder")
                .path
                .clone();
            let listed = load_messages(1, &mut sess, folder_id, &folder, true, Some(&cache))
                .await
                .expect("load");
            cache.upsert_messages(1, &folder, &listed);

            let reply = OutgoingMessage {
                to: account.email.clone(),
                subject: format!("Re: Plans {pid}"),
                body: "Yes.".into(),
                in_reply_to: parent.to_ascii_uppercase(),
                references: parent.clone(),
                ..sample_outgoing()
            };
            let raw = send_smtp(&account, &reply).await.expect("sent");
            let dest = super::reply_copy_path(&account, 1, Some(&cache), &reply.in_reply_to);
            assert_eq!(dest.as_deref(), Some(folder.as_str()));
            let took = file_sent_copy(&mut sess, dest.as_deref(), Some(&sent), &raw)
                .await
                .expect("filed");
            assert_eq!(took.as_deref(), Some(folder.as_str()));
            let after = load_messages(1, &mut sess, folder_id, &folder, true, None)
                .await
                .expect("load");
            let copy = after
                .iter()
                .find(|m| m.subject == reply.subject)
                .expect("the copy is beside its original");
            assert!(copy.references.contains(&parent), "{:?}", copy.references);
            println!(
                "{folder}: {:?}",
                after.iter().map(|m| &m.subject).collect::<Vec<_>>()
            );

            // A folder gone from the server since: Sent takes it.
            let gone = format!("Gone-{pid}");
            let took = file_sent_copy(&mut sess, Some(&gone), Some(&sent), &raw)
                .await
                .expect("filed");
            assert_eq!(took.as_deref(), Some(sent.as_str()));
            let sent_id = folders.iter().find(|f| f.path == sent).unwrap().id;
            let in_sent = load_messages(1, &mut sess, sent_id, &sent, true, None)
                .await
                .expect("load");
            let fallback = in_sent
                .iter()
                .find(|m| m.subject == reply.subject)
                .expect("the copy went to Sent");
            delete_draft(&mut sess, &sent, fallback.uid)
                .await
                .expect("clean up Sent");
            // The reply went to the account itself: take it out of the Inbox.
            for _ in 0..10 {
                let inbox = load_messages(1, &mut sess, 1, "INBOX", true, None)
                    .await
                    .expect("load");
                if let Some(m) = inbox.iter().find(|m| m.subject == reply.subject) {
                    delete_draft(&mut sess, "INBOX", m.uid)
                        .await
                        .expect("clean up the Inbox");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let _ = sess.close().await;
            delete_box(&mut sess, &folder).await.expect("clean up");
        });
    }

    #[test]
    #[ignore]
    fn live_idle_catches_mail_that_landed_between_commands() {
        let Ok(spec) = std::env::var("IMAP_LIVE") else {
            return;
        };
        let p: Vec<&str> = spec.splitn(4, ',').collect();
        let account = AccountConfig {
            imap_host: p[0].into(),
            imap_port: p[1].parse().expect("port"),
            username: p[2].into(),
            password: p[3].into(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                imap_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..sample_account()
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut sess = connect(&account).await.expect("connect");
            load_messages(7, &mut sess, 1, "INBOX", true, None).await.expect("load");
            let subject = format!("hylki-336-{}", std::process::id());
            let raw = format!(
                "From: a@example.test\r\nTo: b@example.test\r\nSubject: {subject}\r\nMessage-ID: <{subject}@example.test>\r\n\r\nhello\r\n"
            );
            let mut other = connect(&account).await.expect("connect");
            append_to_sent(&mut other, "INBOX", raw.as_bytes()).await.expect("append");
            let (_tx, mut rx) = mpsc::unbounded_channel();
            let mut session = Some(sess);
            let events = std::cell::RefCell::new(Vec::new());
            let outcome = idle_wait(
                &mut session, &account, 7, 1, "INBOX", &mut rx, None, &mut true,
                &mut Default::default(), &mut Default::default(), &Default::default(),
                &|e| events.borrow_mut().push(e), 5,
            )
            .await;
            assert!(matches!(outcome, IdleOutcome::Refreshed), "the wait went quiet");
            let got = events.borrow().iter().any(|e| {
                matches!(e, WorkerEvent::Messages { messages, .. } if messages.iter().any(|m| m.subject == subject))
            });
            assert!(got, "the new message was not listed");
        });
    }

    #[ignore]
    fn live_rename_moves_subscriptions() {
        let Ok(spec) = std::env::var("IMAP_LIVE") else {
            return;
        };
        let p: Vec<&str> = spec.splitn(4, ',').collect();
        let account = AccountConfig {
            imap_host: p[0].into(),
            imap_port: p[1].parse().expect("port"),
            username: p[2].into(),
            password: p[3].into(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                imap_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..sample_account()
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut sess = connect(&account).await.expect("connect");
            let delim = session_delimiter(&mut sess).await;
            let (old, sub) = ("Hylki Rig".to_string(), format!("Hylki Rig{delim}Sub one"));
            let (new, new_sub) = (
                "Hylki Rig Moved".to_string(),
                format!("Hylki Rig Moved{delim}Sub one"),
            );
            for f in [&old, &sub] {
                create_box(&mut sess, f).await.expect("create");
                subscribe_box(&mut sess, f).await.expect("subscribe");
            }
            rename_folder(&mut sess, &old, &new).await.expect("rename");
            let subscribed: Vec<String> = sess
                .lsub(Some(""), Some("*"))
                .await
                .expect("lsub")
                .try_collect::<Vec<async_imap::types::Name>>()
                .await
                .expect("lsub")
                .iter()
                .map(|n| n.name().to_string())
                .collect();
            let _ = delete_box(&mut sess, &new_sub).await;
            let _ = delete_box(&mut sess, &new).await;
            println!("subscribed: {subscribed:?}");
            assert!(subscribed.contains(&new) && subscribed.contains(&new_sub));
            assert!(!subscribed.contains(&old) && !subscribed.contains(&sub));
        });
    }

    /// The hierarchy delimiter the server answers `LIST "" ""` with.
    async fn session_delimiter(sess: &mut ImapSession) -> String {
        sess.list(Some(""), Some("\"\""))
            .await
            .expect("list")
            .try_collect::<Vec<async_imap::types::Name>>()
            .await
            .expect("list")
            .first()
            .and_then(|n| n.delimiter().map(String::from))
            .unwrap_or_else(|| "/".into())
    }

    /// A message moved in from another account keeps its read and starred
    /// state as IMAP flags, and asks for none when it had neither (#265).
    #[test]
    fn imported_mail_keeps_its_flags() {
        assert_eq!(
            import_flags(true, true).as_deref(),
            Some("(\\Seen \\Flagged)")
        );
        assert_eq!(import_flags(true, false).as_deref(), Some("(\\Seen)"));
        assert_eq!(import_flags(false, true).as_deref(), Some("(\\Flagged)"));
        assert_eq!(import_flags(false, false), None);
    }

    /// Where the time goes when a message is opened (#259): every step
    /// `render_raw` takes, per message of a directory of saved .eml files.
    /// `HYLKI_OPEN_TIMING=<dir> cargo test --release --bin hylki
    /// worker::tests::open_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn open_timing() {
        use std::time::{Duration, Instant};
        let Ok(dir) = std::env::var("HYLKI_OPEN_TIMING") else {
            return;
        };
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "eml"))
            .collect();
        paths.sort();
        const STEPS: [&str; 7] = [
            "parse",
            "unsubscribe",
            "invite",
            "check",
            "pgp",
            "body",
            "attach",
        ];
        let mut total = [Duration::ZERO; 7];
        let mut whole: Vec<(Duration, [Duration; 7], String, usize)> = Vec::new();
        for p in &paths {
            let raw = std::fs::read(p).unwrap();
            let mut t = [Duration::ZERO; 7];
            let mut lap = |i: usize, f: &mut dyn FnMut()| {
                let at = Instant::now();
                f();
                t[i] = at.elapsed();
            };
            let mut parsed = None;
            lap(0, &mut || {
                parsed = mail_parser::MessageParser::default().parse(&raw[..])
            });
            if let Some(m) = parsed.as_ref() {
                lap(1, &mut || drop(crate::unsubscribe::detect(m)));
                lap(2, &mut || drop(crate::invite::detect(m)));
            }
            lap(3, &mut || drop(crate::verify::check_sender(&raw)));
            lap(4, &mut || drop(crate::pgp::detect(&raw)));
            lap(5, &mut || drop(extract_body(&raw)));
            lap(6, &mut || drop(extract_attachments(&raw)));
            for i in 0..7 {
                total[i] += t[i];
            }
            let sum: Duration = [t[3], t[4], t[5], t[6]].iter().sum();
            whole.push((
                sum,
                t,
                p.file_name().unwrap().to_string_lossy().into_owned(),
                raw.len(),
            ));
        }
        let n = paths.len().max(1) as u32;
        println!("{} messages", paths.len());
        for (i, step) in STEPS.iter().enumerate() {
            println!(
                "{step:>12}: total {:>9.1?}  mean {:>8.2?}",
                total[i],
                total[i] / n
            );
        }
        whole.sort_by(|a, b| b.0.cmp(&a.0));
        println!("slowest (render_raw ~ check+pgp+body+attach):");
        for (sum, t, name, len) in whole.iter().take(12) {
            println!(
                "{sum:>9.2?} {name} {len}B  parse {:.2?} unsub {:.2?} invite {:.2?} check {:.2?} pgp {:.2?} body {:.2?} attach {:.2?}",
                t[0], t[1], t[2], t[3], t[4], t[5], t[6]
            );
        }
    }

    fn flagged_message(uid: u32) -> Message {
        Message {
            id: uid,
            account_id: 7,
            folder_id: 1,
            uid,
            from_name: String::new(),
            from_addr: String::new(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: String::new(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 0,
            unread: true,
            starred: false,
            answered: false,
            forwarded: false,
            keywords: Vec::new(),
            has_attachment: true,
            message_id: String::new(),
            references: String::new(),
            importance: Default::default(),
            due: 0,
            labels: Vec::new(),
        }
    }

    /// A sent message marks what it answers in its own account only, the
    /// reply's original as replied and the forward's as forwarded (#454).
    #[test]
    fn a_sent_message_finds_what_it_answered() {
        let cache = Cache::in_memory().unwrap();
        let copy = |account_id: u32, uid: u32, id: &str| Message {
            account_id,
            message_id: id.into(),
            ..flagged_message(uid)
        };
        cache.upsert_messages(1, "INBOX", &[copy(1, 1, "a@x"), copy(1, 2, "b@x")]);
        cache.upsert_messages(1, "Archive", &[copy(1, 7, "a@x")]);
        cache.upsert_messages(2, "INBOX", &[copy(2, 1, "a@x")]);
        let mut found = answered_copies(Some(&cache), 1, "<A@x>", "");
        found.sort();
        assert_eq!(
            found,
            [
                ("Archive".to_string(), 7, false),
                ("INBOX".to_string(), 1, false)
            ]
        );
        assert_eq!(
            answered_copies(Some(&cache), 1, "", "b@x"),
            [("INBOX".to_string(), 2, true)]
        );
        assert!(answered_copies(Some(&cache), 1, "", "").is_empty());
        assert!(answered_copies(Some(&cache), 1, "gone@x", "").is_empty());
        assert!(answered_copies(None, 1, "a@x", "").is_empty());
    }

    /// A sync whose recent window came back without some previews must give
    /// the same list the cache served, or every quiet poll redraws the list
    /// (#330).
    #[test]
    fn merging_a_recent_window_keeps_the_cached_previews() {
        let cached: Vec<Message> = (1..=3)
            .map(|uid| Message {
                preview: format!("text {uid}"),
                ..flagged_message(uid)
            })
            .collect();
        let recent = vec![
            Message {
                unread: false,
                ..flagged_message(3)
            },
            Message {
                preview: "new text".into(),
                ..flagged_message(2)
            },
        ];
        let merged = merge_index(cached, recent);
        let previews: Vec<&str> = merged.iter().map(|m| m.preview.as_str()).collect();
        assert_eq!(previews, ["text 3", "new text", "text 1"]);
        assert!(!merged[0].unread, "the server's flags still win");
    }

    /// The summary fetch never carries Gmail labels (#374): a merge keeps
    /// the cached ones until the label read replaces them.
    #[test]
    fn merging_a_recent_window_keeps_the_cached_labels() {
        let cached: Vec<Message> = (1..=2)
            .map(|uid| Message {
                labels: vec![format!("Label {uid}")],
                ..flagged_message(uid)
            })
            .collect();
        let merged = merge_index(cached, vec![flagged_message(2), flagged_message(3)]);
        let labels: Vec<Vec<String>> = merged.iter().map(|m| m.labels.clone()).collect();
        assert_eq!(
            labels,
            [
                vec![],
                vec!["Label 2".to_string()],
                vec!["Label 1".to_string()]
            ]
        );
    }

    fn fetch_attrs(raw: &[u8]) -> Vec<async_imap::imap_proto::AttributeValue<'_>> {
        match async_imap::imap_proto::parser::parse_response(raw) {
            Ok((_, async_imap::imap_proto::Response::Fetch(_, attrs))) => attrs,
            other => panic!("not a FETCH reply: {other:?}"),
        }
    }

    /// What Gmail answers `X-GM-LABELS` with (#374): system labels and the
    /// folder's own label stay off the pills, the rest come through as the
    /// server spells them.
    #[test]
    fn gmail_labels_are_read_from_a_fetch_reply() {
        let attrs = fetch_attrs(
            b"* 3 FETCH (UID 7 X-GM-LABELS (\\Inbox \\Important \"My Label\" Work &AOk-t&AOk- \"Clients/Acme\" \\Starred))\r\n",
        );
        let (uid, labels) = gmail_labels_of(&attrs, "INBOX").expect("labels");
        assert_eq!(uid, 7);
        assert_eq!(labels, ["My Label", "Work", "&AOk-t&AOk-", "Clients/Acme"]);
        // Read in the Work folder, Work is the folder, not a label on top.
        let (_, labels) = gmail_labels_of(&attrs, "Work").expect("labels");
        assert_eq!(labels, ["My Label", "&AOk-t&AOk-", "Clients/Acme"]);
        // Nested labels keep their whole path; the folder match is exact.
        let (_, labels) = gmail_labels_of(&attrs, "Clients").expect("labels");
        assert!(labels.contains(&"Clients/Acme".to_string()));

        let none = fetch_attrs(b"* 4 FETCH (UID 8 X-GM-LABELS ())\r\n");
        assert_eq!(gmail_labels_of(&none, "INBOX"), Some((8, Vec::new())));
        let system_only = fetch_attrs(b"* 4 FETCH (UID 8 X-GM-LABELS (\\Sent \\Draft))\r\n");
        assert_eq!(
            gmail_labels_of(&system_only, "INBOX"),
            Some((8, Vec::new()))
        );
        // A flag change Gmail volunteers says nothing about labels.
        let flags = fetch_attrs(b"* 5 FETCH (UID 9 FLAGS (\\Seen))\r\n");
        assert_eq!(gmail_labels_of(&flags, "INBOX"), None);
    }

    /// The loop behind the 2.6 GB report: a message the attachment prefetch
    /// popped but could not finish (OpenPGP, or a read that would not parse)
    /// was queued again by the resync the prefetch triggers, and fetched
    /// again, forever. One attempt per session is all it gets now.
    #[test]
    fn attachment_prefetch_never_requeues_a_message_it_already_tried() {
        let msgs = vec![flagged_message(2383), flagged_message(2372)];
        let mut queue = std::collections::VecDeque::new();
        queue_attachment_prefetch(&mut queue, "INBOX", &msgs, None, 7);
        assert_eq!(
            queue.len(),
            2,
            "both flagged rows are queued the first time"
        );

        // The prefetch pops 2383 and notes the attempt, as run_one_prefetch does
        // before anything can go wrong with the read.
        queue.pop_front();
        note_attachment_prefetch_tried(7, "INBOX", 2383);
        assert!(attachment_prefetch_tried(7, "INBOX", 2383));

        // The resync that follows sees the same rows: 2383 must stay out.
        queue_attachment_prefetch(&mut queue, "INBOX", &msgs, None, 7);
        assert_eq!(
            queue.iter().map(|(_, u)| *u).collect::<Vec<_>>(),
            vec![2372]
        );

        // Another account's row with the same uid and path is its own attempt.
        assert!(!attachment_prefetch_tried(8, "INBOX", 2383));
    }

    pub(super) fn sample_outgoing() -> OutgoingMessage {
        OutgoingMessage {
            from_account_id: 1,
            from_alias: None,
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            reply_to: String::new(),
            subject: "Subject".into(),
            body: "Body".into(),
            html: String::new(),
            attachments: Vec::new(),
            in_reply_to: String::new(),
            references: String::new(),
            forwards: String::new(),
            draft_origin: None,
            outbox_origin: None,
            sign: false,
            encrypt: false,
            send_at: None,
            calendar: None,
        }
    }

    /// #336: mail that lands while the connection is busy shows only as a
    /// moved UIDNEXT on the next SELECT.
    #[test]
    fn a_moved_uid_next_is_missed_mail() {
        let path = "INBOX-336-test";
        assert!(!missed_new_mail(99, path, Some(10)), "nothing loaded yet");
        note_synced_uid_next(99, path, Some(10));
        assert!(!missed_new_mail(99, path, Some(10)));
        assert!(missed_new_mail(99, path, Some(12)));
        assert!(!missed_new_mail(99, path, None));
        assert!(!missed_new_mail(98, path, Some(12)), "another account");
    }

    /// #340: a send that fails on a file gone since it was attached cannot
    /// be queued either, so the message comes back for a composer instead
    /// of being dropped.
    #[test]
    fn an_unqueueable_failed_send_comes_back() {
        let cache = Cache::in_memory().expect("cache");
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.to = "ann@example.com".into();
        msg.attachments = vec!["/nonexistent/hylki-test/moved.pdf".into()];
        let events = std::cell::RefCell::new(Vec::new());
        send_failed(
            Some(&cache),
            1,
            &account,
            &msg,
            None,
            "could not read the attachment",
            false,
            &|e| events.borrow_mut().push(e),
        );
        let events = events.into_inner();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, WorkerEvent::Unsent(m) if m.subject == "Subject")),
            "{events:?}"
        );
        // A message that does build is queued and not handed back.
        msg.attachments.clear();
        let events = std::cell::RefCell::new(Vec::new());
        send_failed(
            Some(&cache),
            1,
            &account,
            &msg,
            None,
            "offline",
            false,
            &|e| events.borrow_mut().push(e),
        );
        assert!(!events
            .into_inner()
            .iter()
            .any(|e| matches!(e, WorkerEvent::Unsent(_))));
    }

    /// #442: a message sent with no network waits in the Outbox with a
    /// notice, not an error: nothing failed.
    #[test]
    fn an_offline_send_waits_quietly() {
        let cache = Cache::in_memory().expect("cache");
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.to = "ann@example.com".into();
        let events = std::cell::RefCell::new(Vec::new());
        send_failed(
            Some(&cache),
            1,
            &account,
            &msg,
            None,
            &offline_text(),
            false,
            &|e| events.borrow_mut().push(e),
        );
        let events = events.into_inner();
        assert!(
            events.iter().any(|e| matches!(e, WorkerEvent::Notice(_))),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, WorkerEvent::Error { .. })),
            "{events:?}"
        );
        assert_eq!(cache.outbox_items(1).len(), 1);
    }

    /// #368: a recipient the server refuses comes back to the composer and
    /// is not queued, where it would fail again on every retry.
    #[test]
    fn a_refused_send_comes_back_unqueued() {
        let cache = Cache::in_memory().expect("cache");
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.to = "ann@example.com".into();
        let events = std::cell::RefCell::new(Vec::new());
        send_failed(
            Some(&cache),
            1,
            &account,
            &msg,
            None,
            "550 no such user",
            true,
            &|e| events.borrow_mut().push(e),
        );
        assert!(events
            .into_inner()
            .iter()
            .any(|e| matches!(e, WorkerEvent::Unsent(_))));
        assert!(cache.outbox_items(1).is_empty());
    }

    /// #368: invisible characters pasted along with an address are dropped
    /// from the address, and kept in a name.
    #[test]
    fn pasted_addresses_lose_invisible_characters() {
        assert_eq!(
            parse_recipients(
                "\u{200b}ann\u{200b}@example.com\u{200e}, Bob <\u{feff}bob@example.com\u{a0}>"
            ),
            vec![
                (String::new(), "ann@example.com".to_string()),
                ("Bob".to_string(), "bob@example.com".to_string())
            ],
        );
        let persian = "\u{0645}\u{06cc}\u{200c}\u{0631}\u{0648}\u{062f}";
        assert_eq!(
            parse_recipients(&format!("{persian} <a@b.com>"))[0].0,
            persian
        );
        assert_eq!(
            clean_recipient_field("ann\u{200b}@example.com, Bob <bob@example.com>").as_deref(),
            Some("ann@example.com, Bob <bob@example.com>")
        );
        assert_eq!(clean_recipient_field("ann@example.com"), None);
        assert_eq!(
            invalid_recipient("ann@example.com, Bob <bob@example.com>"),
            None
        );
        assert_eq!(
            invalid_recipient("ann@example.com, bob@"),
            Some("bob@".to_string())
        );
        assert_eq!(
            invalid_recipient("ann example.com"),
            Some("ann example.com".to_string())
        );
    }

    /// A draft saved before any address is typed: the bytes build without a
    /// To header, and the send path still refuses the same message.
    #[test]
    fn recipient_less_draft_builds_but_send_does_not() {
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.body = "half-written".into();
        let raw = build_draft(&account, &msg)
            .expect("draft builds")
            .formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("\r\nTo:"), "no To header: {text}");
        assert!(text.contains("half-written"), "{text}");
        let err = build_email(&account, &msg)
            .expect_err("send needs a recipient")
            .to_string();
        assert!(err.contains("missing destination"), "{err}");
        // With a recipient the draft is built the ordinary way.
        msg.to = "Peer <peer@hylki.invalid>".into();
        let raw = build_draft(&account, &msg).expect("builds").formatted();
        assert!(String::from_utf8_lossy(&raw).contains("peer@hylki.invalid"));
    }

    /// OpenPGP sending (#133), end to end against a real gpg in a throwaway
    /// keyring: a signed message comes out as PGP/MIME multipart/signed whose
    /// signature the reader's own path verifies over lettre's exact bytes; an
    /// encrypted one as multipart/encrypted that decrypts back to the body.
    /// Skipped where gpg is missing.
    #[test]
    fn build_email_signs_and_encrypts_with_pgp_mime() {
        if !crate::pgp::available() {
            eprintln!("gpg not installed; skipping");
            return;
        }
        let home = std::env::temp_dir().join(format!("hylki-gpg-send-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        // `Gpg::system()` (what build_email uses) honours this override.
        std::env::set_var("HYLKI_GNUPGHOME", &home);
        let gpg = crate::pgp::Gpg::system();
        let mut account = sample_account();
        account.email = "sender@hylki.invalid".into();
        let fpr = crate::pgp::generate_key(&gpg, "Sender", "sender@hylki.invalid", "never", "")
            .expect("own key");
        crate::pgp::generate_key(&gpg, "Peer", "peer@hylki.invalid", "never", "")
            .expect("peer key");

        let mut msg = sample_outgoing();
        msg.to = "Peer <peer@hylki.invalid>".into();
        msg.body = "signed body text".into();
        msg.sign = true;
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/signed"), "{text}");
        assert!(text.contains("micalg=\"pgp-sha"), "{text}");
        assert!(!has_bare_lf(&raw), "{text}");
        let u = crate::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(
            matches!(u.status.signature, crate::models::PgpSignature::Good { .. }),
            "{:?}",
            u.status
        );
        assert!(!u.status.encrypted);
        let inner = String::from_utf8_lossy(&u.inner.unwrap()).to_string();
        assert!(inner.contains("signed body text"), "{inner}");

        // With an attachment the signed entity is the whole mixed multipart.
        let path = home.join("note.txt");
        std::fs::write(&path, b"attached").unwrap();
        msg.attachments = vec![path.to_string_lossy().to_string()];
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let u = crate::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(
            matches!(u.status.signature, crate::models::PgpSignature::Good { .. }),
            "{:?}",
            u.status
        );
        assert!(!extract_attachments(&u.inner.unwrap()).is_empty());

        // Encrypted and signed: readable by the peer and by the sender.
        msg.attachments.clear();
        msg.encrypt = true;
        msg.body = "secret body text".into();
        msg.subject = "Secret plans".into();
        msg.bcc = "Hidden <sender@hylki.invalid>".into();
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/encrypted"), "{text}");
        assert!(
            !text.contains("secret body text"),
            "plaintext must not appear: {text}"
        );
        // Protected headers (#493): the subject is inside, "..." outside.
        assert!(
            !text.contains("Secret plans"),
            "the subject must not appear: {text}"
        );
        assert!(text.contains("Subject: ...\r\n"), "{text}");
        assert!(!has_bare_lf(&raw), "{text}");
        let u = crate::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(u.status.encrypted && u.status.decrypted, "{:?}", u.status);
        assert!(
            matches!(u.status.signature, crate::models::PgpSignature::Good { .. }),
            "{:?}",
            u.status
        );
        assert_eq!(u.status.subject.as_deref(), Some("Secret plans"));
        let inner = String::from_utf8_lossy(&u.inner.unwrap()).to_string();
        assert!(inner.contains("secret body text"), "{inner}");
        assert!(inner.contains("protected-headers=\"v1\""), "{inner}");
        assert!(
            inner.contains("peer@hylki.invalid"),
            "the To travels inside: {inner}"
        );
        assert!(
            !inner.contains("Hidden"),
            "Bcc must stay out of what every recipient reads: {inner}"
        );
        assert_eq!(
            extract_body(inner.as_bytes())
                .matches("secret body text")
                .count(),
            1
        );
        msg.bcc.clear();
        // An account that leaves the subject readable (#493) sends it outside.
        account.plain_subject = true;
        let raw = build_email(&account, &msg).expect("builds").formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("Subject: Secret plans\r\n"), "{text}");
        assert!(
            !text.contains("secret body text"),
            "plaintext must not appear: {text}"
        );
        let u = crate::pgp::unwrap_message(&raw, &gpg).expect("recognised");
        assert!(
            u.status.decrypted && u.status.subject.is_none(),
            "{:?}",
            u.status
        );
        account.plain_subject = false;

        // A recipient without a key is refused by name; a From without a key too.
        msg.cc = "nobody@hylki.invalid".into();
        let err = build_email(&account, &msg)
            .expect_err("no key for nobody")
            .to_string();
        assert!(err.contains("nobody@hylki.invalid"), "{err}");
        msg.cc.clear();
        account.email = "keyless@hylki.invalid".into();
        let err = build_email(&account, &msg)
            .expect_err("no own key")
            .to_string();
        assert!(err.contains("keyless@hylki.invalid"), "{err}");
        // The account's chosen key overrides the address match.
        account.pgp_key = Some(fpr);
        build_email(&account, &msg).expect("chosen key signs");

        // A draft of the same message stays plain.
        let plain = OutgoingMessage {
            sign: false,
            encrypt: false,
            ..msg.clone()
        };
        let raw = build_email(&account, &plain).expect("builds").formatted();
        assert!(String::from_utf8_lossy(&raw).contains("secret body text"));

        std::env::remove_var("HYLKI_GNUPGHOME");
        let _ = std::process::Command::new("gpgconf")
            .env("GNUPGHOME", &home)
            .args(["--kill", "all"])
            .status();
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A reply with no In-Reply-To/References is a new conversation to every
    /// client that receives it — including Hylki's own threading.
    #[test]
    fn reply_to_header_reaches_the_wire() {
        let mut msg = sample_outgoing();
        msg.to = "someone@example.com".into();
        msg.reply_to = "list@example.org, Other <other@example.net>".into();
        let email = build_email(&sample_account(), &msg).expect("builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("Reply-To:"), "Reply-To must be present: {raw}");
        assert!(raw.contains("list@example.org"), "{raw}");
        assert!(raw.contains("other@example.net"), "{raw}");
    }

    #[test]
    fn a_reply_carries_its_threading_headers() {
        let mut msg = sample_outgoing();
        msg.to = "someone@example.com".into();
        msg.in_reply_to = "parent@example.com".into();
        msg.references = "root@example.com parent@example.com".into();
        let email = build_email(&sample_account(), &msg).expect("builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(
            raw.contains("In-Reply-To: <parent@example.com>"),
            "In-Reply-To must be present and bracketed: {raw}"
        );
        assert!(
            raw.contains("References: <root@example.com> <parent@example.com>"),
            "References must carry the whole chain, bracketed: {raw}"
        );
    }

    /// A message that starts a conversation must not carry empty headers.
    /// Without a Message-ID of our own the server assigns one on the way out, so
    /// the copy filed in Sent has none — and nothing that replies to it can be
    /// linked back to it. It must also be in the sender's domain, not this
    /// machine's hostname.
    #[test]
    fn outgoing_mail_carries_a_message_id_in_the_senders_domain() {
        let account = AccountConfig {
            email: "me@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "you@example.org".into(),
            ..sample_outgoing()
        };
        let raw =
            String::from_utf8_lossy(&build_email(&account, &msg).expect("builds").formatted())
                .to_string();
        let line = raw
            .lines()
            .find(|l| l.starts_with("Message-ID:"))
            .expect(&format!("a Message-ID is set: {raw}"));
        assert!(
            line.contains("@example.com>"),
            "in the sender's domain: {line}"
        );
    }

    #[test]
    fn a_new_message_carries_no_threading_headers() {
        let msg = OutgoingMessage {
            to: "someone@example.com".into(),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(!raw.contains("In-Reply-To"), "{raw}");
        assert!(!raw.contains("References"), "{raw}");
    }

    #[test]
    fn mailbox_quotes_display_names_that_are_not_atoms() {
        // Each of these fails if the mailbox is built by formatting
        // "Name <addr>" and parsing the result back.
        for name in ["Alfonso Lizárraga", "Martin, Jason", "Dr. X", "a@b.com"] {
            let mb = mailbox(name, "a@b.com").expect("mailbox builds");
            assert_eq!(mb.email.to_string(), "a@b.com");
        }
    }

    #[test]
    fn mailbox_drops_a_name_that_merely_repeats_the_address() {
        // An import with no separate display name sets both to the address;
        // "a@b.com <a@b.com>" is noise, so the header carries the address alone.
        let mb = mailbox("A@B.com", "a@b.com").expect("mailbox builds");
        assert!(mb.name.is_none());
        assert_eq!(mb.to_string(), "a@b.com");
    }

    fn queued(from: &str, rcpts: &[&str]) -> crate::models::OutboxItem {
        crate::models::OutboxItem {
            id: 1,
            account_id: 1,
            from_addr: from.into(),
            rcpts: rcpts.iter().map(|s| s.to_string()).collect(),
            recipients: String::new(),
            subject: "Notes".into(),
            preview: String::new(),
            raw: b"From: me\r\n\r\nbody".to_vec(),
            sent_path: None,
            queued_at: 0,
            attempts: 1,
            last_error: String::new(),
            send_at: None,
            forwards: String::new(),
            status: String::new(),
        }
    }

    #[test]
    fn a_queued_message_can_be_taken_apart_for_editing() {
        // Round trip: compose → build → (failed send) → edit. What comes back has
        // to be what went in, including the Bcc that lettre strips from the bytes.
        let account = AccountConfig {
            name: "Me".into(),
            email: "me@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "Ada Lovelace <ada@example.com>".into(),
            cc: "carol@example.com".into(),
            bcc: "hidden@example.com".into(),
            subject: "Quarterly numbers".into(),
            body: "First line\nSecond line".into(),
            ..sample_outgoing()
        };
        let email = build_email(&account, &msg).expect("builds");
        let rcpts: Vec<String> = email
            .envelope()
            .to()
            .iter()
            .map(|a| a.to_string())
            .collect();
        let raw = email.formatted();

        // lettre drops Bcc from the wire format, so the envelope is the only
        // record of it — which is exactly why it is stored alongside.
        assert!(!String::from_utf8_lossy(&raw).contains("hidden@example.com"));

        let editable = editable_from_raw(&raw, &rcpts);
        assert_eq!(editable.to, "Ada Lovelace <ada@example.com>");
        assert_eq!(editable.cc, "carol@example.com");
        assert_eq!(editable.bcc, "hidden@example.com");
        assert_eq!(editable.subject, "Quarterly numbers");
        // A plain-text body is escaped into HTML for the editor, keeping its lines.
        assert!(
            editable.body_html.contains("First line"),
            "{}",
            editable.body_html
        );
        assert!(
            editable.body_html.contains("Second line"),
            "{}",
            editable.body_html
        );
        assert!(editable.attachments.is_empty());
    }

    /// The copy filed in Sent names the Bcc recipients the wire copy leaves
    /// out (#476), whether it was sent at once or waited in the Outbox.
    #[test]
    fn a_sent_copy_keeps_its_bcc() {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            cc: "carol@example.com".into(),
            bcc: "Hidden Person <hidden@example.com>, other@example.com".into(),
            subject: "Quarterly numbers".into(),
            body: "Hello".into(),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("builds");
        let rcpts: Vec<String> = email
            .envelope()
            .to()
            .iter()
            .map(|a| a.to_string())
            .collect();
        let raw = email.formatted();
        assert!(!String::from_utf8_lossy(&raw).contains("hidden@example.com"));

        let bcc_of = |bytes: &[u8]| summary_from_raw(1, 1, 1, bytes).bcc;
        let copy = with_bcc(&raw, &msg.bcc);
        assert_eq!(bcc_of(&copy), "hidden@example.com, other@example.com");
        // Nothing else about the message changes: the bytes that went out
        // follow the one header put in front of them.
        assert!(copy.ends_with(&raw));
        let parsed = mail_parser::MessageParser::default()
            .parse(&copy)
            .expect("parses");
        assert_eq!(
            parsed.bcc().and_then(|a| a.first()).and_then(|a| a.name()),
            Some("Hidden Person")
        );
        assert_eq!(parsed.subject(), Some("Quarterly numbers"));

        let queued = queued_with_bcc(&raw, &rcpts);
        assert_eq!(bcc_of(&queued), "hidden@example.com, other@example.com");

        // No Bcc, no change.
        assert_eq!(with_bcc(&raw, ""), raw);
        let to_only: Vec<String> = vec!["ada@example.com".into(), "carol@example.com".into()];
        assert_eq!(queued_with_bcc(&raw, &to_only), raw);
    }

    #[test]
    fn editing_a_queued_message_keeps_its_attachments_and_html() {
        let dir = std::env::temp_dir().join(format!("hylki-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("report.pdf");
        std::fs::write(&file, [0x25, 0x50, 0x44, 0x46, 0x00, 0xff]).expect("write");

        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            subject: "With a file".into(),
            body: "plain fallback".into(),
            html: "<p>rich <b>body</b></p>".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("builds");
        let rcpts: Vec<String> = email
            .envelope()
            .to()
            .iter()
            .map(|a| a.to_string())
            .collect();
        let editable = editable_from_raw(&email.formatted(), &rcpts);

        // The HTML alternative is what the composer edits, not the plain fallback.
        assert!(
            editable.body_html.contains("rich <b>body</b>"),
            "{}",
            editable.body_html
        );
        assert_eq!(editable.attachments.len(), 1);
        assert_eq!(editable.attachments[0].name, "report.pdf");
        assert_eq!(
            editable.attachments[0].data,
            [0x25, 0x50, 0x44, 0x46, 0x00, 0xff]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// #349, #350: a saved draft comes back as it was written: the alias it
    /// was from, its Bcc, its place in the thread, a picture in the text and
    /// a file beside it.
    #[test]
    fn a_saved_draft_comes_back_whole() {
        let dir = std::env::temp_dir().join(format!("hylki-draft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"notes").expect("write");

        let msg = OutgoingMessage {
            from_alias: Some("Work <work@example.com>".into()),
            to: "ada@example.com".into(),
            bcc: "hidden@example.com".into(),
            subject: "Re: Plans".into(),
            html: "<p>see <img src=\"data:image/png;base64,iVBORw0KGgo=\"></p>".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            in_reply_to: "Orig@Example.com".into(),
            references: "first@example.com Orig@Example.com".into(),
            ..sample_outgoing()
        };
        let raw = build_draft(&sample_account(), &msg)
            .expect("builds")
            .formatted();
        let editable = editable_from_raw(&raw, &[]);

        assert_eq!(editable.from, "Work <work@example.com>");
        assert_eq!(editable.bcc, "hidden@example.com");
        assert_eq!(editable.in_reply_to, "Orig@Example.com");
        assert_eq!(editable.references, "first@example.com Orig@Example.com");
        assert!(
            editable.body_html.contains("data:image/png;base64,"),
            "{}",
            editable.body_html
        );
        assert!(
            !editable.body_html.contains("cid:"),
            "{}",
            editable.body_html
        );
        assert_eq!(
            editable.attachments.len(),
            1,
            "the picture is in the text, not a file"
        );
        assert_eq!(editable.attachments[0].name, "notes.txt");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preview_skips_the_link_a_marketing_mail_opens_with() {
        // Cloudways' text/plain alternative, verbatim in shape: the header logo's
        // link renders as a bare URL above the greeting, and that URL was the
        // whole preview.
        let part = concat!(
            "--751d69df691580924654d5924db69f0ae9507550b8b16fd8b2d83daf1d3f\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "( https://www.cloudways.com/en?utm_campaign=3DAPI+Keys+being+replaced+by+Ac=\r\n",
            "cess+Tokens&utm_medium=3Demail_action )\r\n",
            "\r\n",
            "Hi Camp crystal clear,\r\n",
            "\r\n",
            "We have a quick update on how you connect your AI agent to Cloudways.\r\n",
            "--751d69df691580924654d5924db69f0ae9507550b8b16fd8b2d83daf1d3f--\r\n",
        );
        let preview = preview_from_part(part.as_bytes(), None, None);
        assert!(preview.starts_with("Hi Camp crystal clear,"), "{preview}");
        assert!(!preview.contains("utm_campaign"), "{preview}");
    }

    #[test]
    fn a_zip_as_the_whole_body_yields_no_preview() {
        // #241: a DMARC report is a single application/zip part. Base64 or
        // not, its bytes are a file, not a snippet.
        let zip = b"PK\x03\x04\x14\x00\x00\x00\x08\x00\x8a\x1eGoogle report\x00\x00\x12\x03\x00PK\x01\x02";
        let b64 = crate::oauth::base64_encode(zip);
        assert_eq!(preview_from_part(b64.as_bytes(), None, Some("base64")), "");
        assert_eq!(preview_from_part(b64.as_bytes(), None, None), "");
        assert_eq!(preview_from_part(zip, None, Some("binary")), "");
        // Ordinary text with a tab and line breaks is not "binary".
        assert_eq!(
            preview_from_part(b"Hello\tthere\r\nSecond line", None, None),
            "Hello there Second line"
        );
        assert!(!text_like("application/zip"));
        assert!(text_like("multipart/mixed; boundary=x"));
    }

    #[test]
    fn preview_drops_rendered_links_but_keeps_their_text() {
        assert_eq!(
            preview_from_part(
                b"Generate your Access Token ( https://example.com/api?a=1 )Got questions?",
                None,
                None
            ),
            "Generate your Access Token Got questions?"
        );
        // Brackets that are not a link are left exactly as written.
        assert_eq!(
            preview_from_part(b"Lunch (the usual place) at noon", None, None),
            "Lunch (the usual place) at noon"
        );
        // A message that is nothing but a link still shows it: better than a
        // blank row.
        assert_eq!(
            preview_from_part(b"https://example.com/only", None, None),
            "https://example.com/only"
        );
    }

    #[test]
    fn preview_descends_into_a_nested_multipart() {
        // ProtonMail sends mixed(alternative(plain, html), attachment), so
        // `BODY[1]` is the whole alternative container rather than any text — and
        // the preview showed its boundary line ("--b2=_cipkIEq1…") instead of the
        // message. The bytes below are that fetch, verbatim in shape.
        let part = concat!(
            "--b2=_cipkIEq1WkCIMIbpGDVyYc52x8YElrqa8uRU7GKJ8\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "SGVsbG8sIEkgcmVjZW50bHkgaW5zdGFsbGVkIFZpcmVvIGFmdGVyIHJlYWRpbmcgYWJvdXQgaXQg\r\n",
            "b24gdGhlIG9tZyF1YnVudHUgd2Vic2l0ZS4=\r\n",
            "\r\n",
            "--b2=_cipkIEq1WkCIMIbpGDVyYc52x8YElrqa8uRU7GKJ8\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "PGRpdj5IZWxsbzwvZGl2Pg==\r\n",
            "--b2=_cipkIEq1WkCIMIbpGDVyYc52x8YElrqa8uRU7GKJ8--\r\n",
        );
        assert_eq!(
            preview_from_part(part.as_bytes(), None, None),
            "Hello, I recently installed Vireo after reading about it on the omg!ubuntu website."
        );
        // Nothing of the MIME machinery reaches the list.
        assert!(!preview_from_part(part.as_bytes(), None, None).contains("--b2="));
    }

    #[test]
    fn preview_prefers_plain_text_but_settles_for_html() {
        let html_only = concat!(
            "--x\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "\r\n",
            "<div>Only markup here</div>\r\n",
            "--x--\r\n",
        );
        assert_eq!(
            preview_from_part(html_only.as_bytes(), None, None),
            "Only markup here"
        );
        // Two levels of nesting — mixed(alternative(...)) — are followed.
        let nested = concat!(
            "--outer\r\n",
            "Content-Type: multipart/alternative; boundary=\"inner\"\r\n",
            "\r\n",
            "--inner\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "Buried but readable\r\n",
            "--inner--\r\n",
            "--outer--\r\n",
        );
        assert_eq!(
            preview_from_part(nested.as_bytes(), None, None),
            "Buried but readable"
        );
    }

    #[test]
    fn a_message_that_merely_starts_with_dashes_is_not_a_multipart() {
        // A signature separator, or a line of dashes, must still read as text.
        assert_eq!(
            preview_from_part(b"-- \r\nRegards,\r\nSteve", None, None),
            "-- Regards, Steve"
        );
        assert_eq!(
            preview_from_part(b"--\r\nsigned off", None, None),
            "-- signed off"
        );
    }

    /// A PGP/MIME message's first part is the "Version: 1" stub, and an
    /// inline one begins with the armour; the list says what it is instead.
    #[test]
    fn preview_names_an_encrypted_message() {
        let marker = crate::models::ENCRYPTED_PREVIEW;
        assert_eq!(preview_from_part(b"Version: 1\r\n", None, None), marker);
        assert_eq!(preview_from_part(b"version: 1", None, None), marker);
        assert_eq!(
            preview_from_part(
                b"-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA3\r\n-----END PGP MESSAGE-----\r\n",
                None,
                None
            ),
            marker
        );
        assert!(crate::models::preview_is_encrypted(marker));
        assert_eq!(crate::models::preview_display(marker), "Encrypted message");
        assert_eq!(crate::models::preview_display("Hello"), "Hello");
        assert_eq!(pgp_preview("Version 1 of the plan is attached"), None);
        assert_eq!(preview_from_part(b"Hello there", None, None), "Hello there");
    }

    #[test]
    fn preview_reads_plain_text() {
        let text = b"Here are the figures we discussed.\r\n\r\nLet me know if the Q3 line looks wrong.\r\n";
        assert_eq!(
            preview_from_part(text, None, None),
            "Here are the figures we discussed. Let me know if the Q3 line looks wrong."
        );
        assert_eq!(preview_from_part(b"", None, None), "");
    }

    #[test]
    fn a_queued_message_takes_the_sent_copy_folder_it_finds_on_the_way_out() {
        use crate::models::{Folder, FolderKind};
        let folder = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        let folders = vec![
            folder(1, "INBOX", FolderKind::Inbox),
            folder(2, "Sent", FolderKind::Sent),
        ];
        let account = |chosen: Option<&str>| AccountConfig {
            sent_copy_path: chosen.map(str::to_string),
            ..sample_account()
        };
        // The account says Inbox now; the message was queued for Sent. It goes
        // where the account says today, not where it said last week.
        assert_eq!(
            pick_queued_sent_copy(&account(Some("INBOX")), &folders, Some("Sent")).as_deref(),
            Some("INBOX"),
        );
        // Turned back off since queuing: back to the Sent folder.
        assert_eq!(
            pick_queued_sent_copy(&account(None), &folders, Some("INBOX")).as_deref(),
            Some("Sent"),
        );
        // Chosen folder gone from the server: Sent rather than a dead name.
        assert_eq!(
            pick_queued_sent_copy(&account(Some("Gone")), &folders, Some("Gone")).as_deref(),
            Some("Sent"),
        );
        // No folder list to read (offline, no cache): the queued path stands,
        // so a send never loses its copy over this.
        assert_eq!(
            pick_queued_sent_copy(&account(Some("INBOX")), &[], Some("Sent")).as_deref(),
            Some("Sent"),
        );
        // The server files its own: nothing of ours goes anywhere, whatever
        // was queued and whatever folder was picked.
        let server_saves = AccountConfig {
            sent_copy_path: Some("INBOX".into()),
            server_saves_sent: true,
            ..sample_account()
        };
        assert_eq!(
            pick_queued_sent_copy(&server_saves, &folders, Some("Sent")),
            None
        );
        assert_eq!(
            pick_queued_sent_copy(&server_saves, &[], Some("Sent")),
            None
        );
    }

    #[test]
    fn a_reply_is_filed_beside_the_message_it_answers() {
        use crate::config::ReplyCopies;
        let folder = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        let folders = vec![
            folder(1, "INBOX", FolderKind::Inbox),
            folder(2, "Sent", FolderKind::Sent),
            folder(3, "Projects/Hylki", FolderKind::Custom),
            folder(4, "[Gmail]/All Mail", FolderKind::Archive),
            folder(5, "Trash", FolderKind::Trash),
        ];
        let held = |paths: &[&str]| paths.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        let pick = |choice, paths: &[&str]| pick_reply_folder(choice, &folders, &held(paths));
        assert_eq!(
            pick(ReplyCopies::Original, &["Projects/Hylki"]).as_deref(),
            Some("Projects/Hylki")
        );
        assert_eq!(
            pick(ReplyCopies::Original, &["INBOX"]).as_deref(),
            Some("INBOX")
        );
        assert_eq!(pick(ReplyCopies::OriginalNotInbox, &["INBOX"]), None);
        // Gmail's copies under a label, All Mail and the Inbox: the label.
        assert_eq!(
            pick(
                ReplyCopies::OriginalNotInbox,
                &["INBOX", "[Gmail]/All Mail", "Projects/Hylki"]
            )
            .as_deref(),
            Some("Projects/Hylki"),
        );
        assert_eq!(
            pick(ReplyCopies::Original, &["INBOX", "[Gmail]/All Mail"]).as_deref(),
            Some("[Gmail]/All Mail")
        );
        // Binned, already in Sent, or in a folder deleted since: with sent mail.
        assert_eq!(pick(ReplyCopies::Original, &["Trash"]), None);
        assert_eq!(pick(ReplyCopies::Original, &["Sent"]), None);
        assert_eq!(pick(ReplyCopies::Original, &["Gone"]), None);
        assert_eq!(pick(ReplyCopies::Original, &[]), None);

        // Off, or the server files its own copy: nothing is looked up.
        let account = |reply_copies, server_saves_sent| AccountConfig {
            reply_copies,
            server_saves_sent,
            ..sample_account()
        };
        let cache = Cache::in_memory().unwrap();
        cache.save_folders(1, &folders);
        assert_eq!(
            super::reply_copy_path(
                &account(ReplyCopies::WithSent, false),
                1,
                Some(&cache),
                "a@b"
            ),
            None
        );
        assert_eq!(
            super::reply_copy_path(
                &account(ReplyCopies::Original, true),
                1,
                Some(&cache),
                "a@b"
            ),
            None
        );
        assert_eq!(
            super::reply_copy_path(&account(ReplyCopies::Original, false), 1, Some(&cache), ""),
            None
        );
        // On: the original is found by its Message-ID, spelled as the
        // reply spells it, and only among this account's folders.
        let original = Message {
            id: 1,
            account_id: 1,
            folder_id: 3,
            uid: 7,
            from_name: "Ada".into(),
            from_addr: "ada@example.com".into(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Plans".into(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 1,
            unread: false,
            starred: false,
            answered: false,
            forwarded: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: "parent@example.com".into(),
            references: String::new(),
            importance: Default::default(),
            due: 0,
            labels: Vec::new(),
        };
        cache.upsert_messages(1, "Projects/Hylki", std::slice::from_ref(&original));
        let on = account(ReplyCopies::Original, false);
        assert_eq!(
            super::reply_copy_path(&on, 1, Some(&cache), "<Parent@Example.com>").as_deref(),
            Some("Projects/Hylki"),
        );
        assert_eq!(
            super::reply_copy_path(&on, 2, Some(&cache), "parent@example.com"),
            None
        );
        assert_eq!(
            super::reply_copy_path(&on, 1, Some(&cache), "other@example.com"),
            None
        );
    }

    #[test]
    fn a_queued_reply_names_what_it_answers() {
        let raw =
            b"From: a@example.com\r\nTo: b@example.com\r\nIn-Reply-To: <Parent@Example.com>\r\n\
                    Subject: Re: x\r\n\r\nbody\r\n";
        assert_eq!(super::raw_in_reply_to(raw), "Parent@Example.com");
        assert_eq!(super::raw_in_reply_to(b"Subject: x\r\n\r\nbody\r\n"), "");
    }

    #[test]
    fn a_quoted_printable_preheader_is_not_mistaken_for_base64() {
        // Cloudflare's newsletter opens its text/plain part with a long run of
        // invisible padding — U+2007 U+034F over and over — so that the preview
        // a mail client shows is the sender's chosen line rather than whatever
        // the message starts with. Quoted-printable, every byte of it spelled
        // with letters, digits and `=`: base64's alphabet exactly. Guessing
        // read it as base64 and produced a row of replacement characters.
        let mut part = b"AI Bot Controls ".to_vec();
        for _ in 0..40 {
            part.extend_from_slice(b"=E2=80=87=CD=8F ");
        }
        part.extend_from_slice(b"Updates to managing AI crawlers on your account");

        assert!(!looks_like_base64(&part), "padding runs are not base64");

        // Told what it is, which is what the part's own MIME headers say.
        let told = super::preview_from_part(&part, Some("UTF-8"), Some("quoted-printable"));
        assert!(
            !told.contains('\u{fffd}'),
            "no replacement characters: {told}"
        );
        assert!(told.starts_with("AI Bot Controls"), "{told}");
        assert!(told.contains("Updates to managing AI crawlers"), "{told}");

        // And the same left to the guess, now that the guess knows better.
        let guessed = super::preview_from_part(&part, Some("UTF-8"), None);
        assert_eq!(guessed, told);

        // Real base64 still reads as base64, truncated (no padding) or whole.
        let whole = b"VGhlIHF1aWNrIGJyb3duIGZveCBqdW1wcyBvdmVyIHRoZSBsYXp5IGRvZywgdHdpY2Uu";
        assert!(looks_like_base64(whole));
        assert!(looks_like_base64(
            b"VGhlIHF1aWNrIGJyb3duIGZveCBqdW1wcyBvdmVyIHRoZSBsYXp5IGRvZz0="
        ));

        // The slice a preview reads ends on a byte count, routinely mid-escape.
        // The stub is dropped rather than shown as text.
        let cut = b"AI Bot Controls =E2=80=87=CD=8F =E";
        let out = super::preview_from_part(cut, Some("UTF-8"), Some("quoted-printable"));
        assert!(
            !out.contains('='),
            "no half-decoded escape left behind: {out:?}"
        );
        assert_eq!(out.trim(), "AI Bot Controls");
        let cut = b"AI Bot Controls =E2=80=87=CD=8F =";
        let out = super::preview_from_part(cut, Some("UTF-8"), Some("quoted-printable"));
        assert!(!out.contains('='), "nor a bare one: {out:?}");
    }

    #[test]
    fn a_preview_of_nothing_but_preheader_padding_reaches_for_the_real_text() {
        // What the deeper BODY[TEXT] repair read sees: the whole multipart,
        // each part carrying its own headers. The text is thousands of bytes
        // past the padding, which is the point of the padding.
        let mut plain = b"AI Bot Controls ".to_vec();
        for _ in 0..250 {
            plain.extend_from_slice(b"=E2=80=87=CD=8F ");
        }
        plain.extend_from_slice(b"Updates to managing AI crawlers on your account.");
        let mut body = Vec::new();
        body.extend_from_slice(b"--BOUND\r\n");
        body.extend_from_slice(b"Content-Type: text/plain; charset=UTF-8\r\n");
        body.extend_from_slice(b"Content-Transfer-Encoding: quoted-printable\r\n\r\n");
        body.extend_from_slice(&plain);
        body.extend_from_slice(b"\r\n--BOUND--\r\n");

        let preview = super::preview_from_part(&body, None, None);
        assert!(
            preview.contains("Updates to managing AI crawlers"),
            "the padding is stepped over, not shown: {preview:?}",
        );
        assert!(!preview.contains('\u{fffd}'), "{preview:?}");
        assert!(
            !preview.contains('\u{034f}'),
            "joiners are dropped: {preview:?}"
        );
    }

    #[test]
    fn preview_decodes_the_transfer_encoding_it_was_not_told_about() {
        // The fetch asks for body bytes only, so the encoding has to be inferred.
        // Quoted-printable, including a soft line break mid-sentence:
        let qp = b"Caf=C3=A9 meeting at 3pm =\r\nsharp, bring the numbers";
        assert_eq!(
            preview_from_part(qp, None, None),
            "Café meeting at 3pm sharp, bring the numbers"
        );

        // Base64 — shown raw this would be gibberish in the list.
        let encoded = crate::oauth::base64_encode(
            b"Base64 bodies are common from newsletters and phones alike.",
        );
        assert_eq!(
            preview_from_part(encoded.as_bytes(), None, None),
            "Base64 bodies are common from newsletters and phones alike."
        );
    }

    #[test]
    fn preview_survives_being_cut_off_mid_stream() {
        // 2KB of a base64 part almost never lands on a group boundary; the
        // incomplete tail is dropped rather than decoded into noise.
        let full = crate::oauth::base64_encode(
            &b"The quick brown fox jumps over the lazy dog. ".repeat(4),
        );
        let truncated = &full[..full.len() - 3];
        let preview = preview_from_part(truncated.as_bytes(), None, None);
        assert!(preview.starts_with("The quick brown fox"), "{preview}");
        // An escape cut in half is dropped, not shown: the slice ends on a
        // byte count, so its last escape is routinely half there. A lone
        // trailing "=" is a soft line break in quoted-printable and goes the
        // same way.
        assert_eq!(preview_from_part(b"Total: 50=", None, None), "Total: 50");
        assert_eq!(preview_from_part(b"Total: 50=4", None, None), "Total: 50");
        // Malformed rather than cut short: "=Z" is no escape at all, and
        // whatever it is, it isn't ours to remove.
        assert_eq!(preview_from_part(b"Total: 50=Z", None, None), "Total: 50=Z");
    }

    #[test]
    fn preview_reads_html_and_skips_quoted_replies() {
        let html = b"<html><body><p>Meeting moved to <b>Tuesday</b>.</p></body></html>";
        assert_eq!(
            preview_from_part(html, None, None),
            "Meeting moved to Tuesday."
        );
        let reply = b"Sounds good to me.\r\n\r\n> On Monday, Ada wrote:\r\n> the original text\r\n";
        assert_eq!(preview_from_part(reply, None, None), "Sounds good to me.");
    }

    #[test]
    fn preview_is_capped() {
        let long = "word ".repeat(200);
        assert_eq!(
            preview_from_part(long.as_bytes(), None, None)
                .chars()
                .count(),
            PREVIEW_CHARS
        );
    }

    #[test]
    fn short_text_is_not_mistaken_for_base64() {
        // "Meeting" is all base64 characters, but far too short to be a body.
        assert_eq!(preview_from_part(b"Meeting", None, None), "Meeting");
    }

    #[test]
    fn preview_reads_the_charset_the_part_declares() {
        // #159: a Hungarian bank's mail, quoted-printable in iso-8859-2. Read as
        // UTF-8, every accented letter was a replacement character in the list
        // while the reader showed the message fine.
        let qp = b"Tisztelt =DCgyfel=FCnk! K=E9rj=FCk, =F5rizze meg.";
        assert_eq!(
            preview_from_part(qp, Some("iso-8859-2"), None),
            "Tisztelt Ügyfelünk! Kérjük, őrizze meg."
        );
        // The 8-bit bytes as they are, no transfer encoding; label case and
        // quotes as servers send them.
        assert_eq!(
            preview_from_part(b"\xD5rizze meg", Some("\"ISO-8859-2\""), None),
            "Őrizze meg"
        );
        assert_eq!(
            preview_from_part(b"\xF5rizze", Some("windows-1250"), None),
            "őrizze"
        );
        // Base64 is decoded to bytes first, then read in the charset.
        let b64 =
            crate::oauth::base64_encode(b"Caf\xE9 meeting at three, bring the numbers please.");
        assert_eq!(
            preview_from_part(b64.as_bytes(), Some("iso-8859-1"), None),
            "Café meeting at three, bring the numbers please."
        );
        // UTF-8 still reads as UTF-8 whatever it is called.
        assert_eq!(
            preview_from_part("Café".as_bytes(), Some("UTF-8"), None),
            "Café"
        );
        assert_eq!(
            preview_from_part("Café".as_bytes(), Some("utf8"), None),
            "Café"
        );
    }

    #[test]
    fn preview_parts_of_a_multipart_carry_their_own_charset() {
        let nested = b"--b1\r\nContent-Type: text/plain; charset=\"iso-8859-2\"\r\n\
            Content-Transfer-Encoding: quoted-printable\r\n\r\n=D5rizze meg\r\n--b1--\r\n";
        assert_eq!(preview_from_part(nested, None, None), "Őrizze meg");
        // A charset on a folded continuation line still counts.
        let folded = b"--b1\r\nContent-Type: text/html;\r\n\tcharset=iso-8859-2\r\n\r\n\
            <p>\xD5rizze meg</p>\r\n--b1--\r\n";
        assert_eq!(preview_from_part(folded, None, None), "Őrizze meg");
    }

    #[test]
    fn preview_finds_the_parts_behind_a_mime_preamble() {
        // A whole body (the BODY[TEXT] retry) can open with the preamble
        // rather than the boundary.
        let body = b"This is a multi-part message in MIME format.\r\n\r\n--b1\r\n\
            Content-Type: image/png\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--b1\r\n\
            Content-Type: text/plain\r\n\r\nThe photo from Sunday\r\n--b1--\r\n";
        assert_eq!(preview_from_part(body, None, None), "The photo from Sunday");
        // But plain text that merely mentions a dashed line is still text.
        assert_eq!(
            preview_from_part(b"Hello\r\n--foo\r\nbar", None, None),
            "Hello --foo bar"
        );
    }

    #[test]
    fn preview_copes_with_bytes_that_are_not_utf8() {
        // Undeclared 8-bit text reads as Windows-1252, the way browsers do.
        assert_eq!(
            preview_from_part(b"Caf\xE9 au lait", None, None),
            "Café au lait"
        );
        // A slice that ends mid-character drops the fragment, not the row.
        let cut = &"Café".as_bytes()[..4];
        assert_eq!(preview_from_part(cut, None, None), "Caf");
        assert_eq!(preview_from_part(cut, Some("utf-8"), None), "Caf");
        // Declared UTF-8 that isn't: the damage shows as replacement characters.
        assert_eq!(
            preview_from_part(b"a\xFFb", Some("utf-8"), None),
            "a\u{fffd}b"
        );
    }

    #[test]
    fn charset_param_is_read_from_a_content_type() {
        assert_eq!(
            charset_param("text/html; charset=\"iso-8859-2\""),
            Some("iso-8859-2".into())
        );
        assert_eq!(
            charset_param("text/plain; format=flowed; charset=utf-8"),
            Some("utf-8".into())
        );
        assert_eq!(charset_param("multipart/alternative; boundary=b1"), None);
        assert_eq!(
            mime_header(
                "Content-Type: text/plain;\r\n\tcharset=us-ascii\r\nX: y",
                "content-type"
            )
            .as_deref()
            .and_then(charset_param),
            Some("us-ascii".into())
        );
    }

    /// A part header carrying a non-ASCII character where a header name's
    /// length falls mid-character used to panic the account's mail thread, so
    /// its mail never arrived and the spinner turned for good.
    #[test]
    fn a_header_line_with_a_multibyte_character_is_not_sliced_apart() {
        // "content-transfer-encoding" is 25 bytes, and byte 25 of this line
        // sits inside the two-byte character that starts at byte 24.
        let headers = "Content-Description: abc\u{f3}de\r\nContent-Transfer-Encoding: base64\r\n";
        assert_eq!(
            mime_header(headers, "content-transfer-encoding").as_deref(),
            Some("base64")
        );
        assert_eq!(mime_header(headers, "content-type"), None);
    }

    /// The same line reached through the preview path that reads it: a part of
    /// a multipart, headers decoded leniently from raw bytes.
    #[test]
    fn a_preview_survives_a_multibyte_part_header() {
        let part = concat!(
            "--b1\r\n",
            "Content-Description: abc\u{f3}de\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n\r\n",
            "Hello.\r\n",
            "--b1--\r\n",
        );
        assert_eq!(
            preview_from_part(part.as_bytes(), None, None).trim(),
            "Hello."
        );
    }

    /// Issue #9's message: an Apple Mail PDF marked `inline` with a filename and
    /// no Content-ID, inside a top-level multipart/mixed.
    const INLINE_PDF: &str = concat!(
        "Content-Type: multipart/mixed; boundary=\"Apple-Mail=_A9BB\"\r\n\r\n",
        "--Apple-Mail=_A9BB\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n",
        "--Apple-Mail=_A9BB\r\n",
        "Content-Disposition: inline;\r\n\tfilename=\"Report.pdf\"\r\n",
        "Content-Type: application/pdf;\r\n\tname=\"Report.pdf\"\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "JVBERi0xLjQNCiWio4+T\r\n",
        "--Apple-Mail=_A9BB--\r\n",
    );

    /// The same file, but nested under a multipart/alternative — the shape a
    /// forwarded Apple Mail message takes, where the top-level Content-Type says
    /// nothing about attachments.
    const NESTED_INLINE_PDF: &str = concat!(
        "Content-Type: multipart/alternative; boundary=outer\r\n\r\n",
        "--outer\r\nContent-Type: text/plain\r\n\r\nPlease see attached.\r\n",
        "--outer\r\nContent-Type: multipart/mixed; boundary=inner\r\n\r\n",
        "--inner\r\nContent-Type: text/html\r\n\r\n<p>Please see attached.</p>\r\n",
        "--inner\r\nContent-Type: application/pdf; name=\"Report.pdf\"\r\n",
        "Content-Disposition: inline; filename=\"Report.pdf\"\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "JVBERi0xLjQNCiWio4+T\r\n",
        "--inner--\r\n--outer--\r\n",
    );

    #[test]
    fn an_inline_pdf_is_listed_as_an_attachment() {
        // Both shapes: the file is `inline` with no Content-ID, which is what the
        // reporter's Apple Mail message looked like.
        for (name, raw) in [("top level", INLINE_PDF), ("nested", NESTED_INLINE_PDF)] {
            let found = extract_attachments(raw.as_bytes());
            assert_eq!(found.len(), 1, "{name}");
            assert_eq!(found[0].name, "Report.pdf", "{name}");
            assert!(found[0].data.starts_with(b"%PDF-1.4"), "{name}");
        }
    }

    #[test]
    fn a_body_load_reports_the_attachments_the_summary_missed() {
        // The paperclip is guessed before the body is fetched, and the guess
        // misses the nested shape: the top-level type is multipart/alternative,
        // which says nothing. Fetching the body is what settles it — the same
        // check `load_body` runs before emitting HasAttachments.
        let has_attachments = |raw: &str| !extract_attachments(raw.as_bytes()).is_empty();
        assert!(has_attachments(NESTED_INLINE_PDF));
        assert!(has_attachments(INLINE_PDF));

        // The header-only guess, for comparison: right about the first, wrong
        // about the second — which is why the body has the last word.
        let top_level_says_mixed = |raw: &str| {
            mail_parser::MessageParser::default()
                .parse(raw.as_bytes())
                .and_then(|p| {
                    p.header("Content-Type")
                        .and_then(|h| h.as_content_type())
                        .map(|ct| {
                            ct.ctype().eq_ignore_ascii_case("multipart")
                                && ct
                                    .subtype()
                                    .is_some_and(|s| s.eq_ignore_ascii_case("mixed"))
                        })
                })
                .unwrap_or(false)
        };
        assert!(top_level_says_mixed(INLINE_PDF));
        assert!(!top_level_says_mixed(NESTED_INLINE_PDF));
    }

    #[test]
    fn decoration_still_does_not_earn_a_paperclip() {
        // The correction must not undo 1.4.1's fix: a newsletter whose only extra
        // parts are small inline `cid:` images has no attachments.
        assert!(extract_attachments(CID_NEWSLETTER.as_bytes()).is_empty());
    }

    #[test]
    fn folder_names_are_decoded_for_display() {
        // Issue #1: Gmail's Chinese labels arrived as modified UTF-7 and were
        // shown verbatim in the sidebar.
        assert_eq!(display_name("&XfJSoGYfaAc-", Some("/")), "已加星标");
        // Only the leaf is shown, and the parent may be encoded too.
        assert_eq!(display_name("&U,BTFw-/&g0l6Pw-", Some("/")), "草稿");
        assert_eq!(display_name("INBOX.&U,BTFw-", Some(".")), "台北");
        // Ordinary names are untouched, and INBOX keeps its special casing.
        assert_eq!(display_name("INBOX", Some("/")), "Inbox");
        assert_eq!(display_name("[Gmail]/All Mail", Some("/")), "All Mail");
        assert_eq!(display_name("Tom & Jerry", Some("/")), "Tom & Jerry");
    }

    #[test]
    fn a_queued_message_keeps_every_recipient_on_retry() {
        // Bcc exists only in the envelope, so a retry that rebuilds it from the
        // headers would silently drop those recipients.
        let item = queued("me@example.com", &["ada@example.com", "bcc@example.com"]);
        let envelope = outbox_envelope(&item).expect("envelope rebuilds");
        assert_eq!(
            envelope.from().map(|f| f.to_string()).as_deref(),
            Some("me@example.com")
        );
        assert_eq!(
            envelope
                .to()
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>(),
            ["ada@example.com", "bcc@example.com"]
        );
    }

    #[test]
    fn a_queued_message_with_an_unusable_address_is_not_retried() {
        // Better to leave it in the Outbox saying so than to retry forever.
        assert!(outbox_envelope(&queued("me@example.com", &["not-an-address"])).is_none());
        assert!(outbox_envelope(&queued("me@example.com", &[])).is_none());
        // A missing sender is legal (SMTP's null reverse-path), so it is not a
        // reason to refuse.
        assert!(outbox_envelope(&queued("", &["ada@example.com"])).is_some());
    }

    #[test]
    fn an_alias_replaces_from_but_not_the_transport() {
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.to = "someone@example.com".into();
        msg.from_alias = Some("Ann Work <ann@work.example>".into());
        let email = build_email(&account, &msg).expect("builds");
        let wire = String::from_utf8(email.formatted()).expect("utf8");
        let from_line = wire
            .lines()
            .find(|l| l.starts_with("From:"))
            .expect("From header");
        assert!(
            from_line.contains("ann@work.example"),
            "alias address on the wire: {from_line}"
        );
        assert!(
            !from_line.contains(&account.email),
            "account address replaced: {from_line}"
        );
        // The Message-ID follows the alias's domain, so replies thread back.
        let id_line = wire
            .lines()
            .find(|l| l.starts_with("Message-ID:"))
            .expect("id");
        assert!(
            id_line.contains("@work.example"),
            "id in alias domain: {id_line}"
        );
    }

    #[test]
    fn only_an_alias_with_its_own_smtp_switches_transport() {
        use crate::config::AliasConfig;
        let account = AccountConfig {
            email: "me@example.com".into(),
            aliases: vec![
                AliasConfig {
                    identity: "Plain <plain@fwd.example>".into(),
                    ..Default::default()
                },
                AliasConfig {
                    identity: "Ann Work <ann@work.example>".into(),
                    smtp_host: "smtp.work.example".into(),
                    smtp_port: 465,
                    smtp_username: "ann@work.example".into(),
                    ..Default::default()
                },
            ],
            ..sample_account()
        };
        // A plain alias, and the account's own address, keep the account transport.
        assert!(alias_with_own_smtp(&account, "plain@fwd.example").is_none());
        assert!(alias_with_own_smtp(&account, "me@example.com").is_none());
        // The per-SMTP alias resolves — case-insensitively, as addresses are.
        let hit = alias_with_own_smtp(&account, "Ann@Work.example").expect("matches");
        assert_eq!(hit.smtp_host, "smtp.work.example");
        // An address that is nobody's alias resolves to the account transport.
        assert!(alias_with_own_smtp(&account, "stranger@else.example").is_none());
    }

    #[test]
    fn build_email_carries_attachments() {
        // Issue #15: "Send failed: Invalid input" when sending with attachments.
        // "Invalid input" is lettre's AddressError, raised only by parsing a
        // "Name <addr>" string — which 1.8.x did for From/To and 1.9.0 replaced
        // with mailboxes built from parts. This pins the attachment path itself,
        // including a name that would have needed quoting and a UTF-8 filename.
        let dir = std::env::temp_dir().join(format!("hylki-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("Meeting notes, final.pdf");
        // Binary, so the encoder has to base64 it — as it would a real PDF.
        std::fs::write(
            &file,
            [
                0x25, 0x50, 0x44, 0x46, 0x2d, 0x31, 0x2e, 0x34, 0x00, 0x80, 0xff,
            ],
        )
        .expect("write attachment");

        let account = AccountConfig {
            name: "Powers, Benny".into(),
            email: "benny@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "Ada Lovelace <ada@example.com>".into(),
            subject: "Notes".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            ..sample_outgoing()
        };

        let email = build_email(&account, &msg).expect("email builds with an attachment");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("application/pdf"), "{raw}");
        // The filename has a comma and a space, so it has to be quoted or encoded.
        assert!(
            raw.contains("Meeting notes, final.pdf") || raw.contains("Meeting%20notes"),
            "{raw}"
        );
        // Base64 of the bytes written above.
        assert!(raw.contains("base64"), "{raw}");
        assert!(raw.contains("JVBERi0xLjQAgP8="), "{raw}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every line on the wire ends CRLF: JMAP servers refuse a message with a
    /// lone LF anywhere in it (#414).
    fn has_bare_lf(raw: &[u8]) -> bool {
        raw.iter()
            .enumerate()
            .any(|(i, &b)| b == b'\n' && (i == 0 || raw[i - 1] != b'\r'))
    }

    /// #412: plain text with nothing attached still says it is UTF-8 text,
    /// or a client that doesn't guess garbles every accent.
    #[test]
    fn plain_text_declares_its_charset() {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            body: "Plain text with öäü and 🙂\nsecond line".into(),
            ..sample_outgoing()
        };
        let raw = build_email(&sample_account(), &msg)
            .expect("email builds")
            .formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("MIME-Version: 1.0\r\n"), "{text}");
        assert!(
            text.contains("Content-Type: text/plain; charset=utf-8\r\n"),
            "{text}"
        );
        assert!(!has_bare_lf(&raw), "{text}");
        let parsed = mail_parser::MessageParser::default()
            .parse(&raw)
            .expect("parses");
        assert_eq!(
            parsed.body_text(0).as_deref().map(str::trim_end),
            Some("Plain text with öäü and 🙂\r\nsecond line")
        );
    }

    /// #414: a text file with Unix line endings goes out as base64, so its
    /// bytes arrive unchanged and no lone LF reaches the wire.
    #[test]
    fn a_unix_text_attachment_leaves_no_bare_newline() {
        let dir = std::env::temp_dir().join(format!("hylki-lf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"one\ntwo\n").unwrap();
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            attachments: vec![file.to_string_lossy().to_string()],
            ..sample_outgoing()
        };
        let raw = build_email(&sample_account(), &msg)
            .expect("email builds")
            .formatted();
        assert!(!has_bare_lf(&raw), "{}", String::from_utf8_lossy(&raw));
        let parsed = mail_parser::MessageParser::default()
            .parse(&raw)
            .expect("parses");
        assert_eq!(
            parsed.attachment(0).map(|a| a.contents()),
            Some(&b"one\ntwo\n"[..])
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A picture pasted into the composer rides the HTML as a data: URI; the
    /// wire message must carry it as an inline cid: part inside
    /// multipart/related — rendered in place, absent from the recipient's
    /// attachment list — with no data: URI left behind.
    #[test]
    fn pasted_images_go_out_as_inline_cid_parts() {
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01, 0x02,
        ];
        let uri = format!(
            "data:image/png;base64,{}",
            crate::oauth::base64_encode(&png)
        );
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            body: "see picture".into(),
            html: format!("<p>see</p><img src=\"{uri}\" style=\"max-width:100%\"><p>picture</p>"),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("email builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("multipart/related"), "{raw}");
        assert!(raw.contains("Content-ID"), "{raw}");
        assert!(raw.contains("cid:"), "{raw}");
        assert!(raw.contains("Content-Disposition: inline"), "{raw}");
        assert!(raw.contains("image/png"), "{raw}");
        assert!(!raw.contains("data:image"), "{raw}");
    }

    /// A picture dropped in from a file manager keeps the name it had; one
    /// pasted as raw bytes has none to keep.
    #[test]
    fn an_images_own_filename_reaches_the_inline_part() {
        let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00];
        let uri = format!(
            "data:image/png;base64,{}",
            crate::oauth::base64_encode(&png)
        );
        let raw_for = |img: String| {
            let msg = OutgoingMessage {
                to: "ada@example.com".into(),
                body: "see picture".into(),
                html: format!("<p>see</p>{img}"),
                ..sample_outgoing()
            };
            let email = build_email(&sample_account(), &msg).expect("email builds");
            String::from_utf8_lossy(&email.formatted()).to_string()
        };
        let named = raw_for(format!("<img src=\"{uri}\" alt=\"holiday snap.png\">"));
        assert!(named.contains("holiday snap.png"), "{named}");
        let anonymous = raw_for(format!("<img src=\"{uri}\">"));
        assert!(
            anonymous.contains("Content-Disposition: inline"),
            "{anonymous}"
        );
        assert!(!anonymous.contains("filename"), "{anonymous}");
    }

    /// HTML without an embedded image keeps the old, plain alternative shape.
    #[test]
    fn html_without_images_stays_a_plain_alternative() {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            body: "hi".into(),
            html: "<p>hi</p>".into(),
            ..sample_outgoing()
        };
        let email = build_email(&sample_account(), &msg).expect("email builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("multipart/alternative"), "{raw}");
        assert!(!raw.contains("multipart/related"), "{raw}");
    }

    #[test]
    fn build_email_reports_a_missing_attachment_by_name() {
        let msg = OutgoingMessage {
            to: "ada@example.com".into(),
            attachments: vec!["/nonexistent/report.pdf".into()],
            ..sample_outgoing()
        };
        let err = build_email(&sample_account(), &msg).expect_err("missing file fails");
        assert!(err.to_string().contains("report.pdf"), "{err}");
    }

    #[test]
    fn build_email_accepts_named_recipients() {
        let account = AccountConfig {
            name: "Jason Martin".into(),
            email: "me@example.com".into(),
            ..sample_account()
        };
        let msg = OutgoingMessage {
            to: "Alfonso Lizárraga <alfonso@example.com>, plain@example.com".into(),
            ..sample_outgoing()
        };
        let email = build_email(&account, &msg).expect("email builds");
        let raw = String::from_utf8_lossy(&email.formatted()).to_string();
        assert!(raw.contains("alfonso@example.com"), "{raw}");
        assert!(raw.contains("plain@example.com"), "{raw}");
    }

    #[test]
    fn only_this_machine_counts_as_a_local_bridge() {
        for host in [
            "127.0.0.1",
            "localhost",
            "LocalHost",
            "::1",
            "[::1]",
            "127.1.2.3",
        ] {
            assert!(is_loopback_host(host), "{host} should be loopback");
        }
        // A remote host must keep full certificate verification, however it is
        // named — including anything merely containing "localhost".
        for host in [
            "imap.gmail.com",
            "127.0.0.1.example.com",
            "localhost.evil.com",
            "10.0.1.14",
        ] {
            assert!(!is_loopback_host(host), "{host} should not be loopback");
        }
    }

    #[test]
    fn goa_security_wins_over_the_port() {
        let sec = |imap_starttls, smtp_starttls| crate::config::ServerSecurity {
            imap_starttls,
            smtp_starttls,
            ..Default::default()
        };
        // STARTTLS on a port of the server's own, and TLS from the first
        // byte on the ports the guess would upgrade on.
        let account = AccountConfig {
            imap_host: "mail.example.com".into(),
            imap_port: 1143,
            smtp_port: 2525,
            security: Some(sec(true, false)),
            ..sample_account()
        };
        assert!(imap_uses_starttls(&account));
        assert!(SmtpTls::for_account(&account).implicit);
        let account = AccountConfig {
            imap_port: 143,
            smtp_port: 587,
            security: Some(sec(false, false)),
            ..account
        };
        assert!(!imap_uses_starttls(&account));
        assert!(SmtpTls::for_account(&account).implicit);
        // Without GOA's word, the ports decide as before.
        let account = AccountConfig {
            security: None,
            ..account
        };
        assert!(imap_uses_starttls(&account));
        assert!(!SmtpTls::for_account(&account).implicit);
        // An accepted certificate reaches SMTP; the name waiver stays the
        // account's own.
        let account = AccountConfig {
            tls_accept_hostname_mismatch: true,
            security: Some(crate::config::ServerSecurity {
                smtp_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..account
        };
        let tls = SmtpTls::for_account(&account);
        assert!(tls.accept_invalid_certs && tls.accept_hostname_mismatch);
    }

    #[test]
    fn starttls_is_used_for_143_and_local_bridges_only() {
        let starttls = |host: &str, port: u16| {
            imap_uses_starttls(&AccountConfig {
                imap_host: host.into(),
                imap_port: port,
                ..sample_account()
            })
        };
        // Proton Bridge's default endpoint, and a bridge on a changed port.
        assert!(starttls("127.0.0.1", 1143));
        assert!(starttls("localhost", 2143));
        // The conventional STARTTLS port, anywhere.
        assert!(starttls("imap.example.com", 143));
        // 993 is implicit TLS even on a bridge, and a remote host on any other
        // port keeps the implicit-TLS behaviour it had before.
        assert!(!starttls("127.0.0.1", 993));
        assert!(!starttls("imap.gmail.com", 993));
        assert!(!starttls("imap.example.com", 1993));
    }

    #[test]
    fn decode_header_repairs_interior_spaces() {
        // DPD's complaints department sends its display name with a literal
        // trailing space INSIDE the encoded word — illegal, and the decoder
        // used to give up and show the raw `=?UTF-8?Q?…?=` (beta 1.18.0b
        // feedback from p-mitana).
        let raw = b"=?UTF-8?Q?Dzia=C5=82_Reklamacji_DPD ?=";
        assert_eq!(decode_header(raw).trim_end(), "Dzia\u{142} Reklamacji DPD");
    }

    #[test]
    fn decode_header_handles_over_long_encoded_word() {
        // The Marginalian (Mailchimp) sends the entire subject as one Q-encoded
        // word ~250 chars long — far past RFC 2047's 75-char limit. Earlier
        // builds aborted and left the raw `=?utf-8?Q?…?=` in the UI; we now
        // decode it as Apple Mail and Thunderbird do.
        let raw = b"=?utf-8?Q?92=2Dyear=2Dold=20artist=20Sheila=20Hicks=20on=20the=20key=20to=20creative=20vitality=2C=20how=20to=20manage=20heartbreak=20like=20Frida=20Kahlo=2C=20the=20elusive=20science=20of=20the=20present=20moment?=";
        assert_eq!(
            decode_header(raw),
            "92-year-old artist Sheila Hicks on the key to creative vitality, \
             how to manage heartbreak like Frida Kahlo, the elusive science of \
             the present moment"
        );
    }

    #[test]
    fn decode_header_leaves_plain_text_untouched() {
        assert_eq!(
            decode_header(b"Just a normal subject"),
            "Just a normal subject"
        );
    }

    #[test]
    fn references_and_in_reply_to_merge_without_duplicates() {
        // A well-formed reply: In-Reply-To repeats the last id of References, and
        // the ancestry it adds is what lets a thread survive a parent kept in
        // another folder (#21).
        assert_eq!(
            merge_msgids("a@x b@y", "b@y"),
            "a@x b@y",
            "the repeated parent should not be listed twice"
        );
        // Either side may be the only one present.
        assert_eq!(merge_msgids("", "b@y"), "b@y");
        assert_eq!(merge_msgids("a@x", ""), "a@x");
        assert_eq!(merge_msgids("", ""), "");
        // A malformed reply whose In-Reply-To names an ancestor References omits.
        assert_eq!(merge_msgids("a@x", "c@z"), "a@x c@z");
    }

    #[test]
    fn header_fields_unfold_each_requested_header() {
        // #334: References and the priority headers come back in one block.
        let raw =
            b"References: <a@x>\r\n <b@y>\r\nX-Priority: 1 (Highest)\r\nImportance: high\r\n\r\n";
        let fields = header_fields(raw);
        assert_eq!(fields.len(), 3);
        assert_eq!(
            fields[0],
            ("References".to_string(), "<a@x> <b@y>".to_string())
        );
        assert_eq!(normalize_msgids(fields[0].1.as_bytes()), "a@x b@y");
        assert_eq!(
            fields[1],
            ("X-Priority".to_string(), "1 (Highest)".to_string())
        );
        assert!(header_fields(b"").is_empty());
    }

    #[test]
    fn header_references_are_normalized_like_envelope_ids() {
        // Folded exactly as a server sends it, angle brackets and mixed case.
        assert_eq!(
            normalize_msgids(b"<A@x>\r\n <b@Y>\r\n"),
            "a@x b@y",
            "ids should be unwrapped, lowercased and space-joined"
        );
    }

    #[test]
    fn linkify_wraps_http_and_https() {
        assert_eq!(
            linkify("see http://example.com now"),
            "see <a href=\"http://example.com\">http://example.com</a> now"
        );
        assert_eq!(
            linkify("https://a.b/c?d=1"),
            "<a href=\"https://a.b/c?d=1\">https://a.b/c?d=1</a>"
        );
    }

    #[test]
    fn linkify_prefixes_bare_www_with_https() {
        assert_eq!(
            linkify("go to www.example.com today"),
            "go to <a href=\"https://www.example.com\">www.example.com</a> today"
        );
    }

    #[test]
    fn linkify_escapes_and_does_not_double_link() {
        // The surrounding <> are escaped; the URL inside is linked once.
        assert_eq!(
            linkify("<http://x.com>"),
            "&lt;<a href=\"http://x.com\">http://x.com</a>&gt;"
        );
        // A query '&' is escaped in both href and text.
        assert_eq!(
            linkify("http://x.com/?a=1&b=2"),
            "<a href=\"http://x.com/?a=1&amp;b=2\">http://x.com/?a=1&amp;b=2</a>"
        );
    }

    #[test]
    fn linkify_trims_trailing_punctuation_but_keeps_balanced_parens() {
        assert_eq!(
            linkify("visit http://x.com."),
            "visit <a href=\"http://x.com\">http://x.com</a>."
        );
        assert_eq!(
            linkify("(see http://x.com)"),
            "(see <a href=\"http://x.com\">http://x.com</a>)"
        );
        // A balanced ')' belongs to the URL (e.g. a Wikipedia article).
        assert_eq!(
            linkify("http://en.wikipedia.org/wiki/Foo_(bar)"),
            "<a href=\"http://en.wikipedia.org/wiki/Foo_(bar)\">http://en.wikipedia.org/wiki/Foo_(bar)</a>"
        );
    }

    #[test]
    fn linkify_ignores_scheme_inside_a_word() {
        // No word boundary before "http", so it's not a link (just escaped text).
        assert_eq!(linkify("shttp://x.com"), "shttp://x.com");
        assert!(!linkify("email hi@www.x").contains("<a "));
    }

    #[test]
    fn linkify_never_forges_a_dangerous_scheme() {
        // "javascript:" isn't one of our recognized prefixes, so it stays plain text.
        let out = linkify("javascript:alert(1)");
        assert!(!out.contains("<a "), "got: {out}");
    }

    /// Auto-empty (#140): only the roles with an age are swept.
    #[test]
    fn auto_empty_roles_follow_the_ages() {
        let mut acc = sample_account();
        assert!(auto_empty_roles(&acc).is_empty());
        acc.empty_trash_days = 14;
        let roles = auto_empty_roles(&acc);
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].0, FolderKind::Trash);
        assert_eq!(roles[0].2, 14);
        acc.empty_junk_days = 7;
        assert_eq!(auto_empty_roles(&acc).len(), 2);
    }

    /// The manual role assignment (#82) names the folder first; otherwise
    /// the detected kind does.
    #[test]
    fn auto_empty_role_folder_prefers_the_assignment() {
        let mut acc = sample_account();
        let folders = vec![Folder {
            id: 7,
            account_id: 1,
            name: "Deleted Messages".into(),
            path: "Deleted Messages".into(),
            kind: FolderKind::Trash,
            unread: 0,
        }];
        assert_eq!(
            role_folder_path(&acc, &folders, FolderKind::Trash, "trash").as_deref(),
            Some("Deleted Messages")
        );
        assert_eq!(
            role_folder_path(&acc, &folders, FolderKind::Junk, "junk"),
            None
        );
        acc.folder_roles.insert("trash".into(), "INBOX.Bin".into());
        assert_eq!(
            role_folder_path(&acc, &folders, FolderKind::Trash, "trash").as_deref(),
            Some("INBOX.Bin")
        );
    }

    /// The sweep runs once, then not again until the interval has passed.
    #[test]
    fn auto_empty_is_throttled() {
        let mut acc = sample_account();
        let mut last = None;
        assert!(!auto_empty_due(&acc, &mut last), "nothing configured");
        assert!(last.is_none());
        acc.empty_junk_days = 7;
        assert!(auto_empty_due(&acc, &mut last));
        assert!(!auto_empty_due(&acc, &mut last), "just ran");
        last = Some(std::time::Instant::now() - AUTO_EMPTY_MIN - Duration::from_secs(1));
        assert!(auto_empty_due(&acc, &mut last));
    }

    /// Moving list mail (#407) runs on the same terms: set, and not twice
    /// within the interval. Its search leaves starred mail out.
    #[test]
    fn list_clean_up_is_throttled_and_spares_starred_mail() {
        let mut acc = sample_account();
        let mut last = None;
        assert!(!clean_lists_due(&acc, &mut last), "not set");
        acc.clean_lists_days = 90;
        assert!(clean_lists_due(&acc, &mut last));
        assert!(!clean_lists_due(&acc, &mut last), "just ran");
        let q = clean_lists_query(90);
        assert!(q.starts_with("UNFLAGGED BEFORE "), "{q}");
    }

    /// IMAP wants "1-Aug-2026" (no zero padding); Graph an RFC 3339 instant.
    #[test]
    fn auto_empty_cutoffs_are_well_formed() {
        let imap = imap_before_date(0);
        let today = chrono::Utc::now().format("%-d-%b-%Y").to_string();
        assert_eq!(imap, today);
        assert!(!imap.starts_with('0'), "{imap}");
        let parts: Vec<&str> = imap.split('-').collect();
        assert_eq!(parts.len(), 3, "{imap}");
        assert_eq!(parts[1].len(), 3, "{imap}");
        let graph = graph_before_date(30);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&graph).is_ok(),
            "{graph}"
        );
        assert!(graph < chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }

    /// A decrypted entity with protected headers (#493): the part that only
    /// repeats the Subject is neither shown nor listed as a file.
    #[test]
    fn protected_headers_legacy_part_is_hidden() {
        let entity = |legacy_type: &str| {
            format!(
                "Content-Type: multipart/mixed; boundary=\"pp\"; protected-headers=\"v1\"\r\n\
                 Subject: Plans\r\n\r\n\
                 --pp\r\nContent-Type: {legacy_type}; protected-headers=\"v1\"\r\n\
                 Content-Disposition: inline\r\n\r\nSubject: Plans\r\n\r\n\
                 --pp\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\nSee you there.\r\n--pp--\r\n"
            )
        };
        for legacy_type in ["text/plain; charset=UTF-8", "text/rfc822-headers"] {
            let raw = entity(legacy_type);
            let body = extract_body(raw.as_bytes());
            assert!(body.contains("See you there."), "{body}");
            assert!(!body.contains("Subject: Plans"), "{legacy_type}: {body}");
            assert!(
                extract_attachments(raw.as_bytes()).is_empty(),
                "{legacy_type}"
            );
        }
        // The same first part without the flag on the root is ordinary text.
        let raw = entity("text/plain; charset=UTF-8").replacen("; protected-headers=\"v1\"", "", 1);
        assert!(extract_body(raw.as_bytes()).contains("Subject: Plans"));
    }

    #[test]
    fn extract_body_linkifies_plain_text_mail() {
        let raw = b"Content-Type: text/plain\r\n\r\nRead http://example.com/x for more.";
        let body = extract_body(raw);
        assert!(
            body.contains("<a href=\"http://example.com/x\">http://example.com/x</a>"),
            "body was: {body}"
        );
    }

    /// An iPhone photo mail: multipart/mixed interleaving an empty text part, an
    /// *inline* JPEG, and the signature. Exactly the shape Apple Mail produces.
    const IPHONE_PHOTO: &str = concat!(
        "Content-Type: multipart/mixed; boundary=Apple-Mail-32B9517E\r\n",
        "Content-Transfer-Encoding: 7bit\r\n",
        "From: Alex Doe <alex@example.com>\r\n",
        "Subject: Panda\r\n",
        "X-Mailer: iPhone Mail (23F84)\r\n",
        "\r\n",
        "--Apple-Mail-32B9517E\r\n",
        "Content-Type: text/plain;\r\n\tcharset=us-ascii\r\n",
        "Content-Transfer-Encoding: 7bit\r\n\r\n\r\n\r\n",
        "--Apple-Mail-32B9517E\r\n",
        "Content-Type: image/jpeg;\r\n\tname=C21AA3E8.jpeg;\r\n",
        "\tx-apple-part-url=4F5E768A\r\n",
        "Content-Disposition: inline;\r\n\tfilename=C21AA3E8.jpeg\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBk=\r\n",
        "\r\n",
        "--Apple-Mail-32B9517E\r\n",
        "Content-Type: text/plain;\r\n\tcharset=us-ascii\r\n",
        "Content-Transfer-Encoding: 7bit\r\n\r\n",
        "\r\n\r\nAlex Doe\r\nSent from my iPhone\r\n",
        "--Apple-Mail-32B9517E--\r\n",
    );

    /// A newsletter: HTML body plus a `cid:` logo referenced from it.
    const CID_NEWSLETTER: &str = concat!(
        "Content-Type: multipart/related; boundary=R\r\n",
        "Subject: News\r\n\r\n",
        "--R\r\n",
        "Content-Type: text/html; charset=utf-8\r\n\r\n",
        "<p>hi <img src=\"cid:logo\"></p>\r\n",
        "--R\r\n",
        "Content-Type: image/png; name=logo.png\r\n",
        "Content-ID: <logo>\r\n",
        "Content-Disposition: inline; filename=logo.png\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "iVBORw0KGgo=\r\n",
        "--R--\r\n",
    );

    #[test]
    fn iphone_photo_body_keeps_text_and_inlines_the_image() {
        let body = extract_body(IPHONE_PHOTO.as_bytes());
        // The signature lives in the *third* part; taking only the first left the
        // message blank.
        assert!(body.contains("Sent from my iPhone"), "body was: {body}");
        // The photo is embedded, not dropped or fetched from the network.
        assert!(
            body.contains("src=\"data:image/jpeg;base64,/9j/4AAQ"),
            "body was: {body}"
        );
    }

    #[test]
    fn iphone_photo_is_listed_as_an_attachment() {
        let found = extract_attachments(IPHONE_PHOTO.as_bytes());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "C21AA3E8.jpeg");
        assert!(!found[0].data.is_empty());
    }

    /// Gmail photo mail: multipart/related wrapping a plain+HTML alternative and
    /// the picture the HTML references by Content-ID.
    const GMAIL_RELATED_PHOTO: &str = concat!(
        "Content-Type: multipart/related; boundary=\"OUT\"\r\n",
        "Subject: Share A Day NYC\r\n\r\n",
        "--OUT\r\n",
        "Content-Type: multipart/alternative; boundary=\"IN\"\r\n\r\n",
        "--IN\r\n",
        "Content-Type: text/plain; charset=\"UTF-8\"\r\n\r\n",
        "[image: SAD_POSTCARD_FRONT.JPEG]\r\n",
        "--IN\r\n",
        "Content-Type: text/html; charset=\"UTF-8\"\r\n\r\n",
        "<div dir=\"ltr\"><img src=\"cid:ii_mslv0bge0\" ",
        "alt=\"SAD_POSTCARD_FRONT.JPEG\" width=\"420\" height=\"542\"><br></div>\r\n",
        "--IN--\r\n",
        "--OUT\r\n",
        "Content-Type: image/jpeg; name=\"SAD_POSTCARD_FRONT.JPEG\"\r\n",
        "Content-Disposition: inline; filename=\"SAD_POSTCARD_FRONT.JPEG\"\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "Content-ID: <ii_mslv0bge0>\r\n\r\n",
        "/9j/4AAQSkZJRg==\r\n",
        "--OUT--\r\n",
    );

    #[test]
    fn cid_resources_are_not_listed_as_attachments() {
        assert!(extract_attachments(CID_NEWSLETTER.as_bytes()).is_empty());
    }

    /// A `multipart/related` whose `cid:` image decodes to `bytes` bytes.
    fn cid_mail_with_image_of(bytes: usize) -> String {
        // base64 of `bytes` zeroes: 4 chars per 3 bytes, rounded up.
        let payload = crate::oauth::base64_encode(&vec![0u8; bytes]);
        concat!(
            "Content-Type: multipart/related; boundary=R\r\n",
            "Subject: Photo\r\n\r\n",
            "--R\r\n",
            "Content-Type: text/html; charset=utf-8\r\n\r\n",
            "<p><img src=\"cid:pic\"></p>\r\n",
            "--R\r\n",
            "Content-Type: image/jpeg; name=\"pic.jpg\"\r\n",
            "Content-ID: <pic>\r\n",
            "Content-Disposition: inline; filename=\"pic.jpg\"\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\n",
        )
        .to_string()
            + &payload
            + "\r\n--R--\r\n"
    }

    #[test]
    fn large_cid_image_is_listed_as_an_attachment() {
        // A photo someone emailed you arrives in the same shape as a newsletter
        // logo; it has to be saveable, so size decides.
        let found = extract_attachments(cid_mail_with_image_of(INLINE_ATTACHMENT_MIN).as_bytes());
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "pic.jpg");
        assert_eq!(found[0].data.len(), INLINE_ATTACHMENT_MIN);
    }

    #[test]
    fn small_cid_image_is_still_only_decoration() {
        let raw = cid_mail_with_image_of(INLINE_ATTACHMENT_MIN - 1);
        assert!(extract_attachments(raw.as_bytes()).is_empty());
    }

    /// A message in the shape Gmail's composer sends: every uploaded file is
    /// marked `Content-Disposition: attachment` *and* stamped with an `<f_…>`
    /// Content-ID, whatever its size.
    fn gmail_mail_with_files(files: &[(&str, &str, usize)]) -> String {
        let mut raw = String::from("Content-Type: multipart/mixed; boundary=M\r\n");
        raw.push_str("Subject: files\r\n\r\n");
        raw.push_str("--M\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n");
        raw.push_str("Body text.\r\n");
        for (i, (name, ctype, bytes)) in files.iter().enumerate() {
            raw.push_str("--M\r\n");
            raw.push_str(&format!("Content-Type: {ctype}; name=\"{name}\"\r\n"));
            raw.push_str(&format!(
                "Content-Disposition: attachment; filename=\"{name}\"\r\n"
            ));
            raw.push_str("Content-Transfer-Encoding: base64\r\n");
            raw.push_str(&format!("Content-ID: <f_a1b2c3d4{i}>\r\n\r\n"));
            raw.push_str(&crate::oauth::base64_encode(&vec![0u8; *bytes]));
            raw.push_str("\r\n");
        }
        raw.push_str("--M--\r\n");
        raw
    }

    /// Two small files and one large one are sent together; only the large one
    /// came back, because the small parts carried a Content-ID. A part the sender
    /// declared an attachment is never decoration, however small.
    #[test]
    fn small_declared_attachments_survive_their_content_id() {
        let raw = gmail_mail_with_files(&[
            ("first.pdf", "application/pdf", 16_384),
            ("second.pdf", "application/pdf", 40_960),
            (
                "large.bin",
                "application/octet-stream",
                INLINE_ATTACHMENT_MIN + 1,
            ),
        ]);
        let found = extract_attachments(raw.as_bytes());
        let names: Vec<&str> = found.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            ["first.pdf", "second.pdf", "large.bin"],
            "found: {names:?}"
        );
        assert_eq!(found[0].data.len(), 16_384);
        assert_eq!(found[1].data.len(), 40_960);
    }

    /// One attachment, its Content-Disposition supplied verbatim.
    fn mail_with_disposition(disposition: &str) -> String {
        format!(
            concat!(
                "Content-Type: multipart/mixed; boundary=B\r\n",
                "Subject: file\r\n\r\n",
                "--B\r\n",
                "Content-Type: application/octet-stream\r\n",
                "Content-Transfer-Encoding: base64\r\n",
                "Content-Disposition: {disposition}\r\n\r\n",
                "AAAAAA==\r\n",
                "--B--\r\n",
            ),
            disposition = disposition
        )
    }

    /// A sender split one filename across two encoded-words with a space between
    /// them. RFC 2047 §6.2 joins those and drops the space; keeping it yields
    /// `docu ment.pdf` — a PDF with no `.pdf` suffix, so nothing can open it.
    #[test]
    fn a_filename_split_across_encoded_words_is_rejoined() {
        let raw =
            mail_with_disposition("attachment; filename=\"=?utf-8?q?docu?= =?utf-8?q?ment.pdf?=\"");
        let found = extract_attachments(raw.as_bytes());
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "document.pdf");
    }

    /// The same split, folded across two lines: the separator arrives as a tab.
    #[test]
    fn a_filename_folded_between_encoded_words_is_rejoined() {
        let raw = mail_with_disposition(
            "attachment;\r\n\tfilename=\"=?utf-8?q?docu?=\r\n\t=?utf-8?q?ment.pdf?=\"",
        );
        let found = extract_attachments(raw.as_bytes());
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "document.pdf");
    }

    /// `=20` is a space in the name itself, not a separator between two words.
    #[test]
    fn a_space_inside_one_encoded_word_is_kept() {
        let raw = mail_with_disposition("attachment; filename=\"=?utf-8?q?my=20file.pdf?=\"");
        let found = extract_attachments(raw.as_bytes());
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "my file.pdf");
    }

    /// A plain filename with a space in it is nobody's business but the sender's.
    #[test]
    fn an_unencoded_filename_with_a_space_is_untouched() {
        let raw = mail_with_disposition("attachment; filename=\"my file.pdf\"");
        let found = extract_attachments(raw.as_bytes());
        assert_eq!(found.len(), 1, "found: {found:?}");
        assert_eq!(found[0].name, "my file.pdf");
    }

    #[test]
    fn large_cid_image_counts_toward_the_paperclip() {
        // BODYSTRUCTURE reports the base64 size: 4 characters per 3 bytes, padded.
        let octets = INLINE_ATTACHMENT_MIN.div_ceil(3) * 4;
        let raw = format!(
            concat!(
                "* 1 FETCH (BODYSTRUCTURE (",
                "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
                "(\"IMAGE\" \"JPEG\" (\"NAME\" \"pic.jpg\") \"<pic>\" NIL \"BASE64\" {} NIL ",
                "(\"INLINE\" (\"FILENAME\" \"pic.jpg\")) NIL NIL)",
                " \"RELATED\" (\"BOUNDARY\" \"r\") NIL NIL NIL))\r\n",
            ),
            octets
        );
        assert!(structure_has_attachment(&bodystructure(&raw)));
    }

    #[test]
    fn cid_image_is_embedded_in_the_html_body() {
        let body = extract_body(CID_NEWSLETTER.as_bytes());
        // Nothing can fetch a `cid:` URL, so the reference must be gone.
        assert!(!body.contains("cid:logo"), "body was: {body}");
        assert!(
            body.contains("src=\"data:image/png;base64,iVBORw0KGgo=\""),
            "body was: {body}"
        );
    }

    #[test]
    fn gmail_related_photo_renders_the_picture_not_its_filename() {
        let body = extract_body(GMAIL_RELATED_PHOTO.as_bytes());
        assert!(!body.contains("cid:ii_mslv0bge0"), "body was: {body}");
        assert!(
            body.contains("src=\"data:image/jpeg;base64,/9j/4AAQSkZJRg==\""),
            "body was: {body}"
        );
        // The rest of the sender's markup survives the rewrite.
        assert!(body.contains("width=\"420\""), "body was: {body}");
        // The image is rendered in place, so it isn't appended a second time.
        assert_eq!(
            body.matches("/9j/4AAQSkZJRg==").count(),
            1,
            "body was: {body}"
        );
    }

    #[test]
    fn body_search_keys_quote_and_declare_charset() {
        // #191: ASCII needles go as a plain quoted string over the listed
        // uids; quotes and backslashes are escaped; anything beyond ASCII
        // puts CHARSET UTF-8 at the head of the keys; line breaks vanish.
        assert_eq!(
            body_search_query("1:5,9", "unsubscribe"),
            r#"UID 1:5,9 BODY "unsubscribe""#
        );
        assert_eq!(
            body_search_query("1", r#"say "hi" \now"#),
            r#"UID 1 BODY "say \"hi\" \\now""#
        );
        assert_eq!(
            body_search_query("1", "café"),
            r#"CHARSET UTF-8 UID 1 BODY "café""#
        );
        assert_eq!(body_search_query("1", "a\r\nb"), r#"UID 1 BODY "ab""#);
    }

    #[test]
    fn percent_encoded_cid_reference_still_resolves() {
        let raw = concat!(
            "Content-Type: multipart/related; boundary=R\r\n\r\n",
            "--R\r\n",
            "Content-Type: text/html\r\n\r\n",
            "<img src=\"cid:part1%40mail\">\r\n",
            "--R\r\n",
            "Content-Type: image/png\r\n",
            "Content-ID: <part1@mail>\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\n",
            "iVBORw0KGgo=\r\n",
            "--R--\r\n",
        );
        let body = extract_body(raw.as_bytes());
        assert!(body.contains("data:image/png;base64,"), "body was: {body}");
    }

    #[test]
    fn cid_link_target_is_not_rewritten() {
        let raw = concat!(
            "Content-Type: multipart/related; boundary=R\r\n\r\n",
            "--R\r\n",
            "Content-Type: text/html\r\n\r\n",
            "<a href=\"cid:logo\"><img src=\"cid:logo\"></a>\r\n",
            "--R\r\n",
            "Content-Type: image/png\r\n",
            "Content-ID: <logo>\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\n",
            "iVBORw0KGgo=\r\n",
            "--R--\r\n",
        );
        let body = extract_body(raw.as_bytes());
        assert!(body.contains("href=\"cid:logo\""), "body was: {body}");
        assert!(body.contains("src=\"data:image/png;"), "body was: {body}");
    }

    #[test]
    fn unresolvable_cid_reference_is_left_untouched() {
        let raw = b"Content-Type: text/html\r\n\r\n<p>see cid:nope</p><img src=\"cid:gone\">";
        let body = extract_body(raw);
        // A cid with no matching part stays as it was — and a bare mention in prose
        // is never treated as a resource reference.
        assert_eq!(body, "<p>see cid:nope</p><img src=\"cid:gone\">");
    }

    #[test]
    fn lone_html_part_passes_through_untouched() {
        let raw = b"Content-Type: text/html\r\n\r\n<p>hello</p>";
        assert_eq!(extract_body(raw), "<p>hello</p>");
    }

    #[test]
    fn plain_text_only_message_still_renders() {
        let raw = b"Content-Type: text/plain\r\n\r\nhello <there>";
        let body = extract_body(raw);
        assert!(body.contains("hello &lt;there&gt;"), "body was: {body}");
    }

    /// Parse a real server BODYSTRUCTURE response into the structure our IMAP
    /// summary path sees.
    fn bodystructure(raw: &str) -> async_imap::imap_proto::types::BodyStructure<'_> {
        use async_imap::imap_proto::{parser::parse_response, AttributeValue, Response};
        let (_, resp) = parse_response(raw.as_bytes()).expect("parses");
        match resp {
            Response::Fetch(_, attrs) => attrs
                .into_iter()
                .find_map(|a| match a {
                    AttributeValue::BodyStructure(bs) => Some(bs),
                    _ => None,
                })
                .expect("has BODYSTRUCTURE"),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[test]
    fn inline_iphone_photo_counts_as_an_attachment() {
        // Apple marks the JPEG `INLINE` with no Content-ID; requiring a disposition
        // of `attachment` missed it, so no paperclip and no download.
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"us-ascii\") NIL NIL \"7BIT\" 4 2 NIL NIL NIL NIL)",
            "(\"IMAGE\" \"JPEG\" (\"NAME\" \"a.jpeg\") NIL NIL \"BASE64\" 100 NIL ",
            "(\"INLINE\" (\"FILENAME\" \"a.jpeg\")) NIL NIL)",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"us-ascii\") NIL NIL \"7BIT\" 40 4 NIL NIL NIL NIL)",
            " \"MIXED\" (\"BOUNDARY\" \"b\") NIL NIL NIL))\r\n",
        ));
        assert!(structure_has_attachment(&bs));
    }

    #[test]
    fn cid_logo_does_not_count_as_an_attachment() {
        // Same inline disposition, but a Content-ID: it's rendered in the body.
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
            "(\"IMAGE\" \"PNG\" (\"NAME\" \"logo.png\") \"<logo>\" NIL \"BASE64\" 100 NIL ",
            "(\"INLINE\" (\"FILENAME\" \"logo.png\")) NIL NIL)",
            " \"RELATED\" (\"BOUNDARY\" \"r\") NIL NIL NIL))\r\n",
        ));
        assert!(!structure_has_attachment(&bs));
    }

    #[test]
    fn plain_alternative_has_no_attachment() {
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL NIL)",
            "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
            " \"ALTERNATIVE\" (\"BOUNDARY\" \"a\") NIL NIL NIL))\r\n",
        ));
        assert!(!structure_has_attachment(&bs));
    }

    /// A meeting request's calendar part is an attachment, however the
    /// sender labelled it (#223): it arrives as a plain alternative with no
    /// disposition at all, which is the one text part that still counts.
    #[test]
    fn a_calendar_part_counts_as_an_attachment() {
        let bs = bodystructure(concat!(
            "* 1 FETCH (BODYSTRUCTURE (",
            "(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL NIL)",
            "(\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 20 1 NIL NIL NIL NIL)",
            "(\"TEXT\" \"CALENDAR\" (\"CHARSET\" \"utf-8\" \"METHOD\" \"REQUEST\") NIL NIL ",
            "\"7BIT\" 900 20 NIL NIL NIL NIL)",
            " \"ALTERNATIVE\" (\"BOUNDARY\" \"a\") NIL NIL NIL))\r\n",
        ));
        assert!(structure_has_attachment(&bs));
        let metas = structure_attachments(&bs);
        assert_eq!(metas.len(), 1, "only the calendar part: {metas:?}");
        assert_eq!(metas[0].mime, "text/calendar");
        assert_eq!(metas[0].section, "3");
        // Unnamed by the sender, as Outlook sends it: named for its type, so
        // something will open it.
        assert!(metas[0].name.ends_with(".ics"), "{}", metas[0].name);
    }

    /// The same part, read out of the raw message rather than described by
    /// the server, and named the same way.
    #[test]
    fn an_unnamed_calendar_part_is_still_a_file() {
        let raw = concat!(
            "Content-Type: multipart/alternative; boundary=B\r\n",
            "Subject: Invitation\r\n\r\n",
            "--B\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nYou are invited.\r\n",
            "--B\r\nContent-Type: text/calendar; charset=utf-8; method=REQUEST\r\n\r\n",
            "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n--B--\r\n",
        );
        let found = extract_attachments(raw.as_bytes());
        assert_eq!(found.len(), 1);
        assert!(found[0].name.ends_with(".ics"), "{}", found[0].name);
    }

    /// An answer to an invitation goes out as plain text beside a
    /// `text/calendar` part that carries the iTIP method (#223) — a
    /// calendar that cannot find the method ignores the answer.
    #[test]
    fn an_invitation_answer_carries_its_calendar_part() {
        let account = sample_account();
        let msg = OutgoingMessage {
            to: "chair@example.org".into(),
            subject: "Accepted: Architecture sync".into(),
            body: "Jason has accepted this invitation.".into(),
            calendar: Some(CalendarPart {
                ics: "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n".into(),
                method: "REPLY".into(),
            }),
            ..sample_outgoing()
        };
        let raw = build_message(&account, &msg, false)
            .expect("builds")
            .formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/alternative"), "{text}");
        assert!(
            text.contains("text/calendar; charset=utf-8; method=REPLY"),
            "the method rides on the part: {text}"
        );
        assert!(text.contains("METHOD:REPLY"), "{text}");
        assert!(
            text.contains("Jason has accepted this invitation."),
            "{text}"
        );
    }

    #[test]
    fn image_mime_rejects_a_hostile_subtype() {
        // Guards the `data:` URI against a subtype that would break out of it.
        let raw = b"Content-Type: image/\"onerror=alert(1)\r\n\r\nx";
        let parsed = mail_parser::MessageParser::default()
            .parse(raw.as_slice())
            .unwrap();
        let part = parsed.html_bodies().next().unwrap();
        assert!(image_mime(part).is_none());
    }
}

#[cfg(test)]
mod uid_set_tests {
    use super::{imap_unquote, quote_mailbox, uid_set};

    #[test]
    fn mailbox_names_are_quoted_for_copy() {
        assert_eq!(quote_mailbox("Deleted Messages"), "\"Deleted Messages\"");
        assert_eq!(quote_mailbox("INBOX"), "\"INBOX\"");
        assert_eq!(quote_mailbox("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn listed_names_go_back_to_the_server_as_listed() {
        use async_imap::imap_proto::{MailboxDatum, Response};
        let line = b"* LIST (\\HasNoChildren) \"\\\\\" \"\\\\All\"\r\n";
        let Ok((
            _,
            Response::MailboxData(MailboxDatum::List {
                delimiter, name, ..
            }),
        )) = async_imap::imap_proto::parser::parse_response(line)
        else {
            panic!("LIST did not parse");
        };
        assert_eq!(imap_unquote(&name), "\\All");
        assert_eq!(
            delimiter.as_deref().map(imap_unquote).as_deref(),
            Some("\\")
        );
        assert_eq!(quote_mailbox(&imap_unquote(&name)), "\"\\\\All\"");
        assert_eq!(imap_unquote("[Gmail]/All Mail"), "[Gmail]/All Mail");
        assert_eq!(imap_unquote("a\\\"b\\x"), "a\"b\\x");
    }

    #[test]
    fn sets_are_sorted_deduplicated_and_ranged() {
        assert_eq!(uid_set(&[1005, 1003, 999, 1004]), "999,1003:1005");
        assert_eq!(uid_set(&[7, 7, 7]), "7");
        assert_eq!(uid_set(&[3, 4, 5, 9]), "3:5,9");
        assert_eq!(uid_set(&[]), "");
    }
}

#[cfg(test)]
mod attachment_scan_tests {
    use super::{mime_extension, structure_attachments, INLINE_ATTACHMENT_MIN};
    use async_imap::imap_proto::types::{
        BodyContentCommon, BodyContentSinglePart, BodyStructure, ContentDisposition,
        ContentEncoding, ContentType,
    };
    use std::borrow::Cow;

    fn ctype(
        ty: &'static str,
        subtype: &'static str,
        name: Option<&'static str>,
    ) -> ContentType<'static> {
        ContentType {
            ty: Cow::Borrowed(ty),
            subtype: Cow::Borrowed(subtype),
            params: name.map(|n| vec![(Cow::Borrowed("name"), Cow::Borrowed(n))]),
        }
    }

    fn disposition(
        ty: &'static str,
        filename: Option<&'static str>,
    ) -> ContentDisposition<'static> {
        ContentDisposition {
            ty: Cow::Borrowed(ty),
            params: filename.map(|f| vec![(Cow::Borrowed("filename"), Cow::Borrowed(f))]),
        }
    }

    fn single(
        ty: &'static str,
        subtype: &'static str,
        octets: u32,
        encoding: ContentEncoding<'static>,
        disp: Option<ContentDisposition<'static>>,
        content_id: Option<&'static str>,
        name: Option<&'static str>,
    ) -> BodyStructure<'static> {
        BodyStructure::Basic {
            common: BodyContentCommon {
                ty: ctype(ty, subtype, name),
                disposition: disp,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: content_id.map(Cow::Borrowed),
                md5: None,
                description: None,
                transfer_encoding: encoding,
                octets,
            },
            extension: None,
        }
    }

    fn text(subtype: &'static str, octets: u32) -> BodyStructure<'static> {
        BodyStructure::Text {
            common: BodyContentCommon {
                ty: ctype("text", subtype, None),
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::SevenBit,
                octets,
            },
            lines: 10,
            extension: None,
        }
    }

    fn multipart(bodies: Vec<BodyStructure<'static>>) -> BodyStructure<'static> {
        BodyStructure::Multipart {
            common: BodyContentCommon {
                ty: ctype("multipart", "mixed", None),
                disposition: None,
                language: None,
                location: None,
            },
            bodies,
            extension: None,
        }
    }

    /// The ordinary case: a message with a body and one attached file. The body
    /// is not an attachment; the file is, named from its disposition, and its
    /// section is what a FETCH would ask for.
    #[test]
    fn a_body_and_one_attachment_yields_just_the_attachment() {
        let bs = multipart(vec![
            text("plain", 400),
            single(
                "application",
                "pdf",
                4000,
                ContentEncoding::Base64,
                Some(disposition("attachment", Some("report.pdf"))),
                None,
                None,
            ),
        ]);
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "report.pdf");
        assert_eq!(found[0].mime, "application/pdf");
        assert_eq!(found[0].section, "2", "the second part of the multipart");
        assert_eq!(
            found[0].size, 3000,
            "base64 inflates by 4/3, so decode it back"
        );
    }

    /// A newsletter's logo: inline, small, with a Content-ID. Decoration, not a
    /// file anyone wants in their gallery.
    #[test]
    fn a_small_inline_image_with_a_content_id_is_decoration() {
        let bs = multipart(vec![
            text("html", 900),
            single(
                "image",
                "png",
                2000,
                ContentEncoding::Base64,
                None,
                Some("logo@x"),
                None,
            ),
        ]);
        assert!(structure_attachments(&bs).is_empty());
    }

    /// The same shape, but big enough to be a photograph someone sent: Apple
    /// Mail attaches pictures exactly this way, and they have to be saveable.
    #[test]
    fn a_large_inline_image_is_content_however_it_is_labelled() {
        let octets = (INLINE_ATTACHMENT_MIN as u32 + 1) * 4 / 3 + 8;
        let bs = multipart(vec![
            text("plain", 40),
            single(
                "image",
                "jpeg",
                octets,
                ContentEncoding::Base64,
                None,
                Some("img@x"),
                None,
            ),
        ]);
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].name, "attachment-1.jpg",
            "unnamed, so named by its type"
        );
    }

    /// Nested multiparts number their sections through: this is what an
    /// alternative body beside an attachment looks like.
    #[test]
    fn nested_parts_are_numbered_through() {
        let bs = multipart(vec![
            multipart(vec![text("plain", 100), text("html", 200)]),
            single(
                "application",
                "zip",
                900,
                ContentEncoding::Base64,
                Some(disposition("attachment", Some("bundle.zip"))),
                None,
                None,
            ),
        ]);
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].section, "2");
    }

    /// A text part is only an attachment when it says so — otherwise every
    /// message's own body would turn up in the gallery.
    #[test]
    fn a_text_part_counts_only_when_it_declares_itself() {
        let plain = multipart(vec![text("plain", 500)]);
        assert!(structure_attachments(&plain).is_empty());

        let attached = multipart(vec![BodyStructure::Text {
            common: BodyContentCommon {
                ty: ctype("text", "csv", None),
                disposition: Some(disposition("attachment", Some("rows.csv"))),
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::SevenBit,
                octets: 700,
            },
            lines: 9,
            extension: None,
        }]);
        let found = structure_attachments(&attached);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "rows.csv");
        assert_eq!(found[0].size, 700, "not base64, so the octets stand");
    }

    /// The filename falls back to the content type's own `name` parameter,
    /// which is where older clients put it.
    #[test]
    fn a_name_parameter_is_used_when_there_is_no_filename() {
        let bs = multipart(vec![
            text("plain", 10),
            single(
                "application",
                "msword",
                600,
                ContentEncoding::SevenBit,
                Some(disposition("attachment", None)),
                None,
                Some("minutes.doc"),
            ),
        ]);
        assert_eq!(structure_attachments(&bs)[0].name, "minutes.doc");
    }

    /// An RFC 2047 filename is decoded, not shown as the encoded word.
    #[test]
    fn an_encoded_filename_is_decoded() {
        let bs = multipart(vec![
            text("plain", 10),
            single(
                "application",
                "pdf",
                600,
                ContentEncoding::SevenBit,
                Some(disposition(
                    "attachment",
                    Some("=?utf-8?Q?rapport=20final?="),
                )),
                None,
                None,
            ),
        ]);
        assert_eq!(structure_attachments(&bs)[0].name, "rapport final");
    }

    /// A message that is not multipart at all is one part, section 1.
    #[test]
    fn a_single_part_message_is_section_one() {
        let bs = single(
            "application",
            "pdf",
            800,
            ContentEncoding::SevenBit,
            Some(disposition("attachment", Some("lone.pdf"))),
            None,
            None,
        );
        let found = structure_attachments(&bs);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].section, "1");
    }

    #[test]
    fn only_image_types_get_a_guessed_extension() {
        assert_eq!(mime_extension("image", "jpeg"), ".jpg");
        assert_eq!(mime_extension("IMAGE", "PNG"), ".png");
        assert_eq!(mime_extension("application", "pdf"), "");
        assert_eq!(mime_extension("image", "svg+xml"), "");
    }
}

#[cfg(test)]
mod fetch_mode_tests {
    use super::*;

    #[test]
    fn readable_error_keeps_the_first_line_of_the_reply() {
        let text = "Could not load INBOX: io: Error(Error { input: [42, 32, 55, 57, 56, 32, 70], code: Tag }) during parsing of \"* 798 FETCH (FLAGS (\\\\Seen) UID 4321 BODY[HEADER] {9821}\\r\\nReceived: from a\\r\\n\"";
        let out = readable_error(text);
        assert!(
            out.starts_with("Could not load INBOX: the server's reply could not be read"),
            "{out}"
        );
        assert!(
            out.contains("* 798 FETCH (FLAGS (\\\\Seen) UID 4321 BODY[HEADER] {9821}"),
            "{out}"
        );
        assert!(!out.contains("Received:"), "{out}");
        assert!(!out.contains("42, 32"), "{out}");
    }

    #[test]
    fn readable_error_leaves_other_errors_alone() {
        assert_eq!(
            readable_error("Could not load INBOX: connection lost"),
            "Could not load INBOX: connection lost"
        );
    }

    #[test]
    fn ladder_visits_every_way_of_asking_once() {
        // A fresh account id so the test does not share the set with others.
        let account_id = 900_226;
        let mut use_envelope = true;
        // 1 → 2: raw headers, previews kept.
        assert!(step_fetch_mode(account_id, &mut use_envelope));
        assert!(!use_envelope);
        // 2 → 3: back to ENVELOPE, previews dropped.
        assert!(step_fetch_mode(account_id, &mut use_envelope));
        assert!(use_envelope);
        assert!(!inline_previews_wanted(account_id));
        // 3 → 4: raw headers, still no previews.
        assert!(step_fetch_mode(account_id, &mut use_envelope));
        assert!(!use_envelope);
        // 4: nothing left.
        assert!(!step_fetch_mode(account_id, &mut use_envelope));
        assert!(!use_envelope);
    }
}

#[cfg(test)]
mod reader_preemption_tests {
    use super::*;

    fn body() -> MailRequest {
        MailRequest::LoadBody {
            message_id: 42,
            path: "INBOX".into(),
            uid: 42,
        }
    }

    #[test]
    fn a_refresh_tick_does_not_leave_a_folder_sync_ahead_of_the_reader() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(MailRequest::RefreshUnread).unwrap();
        tx.send(MailRequest::LoadMessages {
            folder_id: 1,
            path: "INBOX".into(),
        })
        .unwrap();
        tx.send(body()).unwrap();
        let mut backlog = std::collections::VecDeque::new();
        let first = reorder_reader_loads(
            MailRequest::SyncFolder {
                folder_id: 1,
                path: "INBOX".into(),
            },
            &mut rx,
            &mut backlog,
        );
        assert!(matches!(first, MailRequest::LoadBody { .. }));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::SyncFolder { .. })
        ));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::RefreshUnread)
        ));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::LoadMessages { .. })
        ));
    }

    #[test]
    fn overtaking_refresh_ticks_still_preserves_mutation_order() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(MailRequest::RefreshUnread).unwrap();
        tx.send(MailRequest::SetSeen {
            path: "INBOX".into(),
            uid: 42,
            seen: true,
        })
        .unwrap();
        tx.send(MailRequest::LoadMessages {
            folder_id: 1,
            path: "INBOX".into(),
        })
        .unwrap();
        tx.send(MailRequest::RefreshUnread).unwrap();
        tx.send(body()).unwrap();
        let mut backlog = std::collections::VecDeque::new();
        let first = reorder_reader_loads(
            MailRequest::SyncFolder {
                folder_id: 1,
                path: "INBOX".into(),
            },
            &mut rx,
            &mut backlog,
        );
        assert!(matches!(first, MailRequest::SyncFolder { .. }));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::RefreshUnread)
        ));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::SetSeen { .. })
        ));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::LoadBody { .. })
        ));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::LoadMessages { .. })
        ));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::RefreshUnread)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_reader_cancels_an_inflight_folder_read() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut backlog = std::collections::VecDeque::new();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(body()).unwrap();
        });
        let start = tokio::time::Instant::now();
        let result = interrupt_read_for_reader(
            tokio::time::sleep(Duration::from_secs(10)),
            &mut rx,
            &mut backlog,
        )
        .await;
        assert!(result.is_none());
        assert_eq!(start.elapsed(), Duration::from_millis(20));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::LoadBody { uid: 42, .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_reader_does_not_overtake_a_queued_mutation() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut backlog = std::collections::VecDeque::from([MailRequest::SetSeen {
            path: "INBOX".into(),
            uid: 42,
            seen: true,
        }]);
        tx.send(body()).unwrap();
        let result = interrupt_read_for_reader(
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                7
            },
            &mut rx,
            &mut backlog,
        )
        .await;
        assert_eq!(result, Some(7));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::SetSeen { .. })
        ));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::LoadBody { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn an_arriving_mutation_remains_a_barrier_to_a_later_reader() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut backlog = std::collections::VecDeque::new();
        tx.send(MailRequest::SetSeen {
            path: "INBOX".into(),
            uid: 42,
            seen: true,
        })
        .unwrap();
        tx.send(body()).unwrap();
        let result = interrupt_read_for_reader(
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                7
            },
            &mut rx,
            &mut backlog,
        )
        .await;
        assert_eq!(result, Some(7));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::SetSeen { .. })
        ));
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::LoadBody { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_an_owned_read_drops_the_connection() {
        use tokio::io::AsyncReadExt;
        let (client, mut server) = tokio::io::duplex(16);
        let mut session = Some(client);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut backlog = std::collections::VecDeque::new();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(body()).unwrap();
        });
        let result = interrupt_read_for_reader(
            async {
                let mut owned = session.take().unwrap();
                let result = owned.read(&mut [0]).await;
                session = Some(owned);
                result
            },
            &mut rx,
            &mut backlog,
        )
        .await;
        assert!(result.is_none());
        assert!(session.is_none());
        // EOF proves the reader cannot accidentally reuse an interrupted stream.
        assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
        assert!(matches!(
            backlog.pop_front(),
            Some(MailRequest::LoadBody { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_backfill_restores_drained_uids_without_duplicate_jobs() {
        let job = Backfill {
            folder_id: 1,
            path: "INBOX".into(),
            remaining: Some(vec![3, 2, 1]),
            gallery: false,
        };
        let mut queue = std::collections::VecDeque::from([job.clone()]);
        // Cancellation before the future's first poll leaves the job queued.
        restore_interrupted_backfill(&mut queue, job.clone());
        assert_eq!(queue.len(), 1);
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(body()).unwrap();
        });
        let result = interrupt_read_for_reader(
            async {
                let mut owned = queue.pop_front().unwrap();
                owned.remaining.as_mut().unwrap().drain(..2);
                std::future::pending::<()>().await;
            },
            &mut rx,
            &mut Default::default(),
        )
        .await;
        assert!(result.is_none());
        assert!(queue.is_empty());
        restore_interrupted_backfill(&mut queue, job);
        assert_eq!(queue.len(), 1);
        assert_eq!(
            queue.front().unwrap().remaining.as_deref(),
            Some([3, 2, 1].as_slice())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_queue_does_not_spin_or_cancel_the_read() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        drop(tx);
        let result = interrupt_read_for_reader(
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                7
            },
            &mut rx,
            &mut Default::default(),
        )
        .await;
        assert_eq!(result, Some(7));
    }
}

#[cfg(test)]
mod gmail_label_tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    async fn session() -> (Session<DuplexStream>, BufReader<DuplexStream>) {
        let (client, server) = tokio::io::duplex(4096);
        let login = tokio::spawn(async move {
            async_imap::Client::new(client)
                .login("test", "test")
                .await
                .unwrap()
        });
        let mut server = BufReader::new(server);
        let tag = command(&mut server, "LOGIN").await;
        server
            .get_mut()
            .write_all(format!("{tag} OK logged in\r\n").as_bytes())
            .await
            .unwrap();
        (login.await.unwrap(), server)
    }

    async fn command(server: &mut BufReader<DuplexStream>, expected: &str) -> String {
        let mut line = String::new();
        server.read_line(&mut line).await.unwrap();
        assert!(line.contains(expected), "{line:?}");
        line.split_whitespace().next().unwrap().to_string()
    }

    fn labelled(uid: u32, labels: &[&str]) -> Message {
        Message {
            id: uid,
            account_id: 7,
            folder_id: 1,
            uid,
            from_name: String::new(),
            from_addr: String::new(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: String::new(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 0,
            unread: false,
            starred: false,
            answered: false,
            forwarded: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: String::new(),
            references: String::new(),
            importance: Default::default(),
            due: 0,
            labels: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    /// Each message's X-GM-MSGID is read past the server's other news, and
    /// a server that is not Gmail says so with BAD (#471).
    #[tokio::test]
    async fn gmail_msgids_are_read_or_refused() {
        for (reply, want) in [
            (
                "* 3 FETCH (UID 7 X-GM-MSGID 1278455344230334865)\r\n\
                 * 12 EXISTS\r\n\
                 * 4 FETCH (UID 8 X-GM-MSGID 42)\r\n\
                 {tag} OK Success\r\n",
                Some(vec![1278455344230334865, 42]),
            ),
            ("{tag} BAD Unknown fetch item\r\n", None),
        ] {
            let (mut client, mut server) = session().await;
            let task = tokio::spawn(async move { fetch_gmail_msgids(&mut client, &[7, 8]).await });
            let tag = command(&mut server, "UID FETCH 7:8 (UID X-GM-MSGID)").await;
            server
                .get_mut()
                .write_all(reply.replace("{tag}", &tag).as_bytes())
                .await
                .unwrap();
            let got = tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("the read finished")
                .unwrap();
            assert_eq!(got.expect("read"), want);
        }
    }

    #[test]
    fn gmail_msgid_search_pairs_its_keys() {
        assert_eq!(gmail_msgid_query(&[1]), "X-GM-MSGID 1");
        assert_eq!(
            gmail_msgid_query(&[1, 2, 3]),
            "OR OR X-GM-MSGID 1 X-GM-MSGID 2 X-GM-MSGID 3"
        );
    }

    /// Read the labels for UIDs 7, 8 and 9 against a server that answers
    /// with `reply` (`{tag}` standing for the command's tag).
    async fn read(
        account_id: u32,
        reply: &str,
    ) -> (Result<(), async_imap::error::Error>, Vec<Message>) {
        GMAIL_LABELS.lock().unwrap().insert(account_id, true);
        let (mut client, mut server) = session().await;
        let mut msgs = vec![
            labelled(7, &["Old"]),
            labelled(8, &["Gone"]),
            labelled(9, &["Kept"]),
        ];
        let task = tokio::spawn(async move {
            let r =
                read_gmail_labels(account_id, &mut client, "INBOX", &[7, 8, 9], &mut msgs).await;
            (r, msgs)
        });
        let tag = command(&mut server, "UID FETCH 7:9 (UID X-GM-LABELS)").await;
        server
            .get_mut()
            .write_all(reply.replace("{tag}", &tag).as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the read finished")
            .unwrap()
    }

    fn labels_of(msgs: &[Message]) -> Vec<Vec<String>> {
        msgs.iter().map(|m| m.labels.clone()).collect()
    }

    /// The labels are read past what else the server says in between, a
    /// message left with none loses its old ones, and one the server said
    /// nothing about keeps what it had.
    #[tokio::test]
    async fn labels_are_read_past_unsolicited_replies() {
        let account_id = 900_374;
        let (r, msgs) = read(
            account_id,
            "* 3 FETCH (UID 7 X-GM-LABELS (\\Inbox \"My Label\"))\r\n\
             * 12 EXISTS\r\n\
             * 4 FETCH (UID 8 X-GM-LABELS ())\r\n\
             {tag} OK Success\r\n",
        )
        .await;
        r.expect("read");
        assert_eq!(
            labels_of(&msgs),
            [
                vec!["My Label".to_string()],
                vec![],
                vec!["Kept".to_string()]
            ]
        );
        assert!(gmail_labels_wanted(account_id));
    }

    /// A server that only looks like Gmail refuses the item: nothing
    /// changes, the session carries on, and it is not asked again.
    #[tokio::test]
    async fn a_refused_item_leaves_labels_alone_and_is_not_asked_again() {
        let account_id = 900_375;
        let (r, msgs) = read(account_id, "{tag} BAD Unknown FETCH item\r\n").await;
        r.expect("a refusal is no error");
        assert_eq!(
            labels_of(&msgs),
            [
                vec!["Old".to_string()],
                vec!["Gone".to_string()],
                vec!["Kept".to_string()]
            ]
        );
        assert!(!gmail_labels_wanted(account_id));
    }

    /// An unreadable reply wedges the stream: the caller has to reconnect,
    /// but not move its summary fetch down the ladder (#226), and the retry
    /// must not ask for labels again. A label sent as a literal is one such
    /// reply: imap-proto reads labels as atoms and quoted strings only.
    #[tokio::test]
    async fn an_unreadable_reply_asks_for_a_reconnect_without_labels() {
        let account_id = 900_376;
        let (r, mut msgs) = read(
            account_id,
            "* 3 FETCH (UID 7 X-GM-LABELS ({5}\r\nHello))\r\n{tag} OK Success\r\n",
        )
        .await;
        let e = r.expect_err("the stream cannot be trusted");
        assert!(!is_parse_error(&e), "{e}");
        assert_eq!(labels_of(&msgs)[0], ["Old"]);
        assert!(!gmail_labels_wanted(account_id));
        // The retry on a fresh connection sends the server nothing.
        let (mut client, _server) = session().await;
        read_gmail_labels(account_id, &mut client, "INBOX", &[7], &mut msgs)
            .await
            .expect("skipped");
    }
}

/// Pins of current behaviour for the pure helpers the restructure moves
/// (Area B, step B0): quirks included, so a move that changes one fails.
#[cfg(test)]
mod characterization_tests {
    use super::*;
    use crate::worker::headers::label_from_timestamp;
    use crate::worker::ids::first_msgid;
    use crate::worker::pop3::client::strip_crlf;
    use async_imap::types::{Flag, NameAttribute};
    use std::borrow::Cow;

    #[test]
    fn special_use_attributes_win_over_names_and_placement() {
        let cases = [
            (NameAttribute::Sent, FolderKind::Sent),
            (NameAttribute::Drafts, FolderKind::Drafts),
            (NameAttribute::Trash, FolderKind::Trash),
            (NameAttribute::Junk, FolderKind::Junk),
            (NameAttribute::Archive, FolderKind::Archive),
            (NameAttribute::Flagged, FolderKind::Starred),
        ];
        for (attr, kind) in cases {
            // Even deep in a hierarchy and under a name that says otherwise.
            assert_eq!(
                classify_with_source("a/b/c/Inbox", Some("/"), &[attr]),
                (kind, true)
            );
        }
        // `\All` and the other attributes are not mapped: the name decides.
        assert_eq!(
            classify_with_source("[Gmail]/All Mail", Some("/"), &[NameAttribute::All]),
            (FolderKind::Archive, false)
        );
        assert_eq!(
            classify_with_source("Lists", Some("/"), &[NameAttribute::All]),
            (FolderKind::Custom, false)
        );
        // The first mapped attribute decides.
        assert_eq!(
            classify_with_source(
                "X",
                Some("/"),
                &[
                    NameAttribute::Marked,
                    NameAttribute::Junk,
                    NameAttribute::Sent
                ]
            ),
            (FolderKind::Junk, true)
        );
    }

    #[test]
    fn folder_names_map_to_roles_where_servers_keep_them() {
        let kind = |p: &str, d: Option<&str>| classify_with_source(p, d, &[]).0;
        for (name, want) in [
            ("Sent Items", FolderKind::Sent),
            ("sent mail", FolderKind::Sent),
            ("Deleted", FolderKind::Trash),
            ("Deleted Items", FolderKind::Trash),
            ("Bin", FolderKind::Trash),
            ("Spam", FolderKind::Junk),
            ("All Mail", FolderKind::Archive),
            ("Archive", FolderKind::Archive),
            ("Flagged", FolderKind::Starred),
            ("Starred", FolderKind::Starred),
            ("Outbox", FolderKind::Custom),
        ] {
            assert_eq!(kind(name, Some("/")), want, "{name}");
        }
        // No delimiter (or an empty one): both `/` and `.` split levels.
        assert_eq!(kind("INBOX.Sent", None), FolderKind::Sent);
        assert_eq!(kind("INBOX/Sent", Some("")), FolderKind::Sent);
        assert_eq!(kind("a.b.Sent", None), FolderKind::Custom);
        // A bracketed container, not only Gmail's, places a role.
        assert_eq!(kind("[Mail]/Trash", Some("/")), FolderKind::Trash);
        assert_eq!(kind("Mail/Trash", Some("/")), FolderKind::Custom);
    }

    #[test]
    fn folder_order_is_the_persisted_numbering_order() {
        let order = [
            FolderKind::Inbox,
            FolderKind::Starred,
            FolderKind::Drafts,
            FolderKind::Templates,
            FolderKind::Sent,
            FolderKind::Archive,
            FolderKind::Junk,
            FolderKind::Trash,
            FolderKind::Custom,
        ];
        for (i, kind) in order.into_iter().enumerate() {
            assert_eq!(folder_order(kind), i as u8);
        }
    }

    #[test]
    fn display_name_without_a_delimiter_splits_on_slash_and_dot() {
        assert_eq!(display_name("inbox", None), "Inbox");
        assert_eq!(display_name("a.b/c", None), "c");
        assert_eq!(display_name("a.b", Some("/")), "a.b");
        assert_eq!(display_name("a/b.c", Some("")), "c");
    }

    #[test]
    fn thread_ids_list_in_reply_to_before_references() {
        let raw = b"Message-ID: <ABC@Example.COM>\r\n\
In-Reply-To: <Parent@x>\r\n\
References: <Root@x> <PARENT@x>\r\n\
Subject: t\r\n\r\nbody\r\n";
        let parsed = mail_parser::MessageParser::default().parse(&raw[..]);
        assert_eq!(
            mp_thread_ids(parsed.as_ref()),
            ("abc@example.com".to_string(), "parent@x root@x".to_string())
        );
        assert_eq!(mp_thread_ids(None), (String::new(), String::new()));
    }

    #[test]
    fn a_pop3_summary_comes_from_the_raw_message() {
        let raw = b"From: Ann <ann@x.example>\r\n\
To: b@y.example, c@z.example\r\n\
Cc: d@w.example\r\n\
Reply-To: ANN@x.example\r\n\
Subject: Hello\r\n\
Date: Tue, 1 Jul 2003 10:52:37 +0200\r\n\
Message-ID: <M1@x>\r\n\
X-Priority: 1\r\n\r\nbody\r\n";
        let m = summary_from_raw(3, 1, 42, raw);
        assert_eq!((m.id, m.account_id, m.folder_id, m.uid), (42, 3, 1, 42));
        assert_eq!(m.from_name, "Ann");
        assert_eq!(m.from_addr, "ann@x.example");
        assert_eq!(m.to, "b@y.example, c@z.example");
        assert_eq!(m.cc, "d@w.example");
        // A Reply-To that only repeats From is dropped.
        assert_eq!(m.reply_to, "");
        assert_eq!(m.subject, "Hello");
        assert_eq!(m.timestamp, 1057049557);
        assert_eq!(m.date, label_from_timestamp(1057049557));
        assert_eq!(m.message_id, "m1@x");
        assert!(m.unread);
        assert!(!m.starred && !m.answered && !m.has_attachment);
        assert!(m.preview.is_empty() && m.body.is_empty());

        let bare = summary_from_raw(3, 1, 7, b"\r\nno headers\r\n");
        assert_eq!(bare.from_name, "Unknown");
        assert_eq!(bare.from_addr, "");
        assert_eq!(bare.subject, "(no subject)");
        assert_eq!(bare.timestamp, 0);

        let nameless = summary_from_raw(3, 1, 7, b"From: a@x\r\n\r\n");
        assert_eq!(nameless.from_name, "a@x");
    }

    #[test]
    fn first_msgid_is_lowercased_without_brackets() {
        assert_eq!(first_msgid("<A@X> <b@y>").as_deref(), Some("a@x"));
        assert_eq!(first_msgid("  plain@id  ").as_deref(), Some("plain@id"));
        assert_eq!(first_msgid("<>"), None);
        assert_eq!(first_msgid(""), None);
    }

    #[test]
    fn gmail_labels_shown_skip_system_labels_the_folder_and_repeats() {
        let labels = ["\\Inbox", "Work", "", "INBOX", "work", "Work", "Travel"];
        assert_eq!(
            shown_gmail_labels(labels.into_iter(), "inbox"),
            ["Work", "work", "Travel"]
        );
    }

    #[test]
    fn kept_flags_keep_the_ones_an_append_can_set() {
        let flags = vec![
            Flag::Seen,
            Flag::Deleted,
            Flag::Recent,
            Flag::Custom(Cow::Borrowed("$Label")),
            Flag::Custom(Cow::Borrowed("\\Weird")),
            Flag::Draft,
            Flag::Answered,
            Flag::Flagged,
            Flag::MayCreate,
        ];
        assert_eq!(
            kept_flags(flags.into_iter()),
            ["\\Seen", "$Label", "\\Draft", "\\Answered", "\\Flagged"]
        );
    }

    #[test]
    fn xoauth2_sends_the_bearer_once_then_an_empty_answer() {
        use async_imap::Authenticator;
        let mut auth = XOAuth2 {
            user: "me@x".into(),
            token: "tok".into(),
            step: 0,
        };
        assert_eq!(
            auth.process(b""),
            b"user=me@x\x01auth=Bearer tok\x01\x01".to_vec()
        );
        assert_eq!(auth.process(b"{\"status\":\"400\"}"), Vec::<u8>::new());
        assert_eq!(auth.process(b""), Vec::<u8>::new());
    }

    #[test]
    fn strip_crlf_drops_every_trailing_line_end() {
        assert_eq!(strip_crlf(b"+OK\r\n"), b"+OK");
        assert_eq!(strip_crlf(b"a\n\r\n"), b"a");
        assert_eq!(strip_crlf(b"\r\n"), b"");
        assert_eq!(strip_crlf(b"a\rb"), b"a\rb");
    }

    #[test]
    fn dates_parse_or_fall_back_to_the_text() {
        assert_eq!(
            format_date(b" Tue, 1 Jul 2003 10:52:37 +0200 ").1,
            1057049557
        );
        assert_eq!(format_date(b" not a date "), ("not a date".to_string(), 0));
        assert_eq!(format_timestamp(0), "");
        assert_eq!(format_timestamp(-5), "");
        assert_eq!(
            format_timestamp(1057049557),
            label_from_timestamp(1057049557)
        );
    }

    #[test]
    fn system_keywords_are_told_from_tags() {
        for k in [
            "\\Seen",
            "$Junk",
            "NonJunk",
            "$Forwarded",
            "$MDNSent",
            "$MailFlagBit0",
            "$X-ME-Annot-2",
            "$IsNotification",
        ] {
            assert!(is_system_keyword(k), "{k}");
        }
        for k in ["$label1", "Work", "$Important", "important"] {
            assert!(!is_system_keyword(k), "{k}");
        }
    }

    #[test]
    fn the_wire_log_shows_three_words_of_a_command() {
        assert_eq!(cmd_head("UID FETCH 1:5 (FLAGS)"), "UID FETCH 1:5");
        assert_eq!(cmd_head("  NOOP "), "NOOP");
        assert_eq!(cmd_head(""), "");
    }
}
