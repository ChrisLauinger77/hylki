//! The mail workers: one per account, and their events turned into AppMsg.

use super::*;

pub(super) fn map_event(account_id: u32, event: WorkerEvent) -> AppMsg {
    match event {
        WorkerEvent::AttachmentsScanned {
            folder_path,
            added,
            remaining,
        } => AppMsg::AttachmentsScanned {
            account_id,
            folder_path,
            added,
            remaining,
        },
        WorkerEvent::BulkComplete => AppMsg::BulkComplete,
        WorkerEvent::Related {
            message_id,
            messages,
        } => AppMsg::Related {
            account_id,
            message_id,
            messages,
        },
        WorkerEvent::ThreadSummaries { summaries } => AppMsg::ThreadSummaries {
            account_id,
            summaries,
        },
        WorkerEvent::Account(a) => AppMsg::SetAccount(a),
        WorkerEvent::Identities(identities) => AppMsg::ServerIdentities {
            account_id,
            identities,
        },
        WorkerEvent::Folders(folders) => AppMsg::SetFolders {
            account_id,
            folders,
        },
        WorkerEvent::Messages {
            folder_id,
            messages,
        } => AppMsg::Messages {
            account_id,
            folder_id,
            messages,
        },
        WorkerEvent::BodyHits { folder_id, hits } => AppMsg::BodyHits {
            account_id,
            folder_id,
            hits,
        },
        WorkerEvent::MessagesAppend {
            folder_id,
            messages,
        } => AppMsg::MessagesAppend {
            account_id,
            folder_id,
            messages,
        },
        WorkerEvent::Located { message_id, hit } => AppMsg::MidLocated {
            account_id,
            message_id,
            hit,
        },
        WorkerEvent::KeywordsFound(findings) => AppMsg::KeywordsFound {
            account_id,
            findings,
        },
        WorkerEvent::Quota(quota) => AppMsg::QuotaFound { account_id, quota },
        WorkerEvent::KeywordsSynced { paths } => AppMsg::KeywordsSynced { account_id, paths },
        WorkerEvent::Restored {
            folder_id,
            message_ids,
        } => AppMsg::UndoRestored {
            account_id,
            folder_id,
            message_ids,
        },
        WorkerEvent::BackfillDone { folder_id } => AppMsg::BackfillDone {
            account_id,
            folder_id,
        },
        WorkerEvent::FolderSynced { folder_id } => AppMsg::FolderSynced {
            account_id,
            folder_id,
        },
        WorkerEvent::FolderUnread { folder_id, unread } => AppMsg::FolderUnread {
            account_id,
            folder_id,
            unread,
        },
        WorkerEvent::FolderUnreadByPath { path, unread } => AppMsg::FolderUnreadByPath {
            account_id,
            path,
            unread,
        },
        WorkerEvent::SeenSettled { path, uid } => AppMsg::SeenSettled {
            account_id,
            path,
            uid,
        },
        WorkerEvent::Answered {
            path,
            uid,
            forwarded,
        } => AppMsg::Answered {
            account_id,
            path,
            uid,
            forwarded,
        },
        WorkerEvent::TagSettled { path, uid, keyword } => AppMsg::TagSettled {
            account_id,
            path,
            uid,
            keyword,
        },
        WorkerEvent::MovesSettled { path, uids } => AppMsg::MovesSettled {
            account_id,
            path,
            uids,
        },
        WorkerEvent::RawExported { token, raw } => AppMsg::RawExported { token, raw },
        WorkerEvent::RawImported { token, result } => AppMsg::RawImported { token, result },
        WorkerEvent::AttachmentDeleted {
            message_id,
            path,
            uid,
            new_uid,
            name,
            size,
        } => AppMsg::AttachmentDeleted {
            account_id,
            message_id,
            path,
            uid,
            new_uid,
            name,
            size,
        },
        WorkerEvent::AttachmentNotDeleted {
            message_id,
            name,
            size,
        } => AppMsg::AttachmentNotDeleted {
            account_id,
            message_id,
            name,
            size,
        },
        WorkerEvent::Gone {
            message_id,
            path,
            uid,
        } => AppMsg::MessageGone {
            account_id,
            message_id,
            path,
            uid,
        },
        WorkerEvent::RefsRepaired { folder_id } => AppMsg::RefsRepaired {
            account_id,
            folder_id,
        },
        WorkerEvent::Body {
            message_id,
            path,
            body,
        } => {
            let body = crate::legacy::current_markup(body);
            AppMsg::Body {
                account_id,
                message_id,
                path,
                body,
            }
        }
        WorkerEvent::SenderChecked {
            path,
            message_id,
            check,
        } => AppMsg::SenderChecked {
            account_id,
            path,
            message_id,
            check: Box::new(check),
        },
        WorkerEvent::Source { text, .. } => AppMsg::Source { text },
        WorkerEvent::Attachments {
            path,
            message_id,
            items,
        } => AppMsg::Attachments {
            account_id,
            path,
            message_id,
            items,
        },
        WorkerEvent::AttachmentsPending { path, message_id } => AppMsg::AttachmentsPending {
            account_id,
            path,
            message_id,
        },
        WorkerEvent::NoAttachments { path, message_id } => AppMsg::NoAttachments {
            account_id,
            path,
            message_id,
        },
        WorkerEvent::HasAttachments { path, message_id } => AppMsg::HasAttachments {
            account_id,
            path,
            message_id,
        },
        WorkerEvent::Sent => AppMsg::Sent { account_id },
        WorkerEvent::SendDone { ticket, sent } => AppMsg::SendDone { ticket, sent },
        WorkerEvent::Unsent(message) => AppMsg::Unsent {
            account_id,
            message,
        },
        WorkerEvent::Outbox { items } => AppMsg::OutboxItems { account_id, items },
        WorkerEvent::Notice(text) => AppMsg::Notice(text),
        WorkerEvent::DraftSaved {
            autosave,
            message_id,
        } => AppMsg::DraftSaved {
            account_id,
            autosave,
            message_id,
        },
        WorkerEvent::Status(text) => AppMsg::Status { account_id, text },
        WorkerEvent::Error { text, connectivity } => AppMsg::Error {
            account_id,
            text,
            connectivity,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_and_worker_outputs_map_to_app_messages() {
        let dbg = |m: AppMsg| format!("{m:?}");
        assert_eq!(
            dbg(sidebar_output_msg(SidebarOutput::UnifiedSelected)),
            dbg(AppMsg::UnifiedSelected(UnifiedView::Kind(
                FolderKind::Inbox
            )))
        );
        assert_eq!(
            dbg(sidebar_output_msg(SidebarOutput::UnifiedTagsSelected)),
            dbg(AppMsg::TagSelected {
                keyword: None,
                account: None
            })
        );
        assert_eq!(
            dbg(map_event(3, WorkerEvent::BulkComplete)),
            dbg(AppMsg::BulkComplete)
        );
        assert_eq!(
            dbg(map_event(
                3,
                WorkerEvent::AttachmentsScanned {
                    folder_path: "INBOX".into(),
                    added: 1,
                    remaining: 2
                }
            )),
            dbg(AppMsg::AttachmentsScanned {
                account_id: 3,
                folder_path: "INBOX".into(),
                added: 1,
                remaining: 2
            })
        );
    }
}
