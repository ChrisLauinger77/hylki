//! The main window's remembered layout: sidebar.toml (account order,
//! open sections, folder order, favorites) and window.toml (size).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::default_on;
use super::{config_base, write_private};

// ---------------------------------------------------------------------------
// Sidebar state (account display order + collapsed accounts), keyed by email
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct SidebarFile {
    /// Account emails in the user's preferred display order.
    #[serde(default)]
    pub(super) order: Vec<String>,
    /// Account emails whose folder list is collapsed.
    #[serde(default)]
    pub(super) collapsed: Vec<String>,
    /// Account emails whose custom-folders section is expanded (default hidden).
    #[serde(default)]
    pub(super) folders_expanded: Vec<String>,
    /// Whether the whole sidebar is in icon-only (collapsed) mode.
    #[serde(default)]
    pub(super) icon_only: bool,
    /// Collapsed folder-tree nodes, as "email\tpath" entries.
    #[serde(default)]
    pub(super) tree_collapsed: Vec<String>,
    /// Folders the user has put in an order of their own, as "email\tpath"
    /// entries: a folder sorts among its siblings by where it stands here.
    #[serde(default)]
    pub(super) folder_order: Vec<String>,
    /// The three sections' open state; open when the file predates them,
    /// which is how they always started.
    #[serde(default = "default_on")]
    pub(super) unified_expanded: bool,
    #[serde(default = "default_on")]
    pub(super) filtered_expanded: bool,
    #[serde(default = "default_on")]
    pub(super) tags_expanded: bool,
    /// The unified Starred / Sent / Drafts rows' account lists; closed
    /// until opened.
    #[serde(default)]
    pub(super) starred_expanded: bool,
    #[serde(default)]
    pub(super) sent_expanded: bool,
    #[serde(default)]
    pub(super) drafts_expanded: bool,
    #[serde(default)]
    pub(super) archive_expanded: bool,
    #[serde(default)]
    pub(super) junk_expanded: bool,
    #[serde(default)]
    pub(super) trash_expanded: bool,
    /// Folders picked as favorites (#367), as "email\tpath" entries in the
    /// order they were added, and whether their list is open.
    #[serde(default)]
    pub(super) favorites: Vec<String>,
    #[serde(default = "default_on")]
    pub(super) favorites_expanded: bool,
    /// Whether the Smart Folders list (#417) is open.
    #[serde(default = "default_on")]
    pub(super) smart_expanded: bool,
    /// Whether the Contacts row's address books are shown.
    #[serde(default)]
    pub(super) contacts_expanded: bool,
    /// The accounts and books folded under the Contacts row, by key.
    #[serde(default)]
    pub(super) contacts_folded: Vec<String>,
    /// Account emails whose own Filtered Folders / Tags sections are open
    /// (closed by default, like their custom folders).
    #[serde(default)]
    pub(super) filtered_expanded_accounts: Vec<String>,
    #[serde(default)]
    pub(super) tags_expanded_accounts: Vec<String>,
}

pub(super) fn sidebar_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("sidebar.toml"))
}

/// Sidebar state persisted across restarts.
#[derive(Debug, Default)]
pub struct SidebarState {
    /// Account emails in display order.
    pub order: Vec<String>,
    /// Account emails whose folder list is collapsed.
    pub collapsed: Vec<String>,
    /// Account emails whose custom-folders section is expanded (default hidden).
    pub folders_expanded: Vec<String>,
    /// Whether the sidebar is in icon-only mode.
    pub icon_only: bool,
    /// Collapsed folder-tree nodes, as "email\tpath" entries.
    pub tree_collapsed: Vec<String>,
    /// The user's folder order, as "email\tpath" entries.
    pub folder_order: Vec<String>,
    /// Whether the per-account inbox list under All Inboxes is open.
    pub unified_expanded: bool,
    /// Whether the Filtered Folders section is open.
    pub filtered_expanded: bool,
    /// Whether the Tags section is open.
    pub tags_expanded: bool,
    /// The unified Starred / Sent / Drafts rows' account lists.
    pub starred_expanded: bool,
    pub sent_expanded: bool,
    pub drafts_expanded: bool,
    pub archive_expanded: bool,
    pub junk_expanded: bool,
    pub trash_expanded: bool,
    /// Favorite folders (#367), as "email\tpath" entries.
    pub favorites: Vec<String>,
    pub favorites_expanded: bool,
    pub smart_expanded: bool,
    pub contacts_expanded: bool,
    pub contacts_folded: Vec<String>,
    /// Account emails whose own Filtered Folders / Tags sections are open.
    pub filtered_expanded_accounts: Vec<String>,
    pub tags_expanded_accounts: Vec<String>,
}

pub fn load_sidebar_state() -> SidebarState {
    let Some(path) = sidebar_path() else {
        return SidebarState::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return SidebarState::default();
    };
    toml::from_str::<SidebarFile>(&text)
        .map(|s| SidebarState {
            order: s.order,
            collapsed: s.collapsed,
            folders_expanded: s.folders_expanded,
            icon_only: s.icon_only,
            tree_collapsed: s.tree_collapsed,
            folder_order: s.folder_order,
            unified_expanded: s.unified_expanded,
            filtered_expanded: s.filtered_expanded,
            tags_expanded: s.tags_expanded,
            starred_expanded: s.starred_expanded,
            sent_expanded: s.sent_expanded,
            drafts_expanded: s.drafts_expanded,
            archive_expanded: s.archive_expanded,
            junk_expanded: s.junk_expanded,
            trash_expanded: s.trash_expanded,
            favorites: s.favorites,
            favorites_expanded: s.favorites_expanded,
            smart_expanded: s.smart_expanded,
            contacts_expanded: s.contacts_expanded,
            contacts_folded: s.contacts_folded,
            filtered_expanded_accounts: s.filtered_expanded_accounts,
            tags_expanded_accounts: s.tags_expanded_accounts,
        })
        .unwrap_or_default()
}

pub fn save_sidebar_state(state: &SidebarState) {
    let Some(path) = sidebar_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = SidebarFile {
        order: state.order.clone(),
        collapsed: state.collapsed.clone(),
        folders_expanded: state.folders_expanded.clone(),
        icon_only: state.icon_only,
        tree_collapsed: state.tree_collapsed.clone(),
        folder_order: state.folder_order.clone(),
        unified_expanded: state.unified_expanded,
        filtered_expanded: state.filtered_expanded,
        tags_expanded: state.tags_expanded,
        starred_expanded: state.starred_expanded,
        sent_expanded: state.sent_expanded,
        drafts_expanded: state.drafts_expanded,
        archive_expanded: state.archive_expanded,
        junk_expanded: state.junk_expanded,
        trash_expanded: state.trash_expanded,
        favorites: state.favorites.clone(),
        favorites_expanded: state.favorites_expanded,
        smart_expanded: state.smart_expanded,
        contacts_expanded: state.contacts_expanded,
        contacts_folded: state.contacts_folded.clone(),
        filtered_expanded_accounts: state.filtered_expanded_accounts.clone(),
        tags_expanded_accounts: state.tags_expanded_accounts.clone(),
    };
    match toml::to_string_pretty(&file) {
        Ok(toml) => {
            if let Err(e) = write_private(&path, &toml) {
                tracing::warn!("could not save sidebar state: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize sidebar state: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Window state (size + maximized). Position/monitor can't be persisted on
// Wayland — the compositor owns window placement.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct WindowFile {
    pub(super) width: i32,
    pub(super) height: i32,
    #[serde(default)]
    pub(super) maximized: bool,
}

impl Default for WindowFile {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 840,
            maximized: false,
        }
    }
}

pub(super) fn window_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("window.toml"))
}

/// Returns the saved `(width, height, maximized)`, or sensible defaults.
pub fn load_window_state() -> (i32, i32, bool) {
    let file = window_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| toml::from_str::<WindowFile>(&t).ok())
        .unwrap_or_default();
    // Guard against absurd/zero sizes from a bad file.
    let width = if file.width >= 360 { file.width } else { 1280 };
    let height = if file.height >= 300 { file.height } else { 840 };
    (width, height, file.maximized)
}

pub fn save_window_state(width: i32, height: i32, maximized: bool) {
    let Some(path) = window_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = WindowFile {
        width,
        height,
        maximized,
    };
    if let Ok(toml) = toml::to_string_pretty(&file) {
        let _ = std::fs::write(&path, toml);
    }
}
