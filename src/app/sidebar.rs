//! The sidebar: its outputs, its header in rail mode, and the state it keeps
//! (collapsed accounts, folder order, sizes).

use super::*;

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

/// Trim the sidebar header in the icon-only rail so it no longer forces a minimum
/// width wider than the rail: hide the (redundant) window-control buttons and tag
/// the header so its Compose/Menu buttons shrink to fit (see `.rail-header` in the
/// stylesheet). The reader pane's header still carries the window's close button,
/// so nothing becomes unreachable.
pub(super) fn set_sidebar_header_compact(
    header: &adw::HeaderBar,
    title: &gtk::Label,
    menu: &gtk::MenuButton,
    refresh: &gtk::Button,
    compact: bool,
) {
    header.set_show_start_title_buttons(!compact);
    header.set_show_end_title_buttons(!compact);
    // The menu is always in exactly one of three spots — packed end
    // (expanded), the title slot (rail), or packed start (peek) — and
    // HeaderBar::remove detaches it from any of them, so transitions are
    // free to start from whichever state is current. Refresh is either packed
    // start (expanded / peek) or unparented (rail, which stacks its own
    // refresh under the menu instead) — only remove it while it is a child.
    header.remove(menu);
    if refresh.parent().is_some() {
        header.remove(refresh);
    }
    menu.set_margin_start(0);
    // In the rail there is no title to show, so the menu button takes the title
    // slot — the only position a header bar centres — instead of hugging the
    // right edge of an 80px strip. Both widgets are held by the model, so the
    // one being displaced survives being unparented here.
    if compact {
        header.add_css_class("rail-header");
        header.set_title_widget(Some(menu));
    } else {
        header.remove_css_class("rail-header");
        header.set_title_widget(Some(title));
        header.pack_end(menu);
        header.pack_start(refresh);
    }
    title.set_visible(!compact);
}
