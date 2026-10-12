//! The EWS account loop (#391): the Microsoft 365 backend's shape, with an
//! Exchange server's own operations behind each request.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc;

use super::client::{Client, EwsError};
use super::http::{Connection, Endpoint, TlsOptions};
use super::ops::{self, Change, ItemRef};
use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::models::{Account, Folder, FolderKind, Message};
use crate::worker::{
    accent_for, auto_empty_due, auto_empty_roles, cache_rewritten, role_folder_path, body_cacheable, build_draft, build_email, chip_count_of, delete_queued,
    draft_message_id, draft_not_saved, drop_superseded_outbox, emit_outbox, extract_attachments, hash_uid,
    parse_recipients, related_from_cache, render_raw, restore_msgid_case, saving_text, schedule_send, send_failed,
    strip_refused, thread_summaries_with_members, MailRequest, OutgoingMessage, WorkerEvent,
};

struct State {
    account_id: u32,
    client: Client,
    /// Folder path → (folder id, EWS folder id), from the last listing.
    folders: HashMap<String, (u32, String)>,
    /// Message uid (hashed item id) → the item, from the last listings.
    items: HashMap<u32, ItemRef>,
    /// The Drafts and Inbox folders' (id, path).
    drafts: Option<(u32, String)>,
    inbox: Option<(u32, String)>,
    junk: Option<String>,
    /// Manual Special Folders assignments (#82) and hidden folders (#239).
    roles: std::collections::BTreeMap<String, String>,
    hidden: Vec<String>,
    /// When Junk and Trash were last emptied of old mail (#140).
    last_auto_empty: Option<std::time::Instant>,
}

/// The connection an account's settings describe: its EWS address, held
/// where IMAP's host is, and its login, the user name or else the address.
fn client_for(account: &AccountConfig) -> Result<Client, String> {
    let endpoint = Endpoint::parse(&account.imap_host)
        .ok_or_else(|| i18n("This Exchange account has no server address. Add it in its settings."))?;
    Ok(Client::new(Connection::new(endpoint, tls_of(account), login_of(account), &account.password)))
}

fn tls_of(account: &AccountConfig) -> TlsOptions {
    TlsOptions {
        accept_hostname_mismatch: account.tls_accept_hostname_mismatch,
        accept_invalid_certs: account.security.as_ref().is_some_and(|s| s.imap_accept_invalid_certs),
    }
}

fn login_of(account: &AccountConfig) -> &str {
    if account.username.trim().is_empty() { account.email.as_str() } else { account.username.trim() }
}

/// An account saved without a server address gets one: Autodiscover's, or
/// failing that the usual `mail.<domain>` address if it answers. `Some` is
/// the address found.
async fn discover_if_needed(account: &mut AccountConfig) -> Result<Option<String>, String> {
    if !account.imap_host.trim().is_empty() {
        return Ok(None);
    }
    let tls = tls_of(account);
    let found = super::autodiscover::discover(&account.email, login_of(account), &account.password, tls).await;
    let url = match found {
        Ok(url) => url,
        Err(why) => {
            tracing::info!(target: "hylki::ews", "autodiscover for {}: {why}", account.email);
            let guess = super::autodiscover::guess(&account.email).unwrap_or_default();
            let mut probe = account.clone();
            probe.imap_host = guess.clone();
            let mut client = client_for(&probe)?;
            if ops::list_folders(&mut client, 0, &Default::default()).await.is_err() {
                return Err(i18n_f(
                    "Could not find the Exchange server for {email}: {why}. Enter its EWS address, such as https://mail.example.org/EWS/Exchange.asmx.",
                    &[("email", &account.email), ("why", &why)],
                ));
            }
            guess
        }
    };
    tracing::info!(target: "hylki::ews", "{}: EWS at {url}", account.email);
    account.imap_host = url.clone();
    Ok(Some(url))
}

/// What a failed request says to the user.
fn say(e: &EwsError, account: &AccountConfig) -> String {
    if e.is_auth() {
        return i18n_f(
            "The Exchange server refused the sign-in for {email}. Check the user name and password in its settings.",
            &[("email", &account.email)],
        );
    }
    e.to_string()
}

/// Report a failure, `what` putting the reason into its sentence (a literal
/// each, so the string extractor sees them).
fn report(e: &EwsError, account: &AccountConfig, what: impl Fn(&str) -> String, emit: &impl Fn(WorkerEvent)) {
    let text = what(&say(e, account));
    emit(if e.is_network() { WorkerEvent::net_error(text) } else { WorkerEvent::error(text) });
}

impl State {
    fn adopt(&mut self, list: &[ops::EwsFolder]) {
        self.folders = list.iter().map(|f| (f.folder.path.clone(), (f.folder.id, f.ews_id.clone()))).collect();
        self.drafts =
            list.iter().find(|f| f.folder.kind == FolderKind::Drafts).map(|f| (f.folder.id, f.folder.path.clone()));
        self.inbox =
            list.iter().find(|f| f.folder.kind == FolderKind::Inbox).map(|f| (f.folder.id, f.folder.path.clone()));
        self.junk = list.iter().find(|f| f.folder.kind == FolderKind::Junk).map(|f| f.folder.path.clone());
    }

    fn chip_count(&self, folder_id: u32, messages: &[Message]) -> u32 {
        let kind = match &self.drafts {
            Some((id, _)) if *id == folder_id => FolderKind::Drafts,
            _ => FolderKind::Custom,
        };
        chip_count_of(kind, messages)
    }

    /// List the folders again; the list, and with `counts` each folder's
    /// count (the only event that can set a real zero), go to the app.
    async fn refresh_folders(&mut self, cache: Option<&Cache>, counts: bool) -> Result<Vec<Folder>, EwsError> {
        let mut list = ops::list_folders(&mut self.client, self.account_id, &self.roles).await?;
        list.retain(|f| !crate::models::folder_is_hidden(&f.folder.path, Some("/"), &self.hidden));
        self.adopt(&list);
        let folders: Vec<Folder> = list.into_iter().map(|f| f.folder).collect();
        if let Some(c) = cache {
            c.save_folders(self.account_id, &folders);
        }
        let _ = counts;
        Ok(folders)
    }

    async fn folder_ews_id(&mut self, path: &str, cache: Option<&Cache>) -> Result<String, EwsError> {
        if !self.folders.contains_key(path) {
            self.refresh_folders(cache, false).await?;
        }
        self.folders
            .get(path)
            .map(|(_, id)| id.clone())
            .ok_or_else(|| EwsError::Parse(i18n_f("unknown folder {path}", &[("path", path)])))
    }

    /// A folder's newest messages: listed with their state, the ones the
    /// cache lacks fetched in full, the rest refreshed from the listing. The
    /// cache then holds exactly the listing.
    async fn load_folder(&mut self, folder_id: u32, path: &str, cache: Option<&Cache>) -> Result<Vec<Message>, EwsError> {
        let ews_id = self.folder_ews_id(path, cache).await?;
        let listed = ops::list_items(&mut self.client, &ews_id, ops::INDEX_CAP).await?;
        let mut cached: HashMap<u32, Message> = cache
            .map(|c| c.load_messages(self.account_id, path, folder_id))
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m.uid, m))
            .collect();
        let missing: Vec<ItemRef> = listed
            .iter()
            .filter(|l| !cached.contains_key(&hash_uid(&l.item.id)))
            .map(|l| l.item.clone())
            .collect();
        let mut fresh: HashMap<u32, Message> = HashMap::new();
        if !missing.is_empty() {
            for (m, item) in ops::get_summaries(&mut self.client, &missing, self.account_id, folder_id).await? {
                self.items.insert(m.uid, item);
                fresh.insert(m.uid, m);
            }
        }
        let mut messages = Vec::with_capacity(listed.len());
        for l in listed {
            let uid = hash_uid(&l.item.id);
            self.items.insert(uid, l.item);
            let m = match fresh.remove(&uid) {
                Some(m) => m,
                None => match cached.remove(&uid) {
                    Some(mut m) => {
                        m.folder_id = folder_id;
                        m.unread = l.unread;
                        if let Some(s) = l.starred {
                            m.starred = s;
                        }
                        m.keywords = l.categories;
                        m
                    }
                    None => continue,
                },
            };
            messages.push(m);
        }
        if let Some(c) = cache {
            c.save_messages(self.account_id, path, &messages);
        }
        Ok(messages)
    }

    /// The item a uid stands for, listing its folder again once if it is
    /// not known (a fresh start from the cache, or a message moved).
    async fn resolve(&mut self, path: &str, uid: u32, cache: Option<&Cache>) -> Option<ItemRef> {
        if let Some(i) = self.items.get(&uid) {
            return Some(i.clone());
        }
        let folder_id = self.folders.get(path).map(|(id, _)| *id).unwrap_or(0);
        let _ = self.load_folder(folder_id, path, cache).await;
        self.items.get(&uid).cloned()
    }

    async fn resolve_all(&mut self, path: &str, uids: &[u32], cache: Option<&Cache>) -> Vec<(u32, ItemRef)> {
        let mut out = Vec::with_capacity(uids.len());
        for &uid in uids {
            if let Some(i) = self.resolve(path, uid, cache).await {
                out.push((uid, i));
            }
        }
        out
    }

    async fn fetch_raw(&mut self, path: &str, uid: u32, cache: Option<&Cache>) -> Result<Vec<u8>, String> {
        let item = self.resolve(path, uid, cache).await.ok_or_else(|| i18n("message not found"))?;
        ops::fetch_mime(&mut self.client, &item).await.map_err(|e| e.to_string())
    }

    async fn set(&mut self, path: &str, uids: &[u32], change: Change, cache: Option<&Cache>) {
        let items: Vec<ItemRef> = self.resolve_all(path, uids, cache).await.into_iter().map(|(_, i)| i).collect();
        if items.is_empty() {
            return;
        }
        if let Err(e) = ops::update(&mut self.client, &items, change).await {
            tracing::warn!("ews: could not update {} message(s): {e}", items.len());
        }
    }

    /// Move messages to another folder. Each gets a new id there; the moved
    /// ones leave the source's cache, and the destination picks them up at
    /// its next listing.
    async fn move_uids(&mut self, path: &str, uids: &[u32], dest: &str, cache: Option<&Cache>) -> Result<(), EwsError> {
        let dest_id = self.folder_ews_id(dest, cache).await?;
        let found = self.resolve_all(path, uids, cache).await;
        if found.is_empty() {
            return Ok(());
        }
        let items: Vec<ItemRef> = found.iter().map(|(_, i)| i.clone()).collect();
        let moved = ops::move_items(&mut self.client, &items, &dest_id).await?;
        for ((uid, _), new) in found.iter().zip(moved) {
            self.items.remove(uid);
            if let Some(new) = new {
                self.items.insert(hash_uid(&new.id), new);
            }
            if let Some(c) = cache {
                c.delete_message(self.account_id, path, *uid);
            }
        }
        Ok(())
    }

    /// Report messages as junk or as wanted, which teaches the server's
    /// filter and moves them; a server without the operation (gromox,
    /// Exchange 2010) has them moved to `dest` plainly.
    async fn junk(&mut self, path: &str, uids: &[u32], dest: &str, junk: bool, cache: Option<&Cache>) -> Result<(), EwsError> {
        // The operation moves to Junk Email or back to the Inbox: for any
        // other destination (a folder assigned the role by hand), a move.
        let standard = if junk {
            self.junk.as_deref() == Some(dest)
        } else {
            self.inbox.as_ref().is_some_and(|(_, p)| p == dest)
        };
        if !standard {
            return self.move_uids(path, uids, dest, cache).await;
        }
        let found = self.resolve_all(path, uids, cache).await;
        if found.is_empty() {
            return Ok(());
        }
        let items: Vec<ItemRef> = found.iter().map(|(_, i)| i.clone()).collect();
        let moved = match ops::mark_as_junk(&mut self.client, &items, junk).await {
            Ok(moved) => moved,
            Err(e) => {
                tracing::info!("ews: MarkAsJunk refused ({e}), moving instead");
                vec![None; items.len()]
            }
        };
        let mut left = Vec::new();
        for ((uid, _), new) in found.iter().zip(moved.into_iter().chain(std::iter::repeat(None))) {
            match new {
                Some(new) => {
                    self.items.remove(uid);
                    self.items.insert(hash_uid(&new.id), new);
                    if let Some(c) = cache {
                        c.delete_message(self.account_id, path, *uid);
                    }
                }
                None => left.push(*uid),
            }
        }
        if left.is_empty() { Ok(()) } else { self.move_uids(path, &left, dest, cache).await }
    }

    async fn purge(&mut self, path: &str, uids: &[u32], cache: Option<&Cache>) -> Result<(), EwsError> {
        let found = self.resolve_all(path, uids, cache).await;
        let items: Vec<ItemRef> = found.iter().map(|(_, i)| i.clone()).collect();
        ops::delete_items(&mut self.client, &items, true).await?;
        for (uid, _) in found {
            self.items.remove(&uid);
            if let Some(c) = cache {
                c.delete_message(self.account_id, path, uid);
            }
        }
        Ok(())
    }

    /// Delete the draft a message was opened from, once it has been sent,
    /// scheduled or saved again.
    async fn drop_draft_origin(&mut self, origin: &crate::models::DraftOrigin, cache: Option<&Cache>) {
        if origin.account_id != self.account_id {
            return;
        }
        if let Err(e) = self.purge(&origin.path, &[origin.uid], cache).await {
            tracing::warn!("ews: could not delete the previous draft: {e}");
        }
    }

    /// Re-list a folder and hand it to the app.
    async fn reload(&mut self, folder_id: u32, path: &str, cache: Option<&Cache>, emit: &impl Fn(WorkerEvent)) {
        if let Ok(messages) = self.load_folder(folder_id, path, cache).await {
            let unread = self.chip_count(folder_id, &messages);
            emit(WorkerEvent::Messages { folder_id, messages });
            emit(WorkerEvent::FolderUnread { folder_id, unread });
        }
    }
}

/// The recipients a MIME message leaves out: its blind copies. Bcc is not
/// written into the message, so the server is told them alongside it.
fn bcc_of(message: &OutgoingMessage) -> Vec<String> {
    parse_recipients(&message.bcc).into_iter().map(|(_, addr)| addr).filter(|a| !a.is_empty()).collect()
}

pub(in crate::worker) async fn run_ews(
    account_id: u32,
    mut account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    // The password lives in the keyring, like an IMAP account's.
    if account.password.is_empty() {
        if let Some(pw) = crate::config::load_password(&account.email) {
            account.password = pw;
        }
    }
    let cache = Cache::open().map_err(|e| tracing::warn!("cache unavailable: {e}")).ok();
    let cache = cache.as_ref();

    emit(WorkerEvent::Account(Account {
        id: account_id,
        name: account.name.clone(),
        email: account.email.clone(),
        label: account.display_label(),
        accent: accent_for(account_id).into(),
    }));
    let cached_folders = cache.map(|c| c.load_folders(account_id)).unwrap_or_default();
    if !cached_folders.is_empty() {
        emit(WorkerEvent::Folders(cached_folders));
    }

    if let Err(e) = discover_if_needed(&mut account).await {
        emit(WorkerEvent::error(e));
    }
    let client = match client_for(&account) {
        Ok(c) => c,
        Err(e) => {
            emit(WorkerEvent::error(e.clone()));
            // Nothing can be fetched: answer what waits on an answer.
            while let Some(req) = rx.recv().await {
                if let MailRequest::LoadMessages { folder_id, .. } | MailRequest::SyncFolder { folder_id, .. } = req {
                    emit(WorkerEvent::BackfillDone { folder_id });
                }
            }
            return;
        }
    };
    let mut state = State {
        account_id,
        client,
        folders: HashMap::new(),
        items: HashMap::new(),
        drafts: None,
        inbox: None,
        junk: None,
        roles: account.folder_roles.clone(),
        hidden: account.hidden_folders.clone(),
        last_auto_empty: None,
    };
    match state.refresh_folders(cache, false).await {
        Ok(folders) => emit(WorkerEvent::Folders(folders)),
        Err(e) => report(&e, &account, |e| i18n_f("Could not list folders: {e}", &[("e", e)]), &emit),
    }

    // No push yet: new mail arrives on a poll, at the auto-fetch interval
    // when that is on, else every couple of minutes.
    let poll_secs = match crate::config::load_privacy().fetch_interval_secs {
        0 => 120,
        s => s.max(60),
    };
    let mut poll = tokio::time::interval(Duration::from_secs(poll_secs));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    poll.tick().await;

    // Push (Settings → "Instant new mail", or the account's own switch): a
    // streaming subscription on a second connection. While it is up, the
    // poll only backs it up.
    let (push_tx, mut push_rx) = tokio::sync::mpsc::unbounded_channel();
    let push_enabled = account.push.unwrap_or_else(|| crate::config::load_privacy().push);
    let push_task = match (push_enabled, client_for(&account)) {
        (true, Ok(client)) => Some(tokio::spawn(super::push::run(client, push_tx))),
        _ => None,
    };

    loop {
        let req = tokio::select! {
            req = rx.recv() => match req {
                Some(req) => req,
                None => break,
            },
            _ = poll.tick() => {
                poll_inbox(&mut state, cache, &emit).await;
                continue;
            }
            Some(event) = push_rx.recv() => {
                match event {
                    super::push::Push::Up => {
                        poll = tokio::time::interval(Duration::from_secs(PUSHED_POLL_SECS));
                        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        poll.tick().await;
                    }
                    super::push::Push::Down => {
                        poll = tokio::time::interval(Duration::from_secs(poll_secs));
                        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        poll.tick().await;
                    }
                    super::push::Push::Changed(mut changed) => {
                        // One change is often several events (the item, the
                        // folder's count): a moment's wait gathers them.
                        tokio::time::sleep(Duration::from_millis(PUSH_SETTLE_MS)).await;
                        while let Ok(super::push::Push::Changed(more)) = push_rx.try_recv() {
                            changed.extend(more);
                        }
                        pushed(&mut state, &changed, cache, &emit).await;
                    }
                }
                continue;
            }
        };
        handle(req, &mut state, &account, cache, &emit).await;
    }
    if let Some(task) = push_task {
        task.abort();
    }
}

/// How often the poll runs while push is up: a backstop only.
const PUSHED_POLL_SECS: u64 = 900;
/// How long a change is given for its sibling events to arrive.
const PUSH_SETTLE_MS: u64 = 1500;

/// Folders the server said changed: each one Hylki lists is listed again,
/// and every folder's count follows.
async fn pushed(state: &mut State, changed: &[String], cache: Option<&Cache>, emit: &impl Fn(WorkerEvent)) {
    let folders: Vec<(u32, String)> = state
        .folders
        .iter()
        .filter(|(_, (_, id))| changed.iter().any(|c| c == id))
        .map(|(path, (folder_id, _))| (*folder_id, path.clone()))
        .collect();
    if folders.is_empty() {
        return;
    }
    tracing::debug!(target: "hylki::ews", "push: {} folder(s) changed", folders.len());
    for (folder_id, path) in folders {
        state.reload(folder_id, &path, cache, emit).await;
    }
    refresh_counts(state, cache, emit).await;
}

/// List the folders again after a change to them, and hand the list on.
async fn refresh_and_emit(state: &mut State, account: &AccountConfig, cache: Option<&Cache>, emit: &impl Fn(WorkerEvent)) {
    match state.refresh_folders(cache, false).await {
        Ok(folders) => emit(WorkerEvent::Folders(folders)),
        Err(e) => report(&e, account, |e| i18n_f("Could not list folders: {e}", &[("e", e)]), emit),
    }
}

/// Junk and Trash emptied of mail older than the account's settings ask
/// (#140), at most so often.
async fn auto_empty(state: &mut State, account: &AccountConfig, cache: Option<&Cache>, emit: &impl Fn(WorkerEvent)) {
    if !auto_empty_due(account, &mut state.last_auto_empty) {
        return;
    }
    let folders = cache.map(|c| c.load_folders(state.account_id)).unwrap_or_default();
    let mut purged = false;
    for (kind, role, days) in auto_empty_roles(account) {
        let Some(path) = role_folder_path(account, &folders, kind, role) else { continue };
        let Ok(folder) = state.folder_ews_id(&path, cache).await else { continue };
        let before = crate::datefmt::now() - i64::from(days) * 86_400;
        let old = match ops::items_before(&mut state.client, &folder, before, 500).await {
            Ok(old) => old,
            Err(e) => {
                tracing::warn!("auto-empty: could not list {path}: {e}");
                continue;
            }
        };
        if old.is_empty() {
            continue;
        }
        match ops::delete_items(&mut state.client, &old, true).await {
            Ok(()) => {
                tracing::info!("auto-empty: deleted {} message(s) older than {days} days from {path}", old.len());
                if let Some(c) = cache {
                    for item in &old {
                        c.delete_message(state.account_id, &path, hash_uid(&item.id));
                    }
                }
                purged = true;
            }
            Err(e) => tracing::warn!("auto-empty: could not delete from {path}: {e}"),
        }
    }
    if purged {
        refresh_counts(state, cache, emit).await;
    }
}

/// One poll: the Inbox again, then every folder's count.
async fn poll_inbox(state: &mut State, cache: Option<&Cache>, emit: &impl Fn(WorkerEvent)) {
    if state.inbox.is_none() && state.refresh_folders(cache, false).await.is_err() {
        return;
    }
    if let Some((folder_id, path)) = state.inbox.clone() {
        state.reload(folder_id, &path, cache, emit).await;
    }
    refresh_counts(state, cache, emit).await;
}

/// Every folder's count, quietly: this rides the background poll.
async fn refresh_counts(state: &mut State, cache: Option<&Cache>, emit: &impl Fn(WorkerEvent)) {
    if let Ok(folders) = state.refresh_folders(cache, true).await {
        let counts: Vec<(u32, u32)> = folders.iter().map(|f| (f.id, f.unread)).collect();
        emit(WorkerEvent::Folders(folders));
        for (folder_id, unread) in counts {
            emit(WorkerEvent::FolderUnread { folder_id, unread });
        }
    }
}

async fn emit_body(
    state: &mut State,
    account: &AccountConfig,
    cache: Option<&Cache>,
    message_id: u32,
    path: &str,
    uid: u32,
    emit: &impl Fn(WorkerEvent),
) {
    if let Some(body) = cache.and_then(|c| c.load_body(state.account_id, path, uid)) {
        let check = cache.and_then(|c| c.load_sender_check(state.account_id, path, uid));
        emit(WorkerEvent::Body { message_id, path: path.to_string(), body });
        if let Some(check) = check {
            emit(WorkerEvent::SenderChecked { path: path.to_string(), message_id, check });
        }
        return;
    }
    match state.fetch_raw(path, uid, cache).await {
        Ok(raw) => {
            let (body, check, _) = render_raw(&raw);
            if let Some(c) = cache {
                if body_cacheable(&check) {
                    c.save_body(state.account_id, path, uid, &body);
                }
                c.save_sender_check(state.account_id, path, uid, &check);
            }
            emit(WorkerEvent::Body { message_id, path: path.to_string(), body });
            emit(WorkerEvent::SenderChecked { path: path.to_string(), message_id, check });
        }
        Err(e) => {
            let _ = account;
            emit(WorkerEvent::net_error(i18n_f("Could not load message: {e}", &[("e", &e)])))
        }
    }
}

async fn handle(
    req: MailRequest,
    state: &mut State,
    account: &AccountConfig,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let account_id = state.account_id;
    match req {
        MailRequest::Locate { message_id } => emit(WorkerEvent::Located { message_id, hit: None }),
        MailRequest::Quota => emit(WorkerEvent::Quota(None)),
        MailRequest::FindKeywords => {
            // Categories ride on the messages: the ones in use are those the
            // cache has seen.
            let found = cache
                .map(|c| c.keyword_counts(account_id))
                .unwrap_or_default()
                .into_iter()
                .map(|(keyword, count)| crate::models::KeywordFinding {
                    name: Some(keyword.clone()),
                    keyword,
                    color: None,
                    count,
                    folders: Vec::new(),
                })
                .collect();
            emit(WorkerEvent::KeywordsFound(found));
        }
        MailRequest::RefreshKeywords { .. } => {
            // Categories come with each listing, so listing is the sync.
            let mut paths = Vec::new();
            let folders: Vec<(String, u32)> = state.folders.iter().map(|(p, (id, _))| (p.clone(), *id)).collect();
            for (path, folder_id) in folders {
                if state.load_folder(folder_id, &path, cache).await.is_ok() {
                    paths.push(path);
                }
            }
            emit(WorkerEvent::KeywordsSynced { paths });
        }
        MailRequest::ScanAttachments { folder_path } => {
            emit(WorkerEvent::AttachmentsScanned { folder_path, added: 0, remaining: 0 });
        }
        MailRequest::LoadRelated { message_id, ids } => {
            let messages = cache.map(|c| related_from_cache(c, account_id, &ids)).unwrap_or_default();
            emit(WorkerEvent::Related { message_id, messages });
        }
        MailRequest::LoadThreadSummaries { groups } => {
            let summaries = cache.map(|c| thread_summaries_with_members(c, account_id, &groups)).unwrap_or_default();
            emit(WorkerEvent::ThreadSummaries { summaries });
        }

        MailRequest::LoadMessages { folder_id, path } | MailRequest::SyncFolder { folder_id, path } => {
            if let Some(c) = cache {
                let cached = c.load_messages(account_id, &path, folder_id);
                if !cached.is_empty() {
                    emit(WorkerEvent::Messages { folder_id, messages: cached });
                }
            }
            emit(WorkerEvent::Status(i18n("Syncing…")));
            match state.load_folder(folder_id, &path, cache).await {
                Ok(messages) => {
                    let unread = state.chip_count(folder_id, &messages);
                    emit(WorkerEvent::Messages { folder_id, messages });
                    emit(WorkerEvent::FolderUnread { folder_id, unread });
                }
                Err(e) => report(&e, account, |e| i18n_f("Could not fetch mail: {e}", &[("e", e)]), emit),
            }
            // The newest messages are the whole index here: nothing more is
            // coming in the background.
            emit(WorkerEvent::BackfillDone { folder_id });
            emit(WorkerEvent::Status(String::new()));
        }

        MailRequest::LoadBody { message_id, path, uid } => {
            emit_body(state, account, cache, message_id, &path, uid, emit).await;
        }
        MailRequest::LoadBodies { items, path } => {
            for (message_id, uid) in items {
                emit_body(state, account, cache, message_id, &path, uid, emit).await;
            }
        }
        MailRequest::LoadSource { message_id: _, path, uid } => match state.fetch_raw(&path, uid, cache).await {
            Ok(raw) => emit(WorkerEvent::Source { text: String::from_utf8_lossy(&raw).into_owned() }),
            Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not load source: {e}", &[("e", &e)]))),
        },
        MailRequest::LoadAttachments { message_id, path, uid, download } => {
            if let Some(c) = cache {
                let items = c.load_attachments(account_id, &path, uid);
                if !items.is_empty() {
                    emit(WorkerEvent::Attachments { path, message_id, items });
                    return;
                }
            }
            if !download {
                emit(WorkerEvent::AttachmentsPending { path, message_id });
                return;
            }
            match state.fetch_raw(&path, uid, cache).await {
                Ok(raw) => {
                    let items = extract_attachments(&raw);
                    if let Some(c) = cache {
                        c.save_attachments(account_id, &path, uid, &items);
                    }
                    emit(WorkerEvent::Attachments { path, message_id, items });
                }
                Err(e) => emit(WorkerEvent::net_error(i18n_f("Could not load attachments: {e}", &[("e", &e)]))),
            }
        }

        MailRequest::SetSeen { path, uid, seen } => {
            if let Some(c) = cache {
                c.set_unread(account_id, &path, uid, !seen);
            }
            state.set(&path, &[uid], Change::Read(seen), cache).await;
            emit(WorkerEvent::SeenSettled { path, uid });
        }
        MailRequest::SetFlagged { path, uid, flagged } => {
            if let Some(c) = cache {
                c.set_starred(account_id, &path, uid, flagged);
            }
            state.set(&path, &[uid], Change::Flagged(flagged), cache).await;
        }
        MailRequest::SetKeyword { path, uid, keyword, add, .. } => {
            // Categories travel as the whole list: the cached row is the
            // base, patched first, then sent as it now stands.
            let categories = match cache {
                Some(c) => {
                    c.set_keyword(account_id, &path, uid, &keyword, add);
                    c.keywords_of(account_id, &path, uid)
                }
                None if add => vec![keyword.clone()],
                None => Vec::new(),
            };
            state.set(&path, &[uid], Change::Categories(categories), cache).await;
            emit(WorkerEvent::TagSettled { path, uid, keyword });
        }
        MailRequest::MarkAllRead { folder_id, path } => {
            let done = match state.folder_ews_id(&path, cache).await {
                Ok(id) => ops::mark_all_read(&mut state.client, &id).await.is_ok(),
                Err(_) => false,
            };
            // A server without the operation (Exchange 2010) marks each
            // listed unread message instead.
            if !done {
                let unread: Vec<u32> = cache
                    .map(|c| c.load_messages(account_id, &path, folder_id).into_iter().filter(|m| m.unread).map(|m| m.uid).collect())
                    .unwrap_or_default();
                state.set(&path, &unread, Change::Read(true), cache).await;
            }
            if let Some(c) = cache {
                c.mark_folder_read(account_id, &path);
            }
            emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
        }

        MailRequest::MoveMessage { path, uid, dest } => {
            if let Err(e) = state.move_uids(&path, &[uid], &dest, cache).await {
                report(&e, account, |e| i18n_f("Could not move message: {e}", &[("e", e)]), emit);
            }
        }
        MailRequest::MarkSpam { path, uid, dest } => {
            if let Err(e) = state.junk(&path, &[uid], &dest, true, cache).await {
                report(&e, account, |e| i18n_f("Could not mark as spam: {e}", &[("e", e)]), emit);
            }
        }
        MailRequest::MarkHam { path, uid, dest } => {
            if let Err(e) = state.junk(&path, &[uid], &dest, false, cache).await {
                report(&e, account, |e| i18n_f("Could not mark as not spam: {e}", &[("e", e)]), emit);
            }
        }
        MailRequest::MarkHamMany { path, uids, dest } => {
            if let Err(e) = state.junk(&path, &uids, &dest, false, cache).await {
                report(&e, account, |e| i18n_f("Could not mark as not spam: {e}", &[("e", e)]), emit);
            }
            emit(WorkerEvent::BulkComplete);
        }
        MailRequest::MoveMessages { path, uids, dest } => {
            if let Err(e) = state.move_uids(&path, &uids, &dest, cache).await {
                report(&e, account, |e| i18n_f("Could not move messages: {e}", &[("e", e)]), emit);
            }
            emit(WorkerEvent::BulkComplete);
        }
        MailRequest::PurgeMessages { path, uids } => {
            if let Err(e) = state.purge(&path, &uids, cache).await {
                report(&e, account, |e| i18n_f("Could not delete messages: {e}", &[("e", e)]), emit);
            }
            emit(WorkerEvent::BulkComplete);
        }
        MailRequest::EmptyFolder { folder_id, path } => {
            let r = match state.folder_ews_id(&path, cache).await {
                Ok(id) => ops::empty_folder(&mut state.client, &id).await,
                Err(e) => Err(e),
            };
            match r {
                Ok(()) => {
                    if let Some(c) = cache {
                        c.save_messages(account_id, &path, &[]);
                    }
                    emit(WorkerEvent::Messages { folder_id, messages: Vec::new() });
                    emit(WorkerEvent::FolderUnread { folder_id, unread: 0 });
                }
                Err(e) => report(&e, account, |e| i18n_f("Could not empty the folder: {e}", &[("e", e)]), emit),
            }
        }
        MailRequest::UndoMove { path, dest, dest_folder_id, message_ids } => {
            // The messages sit in `path` under new ids: found again by
            // Message-ID, and moved back.
            let folder_id = state.folders.get(&path).map(|(id, _)| *id).unwrap_or(0);
            match state.load_folder(folder_id, &path, cache).await {
                Ok(listed) => {
                    let uids: Vec<u32> = listed
                        .iter()
                        .filter(|m| message_ids.iter().any(|id| id == &m.message_id))
                        .map(|m| m.uid)
                        .collect();
                    if uids.is_empty() {
                        tracing::info!("undo: the messages are no longer where that move put them");
                    } else if let Err(e) = state.move_uids(&path, &uids, &dest, cache).await {
                        report(&e, account, |e| i18n_f("Undo failed: {e}", &[("e", e)]), emit);
                    } else {
                        state.reload(dest_folder_id, &dest, cache, emit).await;
                    }
                }
                Err(e) => report(&e, account, |e| i18n_f("Undo failed: {e}", &[("e", e)]), emit),
            }
            emit(WorkerEvent::BulkComplete);
        }

        MailRequest::CreateFolder { path } => {
            let (parent, name) = match path.rsplit_once('/') {
                Some((parent, leaf)) => (state.folders.get(parent).map(|(_, id)| id.clone()), leaf.to_string()),
                None => (None, path.clone()),
            };
            match ops::create_folder(&mut state.client, parent.as_deref(), &name).await {
                Ok(_) => refresh_and_emit(state, account, cache, emit).await,
                Err(e) => report(&e, account, |e| i18n_f("Could not create folder: {e}", &[("e", e)]), emit),
            }
        }
        MailRequest::RenameFolder { old_path, new_path } => {
            let Some((_, id)) = state.folders.get(&old_path).cloned() else {
                emit(WorkerEvent::error(i18n("Could not rename folder: unknown folder")));
                return;
            };
            let parent_of = |p: &str| p.rsplit_once('/').map(|(parent, _)| parent.to_string());
            let leaf = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
            let r = async {
                // A new parent is a move; a new last part, a rename.
                if parent_of(&old_path) != parent_of(&new_path) {
                    let dest = match parent_of(&new_path) {
                        Some(p) => Some(state.folder_ews_id(&p, cache).await?),
                        None => None,
                    };
                    ops::move_folder(&mut state.client, &id, dest.as_deref()).await?;
                }
                if leaf(&old_path) != leaf(&new_path) {
                    ops::rename_folder(&mut state.client, &id, &leaf(&new_path)).await?;
                }
                Ok::<(), EwsError>(())
            }
            .await;
            match r {
                Ok(()) => refresh_and_emit(state, account, cache, emit).await,
                Err(e) => report(&e, account, |e| i18n_f("Could not rename folder: {e}", &[("e", e)]), emit),
            }
        }
        MailRequest::DeleteFolder { path, trash: _ } => {
            // Exchange moves the folder, contents and all, into Deleted
            // Items itself.
            let Some((_, id)) = state.folders.get(&path).cloned() else {
                emit(WorkerEvent::error(i18n("Could not delete folder: unknown folder")));
                return;
            };
            match ops::delete_folder(&mut state.client, &id).await {
                Ok(()) => refresh_and_emit(state, account, cache, emit).await,
                Err(e) => report(&e, account, |e| i18n_f("Could not delete folder: {e}", &[("e", e)]), emit),
            }
        }
        MailRequest::SetHiddenFolders { paths } => {
            state.hidden = paths;
            match state.refresh_folders(cache, false).await {
                Ok(folders) => emit(WorkerEvent::Folders(folders)),
                Err(e) => report(&e, account, |e| i18n_f("Could not list folders: {e}", &[("e", e)]), emit),
            }
        }

        MailRequest::SaveDraft { message, folder_id, path, autosave, template } => {
            if autosave.is_none() {
                emit(WorkerEvent::Status(saving_text(template)));
            }
            let mut message = OutgoingMessage { sign: false, encrypt: false, ..*message };
            restore_msgid_case(cache, &mut message);
            let saved: Result<String, String> = async {
                let email = build_draft(account, &message).map_err(|e| e.to_string())?;
                let message_id = draft_message_id(&email);
                let folder = state.folder_ews_id(&path, cache).await.map_err(|e| say(&e, account))?;
                ops::save_mime(&mut state.client, &email.formatted(), &folder).await.map_err(|e| say(&e, account))?;
                Ok(message_id)
            }
            .await;
            emit(WorkerEvent::Status(String::new()));
            match saved {
                Ok(id) => {
                    if let Some(o) = &message.draft_origin {
                        state.drop_draft_origin(o, cache).await;
                    }
                    if let Ok(messages) = state.load_folder(folder_id, &path, cache).await {
                        emit(WorkerEvent::Messages { folder_id, messages });
                    }
                    emit(WorkerEvent::DraftSaved { autosave, message_id: Some(id) });
                }
                Err(why) => draft_not_saved(autosave, template, why, message, emit),
            }
        }

        // The server files the Sent copy itself, so `sent_path` goes unused.
        // Send Later (#145): into the Outbox until its time.
        MailRequest::Send { mut message, sent_path: _ } if message.send_at.is_some_and(|t| t > crate::datefmt::now()) => {
            let at = message.send_at.unwrap_or_default();
            restore_msgid_case(cache, &mut message);
            schedule_send(cache, account_id, account, &message, None, at, emit);
            if let Some(o) = &message.draft_origin {
                state.drop_draft_origin(o, cache).await;
            }
        }
        MailRequest::Send { mut message, sent_path: _ } => {
            emit(WorkerEvent::Status(i18n("Sending…")));
            restore_msgid_case(cache, &mut message);
            let sent: Result<(), String> = async {
                let email = build_email(account, &message).map_err(|e| e.to_string())?;
                ops::send_mime(&mut state.client, &email.formatted(), &bcc_of(&message))
                    .await
                    .map_err(|e| say(&e, account))
            }
            .await;
            emit(WorkerEvent::Status(String::new()));
            match sent {
                Ok(()) => {
                    if let Some(o) = message.draft_origin.clone() {
                        if o.account_id == account_id {
                            state.drop_draft_origin(&o, cache).await;
                            if let Ok(messages) = state.load_folder(o.folder_id, &o.path, cache).await {
                                emit(WorkerEvent::Messages { folder_id: o.folder_id, messages });
                            }
                        }
                    }
                    drop_superseded_outbox(cache, account_id, &message, emit);
                    // The copy is in Sent Items: list it, so it joins the
                    // conversation it answers (#199).
                    let sent = cache.and_then(|c| c.load_folders(account_id).into_iter().find(|f| f.kind == FolderKind::Sent));
                    if let Some(sent) = sent {
                        if let Ok(messages) = state.load_folder(sent.id, &sent.path, cache).await {
                            emit(WorkerEvent::Messages { folder_id: sent.id, messages });
                        }
                    }
                    emit(WorkerEvent::Sent);
                }
                Err(e) => send_failed(cache, account_id, account, &message, None, &e, false, emit),
            }
        }
        MailRequest::LoadOutbox => emit_outbox(cache, account_id, emit),
        MailRequest::DeleteOutbox { id } => delete_queued(cache, account_id, id, emit),
        MailRequest::FlushOutbox { id } => flush_outbox(state, cache, id, emit).await,

        MailRequest::RefreshUnread => {
            refresh_counts(state, cache, emit).await;
            auto_empty(state, account, cache, emit).await;
        }
        MailRequest::Reconnect => match state.refresh_folders(cache, false).await {
            Ok(folders) => emit(WorkerEvent::Folders(folders)),
            Err(e) => report(&e, account, |e| i18n_f("Could not list folders: {e}", &[("e", e)]), emit),
        },
        MailRequest::Settle { path, uids } => emit(WorkerEvent::MovesSettled { path, uids }),
        MailRequest::ExportRaw { token, path, uid, .. } => {
            let raw = state.fetch_raw(&path, uid, cache).await;
            emit(WorkerEvent::RawExported { token, raw });
        }
        MailRequest::ImportRaw { token, path, raw, seen, flagged } => {
            // A message moved in from another account, filed as received
            // mail with its read state and star.
            let r: Result<(), EwsError> = async {
                let folder = state.folder_ews_id(&path, cache).await?;
                let item = ops::import_mime(&mut state.client, &raw, &folder, seen).await?;
                if let (true, Some(item)) = (flagged, item) {
                    ops::update(&mut state.client, &[item], Change::Flagged(true)).await?;
                }
                Ok(())
            }
            .await;
            emit(WorkerEvent::RawImported { token, result: r.map_err(|e| say(&e, account)) });
        }
        MailRequest::DeleteAttachment { message_id, path, uid, name, size } => {
            emit(WorkerEvent::Status(i18n("Removing the attachment…")));
            let r: Result<(), String> = async {
                let item = state.resolve(&path, uid, cache).await.ok_or_else(|| i18n("message not found"))?;
                let listed = ops::attachments_of(&mut state.client, &item).await.map_err(|e| say(&e, account))?;
                // The namesake nearest in size, the way the other backends
                // pick between two of one name.
                let id = listed
                    .into_iter()
                    .filter(|(_, n, _)| *n == name)
                    .min_by_key(|(_, _, s)| s.abs_diff(size))
                    .map(|(id, _, _)| id)
                    .ok_or_else(|| i18n("The attachment is no longer in the message on the server"))?;
                ops::delete_attachment(&mut state.client, &id).await.map_err(|e| say(&e, account))
            }
            .await;
            emit(WorkerEvent::Status(String::new()));
            match r {
                Ok(()) => {
                    // The message keeps its id: only what the cache holds of
                    // it changes.
                    if let Some(c) = cache {
                        c.delete_body(account_id, &path, uid);
                        match state.fetch_raw(&path, uid, cache).await {
                            Ok(raw) => cache_rewritten(c, account_id, &path, uid, &raw),
                            Err(_) => {
                                c.save_attachments(account_id, &path, uid, &[]);
                                c.save_attachment_meta(account_id, &path, uid, &[]);
                            }
                        }
                    }
                    emit(WorkerEvent::AttachmentDeleted { message_id, path, uid, new_uid: Some(uid), name, size });
                }
                Err(e) => strip_refused(emit, i18n_f("Could not remove the attachment: {e}", &[("e", &e)]), message_id, name, size),
            }
        }
    }
}

/// The Outbox for an Exchange account: the same queue as the others', sent
/// through the server, which files each copy. Blind copies are the
/// envelope's recipients the message's own headers leave out.
async fn flush_outbox(state: &mut State, cache: Option<&Cache>, id: Option<u32>, emit: &impl Fn(WorkerEvent)) {
    let Some(cache) = cache else { return };
    let now = crate::datefmt::now();
    let items: Vec<crate::models::OutboxItem> = cache
        .outbox_items(state.account_id)
        .into_iter()
        .filter(|item| match id {
            Some(wanted) => wanted == item.id,
            None => item.send_at.is_none_or(|t| t <= now),
        })
        .collect();
    if items.is_empty() {
        return;
    }
    emit(WorkerEvent::Status(i18n("Sending…")));
    let mut sent_any = false;
    for item in items {
        let headers = String::from_utf8_lossy(&item.raw).to_ascii_lowercase();
        let head = headers.split("\r\n\r\n").next().unwrap_or("");
        let bcc: Vec<String> =
            item.rcpts.iter().filter(|r| !head.contains(&r.to_ascii_lowercase())).cloned().collect();
        match ops::send_mime(&mut state.client, &item.raw, &bcc).await {
            Ok(()) => {
                sent_any = true;
                cache.delete_outbox(item.id);
            }
            Err(e) => {
                let e = e.to_string();
                cache.record_outbox_failure(item.id, &e);
                emit(WorkerEvent::error(i18n_f(
                    "Still could not send “{subject}”: {e}",
                    &[("subject", &item.subject), ("e", &e)],
                )));
                break;
            }
        }
    }
    emit(WorkerEvent::Status(String::new()));
    if sent_any {
        emit(WorkerEvent::Sent);
    }
    emit_outbox(Some(cache), state.account_id, emit);
}

/// The connection test of the account settings: sign in, and find the
/// mailbox's folder root.
/// `Some` is the EWS address Autodiscover found, for the setup to keep.
pub(in crate::worker) async fn test_ews(account: &AccountConfig) -> Result<Option<String>, String> {
    let mut account = account.clone();
    if account.password.is_empty() {
        account.password = crate::config::load_password(&account.email).unwrap_or_default();
    }
    let found = discover_if_needed(&mut account).await?;
    let mut client = client_for(&account)?;
    ops::list_folders(&mut client, 0, &Default::default()).await.map_err(|e| say(&e, &account))?;
    Ok(found)
}
