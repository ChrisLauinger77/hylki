//! Folder roles, names and order, shared by every backend. Folder ids are
//! positional (1..n in [`folder_order`] then by lowercased path), so the
//! order here is persisted.

use async_imap::types::NameAttribute;

use crate::i18n::i18n;
use crate::models::{Folder, FolderKind, Message};

/// Whether a folder's sidebar chip counts every message rather than unread
/// ones. Drafts: a draft is never unread, and the chip is there to say how
/// many are waiting to be finished.
pub fn chip_counts_all(kind: FolderKind) -> bool {
    kind == FolderKind::Drafts
}

/// The chip number for a folder whose messages are all in hand.
pub(super) fn chip_count_of(kind: FolderKind, messages: &[Message]) -> u32 {
    if chip_counts_all(kind) {
        messages.len() as u32
    } else {
        messages.iter().filter(|m| m.unread).count() as u32
    }
}

/// Classify a folder, also reporting whether the kind came from an RFC 6154
/// SPECIAL-USE attribute (`true`) rather than name matching (`false`). This lets
/// `list_folders` prefer the real special-use folder when a server also exposes a
/// stray folder that merely *looks* like it fills the same role — e.g. Gmail's
/// real `[Gmail]/Trash` (\Trash) next to a plain top-level `Trash` label.
pub(super) fn classify_with_source(
    path: &str,
    delimiter: Option<&str>,
    attrs: &[NameAttribute],
) -> (FolderKind, bool) {
    // Prefer RFC 6154 SPECIAL-USE attributes; fall back to name matching.
    for a in attrs {
        match a {
            NameAttribute::Sent => return (FolderKind::Sent, true),
            NameAttribute::Drafts => return (FolderKind::Drafts, true),
            NameAttribute::Trash => return (FolderKind::Trash, true),
            NameAttribute::Junk => return (FolderKind::Junk, true),
            NameAttribute::Archive => return (FolderKind::Archive, true),
            NameAttribute::Flagged => return (FolderKind::Starred, true),
            _ => {}
        }
    }

    // INBOX is the one name RFC 3501 fixes, and only as the whole path: an
    // `Archiv/2020/Inbox` is an ordinary folder (#344).
    if path.eq_ignore_ascii_case("INBOX") {
        return (FolderKind::Inbox, false);
    }
    // The other roles go by name only where a server keeps them: at the top,
    // or one level down under INBOX (Dovecot, Courier) or a bracketed
    // container ([Gmail]). A `Sent` deeper in an archive is the user's own.
    let parts: Vec<&str> = match delimiter {
        Some(d) if !d.is_empty() => path.split(d).collect(),
        _ => path.split(['/', '.']).collect(),
    };
    let placed = match parts.as_slice() {
        [_] => true,
        [parent, _] => parent.eq_ignore_ascii_case("INBOX") || parent.starts_with('['),
        _ => false,
    };
    if !placed {
        return (FolderKind::Custom, false);
    }
    let leaf = parts.last().copied().unwrap_or(path).to_lowercase();
    let kind = match leaf.as_str() {
        "sent" | "sent items" | "sent mail" => FolderKind::Sent,
        "drafts" => FolderKind::Drafts,
        name if crate::models::is_templates_name(name) => FolderKind::Templates,
        "trash" | "deleted" | "deleted items" | "bin" => FolderKind::Trash,
        "junk" | "spam" => FolderKind::Junk,
        "archive" | "all mail" => FolderKind::Archive,
        "starred" | "flagged" => FolderKind::Starred,
        _ => FolderKind::Custom,
    };
    (kind, false)
}

pub(crate) fn folder_order(kind: FolderKind) -> u8 {
    match kind {
        FolderKind::Inbox => 0,
        FolderKind::Starred => 1,
        FolderKind::Drafts => 2,
        FolderKind::Templates => 3,
        FolderKind::Sent => 4,
        FolderKind::Archive => 5,
        FolderKind::Junk => 6,
        FolderKind::Trash => 7,
        FolderKind::Custom => 8,
    }
}

/// Show only the leaf segment of a hierarchical mailbox path, with INBOX
/// special-cased.
pub(super) fn display_name(path: &str, delimiter: Option<&str>) -> String {
    if path.eq_ignore_ascii_case("inbox") {
        return i18n("Inbox");
    }
    let leaf = match delimiter {
        Some(d) if !d.is_empty() => path.rsplit(d).next().unwrap_or(path),
        _ => path.rsplit(['/', '.']).next().unwrap_or(path),
    };
    // Split first, decode second: the delimiter is ASCII and modified UTF-7
    // never produces one inside an escaped run (that is why `,` stands in for
    // `/`), so the leaf is always a whole name (issue #1).
    crate::mutf7::decode(leaf)
}

/// Put a folder listing in the order Hylki keeps and number it 1..n:
/// the essential folders first in [`folder_order`], the custom ones by
/// their full path (case-insensitively), so sub-folders sit under their
/// parents whatever order the server listed them in. Folder ids are
/// positional and stored, so this order must not change.
pub(super) fn number_folders<T>(
    items: &mut [T],
    folder: impl Fn(&T) -> &Folder,
    folder_mut: impl Fn(&mut T) -> &mut Folder,
) {
    items.sort_by(|a, b| {
        let (a, b) = (folder(a), folder(b));
        folder_order(a.kind)
            .cmp(&folder_order(b.kind))
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
    for (i, f) in items.iter_mut().enumerate() {
        folder_mut(f).id = i as u32 + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &str, kind: FolderKind) -> Folder {
        Folder {
            id: 0,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        }
    }

    /// The ids are stored: roles first in their fixed order, then the
    /// rest by lowercased path, numbered from 1.
    #[test]
    fn folders_are_numbered_roles_first_then_by_path() {
        let mut list = vec![
            folder("Work/b", FolderKind::Custom),
            folder("Trash", FolderKind::Trash),
            folder("archive2", FolderKind::Custom),
            folder("INBOX", FolderKind::Inbox),
            folder("Work", FolderKind::Custom),
            folder("Sent", FolderKind::Sent),
            folder("Drafts", FolderKind::Drafts),
            folder("Work/A", FolderKind::Custom),
        ];
        number_folders(&mut list, |f| f, |f| f);
        let got: Vec<(u32, &str)> = list.iter().map(|f| (f.id, f.path.as_str())).collect();
        assert_eq!(
            got,
            [
                (1, "INBOX"),
                (2, "Drafts"),
                (3, "Sent"),
                (4, "Trash"),
                (5, "archive2"),
                (6, "Work"),
                (7, "Work/A"),
                (8, "Work/b"),
            ]
        );
    }
}
