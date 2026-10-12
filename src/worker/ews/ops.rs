//! What a mailbox does over EWS (#391), in Hylki's terms: the folder list,
//! a folder's newest messages, one message's MIME, flags, moves, deletes,
//! sending and drafts.

use base64::Engine as _;
use ews::response::ResponseClass;
use ews::{
    create_item::CreateItem, delete_item::DeleteItem, empty_folder::EmptyFolder, find_item::FindItem,
    get_folder::GetFolder, get_item::GetItem, mark_all_read::MarkAllItemsAsRead, move_item::MoveItem,
    sync_folder_hierarchy::{Change as FolderChange, SyncFolderHierarchy},
    update_item::{ConflictResolution, ItemChange, ItemChangeDescription, ItemChangeInner, UpdateItem, Updates},
    BaseFolderId, BaseItemId, BaseShape, CopyMoveItemData, DeleteType, Folder as EwsFolderItem, FolderShape,
    ItemShape, MessageDisposition, MimeContent, PathToElement, RealItem,
};

use super::client::{Client, EwsError};
use crate::models::{Folder, FolderKind, Message};

/// How many of a folder's newest messages are kept listed, as for
/// Microsoft 365.
pub const INDEX_CAP: usize = 300;
/// Items per GetItem: well under what a server takes in one request.
const GET_BATCH: usize = 100;

/// A folder as the mailbox knows it.
#[derive(Clone, Debug)]
pub struct EwsFolder {
    pub folder: Folder,
    pub ews_id: String,
}

/// An item's server identity: its id, and the change key that proves an
/// update was made against the version last seen.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemRef {
    pub id: String,
    pub change_key: Option<String>,
}

impl ItemRef {
    fn base(&self) -> BaseItemId {
        BaseItemId::ItemId { id: self.id.clone(), change_key: self.change_key.clone() }
    }
}

fn fail_first<T>(messages: Vec<ResponseClass<T>>) -> Result<Vec<T>, EwsError> {
    messages
        .into_iter()
        .map(|m| match m {
            ResponseClass::Success(m) | ResponseClass::Warning(m) => Ok(m),
            ResponseClass::Error(e) => Err(EwsError::Response(e)),
        })
        .collect()
}

fn field(uri: &str) -> PathToElement {
    PathToElement::FieldURI { field_URI: uri.to_string() }
}

/// The well-known folders and their roles, by distinguished id.
const ROLES: [(&str, FolderKind); 5] = [
    ("inbox", FolderKind::Inbox),
    ("sentitems", FolderKind::Sent),
    ("drafts", FolderKind::Drafts),
    ("deleteditems", FolderKind::Trash),
    ("junkemail", FolderKind::Junk),
];

/// The id of each distinguished folder that exists, by distinguished name.
async fn distinguished(c: &mut Client, names: &[&str]) -> Vec<(String, String)> {
    let op = GetFolder {
        folder_shape: FolderShape { base_shape: BaseShape::IdOnly },
        folder_ids: names.iter().map(|n| BaseFolderId::new_distinguished(*n)).collect(),
    };
    let Ok(messages) = c.call(&op).await else { return Vec::new() };
    names
        .iter()
        .zip(messages)
        .filter_map(|(name, m)| match m {
            ResponseClass::Success(r) | ResponseClass::Warning(r) => {
                r.folders.inner.first().and_then(folder_id_of).map(|id| (name.to_string(), id))
            }
            ResponseClass::Error(_) => None,
        })
        .collect()
}

fn folder_id_of(f: &EwsFolderItem) -> Option<String> {
    let id = match f {
        EwsFolderItem::Folder { folder_id, .. }
        | EwsFolderItem::CalendarFolder { folder_id, .. }
        | EwsFolderItem::ContactsFolder { folder_id, .. }
        | EwsFolderItem::SearchFolder { folder_id, .. }
        | EwsFolderItem::TasksFolder { folder_id, .. } => folder_id,
    };
    id.as_ref().map(|i| i.id.clone())
}

/// The mailbox's mail folders, sorted and numbered the way the other
/// backends number theirs, with roles and chip counts.
pub async fn list_folders(
    c: &mut Client,
    account_id: u32,
    assignments: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<EwsFolder>, EwsError> {
    let mut names: Vec<&str> = ROLES.iter().map(|(n, _)| *n).collect();
    names.extend(["msgfolderroot", "outbox"]);
    let known = distinguished(c, &names).await;
    let root = known
        .iter()
        .find(|(n, _)| n == "msgfolderroot")
        .map(|(_, id)| id.clone())
        .ok_or_else(|| EwsError::Parse("the mailbox has no folder root".into()))?;
    // Exchange's own Outbox holds what Outlook has yet to send: Hylki keeps
    // an Outbox of its own, and this one stays out of the list.
    let outbox = known.iter().find(|(n, _)| n == "outbox").map(|(_, id)| id.clone());
    let mut roles: std::collections::HashMap<String, FolderKind> = known
        .iter()
        .filter_map(|(n, id)| ROLES.iter().find(|(r, _)| r == n).map(|(_, k)| (id.clone(), *k)))
        .collect();
    // Not every server knows `archive` (Exchange Online does), and one that
    // does not may refuse the whole request: asked alone.
    if let Some((_, id)) = distinguished(c, &["archive"]).await.into_iter().next() {
        roles.insert(id, FolderKind::Archive);
    }

    // The whole tree under the root, in as many rounds as it takes.
    struct Raw {
        id: String,
        parent: String,
        name: String,
        total: u32,
        unread: u32,
    }
    let mut raw: Vec<Raw> = Vec::new();
    let mut sync_state = None;
    for _ in 0..20 {
        let op = SyncFolderHierarchy {
            folder_shape: FolderShape { base_shape: BaseShape::AllProperties },
            sync_folder_id: Some(BaseFolderId::new_folder(root.clone())),
            sync_state: sync_state.clone(),
        };
        let r = c.call_one(&op).await?;
        for change in r.changes.inner {
            let (FolderChange::Create { folder } | FolderChange::Update { folder }) = change else { continue };
            // Mail folders only: calendars, contacts, tasks, notes, and the
            // configuration folders Outlook keeps are left out.
            let EwsFolderItem::Folder {
                folder_id: Some(id),
                parent_folder_id,
                folder_class,
                display_name,
                total_count,
                unread_count,
                ..
            } = folder
            else {
                continue;
            };
            if !folder_class.as_deref().is_none_or(|c| c == "IPF.Note" || c.starts_with("IPF.Note."))
                || outbox.as_deref() == Some(id.id.as_str())
            {
                continue;
            }
            raw.retain(|f| f.id != id.id);
            raw.push(Raw {
                id: id.id,
                parent: parent_folder_id.map(|p| p.id).unwrap_or_default(),
                name: display_name.unwrap_or_default(),
                total: total_count.unwrap_or(0),
                unread: unread_count.unwrap_or(0),
            });
        }
        sync_state = Some(r.sync_state);
        if r.includes_last_folder_in_range {
            break;
        }
    }

    // Paths join names with '/', from the root down.
    let by_id: std::collections::HashMap<&str, &Raw> = raw.iter().map(|f| (f.id.as_str(), f)).collect();
    let path_of = |f: &Raw| -> Option<String> {
        let mut parts = vec![f.name.clone()];
        let mut parent = f.parent.as_str();
        for _ in 0..16 {
            if parent == root {
                parts.reverse();
                return Some(parts.join("/"));
            }
            let p = by_id.get(parent)?;
            parts.push(p.name.clone());
            parent = p.parent.as_str();
        }
        None
    };
    let mut out: Vec<(EwsFolder, u32, u32)> = Vec::new();
    for f in &raw {
        let Some(path) = path_of(f) else { continue };
        let top = !path.contains('/');
        let kind = match roles.get(&f.id) {
            Some(k) => *k,
            None if top && crate::models::is_templates_name(&f.name) => FolderKind::Templates,
            None if top && f.name.eq_ignore_ascii_case("Archive") => FolderKind::Archive,
            None => FolderKind::Custom,
        };
        out.push((
            EwsFolder {
                folder: Folder { id: 0, account_id, name: f.name.clone(), path, kind, unread: 0 },
                ews_id: f.id.clone(),
            },
            f.total,
            f.unread,
        ));
    }
    // One Archive: a server's own role wins over a folder merely named so.
    if out.iter().filter(|(f, _, _)| f.folder.kind == FolderKind::Archive).count() > 1 {
        for (f, _, _) in out.iter_mut() {
            if f.folder.kind == FolderKind::Archive && !roles.contains_key(&f.ews_id) {
                f.folder.kind = FolderKind::Custom;
            }
        }
    }
    out.sort_by(|(a, _, _), (b, _, _)| {
        super::super::folder_order(a.folder.kind)
            .cmp(&super::super::folder_order(b.folder.kind))
            .then_with(|| a.folder.path.to_lowercase().cmp(&b.folder.path.to_lowercase()))
    });
    for (i, (f, _, _)) in out.iter_mut().enumerate() {
        f.folder.id = i as u32 + 1;
    }
    let mut folders: Vec<Folder> = out.iter().map(|(f, _, _)| f.folder.clone()).collect();
    crate::models::assign_folder_roles(assignments, &mut folders);
    Ok(out
        .into_iter()
        .zip(folders)
        .map(|((mut ef, total, unread), f)| {
            ef.folder.kind = f.kind;
            ef.folder.unread = if super::super::chip_counts_all(f.kind) { total } else { unread };
            ef
        })
        .collect())
}

/// One listed item: its identity and the state a poll can refresh without
/// fetching the item again.
pub struct Listed {
    pub item: ItemRef,
    pub unread: bool,
    pub starred: Option<bool>,
    pub categories: Vec<String>,
}

/// A folder's newest items, newest first, with their read state, flag and
/// categories: FindItem, written out for its sort order.
pub async fn list_items(c: &mut Client, folder: &str, cap: usize) -> Result<Vec<Listed>, EwsError> {
    let flag = if c.has_2013() { "<t:FieldURI FieldURI=\"item:Flag\"/>" } else { "" };
    let body = format!(
        "<m:FindItem Traversal=\"Shallow\">\
         <m:ItemShape><t:BaseShape>IdOnly</t:BaseShape><t:AdditionalProperties>\
         <t:FieldURI FieldURI=\"message:IsRead\"/><t:FieldURI FieldURI=\"item:Categories\"/>{flag}\
         </t:AdditionalProperties></m:ItemShape>\
         <m:IndexedPageItemView MaxEntriesReturned=\"{cap}\" Offset=\"0\" BasePoint=\"Beginning\"/>\
         <m:SortOrder><t:FieldOrder Order=\"Descending\"><t:FieldURI FieldURI=\"item:DateTimeReceived\"/></t:FieldOrder></m:SortOrder>\
         <m:ParentFolderIds><t:FolderId Id=\"{}\"/></m:ParentFolderIds>\
         </m:FindItem>",
        xml_attr(folder)
    );
    let found = fail_first(c.call_xml::<FindItem>(&body).await?)?;
    Ok(found
        .into_iter()
        .flat_map(|r| r.root_folder.items.inner)
        .filter_map(|item| {
            let m = item.into_inner_message();
            let id = m.item_id?;
            Some(Listed {
                item: ItemRef { id: id.id, change_key: id.change_key },
                unread: !m.is_read.unwrap_or(true),
                starred: m.flag.map(|f| f.flag_status == Some(ews::FlagStatus::Flagged)),
                categories: m.categories.unwrap_or_default().into_iter().map(|s| s.string).collect(),
            })
        })
        .collect())
}

/// What a message summary is read from.
fn summary_shape(with_2013: bool) -> ItemShape {
    let mut fields = vec![
        "item:Subject",
        "item:DateTimeReceived",
        "item:HasAttachments",
        "item:Categories",
        "item:Importance",
        "message:From",
        "message:ToRecipients",
        "message:CcRecipients",
        "message:ReplyTo",
        "message:IsRead",
        "message:InternetMessageId",
        "message:References",
        "message:InReplyTo",
    ];
    if with_2013 {
        fields.extend(["item:Flag", "item:Preview"]);
    }
    ItemShape {
        base_shape: BaseShape::IdOnly,
        include_mime_content: None,
        additional_properties: Some(fields.into_iter().map(field).collect()),
    }
}

/// The summaries of these items, in batches; an item gone meanwhile is
/// left out.
pub async fn get_summaries(
    c: &mut Client,
    items: &[ItemRef],
    account_id: u32,
    folder_id: u32,
) -> Result<Vec<(Message, ItemRef)>, EwsError> {
    let mut out = Vec::with_capacity(items.len());
    for batch in items.chunks(GET_BATCH) {
        let op = GetItem { item_shape: summary_shape(c.has_2013()), item_ids: batch.iter().map(ItemRef::base).collect() };
        for r in c.call(&op).await? {
            let (ResponseClass::Success(r) | ResponseClass::Warning(r)) = r else { continue };
            for item in r.items.inner {
                if let Some(pair) = summary(item, account_id, folder_id) {
                    out.push(pair);
                }
            }
        }
    }
    Ok(out)
}

/// One EWS item as a [`Message`] summary.
fn summary(item: RealItem, account_id: u32, folder_id: u32) -> Option<(Message, ItemRef)> {
    use super::super::{hash_uid, label_from_timestamp, normalize_msgid, normalize_msgids};
    let m = item.into_inner_message();
    let id = m.item_id.clone()?;
    let uid = hash_uid(&id.id);
    let addrs = |list: &Option<ews::ArrayOfRecipients>| -> String {
        list.as_ref()
            .map(|l| l.iter().filter_map(|r| r.mailbox.email_address.clone()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    };
    let from = m.from.as_ref().map(|r| &r.mailbox);
    let from_addr = from.and_then(|f| f.email_address.clone()).unwrap_or_default();
    let reply_to = addrs(&m.reply_to);
    let reply_to = if reply_to.eq_ignore_ascii_case(&from_addr) { String::new() } else { reply_to };
    let ts = m.date_time_received.as_ref().map(|d| d.0.unix_timestamp()).unwrap_or(0);
    let preview: String = m
        .preview
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect();
    let preview = super::super::pgp_preview(&preview).unwrap_or(preview);
    let refs = format!("{} {}", m.references.as_deref().unwrap_or(""), m.in_reply_to.as_deref().unwrap_or(""));
    let mut references: Vec<String> = Vec::new();
    for r in normalize_msgids(refs.as_bytes()).split_whitespace() {
        if !references.iter().any(|x| x == r) {
            references.push(r.to_string());
        }
    }
    let flagged = m.flag.as_ref().is_some_and(|f| f.flag_status == Some(ews::FlagStatus::Flagged));
    let msg = Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name: from.and_then(|f| f.name.clone()).unwrap_or_default(),
        from_addr,
        reply_to,
        to: addrs(&m.to_recipients),
        cc: addrs(&m.cc_recipients),
        subject: m.subject.clone().unwrap_or_default(),
        preview,
        body: String::new(),
        date: if ts > 0 { label_from_timestamp(ts) } else { String::new() },
        timestamp: ts,
        unread: !m.is_read.unwrap_or(true),
        starred: flagged,
        keywords: m.categories.clone().unwrap_or_default().into_iter().map(|s| s.string).collect(),
        has_attachment: m.has_attachments.unwrap_or(false),
        message_id: normalize_msgid(m.internet_message_id.as_deref().unwrap_or("").as_bytes()),
        references: references.join(" "),
        importance: match m.importance {
            Some(ews::Importance::High) => crate::models::Importance::High,
            Some(ews::Importance::Low) => crate::models::Importance::Low,
            _ => crate::models::Importance::Normal,
        },
        due: if flagged { m.flag.as_ref().and_then(|f| f.due_date.as_deref()).map(due_of).unwrap_or(0) } else { 0 },
        labels: Vec::new(),
    };
    Some((msg, ItemRef { id: id.id, change_key: id.change_key }))
}

/// A flag's due date (`2026-10-14T00:00:00Z`, any fraction) as Unix seconds.
fn due_of(at: &str) -> i64 {
    let at = at.trim_end_matches('Z');
    let at = at.split('.').next().unwrap_or(at);
    chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S").map(|t| t.and_utc().timestamp()).unwrap_or(0)
}

/// A message's raw RFC 822 bytes.
pub async fn fetch_mime(c: &mut Client, item: &ItemRef) -> Result<Vec<u8>, EwsError> {
    let op = GetItem {
        item_shape: ItemShape { base_shape: BaseShape::IdOnly, include_mime_content: Some(true), additional_properties: None },
        item_ids: vec![item.base()],
    };
    let r = c.call_one(&op).await?;
    let content = r
        .items
        .inner
        .into_iter()
        .next()
        .and_then(|i| i.into_inner_message().mime_content)
        .ok_or_else(|| EwsError::Parse("the message came without its MIME content".into()))?;
    let b64: String = content.content.split_whitespace().collect();
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| EwsError::Parse(format!("the message's MIME content: {e}")))
}

/// What a flag update sets.
pub enum Change {
    Read(bool),
    Flagged(bool),
    Categories(Vec<String>),
}

/// Set one property on items, overwriting whatever changed meanwhile: a
/// read state or a star is the user's latest word.
pub async fn update(c: &mut Client, items: &[ItemRef], change: Change) -> Result<(), EwsError> {
    let (uri, message) = match change {
        Change::Read(read) => ("message:IsRead", ews::Message { is_read: Some(read), ..Default::default() }),
        Change::Flagged(on) => (
            "item:Flag",
            ews::Message {
                flag: Some(ews::Flag {
                    flag_status: Some(if on { ews::FlagStatus::Flagged } else { ews::FlagStatus::NotFlagged }),
                    start_date: None,
                    due_date: None,
                    complete_date: None,
                }),
                ..Default::default()
            },
        ),
        Change::Categories(list) => (
            "item:Categories",
            ews::Message {
                categories: Some(list.into_iter().map(|string| ews::StringElement { string }).collect()),
                ..Default::default()
            },
        ),
    };
    for batch in items.chunks(GET_BATCH) {
        let op = UpdateItem {
            message_disposition: MessageDisposition::SaveOnly,
            conflict_resolution: Some(ConflictResolution::AlwaysOverwrite),
            item_changes: batch
                .iter()
                .map(|i| ItemChange {
                    item_change: ItemChangeInner {
                        // No change key: AlwaysOverwrite needs none, and one
                        // gone stale would fail the update.
                        item_id: BaseItemId::ItemId { id: i.id.clone(), change_key: None },
                        updates: Updates {
                            inner: vec![ItemChangeDescription::SetItemField { field_uri: field(uri), message: message.clone() }],
                        },
                    },
                })
                .collect(),
        };
        fail_first(c.call(&op).await?)?;
    }
    Ok(())
}

/// Move items into a folder, answering with their new ids in order (an
/// item's id changes when it moves); `None` for one the server did not say.
pub async fn move_items(c: &mut Client, items: &[ItemRef], dest: &str) -> Result<Vec<Option<ItemRef>>, EwsError> {
    let mut out = Vec::with_capacity(items.len());
    for batch in items.chunks(GET_BATCH) {
        let op = MoveItem {
            inner: CopyMoveItemData {
                to_folder_id: BaseFolderId::new_folder(dest),
                item_ids: batch.iter().map(ItemRef::base).collect(),
                return_new_item_ids: Some(true),
            },
        };
        for r in c.call(&op).await? {
            match r {
                ResponseClass::Success(r) | ResponseClass::Warning(r) => out.push(
                    r.items
                        .inner
                        .into_iter()
                        .next()
                        .and_then(|i| i.into_inner_message().item_id)
                        .map(|id| ItemRef { id: id.id, change_key: id.change_key }),
                ),
                ResponseClass::Error(e) => return Err(EwsError::Response(e)),
            }
        }
    }
    Ok(out)
}

/// Delete items for good, or to Deleted Items.
pub async fn delete_items(c: &mut Client, items: &[ItemRef], hard: bool) -> Result<(), EwsError> {
    for batch in items.chunks(GET_BATCH) {
        let op = DeleteItem {
            delete_type: if hard { DeleteType::HardDelete } else { DeleteType::MoveToDeletedItems },
            send_meeting_cancellations: None,
            affected_task_occurrences: None,
            suppress_read_receipts: Some(true),
            item_ids: batch.iter().map(ItemRef::base).collect(),
        };
        for r in c.call(&op).await? {
            if let ResponseClass::Error(e) = r {
                // Already gone is what a delete wanted.
                if e.response_code != ews::response::ResponseCode::ErrorItemNotFound {
                    return Err(EwsError::Response(e));
                }
            }
        }
    }
    Ok(())
}

/// Erase everything in a folder.
pub async fn empty_folder(c: &mut Client, folder: &str) -> Result<(), EwsError> {
    let op = EmptyFolder {
        delete_type: DeleteType::HardDelete,
        delete_sub_folders: false,
        folder_ids: vec![BaseFolderId::new_folder(folder)],
    };
    fail_first(c.call(&op).await?).map(|_| ())
}

/// Mark a folder read in one request, where the server has the operation
/// (Exchange 2013 and later).
pub async fn mark_all_read(c: &mut Client, folder: &str) -> Result<(), EwsError> {
    let op = MarkAllItemsAsRead {
        read_flag: true,
        suppress_read_receipts: true,
        folder_ids: vec![BaseFolderId::new_folder(folder)],
    };
    fail_first(c.call(&op).await?).map(|_| ())
}

/// Send a built message, the server filing its copy in Sent Items. Blind
/// copies go as recipients of their own, which the MIME does not carry.
pub async fn send_mime(c: &mut Client, mime: &[u8], bcc: &[String]) -> Result<(), EwsError> {
    let message = mime_message(mime, bcc);
    let op = CreateItem {
        message_disposition: Some(MessageDisposition::SendAndSaveCopy),
        saved_item_folder_id: Some(BaseFolderId::new_distinguished("sentitems")),
        items: vec![RealItem::Message(message)],
    };
    fail_first(c.call(&op).await?).map(|_| ())
}

/// Save a built message into a folder, unsent: a draft, or a template.
pub async fn save_mime(c: &mut Client, mime: &[u8], folder: &str) -> Result<Option<ItemRef>, EwsError> {
    let op = CreateItem {
        message_disposition: Some(MessageDisposition::SaveOnly),
        saved_item_folder_id: Some(BaseFolderId::new_folder(folder)),
        items: vec![RealItem::Message(mime_message(mime, &[]))],
    };
    let r = fail_first(c.call(&op).await?)?;
    Ok(r.into_iter()
        .next()
        .and_then(|r| r.items.inner.into_iter().next())
        .and_then(|i| i.into_inner_message().item_id)
        .map(|id| ItemRef { id: id.id, change_key: id.change_key }))
}

fn mime_message(mime: &[u8], bcc: &[String]) -> ews::Message {
    ews::Message {
        mime_content: Some(MimeContent {
            character_set: Some("UTF-8".into()),
            content: base64::engine::general_purpose::STANDARD.encode(mime),
        }),
        bcc_recipients: (!bcc.is_empty()).then(|| {
            ews::ArrayOfRecipients(
                bcc.iter()
                    .map(|a| ews::Recipient {
                        mailbox: ews::Mailbox { email_address: Some(a.clone()), ..Default::default() },
                    })
                    .collect(),
            )
        }),
        ..Default::default()
    }
}

/// A new mail folder, under `parent` (its EWS id) or at the top; its id.
pub async fn create_folder(c: &mut Client, parent: Option<&str>, name: &str) -> Result<String, EwsError> {
    let op = ews::create_folder::CreateFolder {
        parent_folder_id: match parent {
            Some(id) => BaseFolderId::new_folder(id),
            None => BaseFolderId::new_distinguished("msgfolderroot"),
        },
        folders: vec![mail_folder(name)],
    };
    let r = c.call_one(&op).await?;
    r.folders.inner.first().and_then(folder_id_of).ok_or_else(|| EwsError::Parse("the new folder has no id".into()))
}

fn mail_folder(name: &str) -> EwsFolderItem {
    EwsFolderItem::Folder {
        folder_id: None,
        parent_folder_id: None,
        folder_class: Some("IPF.Note".into()),
        display_name: Some(name.to_string()),
        total_count: None,
        child_folder_count: None,
        extended_property: None,
        unread_count: None,
    }
}

/// Give a folder another name, where it is.
pub async fn rename_folder(c: &mut Client, id: &str, name: &str) -> Result<(), EwsError> {
    use ews::update_folder::{FolderChange, FolderChanges, UpdateFolder, Updates as FolderUpdates};
    let op = UpdateFolder {
        folder_changes: FolderChanges {
            folder_change: FolderChange {
                folder_id: BaseFolderId::new_folder(id),
                updates: FolderUpdates::SetFolderField {
                    field_URI: field("folder:DisplayName"),
                    folder: EwsFolderItem::Folder {
                        folder_id: None,
                        parent_folder_id: None,
                        folder_class: None,
                        display_name: Some(name.to_string()),
                        total_count: None,
                        child_folder_count: None,
                        extended_property: None,
                        unread_count: None,
                    },
                },
            },
        },
    };
    c.call_one(&op).await.map(|_| ())
}

/// Move a folder under another (`None` for the top level).
pub async fn move_folder(c: &mut Client, id: &str, parent: Option<&str>) -> Result<(), EwsError> {
    let op = ews::move_folder::MoveFolder {
        inner: ews::CopyMoveFolderData {
            to_folder_id: match parent {
                Some(p) => BaseFolderId::new_folder(p),
                None => BaseFolderId::new_distinguished("msgfolderroot"),
            },
            folder_ids: vec![BaseFolderId::new_folder(id)],
        },
    };
    c.call_one(&op).await.map(|_| ())
}

/// Delete a folder: into Deleted Items, contents and all, as Outlook does.
pub async fn delete_folder(c: &mut Client, id: &str) -> Result<(), EwsError> {
    let op = ews::delete_folder::DeleteFolder {
        delete_type: DeleteType::MoveToDeletedItems,
        folder_ids: vec![BaseFolderId::new_folder(id)],
    };
    c.call_one(&op).await.map(|_| ())
}

/// A message's attachments: (attachment id, name, size).
pub async fn attachments_of(c: &mut Client, item: &ItemRef) -> Result<Vec<(String, String, u64)>, EwsError> {
    let op = GetItem {
        item_shape: ItemShape {
            base_shape: BaseShape::IdOnly,
            include_mime_content: None,
            additional_properties: Some(vec![field("item:Attachments")]),
        },
        item_ids: vec![item.base()],
    };
    let r = c.call_one(&op).await?;
    let m = r.items.inner.into_iter().next().map(RealItem::into_inner_message).unwrap_or_default();
    Ok(m.attachments
        .map(|a| a.inner)
        .unwrap_or_default()
        .into_iter()
        .map(|a| match a {
            ews::Attachment::FileAttachment { attachment_id, name, size, .. }
            | ews::Attachment::ItemAttachment { attachment_id, name, size, .. } => {
                (attachment_id.id, name, size.unwrap_or(0) as u64)
            }
        })
        .collect())
}

/// Remove one attachment from its message; the message keeps its id.
pub async fn delete_attachment(c: &mut Client, attachment_id: &str) -> Result<(), EwsError> {
    let body = format!(
        "<m:DeleteAttachment><m:AttachmentIds><t:AttachmentId Id=\"{}\"/></m:AttachmentIds></m:DeleteAttachment>",
        xml_attr(attachment_id)
    );
    c.call_raw(&body).await.map(|_| ())
}

/// Put a whole message, received elsewhere, into a folder: as received
/// mail, not a draft (the MAPI message flags say read or unread and, by
/// leaving out "unsent", that it was sent).
pub async fn import_mime(c: &mut Client, mime: &[u8], folder: &str, seen: bool) -> Result<Option<ItemRef>, EwsError> {
    let mut message = mime_message(mime, &[]);
    message.extended_property = Some(vec![ews::ExtendedProperty {
        extended_field_URI: ews::ExtendedFieldURI {
            distinguished_property_set_id: None,
            property_set_id: None,
            property_tag: Some("0x0E07".into()),
            property_name: None,
            property_id: None,
            property_type: Some(ews::PropertyType::Integer),
        },
        value: if seen { "1".into() } else { "0".into() },
    }]);
    let op = CreateItem {
        message_disposition: Some(MessageDisposition::SaveOnly),
        saved_item_folder_id: Some(BaseFolderId::new_folder(folder)),
        items: vec![RealItem::Message(message)],
    };
    let r = fail_first(c.call(&op).await?)?;
    let item = r
        .into_iter()
        .next()
        .and_then(|r| r.items.inner.into_iter().next())
        .and_then(|i| i.into_inner_message().item_id)
        .map(|id| ItemRef { id: id.id, change_key: id.change_key });
    // Not every server reads the read state from the flags (gromox takes
    // every new item as read): set it as well.
    if let Some(item) = &item {
        update(c, std::slice::from_ref(item), Change::Read(seen)).await?;
    }
    Ok(item)
}

/// Report items as junk or as wanted, so the server's filter learns, and
/// move them (to Junk Email, or back to the Inbox); their new ids.
pub async fn mark_as_junk(c: &mut Client, items: &[ItemRef], junk: bool) -> Result<Vec<Option<ItemRef>>, EwsError> {
    let op = ews::mark_as_junk::MarkAsJunk { is_junk: junk, move_item: true, item_ids: items.iter().map(ItemRef::base).collect() };
    Ok(c.call(&op)
        .await?
        .into_iter()
        .map(|r| match r {
            ResponseClass::Success(m) | ResponseClass::Warning(m) => {
                Some(ItemRef { id: m.moved_item_id.id, change_key: m.moved_item_id.change_key })
            }
            ResponseClass::Error(_) => None,
        })
        .collect())
}

/// Items in a folder received before `before` (Unix seconds), at most `cap`.
pub async fn items_before(c: &mut Client, folder: &str, before: i64, cap: usize) -> Result<Vec<ItemRef>, EwsError> {
    let at = chrono::DateTime::from_timestamp(before, 0).unwrap_or_default().format("%Y-%m-%dT%H:%M:%SZ");
    let body = format!(
        "<m:FindItem Traversal=\"Shallow\">\
         <m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape>\
         <m:IndexedPageItemView MaxEntriesReturned=\"{cap}\" Offset=\"0\" BasePoint=\"Beginning\"/>\
         <m:Restriction><t:IsLessThan><t:FieldURI FieldURI=\"item:DateTimeReceived\"/>\
         <t:FieldURIOrConstant><t:Constant Value=\"{at}\"/></t:FieldURIOrConstant></t:IsLessThan></m:Restriction>\
         <m:ParentFolderIds><t:FolderId Id=\"{}\"/></m:ParentFolderIds>\
         </m:FindItem>",
        xml_attr(folder)
    );
    let found = fail_first(c.call_xml::<FindItem>(&body).await?)?;
    Ok(found
        .into_iter()
        .flat_map(|r| r.root_folder.items.inner)
        .filter_map(|i| i.into_inner_message().item_id)
        .map(|id| ItemRef { id: id.id, change_key: id.change_key })
        .collect())
}

fn xml_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/worker/ews/fixtures/", $name, ".xml"))
        };
    }

    #[test]
    fn a_full_item_becomes_a_summary() {
        let r = super::super::client::parse_reply::<GetItem>(fixture!("09-GetItem-default-nonascii")).unwrap();
        let ResponseClass::Success(r) = r.into_iter().next().unwrap() else { panic!() };
        let item = r.items.inner.into_iter().next().unwrap();
        let (m, id) = summary(item, 3, 1).unwrap();
        assert!(!id.id.is_empty());
        assert_eq!(m.uid, super::super::super::hash_uid(&id.id));
        assert!(!m.subject.is_empty() && m.timestamp > 0, "{m:?}");
        assert!(m.from_addr.contains('@'), "{m:?}");
    }

    #[test]
    fn a_listing_reads_ids_and_state() {
        let r = super::super::client::parse_reply::<FindItem>(fixture!("05-FindItem-inbox")).unwrap();
        let ResponseClass::Success(r) = r.into_iter().next().unwrap() else { panic!() };
        assert!(!r.root_folder.items.inner.is_empty());
    }

    #[test]
    fn due_dates_read_whatever_their_fraction() {
        assert_eq!(due_of("2026-10-14T00:00:00Z"), 1_791_936_000);
        assert_eq!(due_of("2026-10-14T00:00:00.000000000Z"), 1_791_936_000);
        assert_eq!(due_of("nonsense"), 0);
    }
}

/// Against a real server, such as temp/ews-rig's gromox:
/// `EWS_LIVE='https://127.0.0.1:18443/EWS/Exchange.asmx|alice@ews.test|alice-pass-1'`
/// cargo test --bin hylki ews::ops::live -- --ignored --nocapture
#[cfg(test)]
mod live {
    use super::*;
    use crate::worker::ews::http::{Connection, Endpoint, TlsOptions};

    #[tokio::test]
    #[ignore]
    async fn mailbox_round_trip() {
        let Ok(spec) = std::env::var("EWS_LIVE") else { return };
        let f: Vec<&str> = spec.split('|').collect();
        let conn = Connection::new(Endpoint::parse(f[0]).unwrap(), TlsOptions::default(), f[1], f[2]);
        let mut c = Client::new(conn);
        let folders = list_folders(&mut c, 1, &Default::default()).await.unwrap();
        for f in &folders {
            println!("folder {:>2} {:?} {} ({})", f.folder.id, f.folder.kind, f.folder.path, f.folder.unread);
        }
        let inbox = folders.iter().find(|f| f.folder.kind == FolderKind::Inbox).unwrap();
        let listed = list_items(&mut c, &inbox.ews_id, INDEX_CAP).await.unwrap();
        println!("inbox lists {} items", listed.len());
        let refs: Vec<ItemRef> = listed.iter().map(|l| l.item.clone()).collect();
        let summaries = get_summaries(&mut c, &refs, 1, inbox.folder.id).await.unwrap();
        for (m, _) in &summaries {
            println!("  {} | {} <{}> | unread={} star={} att={} kw={:?} refs={:?}", m.date, m.from_name, m.from_addr, m.unread, m.starred, m.has_attachment, m.keywords, m.references);
            println!("    {} | to {} | {}", m.subject, m.to, m.preview.chars().take(60).collect::<String>());
        }
        let first = &summaries[0];
        let raw = fetch_mime(&mut c, &first.1).await.unwrap();
        println!("MIME of the first: {} bytes, starts {:?}", raw.len(), String::from_utf8_lossy(&raw[..raw.len().min(80)]));
        // Toggle read twice: back where it was.
        let was_unread = first.0.unread;
        update(&mut c, &[first.1.clone()], Change::Read(was_unread)).await.unwrap();
        update(&mut c, &[first.1.clone()], Change::Read(!was_unread)).await.unwrap();
        update(&mut c, &[first.1.clone()], Change::Flagged(!first.0.starred)).await.unwrap();
        update(&mut c, &[first.1.clone()], Change::Flagged(first.0.starred)).await.unwrap();
        update(&mut c, &[first.1.clone()], Change::Categories(vec!["Hylki".into()])).await.unwrap();
        update(&mut c, &[first.1.clone()], Change::Categories(first.0.keywords.clone())).await.unwrap();
        println!("flags round trip ok");
    }

    /// Folders made, renamed, moved and deleted; an attachment removed; a
    /// message imported; junk reported; old mail found. Changes the
    /// mailbox: reset the rig afterwards.
    #[tokio::test]
    #[ignore]
    async fn stage_two_round_trip() {
        let Ok(spec) = std::env::var("EWS_LIVE") else { return };
        let f: Vec<&str> = spec.split('|').collect();
        let conn = Connection::new(Endpoint::parse(f[0]).unwrap(), TlsOptions::default(), f[1], f[2]);
        let mut c = Client::new(conn);
        let folders = list_folders(&mut c, 1, &Default::default()).await.unwrap();
        let projects = folders.iter().find(|f| f.folder.path == "Projects").unwrap().ews_id.clone();
        let top = create_folder(&mut c, None, "Hylki Test").await.unwrap();
        let child = create_folder(&mut c, Some(&top), "Child").await.unwrap();
        rename_folder(&mut c, &child, "Renamed").await.unwrap();
        move_folder(&mut c, &child, Some(&projects)).await.unwrap();
        let after = list_folders(&mut c, 1, &Default::default()).await.unwrap();
        let paths: Vec<&str> = after.iter().map(|f| f.folder.path.as_str()).collect();
        println!("folders now: {paths:?}");
        assert!(paths.contains(&"Hylki Test") && paths.contains(&"Projects/Renamed"));
        delete_folder(&mut c, &child).await.unwrap();
        delete_folder(&mut c, &top).await.unwrap();
        println!("folder ops ok");

        let inbox = folders.iter().find(|f| f.folder.kind == FolderKind::Inbox).unwrap();
        let listed = list_items(&mut c, &inbox.ews_id, 50).await.unwrap();
        let refs: Vec<ItemRef> = listed.iter().map(|l| l.item.clone()).collect();
        let summaries = get_summaries(&mut c, &refs, 1, 1).await.unwrap();
        let (_, with) = summaries.iter().find(|(m, _)| m.has_attachment).unwrap();
        let atts = attachments_of(&mut c, with).await.unwrap();
        println!("attachments: {atts:?}");
        match delete_attachment(&mut c, &atts[0].0).await {
            Ok(()) => println!("attachment deleted; left: {:?}", attachments_of(&mut c, with).await.unwrap()),
            Err(e) => println!("DeleteAttachment refused: {e}"),
        }

        let mime = b"From: carol@example.org\r\nTo: alice@ews.test\r\nSubject: Imported\r\nMessage-ID: <imported-1@hylki.test>\r\nContent-Type: text/plain\r\n\r\nMoved in.\r\n";
        let imported = import_mime(&mut c, mime, &projects, false).await.unwrap().expect("an id");
        let got = get_summaries(&mut c, &[imported.clone()], 1, 2).await.unwrap();
        println!("imported: unread={} subject={}", got[0].0.unread, got[0].0.subject);
        assert!(got[0].0.unread);
        match mark_as_junk(&mut c, &[imported.clone()], true).await {
            Ok(moved) => println!("MarkAsJunk moved: {moved:?}"),
            Err(e) => println!("MarkAsJunk refused: {e}"),
        }
        let old = items_before(&mut c, &inbox.ews_id, crate::datefmt::now(), 50).await.unwrap();
        println!("inbox items before now: {}", old.len());
        assert!(!old.is_empty());
        let none = items_before(&mut c, &inbox.ews_id, 0, 50).await.unwrap();
        assert!(none.is_empty());
    }

    /// Sends to the second account, saves a draft, moves it and deletes it.
    /// Leaves mail behind: reset the rig afterwards.
    /// `EWS_LIVE_TO=bob@ews.test` names where the test message goes.
    #[tokio::test]
    #[ignore]
    async fn send_save_move_delete() {
        let (Ok(spec), Ok(to)) = (std::env::var("EWS_LIVE"), std::env::var("EWS_LIVE_TO")) else { return };
        let f: Vec<&str> = spec.split('|').collect();
        let conn = Connection::new(Endpoint::parse(f[0]).unwrap(), TlsOptions::default(), f[1], f[2]);
        let mut c = Client::new(conn);
        let folders = list_folders(&mut c, 1, &Default::default()).await.unwrap();
        let by = |k: FolderKind| folders.iter().find(|f| f.folder.kind == k).unwrap().clone();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let mime = format!(
            "From: {}\r\nTo: {to}\r\nSubject: Hylki EWS test {stamp}\r\nMessage-ID: <ews-{stamp}@hylki.test>\r\n\
             MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSent over EWS. Grüße.\r\n",
            f[1]
        );
        send_mime(&mut c, mime.as_bytes(), &[]).await.unwrap();
        let sent = by(FolderKind::Sent);
        let listed = list_items(&mut c, &sent.ews_id, 20).await.unwrap();
        let refs: Vec<ItemRef> = listed.iter().map(|l| l.item.clone()).collect();
        let found = get_summaries(&mut c, &refs, 1, sent.folder.id).await.unwrap();
        assert!(found.iter().any(|(m, _)| m.message_id == format!("ews-{stamp}@hylki.test")), "the Sent copy");
        println!("sent, and the copy is in {}", sent.folder.path);
        let drafts = by(FolderKind::Drafts);
        let draft = save_mime(&mut c, mime.replace("Subject: Hylki", "Subject: Draft").as_bytes(), &drafts.ews_id)
            .await
            .unwrap()
            .expect("the draft's id");
        let projects = folders.iter().find(|f| f.folder.path == "Projects").unwrap();
        let moved = move_items(&mut c, &[draft.clone()], &projects.ews_id).await.unwrap();
        let moved = moved[0].clone().expect("the moved draft's new id");
        assert_ne!(moved.id, draft.id);
        delete_items(&mut c, &[moved], true).await.unwrap();
        println!("draft saved, moved and deleted");
    }
}
