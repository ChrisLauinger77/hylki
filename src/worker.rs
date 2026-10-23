//! Background mail worker.
//!
//! IMAP is async and its sessions are stateful, while the relm4 UI is driven
//! synchronously on the GTK main thread. To bridge the two, [`spawn`] starts a
//! dedicated OS thread running a tokio runtime that owns the IMAP session. The
//! UI sends [`MailRequest`]s over an unbounded channel; the worker performs the
//! network I/O and pushes [`WorkerEvent`]s back via a caller-supplied callback
//! (in practice, the component's input sender). The mock path implements the
//! exact same protocol so the app behaves identically offline.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::models::{Account, FolderKind, KeywordFinding};

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
mod imap;
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
pub(crate) use auth::{forget_access_token, oauth_token_blocking};
pub(crate) use compose::build::sent_body_preview;
pub use compose::editable::editable_from_raw;
pub use compose::recipients::{
    clean_recipient_field, format_recipient, invalid_recipient, parse_recipients,
};
pub use conn_test::{test_alias_smtp_blocking, test_connection_blocking, ConnTest};
pub use folders::chip_counts_all;
pub(crate) use folders::folder_order;
use graph::run_graph;
pub(crate) use headers::{clean_display_name, decode_header, unescape_quoted_str};
pub use imap::fetch_mode::readable_error;
pub(crate) use imap::gmail::has_gmail_labels;
pub use imap::keywords::is_system_keyword;
use imap::run_imap;
use lane::{cache_lane, related_from_cache, thread_summaries_with_members};
use mock::run_mock;
pub(crate) use outbox::{answered_copies, park_for_next_launch, reply_copy_path};
use pop3::run_pop3;
pub use render::attachments::extract_attachments_of;
pub use render::body::extract_body;
pub(crate) use render::body::wrap_plain;
pub(crate) use render::render_raw;
pub(crate) use summary::summary_from_raw;
pub use tls::is_hostname_mismatch;

/// What a connection that was never tried, because the network is down,
/// reports (#442).
pub(crate) fn offline_text() -> String {
    i18n("no network connection")
}

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
    use crate::models::Folder;
    use crate::models::MailboxQuota;
    use crate::models::Message;
    use crate::worker::auth::sign_in_password_blocking;
    use crate::worker::chores::auto_empty_due;
    use crate::worker::chores::auto_empty_roles;
    use crate::worker::chores::clean_lists_due;
    use crate::worker::chores::clean_lists_query;
    use crate::worker::chores::graph_before_date;
    use crate::worker::chores::imap_before_date;
    use crate::worker::chores::role_folder_path;
    use crate::worker::chores::AUTO_EMPTY_MIN;
    use crate::worker::compose::build::build_draft;
    use crate::worker::compose::build::build_email;
    use crate::worker::compose::build::build_message;
    use crate::worker::compose::editable::queued_with_bcc;
    use crate::worker::compose::editable::with_bcc;
    use crate::worker::compose::recipients::mailbox;
    use crate::worker::folders::classify_with_source;
    use crate::worker::folders::display_name;
    use crate::worker::headers::unescape_quoted;
    use crate::worker::ids::merge_msgids;
    use crate::worker::ids::normalize_msgid;
    use crate::worker::ids::normalize_msgids;
    use crate::worker::imap::bodies::load_raw;
    use crate::worker::imap::connect::connect;
    use crate::worker::imap::connect::imap_uses_starttls;
    use crate::worker::imap::folders::imap_storage_quota;
    use crate::worker::imap::folders::list_folders;
    use crate::worker::imap::gmail::gmail_labels_of;
    use crate::worker::imap::idle::idle_wait;
    use crate::worker::imap::idle::missed_new_mail;
    use crate::worker::imap::idle::note_synced_uid_next;
    use crate::worker::imap::idle::IdleOutcome;
    use crate::worker::imap::mutate::import_flags;
    use crate::worker::imap::mutate::kept_flags;
    use crate::worker::imap::mutate::purge_messages;
    use crate::worker::imap::mutate::rename_folder;
    use crate::worker::imap::prefetch::attachment_prefetch_tried;
    use crate::worker::imap::prefetch::note_attachment_prefetch_tried;
    use crate::worker::imap::prefetch::queue_attachment_prefetch;
    use crate::worker::imap::send::append_draft;
    use crate::worker::imap::send::append_to_sent;
    use crate::worker::imap::send::delete_draft;
    use crate::worker::imap::send::file_sent_copy;
    use crate::worker::imap::strip_attachment::find_appended;
    use crate::worker::imap::strip_attachment::strip_imap_attachment;
    use crate::worker::imap::structure::structure_attachments;
    use crate::worker::imap::structure::structure_has_attachment;
    use crate::worker::imap::summary::address_parts;
    use crate::worker::imap::summary::header_fields;
    use crate::worker::imap::sweep::body_search_query;
    use crate::worker::imap::sync::load_messages;
    use crate::worker::imap::sync::merge_index;
    use crate::worker::imap::watch::watchable;
    use crate::worker::imap::wire::append_dated;
    use crate::worker::imap::wire::create_box;
    use crate::worker::imap::wire::delete_box;
    use crate::worker::imap::wire::fetch_uids;
    use crate::worker::imap::wire::sel;
    use crate::worker::imap::wire::subscribe_box;
    use crate::worker::imap::ImapSession;
    use crate::worker::outbox::outbox_envelope;
    use crate::worker::outbox::pick_queued_sent_copy;
    use crate::worker::outbox::pick_reply_folder;
    use crate::worker::outbox::raw_in_reply_to;
    use crate::worker::outbox::send_failed;
    use crate::worker::preview::charset_param;
    use crate::worker::preview::looks_like_base64;
    use crate::worker::preview::mime_header;
    use crate::worker::preview::pgp_preview;
    use crate::worker::preview::preview_from_part;
    use crate::worker::preview::text_like;
    use crate::worker::preview::PREVIEW_CHARS;
    use crate::worker::render::attachments::extract_attachments;
    use crate::worker::render::attachments::INLINE_ATTACHMENT_MIN;
    use crate::worker::render::body::image_mime;
    use crate::worker::render::body::linkify;
    use crate::worker::smtp::alias_with_own_smtp;
    use crate::worker::smtp::send_smtp;
    use crate::worker::smtp::SmtpTls;
    use crate::worker::tls::is_loopback_host;
    use async_imap::types::Fetch;
    use futures::TryStreamExt;

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
            let dest = reply_copy_path(&account, 1, Some(&cache), &reply.in_reply_to);
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
            reply_copy_path(
                &account(ReplyCopies::WithSent, false),
                1,
                Some(&cache),
                "a@b"
            ),
            None
        );
        assert_eq!(
            reply_copy_path(
                &account(ReplyCopies::Original, true),
                1,
                Some(&cache),
                "a@b"
            ),
            None
        );
        assert_eq!(
            reply_copy_path(&account(ReplyCopies::Original, false), 1, Some(&cache), ""),
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
            reply_copy_path(&on, 1, Some(&cache), "<Parent@Example.com>").as_deref(),
            Some("Projects/Hylki"),
        );
        assert_eq!(
            reply_copy_path(&on, 2, Some(&cache), "parent@example.com"),
            None
        );
        assert_eq!(
            reply_copy_path(&on, 1, Some(&cache), "other@example.com"),
            None
        );
    }

    #[test]
    fn a_queued_reply_names_what_it_answers() {
        let raw =
            b"From: a@example.com\r\nTo: b@example.com\r\nIn-Reply-To: <Parent@Example.com>\r\n\
                    Subject: Re: x\r\n\r\nbody\r\n";
        assert_eq!(raw_in_reply_to(raw), "Parent@Example.com");
        assert_eq!(raw_in_reply_to(b"Subject: x\r\n\r\nbody\r\n"), "");
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
        let told = preview_from_part(&part, Some("UTF-8"), Some("quoted-printable"));
        assert!(
            !told.contains('\u{fffd}'),
            "no replacement characters: {told}"
        );
        assert!(told.starts_with("AI Bot Controls"), "{told}");
        assert!(told.contains("Updates to managing AI crawlers"), "{told}");

        // And the same left to the guess, now that the guess knows better.
        let guessed = preview_from_part(&part, Some("UTF-8"), None);
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
        let out = preview_from_part(cut, Some("UTF-8"), Some("quoted-printable"));
        assert!(
            !out.contains('='),
            "no half-decoded escape left behind: {out:?}"
        );
        assert_eq!(out.trim(), "AI Bot Controls");
        let cut = b"AI Bot Controls =E2=80=87=CD=8F =";
        let out = preview_from_part(cut, Some("UTF-8"), Some("quoted-printable"));
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

        let preview = preview_from_part(&body, None, None);
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
mod reader_preemption_tests {
    use super::*;
    use crate::worker::imap::backfill::restore_interrupted_backfill;
    use crate::worker::imap::backfill::Backfill;
    use crate::worker::preempt::interrupt_read_for_reader;
    use crate::worker::preempt::reorder_reader_loads;

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

/// Pins of current behaviour for the pure helpers the restructure moves
/// (Area B, step B0): quirks included, so a move that changes one fails.
#[cfg(test)]
mod characterization_tests {
    use super::*;
    use crate::worker::folders::classify_with_source;
    use crate::worker::folders::display_name;
    use crate::worker::headers::format_date;
    use crate::worker::headers::format_timestamp;
    use crate::worker::headers::label_from_timestamp;
    use crate::worker::ids::first_msgid;
    use crate::worker::imap::gmail::shown_gmail_labels;
    use crate::worker::imap::keywords::is_system_keyword;
    use crate::worker::imap::mutate::kept_flags;
    use crate::worker::imap::wire::cmd_head;
    use crate::worker::pop3::client::strip_crlf;
    use crate::worker::summary::mp_thread_ids;
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
