//! Folder lists: special-folder roles, where sent copies go, unread counts
//! across a re-listing, and keys that follow a renamed folder.

use super::*;

/// Apply an account's manual special-folder assignments (#82) over the
/// auto-detected kinds (see [`crate::models::assign_folder_roles`]), then
/// re-sort into the fixed role order. Folder ids are untouched — they are
/// referenced from cached messages. The worker already applies the same
/// assignments to every listing it sends; this keeps the live lists right
/// between a Settings save and the reconnect's fresh LIST.
pub(super) fn apply_folder_roles(
    roles: &std::collections::BTreeMap<String, String>,
    folders: &mut Vec<Folder>,
) {
    if roles.is_empty() {
        return;
    }
    crate::models::assign_folder_roles(roles, folders);
    folders.sort_by(|a, b| {
        crate::worker::folder_order(a.kind)
            .cmp(&crate::worker::folder_order(b.kind))
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
    });
}

/// The folder a sent copy is filed in (#199): the chosen one when the account
/// still has it, else the Sent folder, else nowhere. A choice whose folder was
/// renamed or removed server-side falls back rather than appending into a
/// mailbox the server would have to create under an old name.
pub(super) fn pick_sent_copy(chosen: Option<&str>, folders: &[Folder]) -> Option<String> {
    chosen
        .filter(|path| folders.iter().any(|f| f.path == *path))
        .map(str::to_string)
        .or_else(|| {
            folders
                .iter()
                .find(|f| f.kind == FolderKind::Sent)
                .map(|f| f.path.clone())
        })
}

/// Unread counts for a fresh folder listing of `account_id`, as
/// (folder id, count). Merged by PATH, and a zero never overwrites a known
/// count: a refresh's per-folder STATUS can fail silently (Gmail right after
/// a RENAME answers zeros), and adopting them wholesale wiped every unread
/// chip. A genuine zero re-asserts through the per-folder sync events.
/// `known` is the app's own count per (account, folder id), which wins over
/// what the old listing said.
pub(super) fn merged_unread(
    account_id: u32,
    old: Option<&[Folder]>,
    known: &HashMap<(u32, u32), u32>,
    folders: &[Folder],
) -> Vec<(u32, u32)> {
    let prev_by_path: HashMap<&str, u32> = old
        .map(|old| {
            old.iter()
                .map(|f| {
                    let n = known.get(&(account_id, f.id)).copied().unwrap_or(f.unread);
                    (f.path.as_str(), n)
                })
                .collect()
        })
        .unwrap_or_default();
    folders
        .iter()
        .map(|f| {
            let known = prev_by_path.get(f.path.as_str()).copied().unwrap_or(0);
            (f.id, if f.unread > 0 { f.unread } else { known })
        })
        .collect()
}

/// Whether two listings show the same folders: same ids, paths, kinds and
/// names, in the same order. Unread counts do not count.
pub(super) fn same_folder_list(old: &[Folder], new: &[Folder]) -> bool {
    old.len() == new.len()
        && old
            .iter()
            .zip(new)
            .all(|(a, b)| a.id == b.id && a.path == b.path && a.kind == b.kind && a.name == b.name)
}

/// A sidebar key ("email\tpath", `key_prefix` being "email\t") after the
/// folder `path` became `new_path`: the folder's own key and its children's
/// follow it, any other key is left alone.
pub(super) fn rekey_folder_key(
    key: &mut String,
    key_prefix: &str,
    path: &str,
    new_path: &str,
    delim: char,
) {
    let Some(rest) = key.strip_prefix(key_prefix) else {
        return;
    };
    if rest == path {
        *key = format!("{key_prefix}{new_path}");
    } else if let Some(r) = rest.strip_prefix(&format!("{path}{delim}")) {
        *key = format!("{key_prefix}{new_path}{delim}{r}");
    }
}

pub(super) fn kind_label(kind: FolderKind) -> String {
    match kind {
        FolderKind::Archive => i18n("archive"),
        FolderKind::Trash => i18n("trash"),
        _ => i18n("destination"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: u32, path: &str, unread: u32) -> Folder {
        Folder {
            id,
            account_id: 1,
            name: path.into(),
            path: path.into(),
            kind: FolderKind::Custom,
            unread,
        }
    }

    #[test]
    fn folder_roles_override_detection_and_demote_the_old_holder() {
        use crate::models::{Folder, FolderKind};
        let f = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        let mut folders = vec![
            f(1, "INBOX", FolderKind::Inbox),
            f(2, "Sent Items", FolderKind::Custom),
            f(3, "Sent", FolderKind::Sent),
            f(4, "Rubbish", FolderKind::Custom),
        ];
        let roles: std::collections::BTreeMap<String, String> = [
            ("sent".to_string(), "Sent Items".to_string()),
            ("trash".to_string(), "Rubbish".to_string()),
            // A mapping whose folder no longer exists changes nothing.
            ("junk".to_string(), "Gone".to_string()),
        ]
        .into();
        super::apply_folder_roles(&roles, &mut folders);
        let kind_of = |path: &str| folders.iter().find(|f| f.path == path).unwrap().kind;
        assert_eq!(kind_of("Sent Items"), FolderKind::Sent);
        assert_eq!(kind_of("Sent"), FolderKind::Custom, "old holder demotes");
        assert_eq!(kind_of("Rubbish"), FolderKind::Trash);
        assert_eq!(kind_of("INBOX"), FolderKind::Inbox);
        // Ids survive the re-sort (cached messages reference them).
        assert_eq!(
            folders.iter().find(|f| f.path == "Sent Items").unwrap().id,
            2
        );
    }

    #[test]
    fn a_role_never_takes_the_inbox() {
        let mut folders = vec![
            Folder {
                id: 1,
                account_id: 1,
                name: "Inbox".into(),
                path: "INBOX".into(),
                kind: FolderKind::Inbox,
                unread: 0,
            },
            Folder {
                id: 2,
                account_id: 1,
                name: "Sent".into(),
                path: "Sent".into(),
                kind: FolderKind::Sent,
                unread: 0,
            },
        ];
        let mut roles = std::collections::BTreeMap::new();
        roles.insert("sent".to_string(), "INBOX".to_string());
        super::apply_folder_roles(&roles, &mut folders);
        assert_eq!(
            folders.iter().find(|f| f.path == "INBOX").unwrap().kind,
            FolderKind::Inbox
        );
        assert_eq!(
            folders.iter().find(|f| f.path == "Sent").unwrap().kind,
            FolderKind::Sent
        );
    }

    #[test]
    fn sent_copies_go_where_the_account_says_including_the_inbox() {
        let f = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        let folders = vec![
            f(1, "INBOX", FolderKind::Inbox),
            f(2, "Sent", FolderKind::Sent),
            f(3, "Archive", FolderKind::Archive),
        ];
        // Nothing chosen: the Sent folder, as before.
        assert_eq!(
            super::pick_sent_copy(None, &folders).as_deref(),
            Some("Sent")
        );
        // The Inbox is a legitimate choice (#199) — unlike a role (#136), a
        // destination takes nothing away from the folder it names.
        assert_eq!(
            super::pick_sent_copy(Some("INBOX"), &folders).as_deref(),
            Some("INBOX")
        );
        assert_eq!(
            super::pick_sent_copy(Some("Archive"), &folders).as_deref(),
            Some("Archive")
        );
        // A folder that has gone from the server falls back to Sent.
        assert_eq!(
            super::pick_sent_copy(Some("Gone"), &folders).as_deref(),
            Some("Sent")
        );
        // No Sent folder and no choice: the message goes out uncopied.
        let bare = vec![f(1, "INBOX", FolderKind::Inbox)];
        assert_eq!(super::pick_sent_copy(None, &bare), None);
        assert_eq!(super::pick_sent_copy(Some("Gone"), &bare), None);
        assert_eq!(
            super::pick_sent_copy(Some("INBOX"), &bare).as_deref(),
            Some("INBOX")
        );
    }

    #[test]
    fn unread_counts_merge_by_path_and_never_drop_to_zero() {
        let old = vec![folder(1, "INBOX", 5), folder(2, "A", 3), folder(3, "B", 0)];
        let mut known = HashMap::new();
        known.insert((1, 2), 7);
        known.insert((2, 1), 99);
        let new = vec![
            folder(1, "INBOX", 0),
            folder(2, "B", 0),
            folder(3, "A", 0),
            folder(4, "C", 2),
        ];
        assert_eq!(
            merged_unread(1, Some(&old), &known, &new),
            vec![(1, 5), (2, 0), (3, 7), (4, 2)]
        );
        assert_eq!(merged_unread(1, None, &known, &new[..1]), vec![(1, 0)]);
    }

    #[test]
    fn folder_lists_compare_without_their_counts() {
        let a = vec![folder(1, "INBOX", 5), folder(2, "A", 3)];
        let mut b = a.clone();
        b[1].unread = 0;
        assert!(same_folder_list(&a, &b));
        b[1].name = "a".into();
        assert!(!same_folder_list(&a, &b));
        assert!(!same_folder_list(&a, &a[..1]));
    }

    #[test]
    fn sidebar_keys_follow_a_renamed_folder_and_its_children() {
        let rekey = |k: &str, delim| {
            let mut k = k.to_string();
            rekey_folder_key(&mut k, "me@x\t", "A/B", "A/C", delim);
            k
        };
        assert_eq!(rekey("me@x\tA/B", '/'), "me@x\tA/C");
        assert_eq!(rekey("me@x\tA/B/D", '/'), "me@x\tA/C/D");
        assert_eq!(rekey("me@x\tA/BB", '/'), "me@x\tA/BB");
        assert_eq!(rekey("you@x\tA/B", '/'), "you@x\tA/B");
        assert_eq!(rekey("me@x\tA/B.D", '.'), "me@x\tA/C.D");
    }
}
