//! The sidebar's sections: the unified rows and their unread chips, where
//! each section is placed, the start view, folder order and the icon rail's
//! fold-ups.

use super::default_on;
use super::privacy::load_privacy;

/// The unified rows' unread-chip switches. A file from before the
/// per-row switches carried one switch, for All Inboxes; it still counts.
pub fn load_unified_chips() -> UnifiedChips {
    let p = load_privacy();
    let mut chips = p.unified_chips;
    chips.all_inboxes = chips.all_inboxes && p.unified_chip;
    chips
}

/// Which of the unified section's folder rows are shown besides All
/// Inboxes: each combines that folder across every account, and opens to
/// the accounts' own. Starred, Sent, Drafts and Archive are on until
/// switched off; Trash and Junk (#369) are off until switched on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnifiedKinds {
    #[serde(default = "default_on")]
    pub starred: bool,
    #[serde(default = "default_on")]
    pub sent: bool,
    #[serde(default = "default_on")]
    pub drafts: bool,
    #[serde(default = "default_on")]
    pub archive: bool,
    #[serde(default)]
    pub junk: bool,
    #[serde(default)]
    pub trash: bool,
}

impl Default for UnifiedKinds {
    fn default() -> Self {
        UnifiedKinds {
            starred: true,
            sent: true,
            drafts: true,
            archive: true,
            junk: false,
            trash: false,
        }
    }
}

/// Which unified rows show their total-unread chip while folded up
/// (Settings → Sidebar → Unified → Unread counts). All on until switched
/// off; Sent has no chip anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnifiedChips {
    #[serde(default = "default_on")]
    pub all_inboxes: bool,
    #[serde(default = "default_on")]
    pub starred: bool,
    #[serde(default = "default_on")]
    pub drafts: bool,
    #[serde(default = "default_on")]
    pub archive: bool,
    #[serde(default = "default_on")]
    pub junk: bool,
    #[serde(default = "default_on")]
    pub trash: bool,
}

impl Default for UnifiedChips {
    fn default() -> Self {
        UnifiedChips {
            all_inboxes: true,
            starred: true,
            drafts: true,
            archive: true,
            junk: true,
            trash: true,
        }
    }
}

impl UnifiedChips {
    /// Whether the row for `kind` shows its chip (Sent never does).
    pub fn has(self, kind: crate::models::FolderKind) -> bool {
        use crate::models::FolderKind::*;
        match kind {
            Inbox => self.all_inboxes,
            Starred => self.starred,
            Drafts => self.drafts,
            Archive => self.archive,
            Junk => self.junk,
            Trash => self.trash,
            _ => false,
        }
    }
}

impl UnifiedKinds {
    pub const NONE: UnifiedKinds = UnifiedKinds {
        starred: false,
        sent: false,
        drafts: false,
        archive: false,
        junk: false,
        trash: false,
    };

    /// Whether the row for `kind` is on (Inbox and custom folders have none
    /// of their own here, so they are `false`).
    pub fn has(self, kind: crate::models::FolderKind) -> bool {
        use crate::models::FolderKind::*;
        match kind {
            Starred => self.starred,
            Sent => self.sent,
            Drafts => self.drafts,
            Archive => self.archive,
            Junk => self.junk,
            Trash => self.trash,
            _ => false,
        }
    }

    /// The kinds with a row, in sidebar order: an account's own order.
    pub fn listed(self) -> Vec<crate::models::FolderKind> {
        use crate::models::FolderKind::*;
        [Starred, Sent, Drafts, Archive, Junk, Trash]
            .into_iter()
            .filter(|k| self.has(*k))
            .collect()
    }
}

/// Where a sidebar section (Filters, Tags, Favorites, Contacts) is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionPlacement {
    /// In the unified section, as older files have it. Only
    /// ever read: loading moves it (see `LegacySwitches`), and a file
    /// missing the key reads as this so that its old switch decides.
    #[default]
    AllInboxes,
    /// In the scrolling sidebar, above the first account.
    AboveAccounts,
    /// In the scrolling sidebar, after the last account.
    BelowAccounts,
    /// A row of the pinned footer, above Attachments. Only Contacts
    /// offers it, and has it by default.
    Footer,
    /// Not in the sidebar at all.
    Off,
}

/// How Filters, Tags, Favorites and Contacts are drawn (Settings → Sidebar → Sections).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionStyle {
    /// As the unified section's rows: a pill whose header opens the
    /// combined view, with a chevron on the right.
    #[default]
    Unified,
    /// As a tree: the caret that opens and closes the list in the icon's
    /// place, no chevron at the row's end; the name still opens the view.
    #[serde(alias = "headings")]
    Tree,
}

impl SectionPlacement {
    /// Whether the section is in the sidebar's scrolling part, where it
    /// lists what it holds under its row.
    pub fn shown(self) -> bool {
        matches!(self, Self::AboveAccounts | Self::BelowAccounts)
    }
}

/// What the main window shows at launch (#256).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartView {
    /// All Inboxes, or with that row hidden the first account's first
    /// folder.
    #[default]
    AllInboxes,
    /// The inbox of the account whose mail was open last.
    AccountInbox,
    /// Whichever folder was open last, All Inboxes included.
    LastFolder,
}

/// How an account's custom folders are ordered in the sidebar: Settings →
/// Sidebar sets it for every account, and an account can choose its own
/// (`AccountConfig::folder_sort`). The main folders (Inbox, Sent…) keep
/// their order whichever is chosen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderSort {
    /// By name, with the folders the user dragged kept where they were put.
    #[default]
    Custom,
    /// By the name shown, A to Z, ignoring any dragging.
    NameAsc,
    /// By the name shown, Z to A.
    NameDesc,
    /// By the whole path on the server, as Gmail on the web lists labels.
    Path,
}

impl FolderSort {
    pub const ALL: [FolderSort; 4] = [
        FolderSort::Custom,
        FolderSort::NameAsc,
        FolderSort::NameDesc,
        FolderSort::Path,
    ];

    pub fn label(self) -> String {
        match self {
            FolderSort::Custom => crate::i18n::i18n("Custom Order"),
            FolderSort::NameAsc => crate::i18n::i18n("Name (A to Z)"),
            FolderSort::NameDesc => crate::i18n::i18n("Name (Z to A)"),
            FolderSort::Path => crate::i18n::i18n("Full Path"),
        }
    }

    /// Its place in [`FolderSort::ALL`], for combo rows.
    pub fn index(self) -> u32 {
        FolderSort::ALL.iter().position(|s| *s == self).unwrap_or(0) as u32
    }
}

/// "Fold up expanded items" (Settings → Sidebar → Icon rail): while the
/// sidebar is collapsed to its icon rail, the items ticked here show folded
/// up and stay so — their saved expansion is untouched and returns when the
/// sidebar expands. Each item has its own switch under the master; all are
/// on until switched off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RailFold {
    /// The master switch: off, nothing folds for the rail.
    #[serde(default = "default_on")]
    pub enabled: bool,
    /// Every account's folder list.
    #[serde(default = "default_on")]
    pub accounts: bool,
    /// The per-account inbox list under All Inboxes.
    #[serde(default = "default_on")]
    pub all_inboxes: bool,
    /// The unified Starred / Sent / Drafts rows' account lists.
    #[serde(default = "default_on")]
    pub starred: bool,
    #[serde(default = "default_on")]
    pub sent: bool,
    #[serde(default = "default_on")]
    pub drafts: bool,
    #[serde(default = "default_on")]
    pub archive: bool,
    #[serde(default = "default_on")]
    pub junk: bool,
    #[serde(default = "default_on")]
    pub trash: bool,
    /// The Filtered Folders section.
    #[serde(default = "default_on")]
    pub filtered: bool,
    /// The Tags section.
    #[serde(default = "default_on")]
    pub tags: bool,
}

impl Default for RailFold {
    fn default() -> Self {
        RailFold {
            enabled: true,
            accounts: true,
            all_inboxes: true,
            starred: true,
            sent: true,
            drafts: true,
            archive: true,
            junk: true,
            trash: true,
            filtered: true,
            tags: true,
        }
    }
}

impl RailFold {
    /// Whether the accounts fold for the rail.
    pub fn folds_accounts(self) -> bool {
        self.enabled && self.accounts
    }

    /// Whether the unified row for `kind` folds for the rail (Inbox is All
    /// Inboxes; only Starred, Sent and Drafts have rows besides it).
    pub fn folds_kind(self, kind: crate::models::FolderKind) -> bool {
        use crate::models::FolderKind::*;
        self.enabled
            && match kind {
                Inbox => self.all_inboxes,
                Starred => self.starred,
                Sent => self.sent,
                Drafts => self.drafts,
                Archive => self.archive,
                Junk => self.junk,
                Trash => self.trash,
                _ => false,
            }
    }

    pub fn folds_filtered(self) -> bool {
        self.enabled && self.filtered
    }

    pub fn folds_tags(self) -> bool {
        self.enabled && self.tags
    }
}
