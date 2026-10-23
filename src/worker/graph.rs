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
use crate::worker::graph::folders::{graph_poll_inbox, refresh_graph_folders};
use crate::worker::graph::http::graph_token;
use crate::worker::graph::index::{backfill_pause, graph_backfill_step, GraphState};
use crate::worker::graph::ops::graph_fetch_raw;
use crate::worker::graph::send::graph_flush_outbox;

mod chores;
mod folders;
mod handle_read;
mod handle_send;
mod handle_write;
mod http;
mod index;
mod messages;
mod ops;
mod send;
use crate::worker::outbox::{delete_queued, emit_outbox};

/// A label whose name the message leaves out is named from the
/// organization's list (#411) on its way to the reader, whether the
/// check was just made or comes from the cache, which keeps the header's.
fn name_labels(account: &AccountConfig, emit: &impl Fn(WorkerEvent), mut ev: WorkerEvent) {
    if let WorkerEvent::SenderChecked { check, .. } = &mut ev {
        if let Some(label) = check.label.as_mut() {
            crate::graph_labels::fill(account, label);
        }
    }
    emit(ev)
}

/// A Microsoft account's worker: the Graph state (folders, uid map, the
/// background index) and what it reports through.
struct GraphWorker<E> {
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    /// The UI's callback; events go out through [`name_labels`].
    emit: E,
    cache: Option<Cache>,
    state: GraphState,
    /// When the Junk / Trash auto-empty (#140) last ran for this worker.
    last_auto_empty: Option<std::time::Instant>,
}

/// Run a Microsoft account until the UI drops its sender.
pub(super) async fn run_graph(
    account_id: u32,
    account: AccountConfig,
    rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    GraphWorker::start(account_id, account, rx, emit)
        .await
        .run()
        .await;
}

impl<E: Fn(WorkerEvent)> GraphWorker<E> {
    /// Show the account and its cached folders, then fetch a token and the
    /// live folder list.
    async fn start(
        account_id: u32,
        account: AccountConfig,
        rx: mpsc::UnboundedReceiver<MailRequest>,
        emit: E,
    ) -> Self {
        let cache = Cache::open()
            .map_err(|e| tracing::warn!("cache unavailable: {e}"))
            .ok();
        let mut worker = GraphWorker {
            account_id,
            state: GraphState::new(&account),
            account,
            rx,
            emit,
            cache,
            last_auto_empty: None,
        };
        let account_id = worker.account_id;
        let account = &worker.account;
        let cache = &worker.cache;
        let emit = &|ev| name_labels(&worker.account, &worker.emit, ev);
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

        // Fetch a token and the folder list. A GOA token failure here is the one
        // users actually hit (signed out in GNOME Settings), so say that.
        if let Some(token) = graph_token(account, emit).await {
            refresh_graph_folders(&token, account_id, cache.as_ref(), &mut worker.state, emit)
                .await;
        }
        worker
    }

    /// Serve requests until the UI hangs up; between them, poll the Inbox
    /// and index the mailbox in the background.
    async fn run(mut self) {
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
            let indexing = !self.state.backfill.is_empty()
                && crate::power::reaches_internet()
                && !crate::power::saving_data();
            let wait = self.state.backfill_wait;
            let account_id = self.account_id;
            let emit = &|ev| name_labels(&self.account, &self.emit, ev);
            let req = tokio::select! {
                biased;
                req = self.rx.recv() => match req {
                    Some(req) => req,
                    None => break,
                },
                _ = poll.tick() => {
                    if !crate::power::reaches_internet() {
                        continue;
                    }
                    graph_poll_inbox(account_id, &self.account, self.cache.as_ref(), &mut self.state, emit).await;
                    continue;
                }
                _ = backfill_pause(wait), if indexing => {
                    graph_backfill_step(&self.account, account_id, self.cache.as_ref(), &mut self.state, emit).await;
                    continue;
                }
            };
            self.handle(req).await;
        }
    }

    /// The handler for each kind of request.
    async fn handle(&mut self, req: MailRequest) {
        // A folder the user opened, rather than a background refresh.
        let user = matches!(req, MailRequest::LoadMessages { .. });
        let account_id = self.account_id;
        let cache = &self.cache;
        let emit = &|ev| name_labels(&self.account, &self.emit, ev);
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
            MailRequest::FindKeywords => self.on_find_keywords().await,
            MailRequest::RefreshKeywords { .. } => self.on_refresh_keywords().await,
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
            // Cache-only, exactly like the IMAP path: assemble the conversation
            // from every folder's cached summaries.
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
                self.on_load_messages(folder_id, path, user).await
            }
            MailRequest::LoadBody {
                message_id,
                path,
                uid,
            } => self.on_load_body(message_id, path, uid).await,
            MailRequest::LoadBodies { items, path } => self.on_load_bodies(items, path).await,
            MailRequest::LoadSource {
                message_id: _,
                path,
                uid,
            } => match graph_fetch_raw(&self.account, &mut self.state, &path, uid, emit).await {
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
                self.on_load_attachments(message_id, path, uid, download)
                    .await
            }
            MailRequest::SetSeen { path, uid, seen } => self.on_set_seen(path, uid, seen).await,
            MailRequest::SetFlagged { path, uid, flagged } => {
                self.on_set_flagged(path, uid, flagged).await
            }
            MailRequest::SetKeyword {
                path,
                uid,
                keyword,
                add,
                ..
            } => self.on_set_keyword(path, uid, keyword, add).await,
            MailRequest::MarkAllRead { folder_id, path } => {
                self.on_mark_all_read(folder_id, path).await
            }
            MailRequest::MoveMessage { path, uid, dest } => {
                self.on_move_message(path, uid, dest).await
            }
            MailRequest::MarkSpam { path, uid, dest } => self.on_mark_spam(path, uid, dest).await,
            MailRequest::MarkHam { path, uid, dest } => self.on_mark_ham(path, uid, dest).await,
            MailRequest::MarkHamMany { path, uids, dest } => {
                self.on_mark_ham_many(path, uids, dest).await
            }
            MailRequest::MoveMessages { path, uids, dest } => {
                self.on_move_messages(path, uids, dest).await
            }
            MailRequest::PurgeMessages { path, uids } => self.on_purge_messages(path, uids).await,
            MailRequest::EmptyFolder { folder_id, path } => {
                self.on_empty_folder(folder_id, path).await
            }
            MailRequest::UndoMove {
                path,
                dest,
                dest_folder_id,
                message_ids,
            } => {
                self.on_undo_move(path, dest, dest_folder_id, message_ids)
                    .await
            }
            MailRequest::CreateFolder { path } => self.on_create_folder(path).await,
            MailRequest::RenameFolder { old_path, new_path } => {
                self.on_rename_folder(old_path, new_path).await
            }
            MailRequest::SetHiddenFolders { paths } => self.on_set_hidden_folders(paths).await,
            MailRequest::DeleteFolder { path, trash: _ } => self.on_delete_folder(path).await,
            MailRequest::SaveDraft {
                message,
                folder_id,
                path,
                autosave,
                template,
            } => {
                self.on_save_draft(message, folder_id, path, autosave, template)
                    .await
            }
            // `sent_path` is unused: Graph's sendMail files the Sent copy itself.
            // Send Later (#145): into the Outbox until its time.
            MailRequest::Send {
                message,
                sent_path: _,
                ticket,
            } if message.send_at.is_some_and(|t| t > crate::datefmt::now()) => {
                self.on_send_later(message, ticket).await
            }
            MailRequest::Send {
                message,
                sent_path: _,
                ticket,
            } => self.on_send(message, ticket).await,
            MailRequest::LoadOutbox => emit_outbox(cache.as_ref(), account_id, emit),
            MailRequest::DeleteOutbox { id } => delete_queued(cache.as_ref(), account_id, id, emit),
            MailRequest::FlushOutbox { id } => {
                graph_flush_outbox(
                    cache.as_ref(),
                    account_id,
                    &self.account,
                    id,
                    &mut self.state,
                    emit,
                )
                .await;
            }
            MailRequest::RefreshUnread => self.on_refresh_unread().await,
            MailRequest::Reconnect => {
                if let Some(token) = graph_token(&self.account, emit).await {
                    refresh_graph_folders(
                        &token,
                        account_id,
                        cache.as_ref(),
                        &mut self.state,
                        emit,
                    )
                    .await;
                }
            }
            MailRequest::Settle { path, uids } => emit(WorkerEvent::MovesSettled { path, uids }),
            MailRequest::ExportRaw {
                token, path, uid, ..
            } => {
                let raw = graph_fetch_raw(&self.account, &mut self.state, &path, uid, emit).await;
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
            MailRequest::DeleteAttachment {
                message_id,
                path,
                uid,
                name,
                size,
            } => {
                self.on_delete_attachment(message_id, path, uid, name, size)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::models::Message;
    use crate::worker::graph::index::graph_backfill_with;
    use crate::worker::graph::index::graph_index_plan;
    use crate::worker::graph::index::graph_sync_folder;
    use crate::worker::graph::index::GraphRun;
    use crate::worker::graph::index::GRAPH_BACKFILL_CHUNK;
    use crate::worker::graph::messages::graph_due;
    use crate::worker::graph::messages::graph_find_by_msgid;
    use crate::worker::graph::messages::graph_id_page;
    use crate::worker::graph::messages::graph_ids_url;
    use crate::worker::graph::messages::graph_message;
    use crate::worker::graph::messages::GRAPH_FIRST_PAGE;
    use crate::worker::ids::hash_uid;
    use crate::worker::index_queue::IndexQueue;
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
