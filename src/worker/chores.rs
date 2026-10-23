//! Scheduled housekeeping shared by the IMAP, Graph and JMAP paths: which
//! folders the auto-empty (#140) and list clean-up (#407) touch, when they
//! are due, and their cut-off dates.

use std::time::Duration;

use crate::config::AccountConfig;
use crate::models::{Folder, FolderKind};

/// How often an account's Junk / Trash auto-empty (#140) may run: it rides
/// the unread sweep, but a server needs asking only a few times a day.
pub(super) const AUTO_EMPTY_MIN: Duration = Duration::from_secs(6 * 60 * 60);

/// Auto-empty (#140): the roles the account wants emptied, with their ages.
pub(super) fn auto_empty_roles(account: &AccountConfig) -> Vec<(FolderKind, &'static str, u32)> {
    [
        (FolderKind::Junk, "junk", account.empty_junk_days),
        (FolderKind::Trash, "trash", account.empty_trash_days),
    ]
    .into_iter()
    .filter(|(_, _, days)| *days > 0)
    .collect()
}

/// The folder holding a role for this account: the manual assignment (#82)
/// when there is one, else the folder the listing detected.
pub(super) fn role_folder_path(
    account: &AccountConfig,
    folders: &[Folder],
    kind: FolderKind,
    role: &str,
) -> Option<String> {
    if let Some(p) = account.folder_roles.get(role) {
        if !p.is_empty() {
            return Some(p.clone());
        }
    }
    folders
        .iter()
        .find(|f| f.kind == kind)
        .map(|f| f.path.clone())
}

/// Whether the auto-empty is due: something is configured, and the last run
/// on this connection is older than [`AUTO_EMPTY_MIN`] (or never happened).
pub(super) fn auto_empty_due(
    account: &AccountConfig,
    last: &mut Option<std::time::Instant>,
) -> bool {
    if auto_empty_roles(account).is_empty() {
        return false;
    }
    if last.is_some_and(|t| t.elapsed() < AUTO_EMPTY_MIN) {
        return false;
    }
    *last = Some(std::time::Instant::now());
    true
}

/// The cut-off, `days` ago, in the form IMAP SEARCH wants ("1-Aug-2026").
/// SEARCH BEFORE compares internal dates by day, in the server's clock.
pub(super) fn imap_before_date(days: u32) -> String {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
    cutoff.format("%-d-%b-%Y").to_string()
}

/// The same cut-off as an RFC 3339 instant for Graph's `$filter`.
pub(super) fn graph_before_date(days: u32) -> String {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
    cutoff.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Whether moving old list mail (#407) is due, on the same terms as
/// [`auto_empty_due`]: set for the account, and not run in the last
/// [`AUTO_EMPTY_MIN`] on this connection.
pub(super) fn clean_lists_due(
    account: &AccountConfig,
    last: &mut Option<std::time::Instant>,
) -> bool {
    if account.clean_lists_days == 0 || last.is_some_and(|t| t.elapsed() < AUTO_EMPTY_MIN) {
        return false;
    }
    *last = Some(std::time::Instant::now());
    true
}

/// The candidates for the list clean-up (#407), as an IMAP search: older
/// than the cut-off and not starred. Which of them are list mail is read
/// from their headers: a `HEADER List-Unsubscribe ""` search would be one
/// round trip, but not every server answers it (Stalwart searches only the
/// headers it indexes, and finds nothing).
pub(super) fn clean_lists_query(days: u32) -> String {
    format!("UNFLAGGED BEFORE {}", imap_before_date(days))
}
