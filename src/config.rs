//! Hylki's settings: the files under `~/.config/hylki/` (accounts, the
//! preferences in privacy.toml, filters, tags, sidebar and window state, and
//! smaller ones), the secrets kept in the system keyring, and where the
//! config, cache and data directories are.
//!
//! Each file has its own child module; every public item is re-exported
//! here, so callers name them as `config::Item`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[cfg(test)]
pub(crate) mod golden;
#[cfg(test)]
mod snapshot_tests;

mod accounts;

mod look;
mod prefs;

mod bundle;
#[cfg(test)]
mod filter_tests;
mod filters;
mod smart_folders;
mod tags;
pub use bundle::*;
pub use filters::*;
pub use smart_folders::*;
pub use tags::*;
mod privacy;
mod sections;
pub use look::*;
pub use prefs::*;
pub use privacy::*;
pub use sections::*;
mod launch_files;
mod secrets;
mod sound;
pub use accounts::*;
pub use launch_files::*;
pub use secrets::*;
pub use sound::*;

/// Write a config file that only its owner can read, without ever having
/// existed as anything else.
///
/// `fs::write` then `set_permissions` leaves a window in which the file sits on
/// disk with whatever the umask allowed — brief, but these files carry
/// hostnames, usernames, OAuth client secrets and correspondent lists. Creating
/// with the mode already set closes it.
pub(crate) fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents.as_bytes())?;
    // An existing file keeps its old mode through `OpenOptions::mode`, which
    // only applies at creation — so tighten regardless.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// XDG base directories, with one twist: the beta channel running inside its
/// own Flatpak sandbox (app ID co.hyprlab.Hylki.Beta) would get an empty
/// `~/.var/app/co.hyprlab.Hylki.Beta` tree of its own — so it redirects to the
/// STABLE app's tree instead, sharing accounts, settings and the mail cache
/// with the stable install. The keyring service name is identical across
/// channels, so credentials are shared through the same redirection-free path.
/// Outside Flatpak (host builds) both channels already share `~/.config/hylki`
/// et al., so no redirection is needed or done.
fn shared_base(own: fn() -> Option<PathBuf>, sub: &str) -> Option<PathBuf> {
    if cfg!(feature = "beta")
        && std::env::var("FLATPAK_ID").is_ok_and(|id| id == "co.hyprlab.Hylki.Beta")
        && stable_data_present()
    {
        return Some(
            dirs::home_dir()?
                .join(".var/app/co.hyprlab.Hylki")
                .join(sub),
        );
    }
    own()
}

/// Whether the shared flatpak directory is actually reachable. Flatpak
/// silently SKIPS a `--filesystem` grant whose host path doesn't exist, which
/// left `~/.var/app/co.hyprlab.Hylki` pointing at the sandbox's throwaway
/// tmpfs on beta-only installs — accounts "saved" there and vanished on quit
/// (issue #83). The real fix is the manifest's `:create` suffix, which makes
/// flatpak create the host directory itself — a beta-first install thereby
/// establishes the standard persistent home a later stable install picks up,
/// in either install order. This check remains as defence in depth: should
/// the mount ever be missing anyway (an old manifest, a stripped-down
/// installation), the beta falls back to its own persistent home rather than
/// writing into the tmpfs. Decided once at first use, so our own writes
/// creating the path on the tmpfs mid-session can't flip it.
fn stable_data_present() -> bool {
    use std::sync::OnceLock;
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        dirs::home_dir()
            .map(|h| h.join(".var/app/co.hyprlab.Hylki").is_dir())
            .unwrap_or(false)
    })
}

pub fn config_base() -> Option<PathBuf> {
    shared_base(dirs::config_dir, "config")
}

pub fn cache_base() -> Option<PathBuf> {
    shared_base(dirs::cache_dir, "cache")
}

pub fn data_base() -> Option<PathBuf> {
    shared_base(dirs::data_dir, "data")
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn is_true(b: &bool) -> bool {
    *b
}

/// Write a settings file readable by the owner only (for modules outside
/// this one that keep their own file, like `cloud.toml`).
pub fn write_private_file(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    write_private(path, contents)
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ---------------------------------------------------------------------------
// Sidebar state (account display order + collapsed accounts), keyed by email
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, Serialize)]
struct SidebarFile {
    /// Account emails in the user's preferred display order.
    #[serde(default)]
    order: Vec<String>,
    /// Account emails whose folder list is collapsed.
    #[serde(default)]
    collapsed: Vec<String>,
    /// Account emails whose custom-folders section is expanded (default hidden).
    #[serde(default)]
    folders_expanded: Vec<String>,
    /// Whether the whole sidebar is in icon-only (collapsed) mode.
    #[serde(default)]
    icon_only: bool,
    /// Collapsed folder-tree nodes, as "email\tpath" entries.
    #[serde(default)]
    tree_collapsed: Vec<String>,
    /// Folders the user has put in an order of their own, as "email\tpath"
    /// entries: a folder sorts among its siblings by where it stands here.
    #[serde(default)]
    folder_order: Vec<String>,
    /// The three sections' open state; open when the file predates them,
    /// which is how they always started.
    #[serde(default = "default_on")]
    unified_expanded: bool,
    #[serde(default = "default_on")]
    filtered_expanded: bool,
    #[serde(default = "default_on")]
    tags_expanded: bool,
    /// The unified Starred / Sent / Drafts rows' account lists; closed
    /// until opened.
    #[serde(default)]
    starred_expanded: bool,
    #[serde(default)]
    sent_expanded: bool,
    #[serde(default)]
    drafts_expanded: bool,
    #[serde(default)]
    archive_expanded: bool,
    #[serde(default)]
    junk_expanded: bool,
    #[serde(default)]
    trash_expanded: bool,
    /// Folders picked as favorites (#367), as "email\tpath" entries in the
    /// order they were added, and whether their list is open.
    #[serde(default)]
    favorites: Vec<String>,
    #[serde(default = "default_on")]
    favorites_expanded: bool,
    /// Whether the Smart Folders list (#417) is open.
    #[serde(default = "default_on")]
    smart_expanded: bool,
    /// Whether the Contacts row's address books are shown.
    #[serde(default)]
    contacts_expanded: bool,
    /// The accounts and books folded under the Contacts row, by key.
    #[serde(default)]
    contacts_folded: Vec<String>,
    /// Account emails whose own Filtered Folders / Tags sections are open
    /// (closed by default, like their custom folders).
    #[serde(default)]
    filtered_expanded_accounts: Vec<String>,
    #[serde(default)]
    tags_expanded_accounts: Vec<String>,
}

fn sidebar_path() -> Option<PathBuf> {
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
struct WindowFile {
    width: i32,
    height: i32,
    #[serde(default)]
    maximized: bool,
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

fn window_path() -> Option<PathBuf> {
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

// ---------------------------------------------------------------------------
// Keyring health check + one-time setup-help flag
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, Serialize)]
struct StateFile {
    /// Set once the user dismisses the Linux Mint keyring setup tip.
    #[serde(default)]
    mint_keyring_help_dismissed: bool,
    /// In-message attachment drawer: collapsed (showing only its header).
    #[serde(default)]
    drawer_collapsed: bool,
    /// The way of asking for message summaries that worked for an account
    /// whose server rejects the default (#226), by email: "headers",
    /// "no-previews" or "headers-no-previews". Absent = the default. Written
    /// only once a stepped-to mode has actually loaded a folder, so a
    /// launch starts there instead of failing its way down the ladder.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    fetch_modes: std::collections::BTreeMap<String, String>,
    /// Expanded attachment-drawer height in px (the dragged split).
    #[serde(default = "default_drawer_height")]
    drawer_height: i32,
    /// Attachment drawer shows an alphabetical list instead of the thumbnail grid.
    #[serde(default)]
    drawer_list_view: bool,
    /// The drawer's list view sorts Z→A instead of A→Z.
    #[serde(default)]
    drawer_sort_desc: bool,
    /// Attachments gallery shows a table instead of the thumbnail grid.
    #[serde(default)]
    gallery_table_view: bool,
    /// Attachments gallery thumbnail cell width in px.
    #[serde(default = "default_gallery_thumb_width")]
    gallery_thumb_width: i32,
    /// Attachments gallery sort criterion (the sort dropdown's row index).
    #[serde(default)]
    gallery_sort: u32,
    /// Attachments gallery: pull from Archive folders.
    #[serde(default = "default_on")]
    gallery_archive: bool,
    /// Attachments gallery: pull from folders that are neither Inbox nor
    /// Archive (custom folders and Starred).
    #[serde(default = "default_on")]
    gallery_other: bool,
    /// Attachments gallery: folders the user ticked or unticked by hand in the
    /// folder list, as "<account id>\t<path>\t<0|1>". Only folders that differ
    /// from their kind's default are stored, so a new folder on the server
    /// follows the default rather than an entry that predates it.
    #[serde(default)]
    gallery_folders: Vec<String>,
    /// Message-list pane width in px (#28 — it reset every launch).
    #[serde(default = "default_list_pane_width")]
    list_pane_width: i32,
    /// Contacts view: the contact-list pane's width in px.
    #[serde(default = "default_contacts_pane_width")]
    contacts_pane_width: i32,
    /// The About window's height, remembering the user's vertical resize.
    /// (The settings window's is fixed since its two-pane layout, #141; an
    /// old `prefs_height` key in the file is ignored.)
    #[serde(default = "default_about_height")]
    about_height: i32,
    /// Split-reply panel height in px (the dragged divider). 0 = never
    /// dragged: the panel opens at its computed default.
    #[serde(default)]
    split_reply_height: i32,
    /// The welcome wizard has been completed once (Start Reading pressed),
    /// so an install with no accounts is not greeted with it again — a
    /// restart right after the wizard, for the app icon, must not loop.
    #[serde(default)]
    wizard_completed: bool,
    /// The Flatpak runtime commit under which WebKit hung on the host's
    /// fonts and was given the runtime's own instead (#296). Kept per
    /// commit, so a runtime update gets another try with every font.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    web_font_fallback: Option<String>,
    /// The mail view open last (#256): an account's folder, or All Inboxes
    /// when `email` is empty. Accounts are named by address, which stays
    /// true across launches where an account's number may not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_view: Option<LastView>,
    /// The account whose mail was open last, kept through a visit to All
    /// Inboxes so "the last account's inbox" still has an account to mean.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    last_account: String,
    /// The icon generation whose default has been asserted over the stored
    /// choice (see `app_icon::ICON_GENERATION`): a release that brings a new
    /// authoritative icon bumps the constant, and the first start on it puts
    /// that icon on every install once, whatever was chosen before.
    #[serde(default)]
    app_icon_generation: u32,
    /// The chosen app icon (an id from `app_icon::catalog`). Absent until
    /// the first start of a build that offers the choice settles it — see
    /// `app_icon::init_on_startup`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_icon: Option<String>,
    /// Each folder's unread count as of the last time it was looked at
    /// (#343), keyed "address␟folder path".
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    unread_seen: std::collections::BTreeMap<String, u32>,
    /// The fonts last picked with the composer's Other Font… (#358), the
    /// newest first, for the font menu to offer again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recent_fonts: Vec<RecentFont>,
}

/// A font family picked in the composer (#358), and whether it is
/// monospaced, which decides the generic family it falls back to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct RecentFont {
    pub name: String,
    #[serde(default)]
    pub mono: bool,
}

/// How many picked fonts the font menu keeps.
const RECENT_FONTS: usize = 5;

pub fn load_recent_fonts() -> Vec<RecentFont> {
    load_state().recent_fonts
}

/// Put `font` at the head of the recent fonts, once, and keep the newest
/// few.
pub fn note_recent_font(font: RecentFont) {
    update_state(|s| {
        s.recent_fonts
            .retain(|f| !f.name.eq_ignore_ascii_case(&font.name));
        s.recent_fonts.insert(0, font);
        s.recent_fonts.truncate(RECENT_FONTS);
    });
}

fn default_about_height() -> i32 {
    740
}

fn default_list_pane_width() -> i32 {
    350
}

fn default_contacts_pane_width() -> i32 {
    280
}

fn default_drawer_height() -> i32 {
    160
}

fn default_gallery_thumb_width() -> i32 {
    230
}

fn state_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("state.toml"))
}

/// Read the remembered UI state. A missing or unreadable file falls back to
/// deserializing an empty document, not to `StateFile::default()`: the derived
/// Default hands every field its type's zero, which is not what the
/// `#[serde(default = …)]` on each one says a first run should get (a pane
/// width of 0, a gallery that pulls from nothing). Every field carries a serde
/// default, so the empty document always parses.
fn load_state() -> StateFile {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| toml::from_str::<StateFile>(&t).ok())
        .or_else(|| toml::from_str::<StateFile>("").ok())
        .unwrap_or_default()
}

/// Each folder's unread count when it was last looked at (#343).
pub fn load_unread_seen() -> std::collections::BTreeMap<String, u32> {
    load_state().unread_seen
}

pub fn save_unread_seen(seen: &std::collections::BTreeMap<String, u32>) {
    update_state(|s| s.unread_seen = seen.clone());
}

/// Change one thing in state.toml.
fn update_state(change: impl FnOnce(&mut StateFile)) {
    let mut s = load_state();
    change(&mut s);
    save_state(&s);
}

fn save_state(state: &StateFile) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(toml) = toml::to_string_pretty(state) {
        let _ = std::fs::write(&path, toml);
    }
}

/// A mail view to reopen at launch (#256).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct LastView {
    /// The account's address; empty for All Inboxes.
    #[serde(default)]
    pub email: String,
    /// The folder's path on the server.
    #[serde(default)]
    pub path: String,
}

/// The view open last and the account whose mail was open last.
pub fn load_last_view() -> (Option<LastView>, String) {
    let s = load_state();
    (s.last_view, s.last_account)
}

/// Record the view just opened, unless it is the one already recorded:
/// this runs on every folder switch.
pub fn save_last_view(view: LastView) {
    let mut s = load_state();
    let account = if view.email.is_empty() {
        s.last_account.clone()
    } else {
        view.email.clone()
    };
    if s.last_view.as_ref() == Some(&view) && s.last_account == account {
        return;
    }
    s.last_view = Some(view);
    s.last_account = account;
    save_state(&s);
}

/// Whether the welcome wizard has been completed before.
pub fn wizard_completed() -> bool {
    load_state().wizard_completed
}

pub fn mark_wizard_completed() {
    let mut s = load_state();
    if !s.wizard_completed {
        s.wizard_completed = true;
        save_state(&s);
    }
}

/// The runtime commit WebKit's font fallback was settled under (#296).
pub fn load_web_font_fallback() -> Option<String> {
    load_state().web_font_fallback.filter(|s| !s.is_empty())
}

pub fn save_web_font_fallback(runtime: Option<&str>) {
    let mut s = load_state();
    let runtime = runtime.map(str::to_string);
    if s.web_font_fallback != runtime {
        s.web_font_fallback = runtime;
        save_state(&s);
    }
}

/// The chosen app icon id, if one has been settled.
pub fn load_app_icon() -> Option<String> {
    load_state().app_icon.filter(|s| !s.is_empty())
}

pub fn load_app_icon_generation() -> u32 {
    load_state().app_icon_generation
}

pub fn save_app_icon_generation(generation: u32) {
    update_state(|s| s.app_icon_generation = generation);
}

pub fn save_app_icon(id: &str) {
    update_state(|s| s.app_icon = Some(id.to_string()));
}

/// Whether the one-time Mint keyring setup tip has already been dismissed.
pub fn mint_keyring_help_dismissed() -> bool {
    load_state().mint_keyring_help_dismissed
}

/// Persist that the user dismissed the Mint keyring setup tip ("Don't show again").
pub fn dismiss_mint_keyring_help() {
    let mut state = load_state();
    state.mint_keyring_help_dismissed = true;
    save_state(&state);
}

/// Persisted state of the in-message attachment drawer.
#[derive(Debug, Clone, Copy)]
pub struct DrawerState {
    /// Expanded content height in px.
    pub height: i32,
    /// Whether the drawer is collapsed to just its header.
    pub collapsed: bool,
    /// Thumbnail edge in px.
    pub thumb: i32,
    /// Show an alphabetical list instead of the thumbnail grid.
    pub list_view: bool,
    /// Sort the list Z→A instead of A→Z.
    pub sort_desc: bool,
}

impl Default for DrawerState {
    fn default() -> Self {
        Self {
            height: 160,
            collapsed: false,
            thumb: 56,
            list_view: false,
            sort_desc: false,
        }
    }
}

/// Load the attachment drawer's remembered state. The collapsed flag, view
/// settings and dragged height are persisted; thumbnail size always starts at
/// its default.
pub fn load_drawer_state() -> DrawerState {
    let s = load_state();
    DrawerState {
        collapsed: s.drawer_collapsed,
        list_view: s.drawer_list_view,
        sort_desc: s.drawer_sort_desc,
        height: s.drawer_height.clamp(96, 4000),
        ..DrawerState::default()
    }
}

/// Persist whether the attachment drawer is collapsed.
/// The remembered summary-fetch mode for an account (#226), as
/// (use ENVELOPE, preview items rejected). The default when nothing is known.
pub fn load_fetch_mode(email: &str) -> (bool, bool) {
    fetch_mode_from_name(
        load_state()
            .fetch_modes
            .get(&email.to_ascii_lowercase())
            .map(String::as_str),
    )
}

/// A fetch mode as state.toml names it, read back; the default for no name
/// or one this build does not know.
fn fetch_mode_from_name(name: Option<&str>) -> (bool, bool) {
    match name {
        Some("headers") => (false, false),
        Some("no-previews") => (true, true),
        Some("headers-no-previews") => (false, true),
        _ => (true, false),
    }
}

/// The name state.toml keeps a fetch mode under; `None` for the default,
/// which is stored as no entry at all.
fn fetch_mode_name(use_envelope: bool, previews_rejected: bool) -> Option<&'static str> {
    match (use_envelope, previews_rejected) {
        (true, false) => None,
        (false, false) => Some("headers"),
        (true, true) => Some("no-previews"),
        (false, true) => Some("headers-no-previews"),
    }
}

/// Remember the summary-fetch mode that worked for an account (#226); the
/// default is remembered by forgetting.
pub fn save_fetch_mode(email: &str, use_envelope: bool, previews_rejected: bool) {
    let mode = fetch_mode_name(use_envelope, previews_rejected);
    let key = email.to_ascii_lowercase();
    let mut s = load_state();
    let current = s.fetch_modes.get(&key).map(String::as_str);
    if current == mode {
        return;
    }
    match mode {
        Some(m) => {
            s.fetch_modes.insert(key, m.to_string());
        }
        None => {
            s.fetch_modes.remove(&key);
        }
    }
    save_state(&s);
}

pub fn save_drawer_collapsed(collapsed: bool) {
    update_state(|s| s.drawer_collapsed = collapsed);
}

/// Persist the attachment drawer's expanded (dragged) height.
pub fn save_drawer_height(height: i32) {
    update_state(|s| s.drawer_height = height.clamp(96, 4000));
}

/// Persist the attachment drawer's view mode (list vs. thumbnail grid).
pub fn save_drawer_list_view(list_view: bool) {
    update_state(|s| s.drawer_list_view = list_view);
}

/// Persist the attachment drawer's list sort direction.
pub fn save_drawer_sort_desc(desc: bool) {
    update_state(|s| s.drawer_sort_desc = desc);
}

/// The attachments gallery's remembered view settings:
/// (table view, thumbnail width px, sort index).
pub fn load_gallery_view() -> (bool, i32, u32) {
    let s = load_state();
    (
        s.gallery_table_view,
        s.gallery_thumb_width.clamp(140, 380),
        s.gallery_sort,
    )
}

/// Persist whether the attachments gallery shows the table view.
pub fn save_gallery_table_view(table: bool) {
    update_state(|s| s.gallery_table_view = table);
}

/// Persist the attachments gallery's thumbnail width.
pub fn save_gallery_thumb_width(width: i32) {
    update_state(|s| s.gallery_thumb_width = width);
}

/// Persist the attachments gallery's sort criterion (dropdown row index).
pub fn save_gallery_sort(sort: u32) {
    update_state(|s| s.gallery_sort = sort);
}

/// One per-folder gallery override as it is stored: "<account id>\t<path>\t<0|1>".
/// A mailbox path can hold almost any byte a server chooses, but never a tab.
fn encode_gallery_folder(id: u32, path: &str, on: bool) -> String {
    format!("{id}\t{path}\t{}", u8::from(on))
}

/// Read back [`encode_gallery_folder`]; `None` for an entry that is not in that
/// shape, so a hand-edited or older file loses only the bad line.
fn decode_gallery_folder(entry: &str) -> Option<(u32, String, bool)> {
    let mut parts = entry.splitn(3, '\t');
    let id: u32 = parts.next()?.parse().ok()?;
    let path = parts.next()?;
    let on = parts.next()?;
    if path.is_empty() {
        return None;
    }
    Some((id, path.to_string(), on == "1"))
}

/// The attachments gallery's remembered folder scope: (pull from Archive,
/// pull from other folders, per-folder overrides as (account id, path, on)).
pub fn load_gallery_scope() -> (bool, bool, Vec<(u32, String, bool)>) {
    let s = load_state();
    let folders = s
        .gallery_folders
        .iter()
        .filter_map(|e| decode_gallery_folder(e))
        .collect();
    (s.gallery_archive, s.gallery_other, folders)
}

/// Persist the attachments gallery's folder scope.
pub fn save_gallery_scope(archive: bool, other: bool, folders: &[(u32, String, bool)]) {
    let mut s = load_state();
    s.gallery_archive = archive;
    s.gallery_other = other;
    s.gallery_folders = folders
        .iter()
        .map(|(id, path, on)| encode_gallery_folder(*id, path, *on))
        .collect();
    save_state(&s);
}

/// The About window's height: tall by default, remembering the user's own
/// vertical resize across restarts.
/// The split-reply panel's dragged height; 0 when it has never been dragged
/// (the caller computes an opening default from the pane).
pub fn load_split_reply_height() -> i32 {
    let h = load_state().split_reply_height;
    if h == 0 {
        0
    } else {
        h.clamp(220, 4000)
    }
}

pub fn save_split_reply_height(height: i32) {
    update_state(|s| s.split_reply_height = height.clamp(220, 4000));
}

pub fn load_about_height() -> i32 {
    load_state().about_height.clamp(400, 4000)
}

pub fn save_about_height(height: i32) {
    update_state(|s| s.about_height = height.clamp(400, 4000));
}

/// The message-list pane's remembered width (clamped to something sane).
pub fn load_list_pane_width() -> i32 {
    load_state().list_pane_width.clamp(324, 4000)
}

/// Persist the message-list pane's width (#28).
pub fn save_list_pane_width(width: i32) {
    update_state(|s| s.list_pane_width = width.clamp(324, 4000));
}

/// The contacts view's remembered list-pane width (280 is also its floor).
pub fn load_contacts_pane_width() -> i32 {
    load_state().contacts_pane_width.clamp(280, 4000)
}

pub fn save_contacts_pane_width(width: i32) {
    update_state(|s| s.contacts_pane_width = width.clamp(280, 4000));
}

#[cfg(test)]
mod tests {
    use super::{
        decode_gallery_folder, encode_gallery_folder, ConfigFile, ListColumn, PrivacyFile,
        StateFile,
    };

    /// A section once kept in the unified section moves above the accounts
    /// where its switch was on, and off where it was off; one already
    /// placed stays put.
    #[test]
    fn unified_placements_move_out() {
        use super::{LegacySwitches, SectionPlacement::*};
        let text = "filtered_placement = \"all_inboxes\"\nunified_filtered = true\n\
                    tags_placement = \"all_inboxes\"\nunified_tags = false\n\
                    favorites_placement = \"below_accounts\"\nshow_contacts = false\n";
        let mut file: PrivacyFile = toml::from_str(text).unwrap();
        let old: LegacySwitches = toml::from_str(text).unwrap();
        assert!(old.place(&mut file));
        assert_eq!(file.filtered_placement, AboveAccounts);
        assert_eq!(file.tags_placement, Off);
        assert_eq!(file.favorites_placement, BelowAccounts);
        // A file from before the key: its old switch decides.
        assert_eq!(file.contacts_placement, Off);
        assert!(!old.place(&mut file));
        assert_eq!(PrivacyFile::default().contacts_placement, Footer);
    }

    /// Contacts moves to the footer once, from wherever it was put
    /// unless it was off; afterwards it stays where it is put.
    #[test]
    fn contacts_moves_to_the_footer_once() {
        use super::SectionPlacement::*;
        let mut file: PrivacyFile =
            toml::from_str("contacts_placement = \"above_accounts\"\n").unwrap();
        assert!(super::apply_new_defaults(&mut file));
        assert_eq!(file.contacts_placement, Footer);
        file.contacts_placement = BelowAccounts;
        super::apply_new_defaults(&mut file);
        assert_eq!(file.contacts_placement, BelowAccounts);
        let mut off: PrivacyFile = toml::from_str("contacts_placement = \"off\"\n").unwrap();
        super::apply_new_defaults(&mut off);
        assert_eq!(off.contacts_placement, Off);
    }

    /// The keyring thread runs what is queued in order, a call made from
    /// another thread waits behind it, one made on the keyring thread itself
    /// runs at once, and the drain waits for the queue (#466).
    #[test]
    fn the_keyring_thread_keeps_the_order() {
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        for i in 0..5 {
            let seen = seen.clone();
            super::keyring_later(move || {
                std::thread::sleep(std::time::Duration::from_millis(10));
                seen.lock().unwrap().push(i);
            });
        }
        let after = {
            let seen = seen.clone();
            super::on_keyring_thread(move || {
                let inline = super::on_keyring_thread(|| 7);
                seen.lock().unwrap().push(inline);
                seen.lock().unwrap().len()
            })
        };
        assert_eq!(after, 6, "the blocking call waited for the queue");
        let seen_late = seen.clone();
        super::keyring_later(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            seen_late.lock().unwrap().push(9);
        });
        assert!(super::keyring_drain(std::time::Duration::from_secs(5)));
        assert_eq!(*seen.lock().unwrap(), vec![0, 1, 2, 3, 4, 7, 9]);
    }

    /// An account with a protocol this build does not know (an EWS account
    /// from 1.44.0-beta.5) leaves the others readable.
    #[test]
    fn an_unknown_protocol_keeps_the_file_readable() {
        let account = |email: &str, protocol: &str| {
            format!("[[accounts]]\nname = \"\"\nemail = \"{email}\"\nimap_host = \"\"\nusername = \"\"\nprotocol = \"{protocol}\"\n")
        };
        let text = account("a@x", "ews") + &account("b@x", "jmap");
        let cfg: ConfigFile = toml::from_str(&text).expect("parses");
        assert_eq!(cfg.accounts.len(), 2);
        assert_eq!(cfg.accounts[0].protocol, super::Protocol::Imap);
        assert_eq!(cfg.accounts[1].protocol, super::Protocol::Jmap);
    }

    /// #431: a file from before the badge choice keeps what its switch
    /// said, and the choice, once made, wins over the switch.
    #[test]
    fn launcher_badge_follows_the_old_switch() {
        use super::LauncherBadge::*;
        let read = |text: &str| {
            toml::from_str::<PrivacyFile>(text)
                .unwrap()
                .launcher_badge()
        };
        assert_eq!(read(""), AllUnread);
        assert_eq!(read("launcher_count = true"), AllUnread);
        assert_eq!(read("launcher_count = false"), Off);
        assert_eq!(
            read("launcher_count = true\nlauncher_badge = \"new_since_front\""),
            NewSinceFront
        );
        assert_eq!(read("launcher_count = true\nlauncher_badge = \"off\""), Off);
        let saved = PrivacyFile {
            launcher_count: true,
            launcher_badge: Some(NewSinceFront),
            ..Default::default()
        };
        assert_eq!(read(&toml::to_string(&saved).unwrap()), NewSinceFront);
    }

    /// #367: a sidebar file from before Favorites has none, and the list
    /// starts open; the entries come back in the order they were added.
    #[test]
    fn favorites_load_in_order() {
        let old: super::SidebarFile = toml::from_str("icon_only = false").expect("parses");
        assert!(old.favorites.is_empty() && old.favorites_expanded);
        let new: super::SidebarFile =
            toml::from_str("favorites = [\"b@x\\tWork\", \"a@x\\tINBOX\"]").expect("parses");
        assert_eq!(
            new.favorites,
            vec!["b@x\tWork".to_string(), "a@x\tINBOX".to_string()]
        );
    }

    /// #369: unified Junk and Trash are off in a file that predates them,
    /// and list after Archive, in an account's own order.
    #[test]
    fn unified_junk_and_trash_start_off() {
        use crate::models::FolderKind::*;
        let kinds: super::UnifiedKinds = toml::from_str("starred = false").expect("parses");
        assert!(!kinds.junk && !kinds.trash && kinds.archive);
        assert_eq!(kinds.listed(), vec![Sent, Drafts, Archive]);
        let all = super::UnifiedKinds {
            junk: true,
            trash: true,
            ..Default::default()
        };
        assert_eq!(
            all.listed(),
            vec![Starred, Sent, Drafts, Archive, Junk, Trash]
        );
        assert_eq!(
            super::UnifiedKinds {
                trash: true,
                ..super::UnifiedKinds::NONE
            }
            .listed(),
            vec![Trash]
        );
    }

    /// #334: the saved columns come back in order; a name this version does
    /// not know, or a repeat, is dropped, and the subject is never missing.
    #[test]
    fn list_columns_keep_their_order_and_the_subject() {
        let keys = |k: &[&str]| k.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            ListColumn::from_keys(&keys(&["date", "subject", "star", "date", "colour"])),
            vec![ListColumn::Date, ListColumn::Subject, ListColumn::Star]
        );
        assert_eq!(
            ListColumn::from_keys(&keys(&["star", "sender", "date"])),
            vec![
                ListColumn::Star,
                ListColumn::Sender,
                ListColumn::Subject,
                ListColumn::Date
            ]
        );
        assert_eq!(
            ListColumn::from_keys(&ListColumn::to_keys(&ListColumn::DEFAULT)),
            ListColumn::DEFAULT.to_vec()
        );
        // Dragged widths keep their column, within reason; an unknown
        // column's is dropped.
        let saved: std::collections::BTreeMap<String, i32> = [
            ("sender".into(), 240),
            ("date".into(), 5),
            ("colour".into(), 90),
        ]
        .into_iter()
        .collect();
        let widths = super::list_column_widths_from(&saved);
        assert_eq!(widths.len(), 2);
        assert_eq!(widths[&ListColumn::Sender], 240);
        assert_eq!(widths[&ListColumn::Date], 32);
        assert_eq!(
            super::list_column_widths_to(&widths).get("sender"),
            Some(&240)
        );
        // A settings file from before the columns has the old layout.
        let old: PrivacyFile = toml::from_str("list_layout = \"single_line\"").unwrap();
        assert_eq!(
            ListColumn::from_keys(&old.list_columns),
            ListColumn::DEFAULT.to_vec()
        );
    }

    /// Every gallery folder override survives the trip to the file and back,
    /// including the paths servers really use: dots, slashes, spaces, UTF-7.
    #[test]
    fn a_gallery_folder_override_round_trips() {
        let cases = [
            (1, "INBOX", true),
            (2, "INBOX.Sent", false),
            (3, "Archive/2026", true),
            (7, "Saved Mail", false),
            (11, "INBOX.&AMQA5gDo-", true),
        ];
        for (id, path, on) in cases {
            let encoded = encode_gallery_folder(id, path, on);
            assert_eq!(
                decode_gallery_folder(&encoded),
                Some((id, path.to_string(), on)),
                "{path} did not survive the round trip",
            );
        }
    }

    /// A line that is not in the stored shape is dropped, not guessed at.
    #[test]
    fn a_malformed_gallery_override_is_skipped() {
        for bad in ["", "1", "1\tInbox", "notanid\tInbox\t1", "1\t\t1"] {
            assert_eq!(
                decode_gallery_folder(bad),
                None,
                "{bad:?} should not decode"
            );
        }
    }

    /// A first run with no state file must land on the `#[serde(default = …)]`
    /// each field declares, not on the zero its type happens to have: the
    /// gallery's two master switches start on, and the panes have real widths.
    #[test]
    fn a_missing_state_file_falls_back_to_the_declared_defaults() {
        let fresh: StateFile = toml::from_str("").expect("an empty state document must parse");
        assert!(fresh.gallery_archive);
        assert!(fresh.gallery_other);
        assert_eq!(
            fresh.gallery_thumb_width,
            super::default_gallery_thumb_width()
        );
        assert_eq!(fresh.list_pane_width, super::default_list_pane_width());
    }

    #[test]
    fn plain_string_aliases_still_load() {
        // The pre-per-alias-SMTP format (#34): a bare array of identity strings.
        let cfg: ConfigFile = toml::from_str(
            r#"
            [[accounts]]
            name = "Ann"
            email = "ann@example.org"
            imap_host = "imap.example.org"
            username = "ann"
            aliases = ["Ann Work <ann@work.org>", "ann@shop.org"]
            "#,
        )
        .unwrap();
        let aliases = &cfg.accounts[0].aliases;
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[0].identity, "Ann Work <ann@work.org>");
        assert_eq!(aliases[0].address(), "ann@work.org");
        assert!(
            !aliases[0].has_own_smtp(),
            "a plain alias rides the account's SMTP"
        );
        assert_eq!(aliases[1].address(), "ann@shop.org");
    }

    #[test]
    fn aliases_can_carry_their_own_smtp() {
        let cfg: ConfigFile = toml::from_str(
            r#"
            [[accounts]]
            name = "Ann"
            email = "ann@example.org"
            imap_host = "imap.example.org"
            username = "ann"

            [[accounts.aliases]]
            identity = "Ann Work <ann@work.org>"
            smtp_host = "smtp.work.org"
            smtp_port = 465
            smtp_username = "ann@work.org"
            "#,
        )
        .unwrap();
        let alias = &cfg.accounts[0].aliases[0];
        assert!(alias.has_own_smtp());
        assert_eq!(alias.smtp_host, "smtp.work.org");
        assert_eq!(alias.smtp_port, 465);
        assert_eq!(alias.smtp_username, "ann@work.org");
        assert!(
            alias.smtp_password.is_empty(),
            "passwords live in the keyring"
        );
    }

    #[test]
    fn alias_smtp_password_never_reaches_disk() {
        let mut cfg: ConfigFile = toml::from_str(
            r#"
            [[accounts]]
            name = "Ann"
            email = "ann@example.org"
            imap_host = "imap.example.org"
            username = "ann"

            [[accounts.aliases]]
            identity = "ann@work.org"
            smtp_host = "smtp.work.org"
            smtp_username = "ann"
            "#,
        )
        .unwrap();
        cfg.accounts[0].aliases[0].smtp_password = "hunter2".into();
        let out = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            !out.contains("hunter2"),
            "password serialized to disk: {out}"
        );
        // And the round trip keeps the alias's transport settings.
        let back: ConfigFile = toml::from_str(&out).unwrap();
        let alias = &back.accounts[0].aliases[0];
        assert_eq!(alias.smtp_host, "smtp.work.org");
        assert_eq!(
            alias.smtp_port, 587,
            "unwritten port falls back to the default"
        );
    }

    #[test]
    fn preview_lines_default_to_one_and_stay_in_range() {
        // An older privacy.toml has no key at all.
        let p: PrivacyFile = toml::from_str("").unwrap();
        assert_eq!(p.preview_lines, 1);
        // Hand-edited nonsense must not make the list build rows of 40 lines, or
        // of none: the setting offers 1–3 and that is what it is worth honouring.
        // 0 is a real setting — previews off — but nothing above 3 is.
        for (written, expected) in [(0, 0), (1, 1), (3, 3), (99, 3)] {
            let p: PrivacyFile = toml::from_str(&format!("preview_lines = {written}")).unwrap();
            assert_eq!(p.preview_lines.min(3), expected, "for {written}");
        }
    }

    #[test]
    fn notifications_default_on_when_absent() {
        // An older privacy.toml with no `notifications` key opts in by default.
        let p: PrivacyFile = toml::from_str("").unwrap();
        assert!(p.notifications);
    }

    #[test]
    fn new_mail_sound_names_a_builtin_or_falls_back() {
        let resource = |sound: &str| match (super::NewMailSound {
            enabled: true,
            sound: sound.into(),
            when_focused: false,
        })
        .source()
        {
            Some(super::SoundSource::Resource(p)) => p,
            _ => panic!("a built-in plays from the bundle"),
        };
        assert_eq!(resource("hum"), "/co/hyprlab/Hylki/sounds/hum.ogg");
        assert_eq!(resource("bogus"), "/co/hyprlab/Hylki/sounds/click.ogg");
        // A sound.toml from before the in-front switch (#337) still reads.
        let saved: super::NewMailSound =
            toml::from_str("enabled = true\nsound = \"swing\"").unwrap();
        assert_eq!(
            saved,
            super::NewMailSound {
                enabled: true,
                sound: "swing".into(),
                when_focused: false
            }
        );
        let saved: super::NewMailSound =
            toml::from_str("enabled = true\nsound = \"swing\"\nwhen_focused = true").unwrap();
        assert!(saved.when_focused);
    }

    #[test]
    fn notification_buttons_default_to_all_and_round_trip() {
        // An older privacy.toml with no key carries every button.
        let p: PrivacyFile = toml::from_str("").unwrap();
        assert_eq!(
            super::NotificationButtons::from_list(&p.notification_buttons),
            super::NotificationButtons::default()
        );
        // An empty list is no buttons at all, not the default.
        let p: PrivacyFile = toml::from_str("notification_buttons = []").unwrap();
        let none = super::NotificationButtons::NONE;
        assert_eq!(
            super::NotificationButtons::from_list(&p.notification_buttons),
            none
        );
        let p: PrivacyFile =
            toml::from_str(r#"notification_buttons = ["delete", "bogus", "mark_read"]"#).unwrap();
        let b = super::NotificationButtons::from_list(&p.notification_buttons);
        assert_eq!(
            b,
            super::NotificationButtons {
                mark_read: true,
                delete: true,
                ..none
            }
        );
        assert_eq!(
            b.to_list(),
            vec!["mark_read".to_string(), "delete".to_string()]
        );
    }

    #[test]
    fn notification_buttons_stop_at_three() {
        // A hand-edited file naming more than three keeps the first three
        // in button order, whatever order the file lists them in.
        let p: PrivacyFile = toml::from_str(
            r#"notification_buttons = ["spam", "forward", "reply", "delete", "archive"]"#,
        )
        .unwrap();
        let b = super::NotificationButtons::from_list(&p.notification_buttons);
        assert_eq!(
            b,
            super::NotificationButtons {
                archive: true,
                delete: true,
                reply: true,
                ..super::NotificationButtons::NONE
            }
        );
        assert!(b.full());
        assert_eq!(b.count(), 3);
    }

    #[test]
    fn notifications_can_be_disabled() {
        let p: PrivacyFile = toml::from_str("notifications = false").unwrap();
        assert!(!p.notifications);
    }
}

// ---------------------------------------------------------------------------
// Reader toolbar layout: which buttons the reading pane's header shows, on
// which side, in what order (Settings → Appearance → Toolbar).
// ---------------------------------------------------------------------------

/// One button of the reader header. The left group stays on the bar at every
/// width; the right group folds into the ⋯ overflow menu when the pane is
/// narrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolbarItem {
    Reply,
    ReplyAll,
    Forward,
    Star,
    Archive,
    Delete,
    Spam,
    ReadUnread,
    Tags,
    MoveTo,
    Find,
    Print,
}

impl ToolbarItem {
    pub const ALL: [ToolbarItem; 12] = [
        ToolbarItem::Reply,
        ToolbarItem::ReplyAll,
        ToolbarItem::Forward,
        ToolbarItem::Star,
        ToolbarItem::Archive,
        ToolbarItem::Delete,
        ToolbarItem::Spam,
        ToolbarItem::ReadUnread,
        ToolbarItem::Tags,
        ToolbarItem::MoveTo,
        ToolbarItem::Find,
        ToolbarItem::Print,
    ];

    /// The stable name written to `toolbar.toml`.
    pub fn key(self) -> &'static str {
        match self {
            ToolbarItem::Reply => "reply",
            ToolbarItem::ReplyAll => "reply_all",
            ToolbarItem::Forward => "forward",
            ToolbarItem::Star => "star",
            ToolbarItem::Archive => "archive",
            ToolbarItem::Delete => "delete",
            ToolbarItem::Spam => "spam",
            ToolbarItem::ReadUnread => "read_unread",
            ToolbarItem::Tags => "tags",
            ToolbarItem::MoveTo => "move_to",
            ToolbarItem::Find => "find",
            ToolbarItem::Print => "print",
        }
    }

    pub fn from_key(key: &str) -> Option<ToolbarItem> {
        ToolbarItem::ALL.iter().copied().find(|i| i.key() == key)
    }

    /// The symbolic icon the button (and the settings chip) wears.
    pub fn icon(self) -> &'static str {
        match self {
            ToolbarItem::Reply => "mail-reply-sender-symbolic",
            ToolbarItem::ReplyAll => "mail-reply-all-symbolic",
            ToolbarItem::Forward => "mail-forward-symbolic",
            ToolbarItem::Star => "hylki-non-starred-symbolic",
            ToolbarItem::Archive => "mail-archive-symbolic",
            ToolbarItem::Delete => "user-trash-symbolic",
            ToolbarItem::Spam => "mail-mark-junk-symbolic",
            ToolbarItem::ReadUnread => "mail-unread-symbolic",
            ToolbarItem::Tags => "tag-outline-symbolic",
            ToolbarItem::MoveTo => "folder-symbolic",
            ToolbarItem::Find => "loupe-with-arrow-symbolic",
            ToolbarItem::Print => "printer-symbolic",
        }
    }

    /// The untranslated label (callers pass it through `i18n`).
    pub fn label(self) -> &'static str {
        match self {
            ToolbarItem::Reply => crate::i18n::i18n_noop("Reply"),
            ToolbarItem::ReplyAll => crate::i18n::i18n_noop("Reply All"),
            ToolbarItem::Forward => crate::i18n::i18n_noop("Forward"),
            ToolbarItem::Star => crate::i18n::i18n_noop("Star"),
            ToolbarItem::Archive => crate::i18n::i18n_noop("Archive"),
            ToolbarItem::Delete => crate::i18n::i18n_noop("Delete"),
            ToolbarItem::Spam => crate::i18n::i18n_noop("Spam"),
            ToolbarItem::ReadUnread => crate::i18n::i18n_noop("Read/Unread"),
            ToolbarItem::Tags => crate::i18n::i18n_noop("Tags"),
            ToolbarItem::MoveTo => crate::i18n::i18n_noop("Move To"),
            ToolbarItem::Find => crate::i18n::i18n_noop("Find"),
            ToolbarItem::Print => crate::i18n::i18n_noop("Print"),
        }
    }
}

/// Which side of the reader header a button sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarSide {
    Left,
    Right,
}

/// How many buttons one side of the reader toolbar holds at most.
pub const TOOLBAR_SIDE_MAX: usize = 6;

/// The reader header's layout. A button absent from both lists is hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderToolbar {
    pub left: Vec<ToolbarItem>,
    pub right: Vec<ToolbarItem>,
}

impl Default for ReaderToolbar {
    fn default() -> Self {
        ReaderToolbar {
            left: vec![
                ToolbarItem::Reply,
                ToolbarItem::ReplyAll,
                ToolbarItem::Forward,
                ToolbarItem::Star,
                ToolbarItem::Archive,
                ToolbarItem::Delete,
            ],
            right: vec![
                ToolbarItem::Tags,
                ToolbarItem::ReadUnread,
                ToolbarItem::Spam,
                ToolbarItem::MoveTo,
                ToolbarItem::Find,
                ToolbarItem::Print,
            ],
        }
    }
}

impl ReaderToolbar {
    /// The side an item sits on, or None when it is hidden.
    pub fn side(&self, item: ToolbarItem) -> Option<ToolbarSide> {
        if self.left.contains(&item) {
            Some(ToolbarSide::Left)
        } else if self.right.contains(&item) {
            Some(ToolbarSide::Right)
        } else {
            None
        }
    }

    /// The items on neither side, in canonical order.
    pub fn hidden(&self) -> Vec<ToolbarItem> {
        ToolbarItem::ALL
            .iter()
            .copied()
            .filter(|i| self.side(*i).is_none())
            .collect()
    }

    /// Drop repeats (an item may sit on one side only, once), and anything
    /// past a side's room.
    fn sanitize(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.left.retain(|i| seen.insert(*i));
        self.right.retain(|i| seen.insert(*i));
        self.left.truncate(TOOLBAR_SIDE_MAX);
        self.right.truncate(TOOLBAR_SIDE_MAX);
    }

    /// Whether `side` has room for one more button that is not already on
    /// it (`None`, the hidden zone, always has).
    pub fn has_room(&self, side: Option<ToolbarSide>, item: ToolbarItem) -> bool {
        let list = match side {
            Some(ToolbarSide::Left) => &self.left,
            Some(ToolbarSide::Right) => &self.right,
            None => return true,
        };
        list.contains(&item) || list.len() < TOOLBAR_SIDE_MAX
    }

    /// Take `item` out of wherever it is and put it at `index` of `side`
    /// (clamped); `side` None hides it. A full side is left as it is.
    pub fn place(&mut self, item: ToolbarItem, side: Option<ToolbarSide>, index: usize) {
        if !self.has_room(side, item) {
            return;
        }
        self.left.retain(|i| *i != item);
        self.right.retain(|i| *i != item);
        let list = match side {
            Some(ToolbarSide::Left) => &mut self.left,
            Some(ToolbarSide::Right) => &mut self.right,
            None => return,
        };
        let index = index.min(list.len());
        list.insert(index, item);
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ToolbarFile {
    #[serde(default)]
    left: Vec<String>,
    #[serde(default)]
    right: Vec<String>,
}

fn toolbar_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("toolbar.toml"))
}

/// The saved reader toolbar layout, or the default when there is none.
/// Unknown names (a newer Hylki's buttons) are skipped, not fatal.
pub fn load_reader_toolbar() -> ReaderToolbar {
    let Some(text) = toolbar_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return ReaderToolbar::default();
    };
    let Ok(file) = toml::from_str::<ToolbarFile>(&text) else {
        return ReaderToolbar::default();
    };
    let mut layout = ReaderToolbar {
        left: file
            .left
            .iter()
            .filter_map(|k| ToolbarItem::from_key(k))
            .collect(),
        right: file
            .right
            .iter()
            .filter_map(|k| ToolbarItem::from_key(k))
            .collect(),
    };
    layout.sanitize();
    layout
}

pub fn save_reader_toolbar(layout: &ReaderToolbar) {
    let Some(path) = toolbar_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = ToolbarFile {
        left: layout.left.iter().map(|i| i.key().to_string()).collect(),
        right: layout.right.iter().map(|i| i.key().to_string()).collect(),
    };
    if let Ok(toml) = toml::to_string_pretty(&file) {
        let _ = std::fs::write(&path, toml);
    }
}

#[cfg(test)]
mod toolbar_tests {
    use super::*;

    #[test]
    fn place_moves_between_and_within_sides() {
        let mut t = ReaderToolbar::default();
        // The default left group is full: a move onto it changes nothing.
        t.place(ToolbarItem::Print, Some(ToolbarSide::Left), 0);
        assert_eq!(t.left[0], ToolbarItem::Reply);
        assert_eq!(t.right.last(), Some(&ToolbarItem::Print));
        // Make room, then right → left, at the front.
        t.place(ToolbarItem::Star, None, 0);
        t.place(ToolbarItem::Print, Some(ToolbarSide::Left), 0);
        assert_eq!(t.left[0], ToolbarItem::Print);
        assert!(!t.right.contains(&ToolbarItem::Print));
        // Within the left group: Reply (now index 1) to the end.
        t.place(ToolbarItem::Reply, Some(ToolbarSide::Left), 99);
        assert_eq!(t.left.last(), Some(&ToolbarItem::Reply));
        assert_eq!(t.left.len(), TOOLBAR_SIDE_MAX);
        // Hidden: on neither side, listed under hidden().
        t.place(ToolbarItem::Spam, None, 0);
        assert_eq!(t.side(ToolbarItem::Spam), None);
        assert_eq!(t.hidden(), vec![ToolbarItem::Star, ToolbarItem::Spam]);
        // Back from hidden into the right group's middle.
        t.place(ToolbarItem::Spam, Some(ToolbarSide::Right), 1);
        assert_eq!(t.right[1], ToolbarItem::Spam);
        assert_eq!(t.hidden(), vec![ToolbarItem::Star]);
    }

    #[test]
    fn file_round_trip_skips_unknown_names() {
        let file: ToolbarFile =
            toml::from_str("left = [\"reply\", \"bogus\", \"delete\"]\nright = [\"print\"]\n")
                .unwrap();
        let mut layout = ReaderToolbar {
            left: file
                .left
                .iter()
                .filter_map(|k| ToolbarItem::from_key(k))
                .collect(),
            right: file
                .right
                .iter()
                .filter_map(|k| ToolbarItem::from_key(k))
                .collect(),
        };
        layout.sanitize();
        assert_eq!(layout.left, vec![ToolbarItem::Reply, ToolbarItem::Delete]);
        assert_eq!(layout.right, vec![ToolbarItem::Print]);
        for item in ToolbarItem::ALL {
            assert_eq!(ToolbarItem::from_key(item.key()), Some(item));
        }
    }
}

// ---- Focus Mode -----------------------------------------------------------

/// Focus Mode: a distraction-free layout for reading, switched on from the
/// main menu, Ctrl+Shift+F or Settings → Appearance. Each part can be left
/// out, so it strips exactly what its owner finds distracting. Stored in
/// focus.toml (like the toolbar layout) so the ordinary settings file and
/// its long save call stay as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusMode {
    /// The master switch: on, the parts below apply.
    #[serde(default)]
    pub enabled: bool,
    /// Whether the app opens in Focus Mode. Not a part — it strips nothing
    /// itself, it decides what [`Self::enabled`] starts each launch as, so
    /// the mode is a deliberate choice rather than whatever the last
    /// session happened to leave behind.
    #[serde(default)]
    pub start_focused: bool,
    /// Every button of the reading pane's toolbar folds into its ⋯ menu.
    #[serde(default = "default_on")]
    pub reader_toolbar: bool,
    /// The message list's search, filters, count and sort fold into a ⋯
    /// menu; only the sidebar toggle stays.
    #[serde(default = "default_on")]
    pub list_header: bool,
    /// The sidebar's account sections are hidden.
    #[serde(default = "default_on")]
    pub hide_accounts: bool,
    /// The sidebar's unified rows (Inboxes, Starred, …) fold up.
    #[serde(default = "default_on")]
    pub fold_unified: bool,
    /// The message list's sender avatars are hidden.
    #[serde(default = "default_on")]
    pub hide_avatars: bool,
    /// The message list shows at most one line of preview text.
    #[serde(default = "default_on")]
    pub one_preview_line: bool,
    /// The message list shows no preview text at all. Wins over
    /// [`Self::one_preview_line`], which has nothing left to cap.
    #[serde(default = "default_on")]
    pub hide_preview: bool,
    /// The message list shows no subject either: the sender, the date and
    /// the row's own marks, and nothing else. Off unless it is asked for —
    /// the subject is how most people find a message again, so this one is
    /// a step past what the mode does by default.
    #[serde(default)]
    pub hide_subject: bool,
    /// The sidebar shows as the icon rail, with unread dots in place of the
    /// counts. The user's own rail choice is untouched: this is on screen
    /// for as long as Focus Mode is, and the sidebar comes back as it was.
    #[serde(default = "default_on")]
    pub rail_sidebar: bool,
    /// Every message opens in Reader View (the switch still works).
    #[serde(default = "default_on")]
    pub reader_view: bool,
}

impl Default for FocusMode {
    fn default() -> Self {
        FocusMode {
            enabled: false,
            start_focused: false,
            reader_toolbar: true,
            list_header: true,
            hide_accounts: true,
            fold_unified: true,
            hide_avatars: true,
            one_preview_line: true,
            hide_preview: true,
            hide_subject: false,
            rail_sidebar: true,
            reader_view: true,
        }
    }
}

/// One switch of Focus Mode, for Settings rows that all set the same struct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusPart {
    Enabled,
    /// Not a layout part: the launch switch (see [`FocusMode::start_focused`]).
    /// It rides here so Settings can set it the same way as the rest;
    /// [`FocusMode::active`] is meaningless for it and nothing asks.
    StartFocused,
    ReaderToolbar,
    ListHeader,
    HideAccounts,
    FoldUnified,
    HideAvatars,
    OnePreviewLine,
    HidePreview,
    HideSubject,
    RailSidebar,
    ReaderView,
}

impl FocusMode {
    pub fn get(&self, part: FocusPart) -> bool {
        match part {
            FocusPart::Enabled => self.enabled,
            FocusPart::StartFocused => self.start_focused,
            FocusPart::ReaderToolbar => self.reader_toolbar,
            FocusPart::ListHeader => self.list_header,
            FocusPart::HideAccounts => self.hide_accounts,
            FocusPart::FoldUnified => self.fold_unified,
            FocusPart::HideAvatars => self.hide_avatars,
            FocusPart::OnePreviewLine => self.one_preview_line,
            FocusPart::HidePreview => self.hide_preview,
            FocusPart::HideSubject => self.hide_subject,
            FocusPart::RailSidebar => self.rail_sidebar,
            FocusPart::ReaderView => self.reader_view,
        }
    }

    pub fn set(&mut self, part: FocusPart, on: bool) {
        match part {
            FocusPart::Enabled => self.enabled = on,
            FocusPart::StartFocused => self.start_focused = on,
            FocusPart::ReaderToolbar => self.reader_toolbar = on,
            FocusPart::ListHeader => self.list_header = on,
            FocusPart::HideAccounts => self.hide_accounts = on,
            FocusPart::FoldUnified => self.fold_unified = on,
            FocusPart::HideAvatars => self.hide_avatars = on,
            FocusPart::OnePreviewLine => self.one_preview_line = on,
            FocusPart::HidePreview => self.hide_preview = on,
            FocusPart::HideSubject => self.hide_subject = on,
            FocusPart::RailSidebar => self.rail_sidebar = on,
            FocusPart::ReaderView => self.reader_view = on,
        }
    }

    /// Whether `part` is in force: Focus Mode is on and the part is ticked.
    pub fn active(&self, part: FocusPart) -> bool {
        self.enabled && self.get(part)
    }
}

/// A mailing list the user has unsubscribed from through the reader's
/// button, so a later message from the same list can say so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsubscribedList {
    /// [`crate::models::Unsubscribe::key`]: the List-Id, or the From address.
    pub key: String,
    /// Who the mail came from, for the record.
    #[serde(default)]
    pub name: String,
    /// Unix seconds of the request.
    #[serde(default)]
    pub at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UnsubscribedFile {
    #[serde(default)]
    lists: Vec<UnsubscribedList>,
}

fn unsubscribed_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("unsubscribed.toml"))
}

/// The lists left through the reader's Unsubscribe button, newest last.
pub fn load_unsubscribed() -> Vec<UnsubscribedList> {
    let Some(text) = unsubscribed_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    toml::from_str::<UnsubscribedFile>(&text)
        .unwrap_or_default()
        .lists
}

pub fn save_unsubscribed(lists: &[UnsubscribedList]) {
    let Some(path) = unsubscribed_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = UnsubscribedFile {
        lists: lists.to_vec(),
    };
    if let Ok(toml) = toml::to_string_pretty(&file) {
        let _ = std::fs::write(&path, toml);
    }
}

/// How many answered invitations are remembered. An invitation is
/// answered far more often than a mailing list is left, and the file is
/// read at every start — the oldest fall off rather than growing it
/// without end. A meeting whose answer has been forgotten simply offers
/// the buttons again.
const INVITE_ANSWER_LIMIT: usize = 500;

/// A meeting invitation answered through the reader's buttons (#223), so
/// the card says so when the message is opened again. The organizer's
/// calendar is the record that counts; this is only what the reader shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteAnswer {
    /// [`crate::models::Invite::key`]: the event's UID, plus the occurrence
    /// when the answer was to one of a series.
    pub key: String,
    /// The `PARTSTAT` sent: ACCEPTED, TENTATIVE or DECLINED.
    pub status: String,
    /// Unix seconds of the answer.
    #[serde(default)]
    pub at: i64,
    /// What the meeting was called, for the record.
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct InviteAnswerFile {
    #[serde(default)]
    answers: Vec<InviteAnswer>,
}

fn invites_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("invites.toml"))
}

/// The invitations answered through the reader, newest last.
pub fn load_invite_answers() -> Vec<InviteAnswer> {
    let Some(text) = invites_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    toml::from_str::<InviteAnswerFile>(&text)
        .unwrap_or_default()
        .answers
}

pub fn save_invite_answers(answers: &[InviteAnswer]) {
    let Some(path) = invites_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let kept = answers.len().saturating_sub(INVITE_ANSWER_LIMIT);
    let file = InviteAnswerFile {
        answers: answers[kept..].to_vec(),
    };
    if let Ok(toml) = toml::to_string_pretty(&file) {
        let _ = std::fs::write(&path, toml);
    }
}

fn focus_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("focus.toml"))
}

/// The saved Focus Mode settings, or the defaults (off) when there are none.
pub fn load_focus_mode() -> FocusMode {
    let Some(text) = focus_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return FocusMode::default();
    };
    toml::from_str::<FocusMode>(&text).unwrap_or_default()
}

pub fn save_focus_mode(focus: &FocusMode) {
    let Some(path) = focus_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(toml) = toml::to_string_pretty(focus) {
        let _ = std::fs::write(&path, toml);
    }
}

#[cfg(test)]
mod focus_tests {
    use super::*;

    #[test]
    fn focus_defaults_off_with_every_part_ticked() {
        let f = FocusMode::default();
        assert!(!f.enabled);
        // The app opens out of the mode until it is asked to do otherwise.
        assert!(!f.start_focused);
        assert!(f.reader_toolbar && f.list_header && f.hide_accounts && f.fold_unified);
        assert!(f.hide_avatars && f.one_preview_line && f.reader_view);
        assert!(f.hide_preview && f.rail_sidebar);
        // The exception: the subject stays until it is asked for.
        assert!(!f.hide_subject);
        // Off, no part is in force whatever its switch says.
        assert!(!f.active(FocusPart::ReaderView));
    }

    #[test]
    fn focus_round_trips_and_fills_missing_parts() {
        let mut f = FocusMode::default();
        f.set(FocusPart::Enabled, true);
        f.set(FocusPart::HideAvatars, false);
        let text = toml::to_string(&f).unwrap();
        let back: FocusMode = toml::from_str(&text).unwrap();
        assert_eq!(back, f);
        assert!(back.active(FocusPart::ReaderToolbar));
        assert!(!back.active(FocusPart::HideAvatars));
        // A file from before a part existed reads it as ticked — the
        // preview and rail parts included, so an upgrade gets the whole
        // mode without anyone going looking for the new switches.
        let partial: FocusMode = toml::from_str("enabled = true").unwrap();
        assert!(partial.enabled && partial.fold_unified);
        assert!(partial.hide_preview && partial.rail_sidebar);
        // Except the subject, which is off wherever it comes from.
        assert!(!partial.hide_subject);
    }
}
