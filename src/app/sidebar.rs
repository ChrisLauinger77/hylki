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

impl AppModel {
    /// Persist the sidebar's per-account state (order, collapse, custom-folders
    /// expansion, and icon-only mode).
    pub(super) fn save_sidebar_state(&self) {
        config::save_sidebar_state(&config::SidebarState {
            order: self.account_order.clone(),
            collapsed: self.collapsed.clone(),
            folders_expanded: self.folders_expanded.clone(),
            icon_only: self.sidebar_collapsed,
            tree_collapsed: self.tree_collapsed.clone(),
            folder_order: self.folder_order.clone(),
            unified_expanded: self.unified_expanded,
            filtered_expanded: self.filtered_expanded,
            tags_expanded: self.tags_expanded,
            starred_expanded: self.starred_expanded,
            sent_expanded: self.sent_expanded,
            drafts_expanded: self.drafts_expanded,
            archive_expanded: self.archive_expanded,
            junk_expanded: self.junk_expanded,
            trash_expanded: self.trash_expanded,
            favorites: self.favorites.clone(),
            favorites_expanded: self.favorites_expanded,
            smart_expanded: self.smart_expanded,
            contacts_expanded: self.contacts_expanded,
            contacts_folded: self.contacts_folded.clone(),
            filtered_expanded_accounts: self.filtered_expanded_accounts.clone(),
            tags_expanded_accounts: self.tags_expanded_accounts.clone(),
        });
    }

    /// Keep a run of sibling folders in its new order. The run goes to the
    /// end of the list; only the order within a run counts, so the others
    /// stand as they were.
    pub(super) fn keep_folder_order(&mut self, account_id: u32, paths: &[String]) {
        self.store_folder_order(account_id, paths);
        self.save_sidebar_state();
        self.rebuild_sidebar();
    }

    fn store_folder_order(&mut self, account_id: u32, paths: &[String]) {
        let Some(email) = self.email_of(account_id) else {
            return;
        };
        let keys: Vec<String> = paths.iter().map(|p| format!("{email}\t{p}")).collect();
        self.folder_order.retain(|k| !keys.contains(k));
        self.folder_order.extend(keys);
    }

    /// The folder order an account chose for itself, if any.
    pub(super) fn own_folder_sort(&self, account_id: u32) -> Option<config::FolderSort> {
        self.effective_config()
            .get(account_id.saturating_sub(1) as usize)
            .and_then(|c| c.folder_sort)
    }

    /// How an account's custom folders are sorted: its own choice, or
    /// Settings'.
    pub(super) fn folder_sort_of(&self, account_id: u32) -> config::FolderSort {
        self.own_folder_sort(account_id).unwrap_or(self.folder_sort)
    }

    /// An account's folders in the order the sidebar shows them.
    pub(super) fn shown_folders(&self, account_id: u32) -> Vec<Folder> {
        let prefix = self
            .email_of(account_id)
            .map(|e| format!("{e}\t"))
            .unwrap_or_default();
        let order: Vec<String> = self
            .folder_order
            .iter()
            .filter_map(|k| k.strip_prefix(&prefix).map(String::from))
            .collect();
        crate::ui::sidebar::order_folders(
            self.folders.get(&account_id).cloned().unwrap_or_default(),
            &order,
            self.folder_sort_of(account_id),
        )
    }

    /// Give an account its own folder order (`None` follows Settings), in
    /// the file and in the Settings window's copy of the account, which
    /// would otherwise put the old one back when its editor is saved.
    pub(super) fn set_account_folder_sort(
        &mut self,
        account_id: u32,
        sort: Option<config::FolderSort>,
    ) {
        let demo = self.config.is_empty() && demo_mode();
        let accounts = if demo {
            &mut self.demo_config
        } else {
            &mut self.config
        };
        let Some(cfg) = accounts.get_mut(account_id.saturating_sub(1) as usize) else {
            return;
        };
        if cfg.folder_sort == sort {
            return;
        }
        cfg.folder_sort = sort;
        let email = cfg.email.clone();
        let saved = if demo {
            config::save_demo_accounts(&self.demo_config).map_err(|e| e.to_string())
        } else {
            config::save(&self.config).map_err(|e| e.to_string())
        };
        if let Err(e) = saved {
            self.notifications.emit(NotifyInput::Push {
                text: i18n_f("Could not save account: {e}", &[("e", &e)]),
                error: true,
                connectivity: false,
            });
        }
        if let Some(acc) = &self.accounts_win {
            acc.emit(crate::ui::accounts::AccountsInput::SetFolderSort { email, sort });
        }
    }

    /// Dragging a custom folder is arranging them by hand, so an account
    /// sorted another way switches to Custom Order, every custom folder kept
    /// first where the old sort showed it: only the dragged one moves.
    pub(super) fn adopt_custom_order(&mut self, account_id: u32) {
        if self.folder_sort_of(account_id) == config::FolderSort::Custom {
            return;
        }
        let shown: Vec<String> = self
            .shown_folders(account_id)
            .into_iter()
            .filter(|f| f.kind == FolderKind::Custom)
            .map(|f| f.path)
            .collect();
        self.store_folder_order(account_id, &shown);
        let sort =
            (self.folder_sort != config::FolderSort::Custom).then_some(config::FolderSort::Custom);
        self.set_account_folder_sort(account_id, sort);
    }

    /// Account emails in display order: those listed in `account_order` first
    /// (in that order), then any remaining accounts by id.
    pub(super) fn ordered_emails(&self) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();
        for email in &self.account_order {
            if self.accounts.iter().any(|a| &a.email == email) && !result.contains(email) {
                result.push(email.clone());
            }
        }
        let mut rest: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|a| !result.contains(&a.email))
            .collect();
        rest.sort_by_key(|a| a.id);
        for a in rest {
            result.push(a.email.clone());
        }
        result
    }
}
