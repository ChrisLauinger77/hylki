//! The child components' outputs turned into the messages they mean to the
//! root component.

use super::*;

/// What the message list reports, as the message it means here.
pub(super) fn message_list_output_msg(out: MessageListOutput) -> AppMsg {
    match out {
        MessageListOutput::Selected {
            message,
            thread,
            solo,
        } => AppMsg::MessageSelected {
            message,
            thread,
            solo,
        },
        MessageListOutput::SelectionKeys(keys) => AppMsg::SelectionKeys(keys),
        MessageListOutput::Checked(count) => AppMsg::ListChecked(count),
        MessageListOutput::ViewMissed => AppMsg::EmitUnifiedNow,
        MessageListOutput::MoveMessages {
            account_id,
            dest,
            messages,
        } => AppMsg::MoveMessagesTo {
            account_id,
            dest,
            messages,
        },
        MessageListOutput::DeleteThread { messages } => AppMsg::DeleteThread(messages),
        MessageListOutput::ThreadGrew { message, thread } => AppMsg::ThreadGrew {
            message: Box::new(message),
            thread,
        },
        MessageListOutput::ThreadsListed { groups } => AppMsg::ThreadsListed { groups },
        MessageListOutput::CountChanged(text) => AppMsg::ListCount(text),
        MessageListOutput::Activated { message, thread } => {
            AppMsg::OpenMessageWindow { message, thread }
        }
        MessageListOutput::Action {
            action,
            message,
            conversation,
        } => AppMsg::RowAction {
            action,
            message,
            conversation,
        },
        MessageListOutput::SetTag {
            message,
            keyword,
            add,
        } => AppMsg::SetTag {
            message,
            keyword,
            add,
        },
        MessageListOutput::SetTagMany {
            messages,
            keyword,
            add,
        } => AppMsg::SetTagMany {
            messages,
            keyword,
            add,
        },
        MessageListOutput::Bulk { action, messages } => AppMsg::Bulk { action, messages },
        MessageListOutput::MoveTo {
            messages,
            offer_whole,
            x,
            y,
        } => AppMsg::ListMoveTo {
            messages,
            offer_whole,
            x,
            y,
        },
        MessageListOutput::SelectionCleared => AppMsg::ClearReader,
        MessageListOutput::OpenedByKey { already_open } => AppMsg::OpenedByKey { already_open },
        MessageListOutput::SearchActive(active) => AppMsg::SearchActive(active),
        MessageListOutput::SortChanged(key) => AppMsg::ListSortChanged(key),
        MessageListOutput::ColumnWidths(widths) => AppMsg::ListColumnWidths(widths),
        MessageListOutput::CustomizeColumns => AppMsg::CustomizeColumns,
    }
}

/// What the message view reports, as the message it means here.
pub(super) fn message_view_output_msg(out: MessageViewOutput) -> AppMsg {
    match out {
        MessageViewOutput::AllowSender(addr) => AppMsg::AllowSender(addr),
        MessageViewOutput::SetRemote {
            account_id,
            id,
            show,
        } => AppMsg::SetRemoteContent {
            account_id,
            id,
            show,
        },
        MessageViewOutput::OpenWindow(m) => AppMsg::OpenMessageWindow {
            message: *m,
            thread: Vec::new(),
        },
        MessageViewOutput::CardAction { action, message } => AppMsg::CardAction { action, message },
        MessageViewOutput::ContactSender(m) => AppMsg::CardContact(m),
        MessageViewOutput::AttachmentAction {
            account_id,
            id,
            index,
            save,
        } => AppMsg::CardAttachment {
            account_id,
            id,
            index,
            save,
        },
        MessageViewOutput::CardMenu { message, x, y, hit } => AppMsg::CardMenu {
            message,
            x,
            y,
            hit,
            popout: None,
        },
        MessageViewOutput::CardMoveTo { message, x, y } => AppMsg::ListMoveTo {
            messages: vec![*message],
            offer_whole: false,
            x,
            y,
        },
        MessageViewOutput::MarkSeen { account_id, id } => {
            AppMsg::ThreadMessageSeen { account_id, id }
        }
        MessageViewOutput::SelectCards(keys) => AppMsg::SelectCards(keys),
        MessageViewOutput::ComposeTo(addr) => AppMsg::ComposeTo(addr),
        MessageViewOutput::AddContactAddr(addr) => AppMsg::AddContactAddr(addr),
        MessageViewOutput::ReloadBody(m) => AppMsg::ReloadBody(m),
        MessageViewOutput::Notice(text) => AppMsg::Notice(text),
        MessageViewOutput::ReaderMode(on) => AppMsg::SetReaderMode(on),
        MessageViewOutput::Zoom(step) => AppMsg::ZoomMessage(step),
        MessageViewOutput::Unsubscribe { message, info } => AppMsg::Unsubscribe { message, info },
        MessageViewOutput::InviteAction {
            message,
            invite,
            action,
        } => AppMsg::InviteAction {
            message,
            invite,
            action,
        },
    }
}

/// What the attachment drawer reports, as the message it means here.
pub(super) fn attachment_drawer_output_msg(
    out: crate::ui::attachment_drawer::DrawerOutput,
) -> AppMsg {
    match out {
        crate::ui::attachment_drawer::DrawerOutput::ShowLightbox { items, start } => {
            AppMsg::ShowLightbox { items, start }
        }
        crate::ui::attachment_drawer::DrawerOutput::ShowInMessage(att) => {
            AppMsg::ShowAttachmentInMessage(att)
        }
        crate::ui::attachment_drawer::DrawerOutput::DeleteFromServer(att) => {
            AppMsg::DeleteDrawerAttachment(att)
        }
    }
}

/// What the gallery reports, as the message it means here.
pub(super) fn gallery_output_msg(out: GalleryOutput) -> AppMsg {
    match out {
        GalleryOutput::OpenMessage {
            account_id,
            folder_path,
            uid,
        } => AppMsg::OpenAttachmentMessage {
            account_id,
            folder_path,
            uid,
        },
        GalleryOutput::Load(req) => AppMsg::GalleryLoad(Box::new(req)),
        GalleryOutput::Fetch {
            account_id,
            folder_path,
            uid,
        } => AppMsg::GalleryFetch {
            account_id,
            folder_path,
            uid,
        },
        GalleryOutput::DeleteFromServer {
            account_id,
            folder_path,
            uid,
            name,
            size,
        } => AppMsg::AskDeleteAttachment(AttachmentTarget {
            account_id,
            message_id: uid,
            path: folder_path,
            uid,
            name,
            size,
        }),
    }
}

/// What the contacts page reports, as the message it means here.
pub(super) fn contacts_page_output_msg(out: ContactsPageOutput) -> AppMsg {
    match out {
        ContactsPageOutput::Compose(email) => AppMsg::ComposeTo(email),
        ContactsPageOutput::ComposeBcc(field) => AppMsg::ComposeBcc(field),
        ContactsPageOutput::ToggleSidebar => AppMsg::ToggleSidebar,
        ContactsPageOutput::SaveContact { book_uid, vcard } => {
            AppMsg::SaveContact { book_uid, vcard }
        }
        ContactsPageOutput::CreateContact { book_uid, vcard } => {
            AppMsg::CreateContact { book_uid, vcard }
        }
        ContactsPageOutput::SaveGroup {
            book_uid,
            format,
            original,
            edit,
        } => AppMsg::SaveGroup {
            book_uid,
            format,
            original,
            edit,
        },
        ContactsPageOutput::DeleteContacts(keys) => AppMsg::DeleteContacts(keys),
        ContactsPageOutput::MoveGroup {
            from,
            uid,
            to,
            format,
            edit,
        } => AppMsg::MoveGroup {
            from,
            uid,
            to,
            format,
            edit,
        },
        ContactsPageOutput::MoveContacts { to, cards } => AppMsg::MoveContacts { to, cards },
        ContactsPageOutput::CopyContacts { to, cards } => AppMsg::CopyContacts { to, cards },
        ContactsPageOutput::RenameBook { uid, name } => AppMsg::RenameBook { uid, name },
        ContactsPageOutput::Tree(tree) => AppMsg::ContactTree(tree),
        ContactsPageOutput::ImportContacts(paths) => AppMsg::ImportContacts(paths),
        ContactsPageOutput::WantSuggestions => AppMsg::ContactSuggestions,
        ContactsPageOutput::ShowPhoto { name, data } => {
            // The lightbox routes by extension — a bare contact
            // name sent the JPEG down the PDF path, where poppler
            // hung the UI trying to parse it. Name it by content.
            let name = format!("{name}.{}", crate::models::image_ext(&data));
            AppMsg::ShowLightbox {
                items: vec![crate::models::Attachment { name, data }],
                start: 0,
            }
        }
    }
}

/// What the notifications reports, as the message it means here.
pub(super) fn notifications_output_msg(out: NotifyOutput) -> AppMsg {
    match out {
        NotifyOutput::CountChanged(n) => AppMsg::NotifyCount(n),
        NotifyOutput::ExportLog => AppMsg::Pref(PrefOutput::ExportLog),
        NotifyOutput::Shown(shown) => AppMsg::StatusBarShown(shown),
    }
}

/// Route a sidebar instance's output to the app — the docked rail and the
/// floating peek panel are two instances, driven alike.
pub(super) fn sidebar_output_msg(out: SidebarOutput) -> AppMsg {
    match out {
        SidebarOutput::UnifiedSelected => {
            AppMsg::UnifiedSelected(UnifiedView::Kind(FolderKind::Inbox))
        }
        SidebarOutput::UnifiedKindSelected(kind) => {
            AppMsg::UnifiedSelected(UnifiedView::Kind(kind))
        }
        SidebarOutput::AccountStarredSelected(account_id) => {
            AppMsg::AccountStarredSelected(account_id)
        }
        SidebarOutput::UnifiedFilteredSelected => AppMsg::UnifiedSelected(UnifiedView::Filtered),
        SidebarOutput::UnifiedFavoritesSelected => AppMsg::UnifiedSelected(UnifiedView::Favorites),
        SidebarOutput::UnifiedTagsSelected => AppMsg::TagSelected {
            keyword: None,
            account: None,
        },
        SidebarOutput::SmartSelected(id) => AppMsg::SmartSelected(id),
        SidebarOutput::TagSelected { keyword, account } => AppMsg::TagSelected {
            keyword: Some(keyword),
            account,
        },
        SidebarOutput::ToggleAccountTags(id) => AppMsg::ToggleAccountTags(id),
        SidebarOutput::AttachmentsSelected => AppMsg::ShowAttachments,
        SidebarOutput::ContactsClicked => AppMsg::OpenContacts,
        SidebarOutput::RefreshRequested => AppMsg::Refresh,
        SidebarOutput::StatusBarRequested => AppMsg::ToggleNotifications,
        SidebarOutput::OpenGnomeContacts => AppMsg::LaunchGnomeContacts,
        SidebarOutput::ContactScope(scope) => AppMsg::ContactScope(scope),
        SidebarOutput::ContactDrop { keys, on, moving } => AppMsg::ContactDrop { keys, on, moving },
        SidebarOutput::ContactTreeAction(action) => AppMsg::ContactTreeAction(action),
        SidebarOutput::ContactsFolded(keys) => AppMsg::ContactsFolded(keys),
        SidebarOutput::OutboxSelected => AppMsg::ShowOutbox,
        SidebarOutput::FolderSelected {
            account_id,
            folder_id,
            name,
            path,
        } => AppMsg::FolderSelected {
            account_id,
            folder_id,
            name,
            path,
        },
        SidebarOutput::AccountFolded { id, folded } => AppMsg::AccountFolded { id, folded },
        SidebarOutput::ToggleCustomFolders(id) => AppMsg::ToggleCustomFolders(id),
        SidebarOutput::CollapsedChanged(collapsed) => AppMsg::SidebarCollapsed(collapsed),
        SidebarOutput::SectionsOpen {
            all_inboxes,
            filtered,
            tags,
            starred,
            sent,
            drafts,
            archive,
            junk,
            trash,
            favorites,
            smart,
            contacts,
        } => AppMsg::SidebarSectionsOpen {
            all_inboxes,
            filtered,
            tags,
            starred,
            sent,
            drafts,
            archive,
            junk,
            trash,
            favorites,
            smart,
            contacts,
        },
        SidebarOutput::FolderNodeCollapsed {
            account_id,
            path,
            collapsed,
        } => AppMsg::FolderNodeCollapsed {
            account_id,
            path,
            collapsed,
        },
        SidebarOutput::AddAccount => AppMsg::AddFirstAccount,
        SidebarOutput::ComposeRequested => AppMsg::Compose,
        SidebarOutput::ComposeFromTemplateRequested => AppMsg::NewFromTemplate,
        SidebarOutput::Context(action) => AppMsg::SidebarContext(action),
        SidebarOutput::MoveMessages {
            dest_account,
            dest,
            items,
        } => AppMsg::DropMoveMessages {
            dest_account,
            dest,
            items,
        },
        SidebarOutput::MoveFolder {
            account_id,
            path,
            dest,
        } => AppMsg::MoveFolder {
            account_id,
            path,
            dest,
        },
        SidebarOutput::ReorderFolders { account_id, paths } => {
            AppMsg::ReorderFolders { account_id, paths }
        }
        SidebarOutput::PlaceFolder {
            account_id,
            path,
            dest,
            target,
            after,
        } => AppMsg::PlaceFolder {
            account_id,
            path,
            dest,
            target,
            after,
        },
        SidebarOutput::SetFolderSort { account_id, sort } => {
            AppMsg::SetAccountFolderSort { account_id, sort }
        }
    }
}
