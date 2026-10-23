//! The main window's remembered layout: sidebar.toml (account order,
//! open sections, folder order, favorites) and window.toml (size).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{config_file, default_on, read_toml, save_plain_toml, save_private_toml};

/// Sidebar state persisted across restarts, as sidebar.toml holds it.
///
/// A missing file gives `Default` (every section closed), while a file
/// without the section keys opens the ones marked `default_on`.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct SidebarState {
    /// Account emails in the user's preferred display order.
    #[serde(default)]
    pub order: Vec<String>,
    /// Account emails whose folder list is collapsed.
    #[serde(default)]
    pub collapsed: Vec<String>,
    /// Account emails whose custom-folders section is expanded (default hidden).
    #[serde(default)]
    pub folders_expanded: Vec<String>,
    /// Whether the whole sidebar is in icon-only (collapsed) mode.
    #[serde(default)]
    pub icon_only: bool,
    /// Collapsed folder-tree nodes, as "email\tpath" entries.
    #[serde(default)]
    pub tree_collapsed: Vec<String>,
    /// Folders the user has put in an order of their own, as "email\tpath"
    /// entries: a folder sorts among its siblings by where it stands here.
    #[serde(default)]
    pub folder_order: Vec<String>,
    /// The three sections' open state; open when the file predates them,
    /// which is how they always started.
    #[serde(default = "default_on")]
    pub unified_expanded: bool,
    #[serde(default = "default_on")]
    pub filtered_expanded: bool,
    #[serde(default = "default_on")]
    pub tags_expanded: bool,
    /// The unified Starred / Sent / Drafts rows' account lists; closed
    /// until opened.
    #[serde(default)]
    pub starred_expanded: bool,
    #[serde(default)]
    pub sent_expanded: bool,
    #[serde(default)]
    pub drafts_expanded: bool,
    #[serde(default)]
    pub archive_expanded: bool,
    #[serde(default)]
    pub junk_expanded: bool,
    #[serde(default)]
    pub trash_expanded: bool,
    /// Folders picked as favorites (#367), as "email\tpath" entries in the
    /// order they were added, and whether their list is open.
    #[serde(default)]
    pub favorites: Vec<String>,
    #[serde(default = "default_on")]
    pub favorites_expanded: bool,
    /// Whether the Smart Folders list (#417) is open.
    #[serde(default = "default_on")]
    pub smart_expanded: bool,
    /// Whether the Contacts row's address books are shown.
    #[serde(default)]
    pub contacts_expanded: bool,
    /// The accounts and books folded under the Contacts row, by key.
    #[serde(default)]
    pub contacts_folded: Vec<String>,
    /// Account emails whose own Filtered Folders / Tags sections are open
    /// (closed by default, like their custom folders).
    #[serde(default)]
    pub filtered_expanded_accounts: Vec<String>,
    #[serde(default)]
    pub tags_expanded_accounts: Vec<String>,
}

/// sidebar.toml's own name for its shape, used by the settings bundle.
pub(super) type SidebarFile = SidebarState;

pub(super) fn sidebar_path() -> Option<PathBuf> {
    config_file("sidebar.toml")
}

pub fn load_sidebar_state() -> SidebarState {
    read_toml::<SidebarState>(sidebar_path()).unwrap_or_default()
}

pub fn save_sidebar_state(state: &SidebarState) {
    save_private_toml(sidebar_path(), state, "sidebar state", true);
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
    config_file("window.toml")
}

/// Returns the saved `(width, height, maximized)`, or sensible defaults.
pub fn load_window_state() -> (i32, i32, bool) {
    let file = read_toml::<WindowFile>(window_path()).unwrap_or_default();
    // Guard against absurd/zero sizes from a bad file.
    let width = if file.width >= 360 { file.width } else { 1280 };
    let height = if file.height >= 300 { file.height } else { 840 };
    (width, height, file.maximized)
}

pub fn save_window_state(width: i32, height: i32, maximized: bool) {
    let file = WindowFile {
        width,
        height,
        maximized,
    };
    save_plain_toml(window_path(), &file);
}
