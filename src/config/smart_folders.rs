//! Smart folders (#417): saved searches over the cached mail, and
//! smart_folders.toml.

use std::path::PathBuf;

use super::filters::{FilterCondition, FilterField, FilterInput, FilterMatch};
use super::{config_file, is_false, save_private_toml};

/// A smart folder (#417): a saved search over the mail already on this
/// computer, listed in the sidebar's unified section. It looks in one
/// account or every account in the unified section, in one kind of folder
/// or every folder but Trash and Junk, and keeps the messages its
/// conditions match, all of them or any one.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SmartFolder {
    /// A name of its own for the sidebar to find it by, kept when renamed.
    pub id: String,
    pub name: String,
    /// The account it looks in; empty for every account in the unified
    /// section.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account_email: String,
    /// The kind of folder it looks in; none for every folder but Trash and
    /// Junk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<SmartFolderKind>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub any: bool,
    #[serde(default)]
    pub conditions: Vec<SmartCondition>,
}

/// The kinds of folder a smart folder can be held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartFolderKind {
    Inbox,
    Sent,
    Drafts,
    Archive,
    Junk,
    Trash,
}

impl SmartFolderKind {
    /// Every kind, in the order the editor lists them.
    pub const ALL: [SmartFolderKind; 6] = [
        SmartFolderKind::Inbox,
        SmartFolderKind::Sent,
        SmartFolderKind::Drafts,
        SmartFolderKind::Archive,
        SmartFolderKind::Junk,
        SmartFolderKind::Trash,
    ];

    pub fn kind(self) -> crate::models::FolderKind {
        use crate::models::FolderKind;
        match self {
            SmartFolderKind::Inbox => FolderKind::Inbox,
            SmartFolderKind::Sent => FolderKind::Sent,
            SmartFolderKind::Drafts => FolderKind::Drafts,
            SmartFolderKind::Archive => FolderKind::Archive,
            SmartFolderKind::Junk => FolderKind::Junk,
            SmartFolderKind::Trash => FolderKind::Trash,
        }
    }
}

/// One condition of a [`SmartFolder`]: the filter conditions, which look at
/// text, and the message's own state, which a filter has no use for.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SmartCondition {
    pub field: SmartField,
    #[serde(default = "contains_match")]
    pub matcher: FilterMatch,
    /// The text to match, commas separating alternatives, or for
    /// [`SmartField::Tagged`] the tag's keyword (empty for any tag).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
}

fn contains_match() -> FilterMatch {
    FilterMatch::Contains
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartField {
    FromAddress,
    FromName,
    Subject,
    Recipients,
    ReplyTo,
    /// The list preview: a smart folder reads only what is on this computer.
    Body,
    FromInContacts,
    FromNotInContacts,
    Starred,
    NotStarred,
    Unread,
    Read,
    Attachment,
    Tagged,
}

impl SmartField {
    /// Every field, in the order the editor lists them.
    pub const ALL: [SmartField; 14] = [
        SmartField::Starred,
        SmartField::NotStarred,
        SmartField::Unread,
        SmartField::Read,
        SmartField::Attachment,
        SmartField::Tagged,
        SmartField::FromAddress,
        SmartField::FromName,
        SmartField::Subject,
        SmartField::Recipients,
        SmartField::ReplyTo,
        SmartField::Body,
        SmartField::FromInContacts,
        SmartField::FromNotInContacts,
    ];

    /// The filter field the condition matches through, for the ones that
    /// look at text or the address books.
    pub fn filter_field(self) -> Option<FilterField> {
        Some(match self {
            SmartField::FromAddress => FilterField::FromAddress,
            SmartField::FromName => FilterField::FromName,
            SmartField::Subject => FilterField::Subject,
            SmartField::Recipients => FilterField::Recipients,
            SmartField::ReplyTo => FilterField::ReplyTo,
            SmartField::Body => FilterField::Body,
            SmartField::FromInContacts => FilterField::FromInContacts,
            SmartField::FromNotInContacts => FilterField::FromNotInContacts,
            _ => return None,
        })
    }

    /// Whether the field takes text to match.
    pub fn takes_text(self) -> bool {
        self.filter_field().is_some_and(|f| !f.is_contact_check())
    }
}

/// What a smart folder's conditions look at in one message.
pub struct SmartInput<'a> {
    pub text: FilterInput<'a>,
    pub starred: bool,
    pub unread: bool,
    pub attachment: bool,
    pub keywords: &'a [String],
    /// The keywords of the tags there are, lower case: what "any tag" means,
    /// since a server sets keywords of its own ($Forwarded, $Junk) too.
    pub tags: &'a [String],
}

impl SmartCondition {
    pub fn matches(&self, input: &SmartInput) -> bool {
        match self.field {
            SmartField::Starred => input.starred,
            SmartField::NotStarred => !input.starred,
            SmartField::Unread => input.unread,
            SmartField::Read => !input.unread,
            SmartField::Attachment => input.attachment,
            SmartField::Tagged if self.value.is_empty() => input
                .keywords
                .iter()
                .any(|k| input.tags.contains(&k.to_lowercase())),
            SmartField::Tagged => input
                .keywords
                .iter()
                .any(|k| k.eq_ignore_ascii_case(&self.value)),
            field => {
                let Some(field) = field.filter_field() else {
                    return false;
                };
                FilterCondition {
                    field,
                    matcher: self.matcher,
                    value: self.value.clone(),
                }
                .matches(&input.text)
            }
        }
    }
}

/// What the cache can rule out before a message is read: the state the
/// conditions require when every one of them must hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SmartPrefilter {
    pub starred: Option<bool>,
    pub unread: Option<bool>,
    pub attachment: Option<bool>,
}

impl SmartFolder {
    /// Every condition holds, or with `any` one does. A folder with no
    /// conditions keeps everything it looks at.
    pub fn matches(&self, input: &SmartInput) -> bool {
        if self.conditions.is_empty() {
            return true;
        }
        if self.any {
            self.conditions.iter().any(|c| c.matches(input))
        } else {
            self.conditions.iter().all(|c| c.matches(input))
        }
    }

    pub fn prefilter(&self) -> SmartPrefilter {
        let mut pre = SmartPrefilter::default();
        if self.any {
            return pre;
        }
        for c in &self.conditions {
            match c.field {
                SmartField::Starred => pre.starred = Some(true),
                SmartField::NotStarred => pre.starred = Some(false),
                SmartField::Unread => pre.unread = Some(true),
                SmartField::Read => pre.unread = Some(false),
                SmartField::Attachment => pre.attachment = Some(true),
                _ => {}
            }
        }
        pre
    }

    /// Whether a condition asks about the address books, which are read
    /// only when one does.
    pub fn needs_contacts(&self) -> bool {
        self.conditions.iter().any(|c| {
            matches!(
                c.field,
                SmartField::FromInContacts | SmartField::FromNotInContacts
            )
        })
    }
}

/// A new smart folder's id: random, so a folder renamed or recreated under
/// an old name is never taken for another.
pub fn new_smart_folder_id() -> String {
    crate::rng::token(12).unwrap_or_else(|_| {
        format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        )
    })
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct SmartFoldersFile {
    #[serde(default)]
    pub(super) folders: Vec<SmartFolder>,
}

fn smart_folders_path() -> Option<PathBuf> {
    config_file("smart_folders.toml")
}

pub fn load_smart_folders() -> Vec<SmartFolder> {
    let Some(path) = smart_folders_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    toml::from_str::<SmartFoldersFile>(&text)
        .map(|f| f.folders)
        .unwrap_or_else(|e| {
            tracing::warn!("could not read smart folders: {e}");
            Vec::new()
        })
}

pub fn save_smart_folders(folders: &[SmartFolder]) {
    let file = SmartFoldersFile {
        folders: folders.to_vec(),
    };
    save_private_toml(smart_folders_path(), &file, "smart folders", false);
}
