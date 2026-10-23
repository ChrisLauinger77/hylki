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

/// A mail filter rule (#47): file matching inbox arrivals into a folder,
/// Evolution-style, applied client-side whenever Hylki syncs the inbox.
///
/// A rule holds one or more conditions (#192). The first lives at the top
/// level as `field`/`matcher`/`value`, exactly where versions that knew only
/// one condition read it, so a filters file written here still loads there
/// (with the rest of the conditions ignored); `more` holds the others.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(from = "FilterRuleDisk", into = "FilterRuleDisk")]
pub struct FilterRule {
    /// What to call the rule (#197). Optional: an unnamed rule is still
    /// listed by its conditions, the way every rule was before names
    /// existed. Kept first so it reads first in `filters.toml`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The account this rule (and its destination folder) belongs to.
    pub account_email: String,
    /// The first condition: what it inspects…
    pub field: FilterField,
    /// …how…
    pub matcher: FilterMatch,
    /// …and against what. Commas separate alternatives, any one of which
    /// matches (#192): "invoice, receipt".
    pub value: String,
    /// The conditions after the first (#192).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more: Vec<FilterCondition>,
    /// Whether one matching condition is enough (true) or every condition
    /// must match (false, the default).
    #[serde(default, skip_serializing_if = "is_false")]
    pub any: bool,
    /// Destination folder path on the account. Empty leaves the mail where it
    /// is (a rule that only tags, #71).
    #[serde(default)]
    pub dest_path: String,
    /// The keyword of a [`Tag`] to put on matching mail (#71); empty tags
    /// nothing. Tagging and filing combine: the tag goes on first, and the
    /// move carries it along.
    #[serde(default)]
    pub tag: String,
    /// Whether the destination folder's unread mail counts toward the unread
    /// total (the All Inboxes chip, the tray icon and its menu, the
    /// Background Apps status), as inbox mail does (#116). Trash and Junk
    /// destinations never count, whatever this says.
    #[serde(default = "count_unread_default")]
    pub count_unread: bool,
}

fn count_unread_default() -> bool {
    true
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The subject with any leading "Re:", "Fwd:" or "Fw:" (any case, repeated)
/// taken off, and the whitespace around it.
fn strip_reply_prefixes(subject: &str) -> &str {
    const PREFIXES: [&str; 3] = ["re:", "fwd:", "fw:"];
    let mut rest = subject.trim();
    while let Some(next) = PREFIXES.iter().find_map(|p| {
        let head = rest.get(..p.len())?;
        head.eq_ignore_ascii_case(p)
            .then(|| rest[p.len()..].trim_start())
    }) {
        rest = next;
    }
    rest.trim()
}

/// One condition of a [`FilterRule`]: what to look at, how, and for what.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(from = "FilterConditionDisk", into = "FilterConditionDisk")]
pub struct FilterCondition {
    pub field: FilterField,
    pub matcher: FilterMatch,
    /// Commas separate alternatives; any one of them matching is a match.
    pub value: String,
}

/// How `filters.toml` writes a sender-in-Contacts condition (PR #384). An
/// older Hylki knows no such field and would drop the whole file over one,
/// so the condition goes to disk as an empty From address match, which
/// matches nothing there, with `contacts` beside it saying what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContactCheck {
    InContacts,
    NotInContacts,
}

/// The field as written to disk, and the `contacts` key that goes with it.
fn field_to_disk(field: FilterField) -> (FilterField, Option<ContactCheck>) {
    match field {
        FilterField::FromInContacts => (FilterField::FromAddress, Some(ContactCheck::InContacts)),
        FilterField::FromNotInContacts => {
            (FilterField::FromAddress, Some(ContactCheck::NotInContacts))
        }
        other => (other, None),
    }
}

/// The field a condition read from disk stands for.
fn field_from_disk(field: FilterField, contacts: Option<ContactCheck>) -> FilterField {
    match contacts {
        Some(ContactCheck::InContacts) => FilterField::FromInContacts,
        Some(ContactCheck::NotInContacts) => FilterField::FromNotInContacts,
        None => field,
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FilterConditionDisk {
    field: FilterField,
    matcher: FilterMatch,
    value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contacts: Option<ContactCheck>,
}

impl From<FilterConditionDisk> for FilterCondition {
    fn from(d: FilterConditionDisk) -> Self {
        FilterCondition {
            field: field_from_disk(d.field, d.contacts),
            matcher: d.matcher,
            value: d.value,
        }
    }
}

impl From<FilterCondition> for FilterConditionDisk {
    fn from(c: FilterCondition) -> Self {
        let (field, contacts) = field_to_disk(c.field);
        let value = if contacts.is_some() {
            String::new()
        } else {
            c.value
        };
        FilterConditionDisk {
            field,
            matcher: c.matcher,
            value,
            contacts,
        }
    }
}

/// [`FilterRule`] as `filters.toml` holds it: the same keys, plus `contacts`
/// for a first condition that checks the address books.
#[derive(serde::Serialize, serde::Deserialize)]
struct FilterRuleDisk {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    account_email: String,
    field: FilterField,
    matcher: FilterMatch,
    value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contacts: Option<ContactCheck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    more: Vec<FilterCondition>,
    #[serde(default, skip_serializing_if = "is_false")]
    any: bool,
    #[serde(default)]
    dest_path: String,
    #[serde(default)]
    tag: String,
    #[serde(default = "count_unread_default")]
    count_unread: bool,
}

impl From<FilterRuleDisk> for FilterRule {
    fn from(d: FilterRuleDisk) -> Self {
        FilterRule {
            name: d.name,
            account_email: d.account_email,
            field: field_from_disk(d.field, d.contacts),
            matcher: d.matcher,
            value: d.value,
            more: d.more,
            any: d.any,
            dest_path: d.dest_path,
            tag: d.tag,
            count_unread: d.count_unread,
        }
    }
}

impl From<FilterRule> for FilterRuleDisk {
    fn from(r: FilterRule) -> Self {
        let (field, contacts) = field_to_disk(r.field);
        let value = if contacts.is_some() {
            String::new()
        } else {
            r.value
        };
        FilterRuleDisk {
            name: r.name,
            account_email: r.account_email,
            field,
            matcher: r.matcher,
            value,
            contacts,
            more: r.more,
            any: r.any,
            dest_path: r.dest_path,
            tag: r.tag,
            count_unread: r.count_unread,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterField {
    FromAddress,
    FromName,
    Subject,
    Recipients,
    /// The Reply-To header, or the From address when there is none (#191).
    ReplyTo,
    /// The message text (#191). Searched on the server as the inbox syncs
    /// (IMAP `SEARCH BODY`, Microsoft Graph `$search`), so nothing is
    /// downloaded for it; the list preview and a cached body are checked
    /// too. Always a "contains" match, whatever the matcher says: that is
    /// the only search a server offers.
    Body,
    /// The sender's address is in the address books: EDS and the local Hylki
    /// book. Takes no text and ignores the matcher. Never matches while the
    /// address books are unreadable or empty, so a book that failed to load
    /// cannot make every sender look unknown.
    FromInContacts,
    /// The opposite of [`FilterField::FromInContacts`], with the same guard.
    /// Neither is ever written under its own name: see [`ContactCheck`].
    FromNotInContacts,
}

impl FilterField {
    /// Whether the field needs no text and no matcher.
    pub fn is_contact_check(self) -> bool {
        matches!(
            self,
            FilterField::FromInContacts | FilterField::FromNotInContacts
        )
    }

    /// Every field, in the order the editor lists them.
    pub const ALL: [FilterField; 8] = [
        FilterField::FromAddress,
        FilterField::FromName,
        FilterField::Subject,
        FilterField::Recipients,
        FilterField::ReplyTo,
        FilterField::Body,
        FilterField::FromInContacts,
        FilterField::FromNotInContacts,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMatch {
    Contains,
    Equals,
    StartsWith,
    EndsWith,
}

impl FilterMatch {
    /// Every matcher, in the order the editor lists them.
    pub const ALL: [FilterMatch; 4] = [
        FilterMatch::Contains,
        FilterMatch::Equals,
        FilterMatch::StartsWith,
        FilterMatch::EndsWith,
    ];
}

/// What a message offers the conditions to look at.
#[derive(Clone, Copy, Debug, Default)]
pub struct FilterInput<'a> {
    pub from_addr: &'a str,
    pub from_name: &'a str,
    pub subject: &'a str,
    /// To and Cc together.
    pub recipients: &'a str,
    /// The Reply-To header; empty when absent.
    pub reply_to: &'a str,
    /// The message text on hand: the list preview, or the body when the
    /// list carries it.
    pub body: &'a str,
    /// The body alternatives the server confirmed for this message (#191),
    /// lowercased as [`FilterCondition::alternatives`] hands them out.
    pub body_hits: &'a [String],
    /// Lowercased addresses from the address books, or `None` when they are
    /// unknown (unread, empty, or no rule asked for them).
    pub contacts: Option<&'a std::collections::HashSet<String>>,
}

impl FilterCondition {
    /// The alternatives a value names (#192): its comma-separated pieces,
    /// trimmed and lowercased, empties dropped. A value without a comma is
    /// one alternative.
    pub fn alternatives(value: &str) -> Vec<String> {
        value
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Case-insensitive match against one message.
    pub fn matches(&self, input: &FilterInput) -> bool {
        if self.field.is_contact_check() {
            let Some(contacts) = input.contacts else {
                return false;
            };
            let known = contacts.contains(&input.from_addr.trim().to_lowercase());
            return known == (self.field == FilterField::FromInContacts);
        }
        let alts = Self::alternatives(&self.value);
        if alts.is_empty() {
            return false;
        }
        if self.field == FilterField::Body {
            // The server's word (a hit) or the text on hand; a rule set up
            // on a backend without a search still works off the preview.
            let text = input.body.to_lowercase();
            return alts
                .iter()
                .any(|a| input.body_hits.iter().any(|h| h == a) || text.contains(a.as_str()));
        }
        let hay = match self.field {
            FilterField::FromAddress => input.from_addr,
            FilterField::FromName => input.from_name,
            FilterField::Subject => input.subject,
            FilterField::Recipients => input.recipients,
            FilterField::ReplyTo if input.reply_to.trim().is_empty() => input.from_addr,
            FilterField::ReplyTo => input.reply_to,
            FilterField::Body | FilterField::FromInContacts | FilterField::FromNotInContacts => {
                unreachable!()
            }
        }
        .to_lowercase();
        // The recipients are a list. "Contains" reads it whole, names
        // included; the other matchers hold each recipient up on its own,
        // so "is x@y" matches mail sent to x@y and someone else, and "ends
        // with @y" finds any one recipient there, not only the last (#201).
        let hays = if self.field == FilterField::Recipients && self.matcher != FilterMatch::Contains
        {
            mailboxes(&hay)
        } else {
            vec![hay]
        };
        alts.iter().any(|needle| {
            hays.iter().any(|hay| match self.matcher {
                FilterMatch::Contains => hay.contains(needle.as_str()),
                FilterMatch::Equals => hay == needle,
                FilterMatch::StartsWith => hay.starts_with(needle.as_str()),
                FilterMatch::EndsWith => hay.ends_with(needle.as_str()),
            })
        })
    }
}

/// The pieces of a comma-separated recipient list a matcher can be held
/// against: every address on its own, and every display name on its own.
/// `Ann <ann@shop.example>, bob@shop.example` gives `ann`, `ann@shop.example`
/// and `bob@shop.example`.
fn mailboxes(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match (part.rfind('<'), part.rfind('>')) {
            (Some(lt), Some(gt)) if lt < gt => {
                let name = part[..lt].trim().trim_matches('"').trim();
                if !name.is_empty() {
                    out.push(name.to_string());
                }
                out.push(part[lt + 1..gt].trim().to_string());
            }
            _ => out.push(part.to_string()),
        }
    }
    out
}

impl FilterRule {
    /// The rule the "Create Filter from Message…" entry starts the
    /// editor with: the sender's address, and the subject without its
    /// leading "Re:" / "Fwd:" so a whole thread matches. Both have to match;
    /// what the rule does is left for the user to pick. A blank subject
    /// leaves the sender as the only condition.
    pub fn draft_from_message(account_email: &str, from_addr: &str, subject: &str) -> FilterRule {
        let mut conditions = vec![FilterCondition {
            field: FilterField::FromAddress,
            matcher: FilterMatch::Equals,
            value: from_addr.trim().to_string(),
        }];
        let subject = strip_reply_prefixes(subject);
        if !subject.is_empty() {
            conditions.push(FilterCondition {
                field: FilterField::Subject,
                matcher: FilterMatch::Contains,
                value: subject.to_string(),
            });
        }
        let mut rule = FilterRule {
            name: String::new(),
            account_email: account_email.to_string(),
            field: FilterField::FromAddress,
            matcher: FilterMatch::Equals,
            value: String::new(),
            more: Vec::new(),
            any: false,
            dest_path: String::new(),
            tag: String::new(),
            count_unread: count_unread_default(),
        };
        rule.set_conditions(conditions);
        rule
    }

    /// Every condition, the first included.
    pub fn conditions(&self) -> Vec<FilterCondition> {
        let mut all = Vec::with_capacity(1 + self.more.len());
        all.push(FilterCondition {
            field: self.field,
            matcher: self.matcher,
            value: self.value.clone(),
        });
        all.extend(self.more.iter().cloned());
        all
    }

    /// Replace the conditions: the first goes to the top level, the rest to
    /// `more`. Returns false (and changes nothing) when `conds` is empty.
    pub fn set_conditions(&mut self, mut conds: Vec<FilterCondition>) -> bool {
        if conds.is_empty() {
            return false;
        }
        let first = conds.remove(0);
        self.field = first.field;
        self.matcher = first.matcher;
        self.value = first.value;
        self.more = conds;
        true
    }

    /// What to call the rule in a list or a log line (#197): its name, or
    /// its conditions when it has none.
    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        self.conditions()
            .iter()
            .map(|c| format!("{:?} {:?} {}", c.field, c.matcher, c.value))
            .collect::<Vec<_>>()
            .join(if self.any { " or " } else { " and " })
    }

    /// Whether any condition looks the sender up in the address books.
    pub fn needs_contacts(&self) -> bool {
        self.conditions().iter().any(|c| c.field.is_contact_check())
    }

    /// The body alternatives this rule needs a server search for (#191).
    pub fn body_needles(&self) -> Vec<String> {
        self.conditions()
            .iter()
            .filter(|c| c.field == FilterField::Body)
            .flat_map(|c| FilterCondition::alternatives(&c.value))
            .collect()
    }

    /// Case-insensitive match against a message: every condition must hold,
    /// or any one of them with `any` set.
    pub fn matches(&self, input: &FilterInput) -> bool {
        let conds = self.conditions();
        if self.any {
            conds.iter().any(|c| c.matches(input))
        } else {
            conds.iter().all(|c| c.matches(input))
        }
    }
}

/// Every body alternative the rules of `email` search for (#191), lowercased
/// and deduplicated, read fresh from disk: the worker asks at each inbox
/// sync, so a rule added in Settings counts from the next sync on.
pub fn filter_body_needles(email: &str) -> Vec<String> {
    let mut out: Vec<String> = load_filters()
        .iter()
        .filter(|r| r.account_email.eq_ignore_ascii_case(email))
        .flat_map(|r| r.body_needles())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct FiltersFile {
    #[serde(default)]
    rules: Vec<FilterRule>,
}

fn filters_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("filters.toml"))
}

pub fn load_filters() -> Vec<FilterRule> {
    let Some(path) = filters_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    toml::from_str::<FiltersFile>(&text)
        .map(|f| f.rules)
        .unwrap_or_default()
}

pub fn save_filters(rules: &[FilterRule]) {
    let Some(path) = filters_path() else { return };
    let file = FiltersFile {
        rules: rules.to_vec(),
    };
    match toml::to_string_pretty(&file) {
        Ok(toml) => {
            if let Err(e) = write_private(&path, &toml) {
                tracing::warn!("could not save filters: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize filters: {e}"),
    }
}

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
struct SmartFoldersFile {
    #[serde(default)]
    folders: Vec<SmartFolder>,
}

fn smart_folders_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("smart_folders.toml"))
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
    let Some(path) = smart_folders_path() else {
        return;
    };
    let file = SmartFoldersFile {
        folders: folders.to_vec(),
    };
    match toml::to_string_pretty(&file) {
        Ok(toml) => {
            if let Err(e) = write_private(&path, &toml) {
                tracing::warn!("could not save smart folders: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize smart folders: {e}"),
    }
}

/// A tag (#71): a name and a color for one IMAP keyword. Keywords are the
/// standard's own per-message user flags, kept on the server beside `\Seen`
/// and `\Flagged`, so a tag set here is the same tag Thunderbird, Apple Mail
/// or a webmail shows — and theirs show here once a tag names their keyword
/// (Thunderbird's built-in five are `$label1`…`$label5`). Microsoft 365
/// stores them as categories; POP3 and servers that refuse custom keywords
/// keep them in Hylki's own cache instead.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tag {
    /// What the user sees.
    pub name: String,
    /// The keyword the tag is stored as: an IMAP atom (no spaces, quotes,
    /// parentheses, brackets, backslashes or non-ASCII), case-insensitive.
    pub keyword: String,
    /// `#rrggbb`.
    pub color: String,
}

/// The tag palette, GNOME's color set at its middle strength.
pub const TAG_COLORS: &[&str] = &[
    "#1c71d8", "#2ec27e", "#f5c211", "#e66100", "#c01c28", "#813d9c", "#865e3c", "#77767b",
];

impl Tag {
    /// The characters an IMAP keyword (an atom) may hold.
    pub fn is_keyword_char(c: char) -> bool {
        c.is_ascii_graphic()
            && !matches!(
                c,
                '(' | ')' | '{' | '}' | '"' | '\\' | '%' | '*' | ']' | '['
            )
    }

    /// Whether `s` can be sent to a server as a keyword.
    pub fn valid_keyword(s: &str) -> bool {
        !s.is_empty() && s.chars().all(Self::is_keyword_char)
    }

    /// The keyword a tag named `name` gets unless the user picks one: the
    /// name with spaces turned to underscores and anything an atom cannot
    /// carry dropped ("To Do" → `To_Do`, "Réunion" → `Runion`).
    pub fn keyword_for(name: &str) -> String {
        name.trim()
            .chars()
            .map(|c| if c == ' ' { '_' } else { c })
            .filter(|c| Self::is_keyword_char(*c))
            .collect()
    }

    /// The CSS class carrying this tag's color (see the app's tag stylesheet):
    /// the keyword reduced to what a class name may hold.
    pub fn css_class(&self) -> String {
        tag_css_class(&self.keyword)
    }

    /// The name to offer for a keyword found on a server (the tag finder):
    /// Thunderbird's five built-ins get Thunderbird's names; anything else
    /// sheds a leading `$`, turns `_`/`-` into spaces and capitalises each
    /// word (`travel_plans` → "Travel Plans", `$Receipts` → "Receipts").
    pub fn name_for_keyword(keyword: &str) -> String {
        match keyword.to_ascii_lowercase().as_str() {
            "$label1" => return "Important".to_string(),
            "$label2" => return "Work".to_string(),
            "$label3" => return "Personal".to_string(),
            "$label4" => return "To Do".to_string(),
            "$label5" => return "Later".to_string(),
            _ => {}
        }
        let bare = keyword.trim_start_matches('$');
        let name: String = bare
            .split(['_', '-'])
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut cs = w.chars();
                match cs.next() {
                    Some(first) => first.to_uppercase().chain(cs).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        if name.is_empty() {
            keyword.to_string()
        } else {
            name
        }
    }

    /// The color to offer for a found keyword: Thunderbird's built-ins get
    /// their Thunderbird colors (as the palette has them); the rest take
    /// the first palette color not in `taken`, cycling once every color is.
    pub fn color_for_keyword(keyword: &str, taken: &[String]) -> String {
        let fixed = match keyword.to_ascii_lowercase().as_str() {
            "$label1" => Some(TAG_COLORS[4]), // red
            "$label2" => Some(TAG_COLORS[3]), // orange
            "$label3" => Some(TAG_COLORS[1]), // green
            "$label4" => Some(TAG_COLORS[0]), // blue
            "$label5" => Some(TAG_COLORS[5]), // purple
            _ => None,
        };
        if let Some(c) = fixed {
            return c.to_string();
        }
        TAG_COLORS
            .iter()
            .find(|c| !taken.iter().any(|t| t.eq_ignore_ascii_case(c)))
            .copied()
            .unwrap_or(TAG_COLORS[taken.len() % TAG_COLORS.len()])
            .to_string()
    }
}

/// The CSS class for a keyword's color: `tag-` plus the keyword lowercased,
/// with every character a class name can't carry turned into its code so two
/// keywords never share a class.
pub fn tag_css_class(keyword: &str) -> String {
    let mut out = String::from("tag-");
    for c in keyword.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push_str(&format!("_{:x}", c as u32));
        }
    }
    out
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct TagsFile {
    #[serde(default)]
    tags: Vec<Tag>,
}

fn tags_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("tags.toml"))
}

pub fn load_tags() -> Vec<Tag> {
    let Some(path) = tags_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    toml::from_str::<TagsFile>(&text)
        .map(|f| f.tags)
        .unwrap_or_default()
}

pub fn save_tags(tags: &[Tag]) {
    let Some(path) = tags_path() else { return };
    let file = TagsFile {
        tags: tags.to_vec(),
    };
    match toml::to_string_pretty(&file) {
        Ok(toml) => {
            if let Err(e) = write_private(&path, &toml) {
                tracing::warn!("could not save tags: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize tags: {e}"),
    }
}

/// A portable settings bundle (#50): every configuration file Hylki keeps —
/// preferences, accounts (colors, emoji, labels, aliases, folder roles and
/// per-account push included), filters, tags, cloud storage accounts,
/// sidebar layout, window/pane state (the app icon choice with it), and
/// the words taught to the spell checker. Passwords and tokens never
/// appear — their fields are skip_serializing, and they live in the
/// keyring, not on disk; OpenPGP keys live in GnuPG's keyring, and the
/// optional oauth.toml (a client id and secret the user supplied by hand)
/// stays out for the same reason.
///
/// Sections added after the first release are optional, so a bundle from
/// before them imports without wiping what it does not mention.
#[derive(serde::Serialize, serde::Deserialize)]
struct SettingsBundle {
    version: u32,
    privacy: PrivacyFile,
    #[serde(default)]
    accounts: Vec<AccountConfig>,
    #[serde(default)]
    filters: Vec<FilterRule>,
    #[serde(default)]
    tags: Vec<Tag>,
    #[serde(default)]
    sidebar: Option<SidebarFile>,
    #[serde(default)]
    window: Option<WindowFile>,
    #[serde(default)]
    state: Option<StateFile>,
    /// Cloud storage accounts (#144 onward); their sign-ins stay in the
    /// keyring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cloud: Option<Vec<crate::cloud::CloudAccount>>,
    /// The spell checker's personal words, per language (#114).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spell_words: Option<std::collections::BTreeMap<String, Vec<String>>>,
}

/// Parse one of the config directory's TOML files into its struct (None when
/// absent or unreadable — the bundle simply omits it).
fn read_file_struct<T: serde::de::DeserializeOwned>(path: Option<PathBuf>) -> Option<T> {
    let text = std::fs::read_to_string(path?).ok()?;
    toml::from_str(&text).ok()
}

/// Serialize a bundle section back to its config file.
fn write_file_struct<T: serde::Serialize>(
    path: Option<PathBuf>,
    value: &Option<T>,
) -> Result<(), String> {
    let (Some(path), Some(value)) = (path, value) else {
        return Ok(());
    };
    let toml = toml::to_string_pretty(value).map_err(|e| e.to_string())?;
    write_private(&path, &toml).map_err(|e| e.to_string())
}

/// The current configuration as a TOML bundle for Export Settings.
pub fn export_bundle() -> Result<String, String> {
    let bundle = SettingsBundle {
        version: 1,
        privacy: load_privacy(),
        accounts: load().unwrap_or_default(),
        filters: load_filters(),
        tags: load_tags(),
        sidebar: read_file_struct(sidebar_path()),
        window: read_file_struct(window_path()),
        state: read_file_struct(state_path()),
        cloud: Some(crate::cloud::load_accounts()),
        spell_words: Some(crate::spell::all_personal_words()),
    };
    toml::to_string_pretty(&bundle).map_err(|e| e.to_string())
}

/// Parse and persist an exported bundle: privacy.toml and accounts.toml are
/// replaced (via the same writers the app uses; the keyring is untouched
/// since imported accounts carry no secrets). Returns the account count.
pub fn import_bundle(text: &str) -> Result<usize, String> {
    let bundle: SettingsBundle = toml::from_str(text).map_err(|e| e.to_string())?;
    if bundle.version != 1 {
        return Err(format!("unsupported bundle version {}", bundle.version));
    }
    let path = privacy_path().ok_or("no config directory")?;
    let toml = toml::to_string_pretty(&bundle.privacy).map_err(|e| e.to_string())?;
    write_private(&path, &toml).map_err(|e| e.to_string())?;
    save(&bundle.accounts).map_err(|e| e.to_string())?;
    save_filters(&bundle.filters);
    save_tags(&bundle.tags);
    write_file_struct(sidebar_path(), &bundle.sidebar)?;
    write_file_struct(window_path(), &bundle.window)?;
    write_file_struct(state_path(), &bundle.state)?;
    if let Some(cloud) = &bundle.cloud {
        crate::cloud::save_accounts(cloud);
    }
    // Words are merged, never removed: a backup can only teach the
    // checker more.
    if let Some(words) = &bundle.spell_words {
        for (lang, list) in words {
            crate::spell::merge_personal_words(lang, list);
        }
    }
    Ok(bundle.accounts.len())
}

fn default_on() -> bool {
    true
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

#[cfg(test)]
mod filter_tests {
    use super::*;

    fn smart_input<'a>(
        starred: bool,
        unread: bool,
        keywords: &'a [String],
        tags: &'a [String],
    ) -> SmartInput<'a> {
        SmartInput {
            text: FilterInput {
                subject: "Quarterly invoice",
                from_addr: "billing@shop.example",
                ..Default::default()
            },
            starred,
            unread,
            attachment: false,
            keywords,
            tags,
        }
    }

    fn smart(any: bool, conditions: Vec<SmartCondition>) -> SmartFolder {
        SmartFolder {
            id: "x".into(),
            name: "Test".into(),
            account_email: String::new(),
            folder: Some(SmartFolderKind::Sent),
            any,
            conditions,
        }
    }

    fn smart_cond(field: SmartField, value: &str) -> SmartCondition {
        SmartCondition {
            field,
            matcher: FilterMatch::Contains,
            value: value.into(),
        }
    }

    /// #417's own example: sent mail that is starred or carries a tag.
    #[test]
    fn smart_folder_starred_or_tagged() {
        let f = smart(
            true,
            vec![
                smart_cond(SmartField::Starred, ""),
                smart_cond(SmartField::Tagged, ""),
            ],
        );
        let tags = vec!["$label1".to_string()];
        let none: Vec<String> = Vec::new();
        let tagged = vec!["$Label1".to_string()];
        let server_only = vec!["$Forwarded".to_string()];
        assert!(f.matches(&smart_input(true, false, &none, &tags)));
        assert!(f.matches(&smart_input(false, false, &tagged, &tags)));
        // A keyword no tag names is not a tag.
        assert!(!f.matches(&smart_input(false, false, &server_only, &tags)));
        assert!(!f.matches(&smart_input(false, false, &none, &tags)));
        // Nothing can be ruled out ahead when any one condition will do.
        assert_eq!(f.prefilter(), SmartPrefilter::default());
    }

    #[test]
    fn smart_folder_every_condition() {
        let f = smart(
            false,
            vec![
                smart_cond(SmartField::Unread, ""),
                smart_cond(SmartField::Subject, "invoice, receipt"),
            ],
        );
        let none: Vec<String> = Vec::new();
        assert!(f.matches(&smart_input(false, true, &none, &none)));
        assert!(!f.matches(&smart_input(false, false, &none, &none)));
        assert_eq!(
            f.prefilter(),
            SmartPrefilter {
                unread: Some(true),
                ..Default::default()
            }
        );
        let one_tag = smart(false, vec![smart_cond(SmartField::Tagged, "work")]);
        let work = vec!["Work".to_string()];
        assert!(one_tag.matches(&smart_input(false, false, &work, &none)));
        assert!(!one_tag.matches(&smart_input(false, false, &none, &none)));
    }

    #[test]
    fn smart_folders_file_round_trips() {
        let f = smart(
            true,
            vec![
                smart_cond(SmartField::Starred, ""),
                smart_cond(SmartField::FromAddress, "shop"),
            ],
        );
        let text = toml::to_string_pretty(&SmartFoldersFile {
            folders: vec![f.clone()],
        })
        .unwrap();
        assert!(text.contains("folder = \"sent\""), "{text}");
        let back: SmartFoldersFile = toml::from_str(&text).unwrap();
        assert_eq!(back.folders, vec![f]);
        // Only what a folder needs has to be written.
        let short: SmartFoldersFile =
            toml::from_str("[[folders]]\nid = \"a\"\nname = \"Starred\"\n[[folders.conditions]]\nfield = \"starred\"\n").unwrap();
        assert_eq!(
            short.folders[0].conditions[0].matcher,
            FilterMatch::Contains
        );
        assert!(short.folders[0].folder.is_none() && !short.folders[0].any);
    }

    fn rule(field: FilterField, matcher: FilterMatch, value: &str) -> FilterRule {
        FilterRule {
            name: String::new(),
            account_email: "a@b.c".into(),
            field,
            matcher,
            value: value.into(),
            more: Vec::new(),
            any: false,
            dest_path: "Archive".into(),
            tag: String::new(),
            count_unread: true,
        }
    }

    fn cond(field: FilterField, matcher: FilterMatch, value: &str) -> FilterCondition {
        FilterCondition {
            field,
            matcher,
            value: value.into(),
        }
    }

    /// The four header fields the original rules looked at.
    fn headers<'a>(
        from_addr: &'a str,
        from_name: &'a str,
        subject: &'a str,
        recipients: &'a str,
    ) -> FilterInput<'a> {
        FilterInput {
            from_addr,
            from_name,
            subject,
            recipients,
            ..Default::default()
        }
    }

    #[test]
    fn draft_filter_matches_sender_and_subject() {
        let rule =
            FilterRule::draft_from_message("me@x.org", "billing@shop.example", "Your invoice");
        assert_eq!(rule.account_email, "me@x.org");
        assert!(!rule.any, "both conditions have to match");
        assert!(
            rule.dest_path.is_empty() && rule.tag.is_empty(),
            "the user picks what the rule does"
        );
        assert_eq!(
            rule.conditions(),
            vec![
                FilterCondition {
                    field: FilterField::FromAddress,
                    matcher: FilterMatch::Equals,
                    value: "billing@shop.example".into(),
                },
                FilterCondition {
                    field: FilterField::Subject,
                    matcher: FilterMatch::Contains,
                    value: "Your invoice".into(),
                },
            ]
        );
    }

    #[test]
    fn draft_filter_strips_reply_and_forward_prefixes() {
        let subject_of = |subject| {
            FilterRule::draft_from_message("me@x.org", "a@b.org", subject).conditions()[1]
                .value
                .clone()
        };
        assert_eq!(subject_of("Re: Invoice"), "Invoice");
        assert_eq!(subject_of("RE: Fwd: re:  Invoice"), "Invoice");
        assert_eq!(subject_of("Fw: Invoice"), "Invoice");
        // Only a leading prefix goes; one inside the subject is the subject's own.
        assert_eq!(subject_of("Invoice re: March"), "Invoice re: March");
        assert_eq!(subject_of("Reminder: Invoice"), "Reminder: Invoice");
    }

    #[test]
    fn draft_filter_skips_an_empty_subject() {
        for subject in ["", "   ", "Re: ", "Fwd:"] {
            let rule = FilterRule::draft_from_message("me@x.org", "a@b.org", subject);
            assert_eq!(
                rule.conditions().len(),
                1,
                "subject {subject:?} leaves the sender alone"
            );
            assert_eq!(rule.field, FilterField::FromAddress);
        }
    }

    #[test]
    fn found_keywords_get_names_and_colors() {
        // Thunderbird's built-ins keep Thunderbird's names and colors.
        assert_eq!(Tag::name_for_keyword("$label1"), "Important");
        assert_eq!(Tag::name_for_keyword("$LABEL4"), "To Do");
        assert_eq!(Tag::color_for_keyword("$label1", &[]), TAG_COLORS[4]);
        // Anything else reads as words.
        assert_eq!(Tag::name_for_keyword("travel_plans"), "Travel Plans");
        assert_eq!(Tag::name_for_keyword("$Receipts"), "Receipts");
        assert_eq!(Tag::name_for_keyword("to-do"), "To Do");
        assert_eq!(Tag::name_for_keyword("$"), "$");
        // Colors skip what is taken, then cycle.
        let taken: Vec<String> = TAG_COLORS[..2].iter().map(|c| c.to_string()).collect();
        assert_eq!(Tag::color_for_keyword("Receipts", &taken), TAG_COLORS[2]);
        let all: Vec<String> = TAG_COLORS.iter().map(|c| c.to_string()).collect();
        assert_eq!(Tag::color_for_keyword("Receipts", &all), TAG_COLORS[0]);
        // Bookkeeping keywords are not tags.
        assert!(crate::worker::is_system_keyword("$Forwarded"));
        assert!(crate::worker::is_system_keyword("NonJunk"));
        assert!(crate::worker::is_system_keyword("$MailFlagBit1"));
        assert!(!crate::worker::is_system_keyword("$label2"));
        assert!(!crate::worker::is_system_keyword("Receipts"));
    }

    #[test]
    fn tag_keywords_derive_from_names() {
        assert_eq!(Tag::keyword_for("To Do"), "To_Do");
        assert_eq!(Tag::keyword_for("  Work "), "Work");
        assert_eq!(Tag::keyword_for("Réunion (lundi)"), "Runion_lundi");
        assert!(Tag::valid_keyword("$label1"));
        assert!(!Tag::valid_keyword("has space"));
        assert!(!Tag::valid_keyword(""));
        assert_eq!(tag_css_class("$label1"), "tag-_24label1");
        assert_eq!(tag_css_class("Work"), "tag-work");
    }

    #[test]
    fn filters_match_case_insensitively_per_field() {
        let r = rule(FilterField::FromAddress, FilterMatch::Contains, "NEWS@");
        assert!(r.matches(&headers("news@example.com", "", "", "")));
        assert!(!r.matches(&headers("other@example.com", "News", "News", "News")));

        let r = rule(FilterField::Subject, FilterMatch::StartsWith, "[list]");
        assert!(r.matches(&headers("", "", "[LIST] hello", "")));
        assert!(!r.matches(&headers("", "", "re: [list] hello", "")));

        let r = rule(FilterField::Recipients, FilterMatch::Contains, "team@");
        assert!(r.matches(&headers("", "", "", "me@x.org team@x.org")));

        let r = rule(FilterField::FromName, FilterMatch::Equals, "Bank");
        assert!(r.matches(&headers("", "bank", "", "")));
        assert!(!r.matches(&headers("", "bankster", "", "")));

        // An empty needle can never match (a half-filled rule stays inert).
        let r = rule(FilterField::Subject, FilterMatch::Contains, "");
        assert!(!r.matches(&headers("x", "x", "x", "x")));
        let r = rule(FilterField::Subject, FilterMatch::Contains, " , ,");
        assert!(!r.matches(&headers("x", "x", "x", "x")));
    }

    #[test]
    fn contact_conditions_reach_disk_in_a_form_older_versions_read() {
        let mut r = rule(FilterField::FromNotInContacts, FilterMatch::Contains, "");
        r.more
            .push(cond(FilterField::FromInContacts, FilterMatch::Contains, ""));
        r.more
            .push(cond(FilterField::Subject, FilterMatch::Contains, "sale"));
        let text = toml::to_string_pretty(&FiltersFile {
            rules: vec![r.clone()],
        })
        .unwrap();
        assert!(
            !text.contains("from_in_contacts") && !text.contains("from_not_in_contacts"),
            "{text}"
        );
        assert!(text.contains("contacts = \"not_in_contacts\""), "{text}");
        let back: FiltersFile = toml::from_str(&text).unwrap();
        assert_eq!(back.rules, vec![r]);

        // What a version without the conditions reads: the same file, with
        // `contacts` an unknown key it skips, is a From match on nothing.
        #[derive(serde::Deserialize)]
        struct OldCondition {
            field: String,
            value: String,
        }
        #[derive(serde::Deserialize)]
        struct OldRule {
            field: String,
            value: String,
            more: Vec<OldCondition>,
        }
        #[derive(serde::Deserialize)]
        struct OldFile {
            rules: Vec<OldRule>,
        }
        let old: OldFile = toml::from_str(&text).unwrap();
        assert_eq!(
            (old.rules[0].field.as_str(), old.rules[0].value.as_str()),
            ("from_address", "")
        );
        assert_eq!(old.rules[0].more[0].field, "from_address");
        assert_eq!(old.rules[0].more[0].value, "");
        assert_eq!(old.rules[0].more[1].field, "subject");
    }

    #[test]
    fn filters_check_the_sender_against_the_address_books() {
        let book: std::collections::HashSet<String> =
            ["ann@x.org".to_string()].into_iter().collect();
        let with_book = |from_addr| FilterInput {
            from_addr,
            contacts: Some(&book),
            ..Default::default()
        };

        let inside = rule(FilterField::FromInContacts, FilterMatch::Contains, "");
        let outside = rule(FilterField::FromNotInContacts, FilterMatch::Contains, "");
        assert!(inside.matches(&with_book("Ann@X.org ")));
        assert!(!inside.matches(&with_book("bob@x.org")));
        assert!(outside.matches(&with_book("bob@x.org")));
        assert!(!outside.matches(&with_book("ann@x.org")));

        // Unknown address books match neither way, so a book that failed to
        // load cannot make every sender look unknown.
        let none = FilterInput {
            from_addr: "bob@x.org",
            ..Default::default()
        };
        assert!(!inside.matches(&none));
        assert!(!outside.matches(&none));

        // Combined with another condition, and reported to the caller.
        let mut mixed = rule(FilterField::FromNotInContacts, FilterMatch::Contains, "");
        mixed
            .more
            .push(cond(FilterField::Subject, FilterMatch::Contains, "sale"));
        let input = FilterInput {
            from_addr: "bob@x.org",
            subject: "Big SALE",
            contacts: Some(&book),
            ..Default::default()
        };
        assert!(mixed.matches(&input));
        assert!(mixed.needs_contacts());
        assert!(!rule(FilterField::Subject, FilterMatch::Contains, "x").needs_contacts());
    }

    #[test]
    fn filter_values_hold_comma_separated_alternatives() {
        // #192: "invoice, receipt" matches either; spaces around the commas
        // and empty pieces are ignored; a comma-free value is one piece.
        assert_eq!(
            FilterCondition::alternatives(" Invoice ,receipt,, "),
            ["invoice", "receipt"]
        );
        assert_eq!(FilterCondition::alternatives("plain"), ["plain"]);
        let r = rule(
            FilterField::Subject,
            FilterMatch::Contains,
            "invoice, receipt",
        );
        assert!(r.matches(&headers("", "", "Your RECEIPT", "")));
        assert!(r.matches(&headers("", "", "invoice #12", "")));
        assert!(!r.matches(&headers("", "", "hello", "")));
        // Each alternative gets the whole matcher, not just "contains".
        let r = rule(
            FilterField::FromAddress,
            FilterMatch::EndsWith,
            "@a.org, @b.org",
        );
        assert!(r.matches(&headers("x@B.org", "", "", "")));
        assert!(!r.matches(&headers("x@b.org.evil", "", "", "")));
    }

    #[test]
    fn filter_rules_combine_conditions_all_or_any() {
        // #192: a second condition narrows by default…
        let mut r = rule(
            FilterField::FromAddress,
            FilterMatch::EndsWith,
            "@shop.example",
        );
        r.more
            .push(cond(FilterField::Subject, FilterMatch::Contains, "order"));
        assert!(r.matches(&headers("a@shop.example", "", "Order shipped", "")));
        assert!(!r.matches(&headers("a@shop.example", "", "Newsletter", "")));
        assert!(!r.matches(&headers("a@other.example", "", "Order shipped", "")));
        // …and widens with "any".
        r.any = true;
        assert!(r.matches(&headers("a@shop.example", "", "Newsletter", "")));
        assert!(r.matches(&headers("a@other.example", "", "Order shipped", "")));
        assert!(!r.matches(&headers("a@other.example", "", "Newsletter", "")));
        // A condition with nothing to match holds nothing up in "any" mode
        // and blocks an "all" rule, as an inert rule should.
        r.more
            .push(cond(FilterField::Subject, FilterMatch::Contains, ""));
        assert!(r.matches(&headers("a@shop.example", "", "", "")));
        r.any = false;
        assert!(!r.matches(&headers("a@shop.example", "", "Order shipped", "")));

        // The conditions round-trip through the accessors, first one included.
        let conds = r.conditions();
        assert_eq!(conds.len(), 3);
        assert_eq!(
            conds[0],
            cond(
                FilterField::FromAddress,
                FilterMatch::EndsWith,
                "@shop.example"
            )
        );
        assert!(!r.set_conditions(Vec::new()), "a rule needs a condition");
        assert!(r.set_conditions(vec![cond(
            FilterField::FromName,
            FilterMatch::Equals,
            "Bank"
        )]));
        assert_eq!(r.field, FilterField::FromName);
        assert_eq!(r.value, "Bank");
        assert!(r.more.is_empty());
    }

    #[test]
    fn filter_reply_to_falls_back_to_from() {
        // #191: Reply-To when the sender set one, From otherwise.
        let r = rule(FilterField::ReplyTo, FilterMatch::Contains, "sales@");
        let mut input = headers("noreply@shop.example", "", "", "");
        input.reply_to = "Sales@shop.example";
        assert!(r.matches(&input));
        input.reply_to = "";
        assert!(!r.matches(&input));
        input.from_addr = "sales@shop.example";
        assert!(r.matches(&input));
    }

    #[test]
    fn filter_recipient_matchers_look_at_each_recipient() {
        // #201: a rule on the recipients compared the whole To and Cc list
        // as one string, so "is x@y" never matched mail with a second
        // recipient, and "ends with @y" only ever saw the last one.
        let list = "Ann <ann@shop.example>, bob@shop.example, \"Cy, Jr\" <cy@other.example>";
        let input = headers("", "", "", list);
        let r = |m, v| rule(FilterField::Recipients, m, v);
        assert!(r(FilterMatch::Equals, "bob@shop.example").matches(&input));
        assert!(r(FilterMatch::Equals, "ANN@shop.example").matches(&input));
        assert!(r(FilterMatch::Equals, "cy@other.example").matches(&input));
        assert!(!r(FilterMatch::Equals, "shop.example").matches(&input));
        assert!(r(FilterMatch::EndsWith, "@shop.example").matches(&input));
        assert!(!r(FilterMatch::EndsWith, "@nowhere.example").matches(&input));
        assert!(r(FilterMatch::StartsWith, "cy@").matches(&input));
        assert!(r(FilterMatch::Contains, "other").matches(&input));
        // A display name counts on its own too.
        assert!(r(FilterMatch::Equals, "ann").matches(&input));
        // The single bare address most mail carries.
        assert!(r(FilterMatch::Equals, "me@shop.example").matches(&headers(
            "",
            "",
            "",
            "me@shop.example"
        )));
        assert!(!r(FilterMatch::Equals, "me@shop.example").matches(&headers("", "", "", "")));
    }

    #[test]
    fn filter_body_matches_hits_or_text_on_hand() {
        // #191: the server's search result counts, and so does the text the
        // list carries; the matcher is always "contains".
        let r = rule(
            FilterField::Body,
            FilterMatch::Equals,
            "unsubscribe, opt out",
        );
        let hits = vec!["unsubscribe".to_string()];
        let mut input = headers("", "", "", "");
        input.body_hits = &hits;
        assert!(r.matches(&input));
        input.body_hits = &[];
        assert!(!r.matches(&input));
        input.body = "Click here to OPT OUT of these mails";
        assert!(r.matches(&input));
        input.body = "nothing of the sort";
        assert!(!r.matches(&input));
        assert_eq!(r.body_needles(), ["unsubscribe", "opt out"]);
        assert!(rule(FilterField::Subject, FilterMatch::Contains, "x")
            .body_needles()
            .is_empty());
    }

    #[test]
    fn filter_rules_read_the_single_condition_files() {
        // A file from before #192 names one condition per rule and none of
        // the new keys; it must load as a one-condition, all-must-match rule.
        let text = r#"
[[rules]]
account_email = "a@b.c"
field = "subject"
matcher = "contains"
value = "digest"
dest_path = "Lists"
"#;
        let back: FiltersFile = toml::from_str(text).unwrap();
        assert_eq!(back.rules.len(), 1);
        let r = &back.rules[0];
        assert!(r.more.is_empty());
        assert!(!r.any);
        assert_eq!(
            r.conditions(),
            vec![cond(FilterField::Subject, FilterMatch::Contains, "digest")]
        );
        // And a one-condition rule written now carries none of the new keys,
        // so those versions read it back too.
        let text = toml::to_string_pretty(&FiltersFile {
            rules: back.rules.clone(),
        })
        .unwrap();
        assert!(!text.contains("more"), "{text}");
        assert!(!text.contains("any"), "{text}");
    }

    /// Grouping by date is on for a new install and for one whose file
    /// predates it, once; switched off afterwards, it stays off.
    #[test]
    fn date_groups_default_reaches_existing_installs_once() {
        let mut old: PrivacyFile = toml::from_str("list_group_by_date = false\n").unwrap();
        assert!(apply_new_defaults(&mut old));
        assert!(old.list_group_by_date);
        old.list_group_by_date = false;
        let mut again: PrivacyFile =
            toml::from_str(&toml::to_string_pretty(&old).unwrap()).unwrap();
        assert!(!apply_new_defaults(&mut again));
        assert!(!again.list_group_by_date);
        let mut fresh = PrivacyFile::default();
        assert!(fresh.list_group_by_date);
        fresh.list_group_by_date = false;
        apply_new_defaults(&mut fresh);
        assert!(!fresh.list_group_by_date);
    }

    /// An account from before #445 has one `signature`, which reads as a
    /// list of one that new messages start with; a list written now keeps
    /// `signature` on its default for an older Hylki, and no default
    /// leaves it empty.
    #[test]
    fn signatures_read_old_configs_and_write_for_them() {
        let old: AccountConfig = toml::from_str(
            "name = \"A\"\nemail = \"a@b.c\"\nimap_host = \"h\"\nusername = \"a\"\n\
             signature = \"<b>Ada</b>\"\nsignature_html = true\n",
        )
        .unwrap();
        let (list, default) = old.signature_set();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].html, "<b>Ada</b>");
        assert_eq!(default, Some(0));

        let mut acc = old.clone();
        let two = vec![
            NamedSignature {
                name: "English".into(),
                html: "Regards".into(),
            },
            NamedSignature {
                name: "Deutsch".into(),
                html: "Grüße".into(),
            },
        ];
        acc.set_signatures(two.clone(), Some(1));
        assert_eq!(acc.signature.as_deref(), Some("Grüße"));
        let back: AccountConfig = toml::from_str(&toml::to_string(&acc).unwrap()).unwrap();
        assert_eq!(back.signature_set(), (two.clone(), Some(1)));

        acc.set_signatures(two.clone(), None);
        assert!(acc.signature.is_none());
        let text = toml::to_string(&acc).unwrap();
        assert!(!text.contains("default_signature"), "{text}");
        let back: AccountConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.signature_set(), (two, None));

        acc.set_signatures(Vec::new(), None);
        assert!(!toml::to_string(&acc).unwrap().contains("signatures"));
    }

    #[test]
    fn settings_bundle_roundtrip_parses() {
        // Parse-and-serialize only (no disk, no keyring): the wire format
        // itself must round-trip, secrets must never appear.
        let mut acc = AccountConfig {
            name: "A".into(),
            email: "a@b.c".into(),
            protocol: Default::default(),
            imap_host: "imap.b.c".into(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 587,
            username: "a@b.c".into(),
            password: "SECRET".into(),
            smtp_separate: false,
            jmap_token: false,
            jmap_generic: false,
            tls_accept_hostname_mismatch: false,
            security: None,
            smtp_username: String::new(),
            smtp_password: String::new(),
            color: None,
            emoji: None,
            avatar: None,
            gravatar: false,
            signature: None,
            signature_html: false,
            signatures: Vec::new(),
            default_signature: None,
            label: None,
            aliases: Vec::new(),
            enabled: true,
            goa_id: None,
            goa_mail_disabled: false,
            goa_enabled_before_mail_disabled: true,
            oauth: false,
            oauth_settings: None,
            oauth_refresh: "TOKEN".into(),
            push: None,
            folder_roles: Default::default(),
            hidden_folders: Vec::new(),
            folders_seeded: false,
            sent_copy_path: None,
            server_saves_sent: false,
            reply_copies: Default::default(),
            empty_junk_days: 0,
            empty_trash_days: 0,
            clean_lists_days: 0,
            pgp_key: None,
            in_unified: true,
            folder_sort: None,
            sign_by_default: false,
            plain_subject: false,
            graph_directory: false,
            graph_directory_only: false,
            graph_label_names: false,
        };
        acc.aliases = Vec::new();
        let bundle = SettingsBundle {
            version: 1,
            privacy: PrivacyFile::default(),
            accounts: vec![acc],
            filters: Vec::new(),
            tags: Vec::new(),
            sidebar: None,
            window: None,
            state: None,
            cloud: Some(vec![crate::cloud::CloudAccount::empty()]),
            spell_words: Some(std::collections::BTreeMap::from([(
                "en_US".to_string(),
                vec!["Hylki".to_string()],
            )])),
        };
        let text = toml::to_string_pretty(&bundle).unwrap();
        assert!(!text.contains("SECRET"));
        assert!(!text.contains("TOKEN"));
        let back: SettingsBundle = toml::from_str(&text).unwrap();
        assert_eq!(back.accounts[0].email, "a@b.c");
        assert!(back.accounts[0].password.is_empty());
        // The later sections ride along, and a bundle without them (from
        // before they existed) still parses, leaving them unset.
        assert_eq!(back.cloud.as_ref().map(Vec::len), Some(1));
        assert_eq!(
            back.spell_words
                .as_ref()
                .and_then(|w| w.get("en_US"))
                .map(Vec::len),
            Some(1)
        );
        let old: SettingsBundle = toml::from_str("version = 1\n[privacy]\n").unwrap();
        assert!(old.cloud.is_none() && old.spell_words.is_none());
        // Auto-empty (#140): "never" is left out of the file, an age is kept.
        assert!(!text.contains("empty_junk_days"), "{text}");
        assert_eq!(back.accounts[0].empty_trash_days, 0);
        let mut aged = bundle;
        aged.accounts[0].empty_trash_days = 30;
        let text = toml::to_string_pretty(&aged).unwrap();
        assert!(text.contains("empty_trash_days = 30"), "{text}");
        let back: SettingsBundle = toml::from_str(&text).unwrap();
        assert_eq!(back.accounts[0].empty_trash_days, 30);
        assert_eq!(back.accounts[0].empty_junk_days, 0);
        // Left out of the unified section (#267): only "out" is written,
        // and a file without the key keeps the account in.
        assert!(!text.contains("in_unified"), "{text}");
        assert!(back.accounts[0].in_unified);
        let mut apart = aged;
        apart.accounts[0].in_unified = false;
        let text = toml::to_string_pretty(&apart).unwrap();
        assert!(text.contains("in_unified = false"), "{text}");
        let back: SettingsBundle = toml::from_str(&text).unwrap();
        assert!(!back.accounts[0].in_unified);
        // Signing by default (#267) is written only when on.
        assert!(!text.contains("sign_by_default"), "{text}");
        let mut signing = apart;
        signing.accounts[0].sign_by_default = true;
        let text = toml::to_string_pretty(&signing).unwrap();
        assert!(text.contains("sign_by_default = true"), "{text}");
        let back: SettingsBundle = toml::from_str(&text).unwrap();
        assert!(back.accounts[0].sign_by_default);
        // Hiding the subject of encrypted mail (#493) is the default, so
        // only turning it off is written.
        assert!(!text.contains("plain_subject"), "{text}");
        let mut plain = signing;
        plain.accounts[0].plain_subject = true;
        let text = toml::to_string_pretty(&plain).unwrap();
        assert!(text.contains("plain_subject = true"), "{text}");
        let back: SettingsBundle = toml::from_str(&text).unwrap();
        assert!(back.accounts[0].plain_subject);
    }

    #[test]
    fn filter_names_are_optional_and_round_trip() {
        // A rule with no name is written without the key, so a version from
        // before #197 reads the file back unchanged.
        let plain = rule(FilterField::Subject, FilterMatch::Contains, "digest");
        assert_eq!(plain.name, "");
        let text = toml::to_string_pretty(&FiltersFile {
            rules: vec![plain.clone()],
        })
        .unwrap();
        assert!(!text.contains("name"), "{text}");
        // A named one keeps its name across a round trip, and an unknown
        // `name` key in a file from a newer version is simply carried.
        let mut named = plain.clone();
        named.name = "Mailing lists".into();
        let text = toml::to_string_pretty(&FiltersFile {
            rules: vec![named.clone()],
        })
        .unwrap();
        let back: FiltersFile = toml::from_str(&text).unwrap();
        assert_eq!(back.rules, vec![named.clone()]);
        // The label falls back to the conditions when there is no name.
        assert_eq!(named.label(), "Mailing lists");
        assert!(plain.label().contains("digest"));
    }

    #[test]
    fn filter_rules_roundtrip_through_toml() {
        let mut multi = rule(FilterField::Body, FilterMatch::Contains, "unsubscribe");
        multi.more.push(cond(
            FilterField::ReplyTo,
            FilterMatch::EndsWith,
            "@list.example",
        ));
        multi.any = true;
        let rules = vec![
            rule(FilterField::Subject, FilterMatch::EndsWith, "digest"),
            multi,
        ];
        let text = toml::to_string_pretty(&FiltersFile {
            rules: rules.clone(),
        })
        .unwrap();
        let back: FiltersFile = toml::from_str(&text).unwrap();
        assert_eq!(back.rules, rules);
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
