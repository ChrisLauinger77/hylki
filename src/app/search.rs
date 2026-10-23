//! The search pool: every indexed folder's mail merged into one list for a
//! search across folders and accounts, one copy of each mail.

use std::collections::HashMap;

use super::conversation::same_mail;
use crate::models::{FolderKind, Message};
use crate::ui::message_list::SearchScope;

/// Flatten every folder's indexed messages into one pool for cross-folder search.
///
/// Gmail files one message under every label it carries, so an archived mail
/// sits in All Mail and its labels, and an inbox one in INBOX and All Mail
/// too (#472). Each mail is kept once, from the folder `rank` puts first.
pub(super) fn build_search_pool<'a>(
    lists: impl IntoIterator<Item = &'a Vec<Message>>,
    rank: impl Fn(&Message) -> u8,
) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    let mut copies: HashMap<(u32, &str), Vec<usize>> = HashMap::new();
    for m in lists.into_iter().flatten() {
        if m.message_id.is_empty() {
            out.push(m.clone());
            continue;
        }
        let slots = copies
            .entry((m.account_id, m.message_id.as_str()))
            .or_default();
        match slots.iter().copied().find(|&i| same_mail(&out[i], m)) {
            Some(i) => {
                if rank(m) < rank(&out[i]) {
                    out[i] = m.clone();
                }
            }
            None => {
                slots.push(out.len());
                out.push(m.clone());
            }
        }
    }
    out
}

/// Which copy of a mail filed in several folders a search shows, lowest
/// first: the inbox's, then a folder of its own, then the folders that
/// gather everything (Gmail's All Mail and Starred), and Trash and Junk last,
/// which "All folders" leaves out.
pub(super) fn pool_rank(kind: Option<FolderKind>) -> u8 {
    match kind {
        Some(FolderKind::Inbox) => 0,
        Some(FolderKind::Archive | FolderKind::Starred) => 2,
        Some(FolderKind::Trash | FolderKind::Junk) => 3,
        _ => 1,
    }
}

/// Where a search starts in a view just opened (#472): across the mailbox
/// from an Inbox, and in the folder or view itself from anywhere else, as a
/// search of Trash or Junk most often means.
pub(super) fn default_scope(inbox: bool) -> SearchScope {
    if inbox {
        SearchScope::AllFolders
    } else {
        SearchScope::ThisFolder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    #[test]
    fn search_pool_spans_every_folder_and_account() {
        let mut cache: HashMap<(u32, u32), Vec<Message>> = HashMap::new();
        cache.insert((1, 10), vec![msg(1, 10, 1), msg(1, 10, 2)]); // acct 1, inbox
        cache.insert((1, 11), vec![msg(1, 11, 3)]); // acct 1, archive
        cache.insert((2, 20), vec![msg(2, 20, 4), msg(2, 20, 5)]); // acct 2, inbox

        let pool = build_search_pool(cache.values(), |_| 0);
        assert_eq!(pool.len(), 5, "pool must include every folder's messages");

        let folders: std::collections::HashSet<(u32, u32)> =
            pool.iter().map(|m| (m.account_id, m.folder_id)).collect();
        assert_eq!(folders.len(), 3, "pool must span all three folders");
    }

    #[test]
    fn search_pool_is_empty_when_nothing_indexed() {
        assert!(
            build_search_pool(HashMap::<(u32, u32), Vec<Message>>::new().values(), |_| 0)
                .is_empty()
        );
    }

    /// Gmail's label copies of one mail are one result, from the folder that
    /// says most about it (#472).
    #[test]
    fn search_pool_keeps_one_copy_of_a_labelled_mail() {
        let copy = |folder_id: u32, uid: u32, mid: &str| {
            let mut m = msg(1, folder_id, uid);
            m.message_id = mid.into();
            m.from_addr = "ada@x".into();
            m.timestamp = 500;
            m
        };
        // INBOX = 1, All Mail = 2, a label = 3.
        let inbox = vec![copy(1, 10, "in@x")];
        let all_mail = vec![
            copy(2, 20, "in@x"),
            copy(2, 21, "archived@x"),
            copy(2, 22, "only@x"),
        ];
        let label = vec![copy(3, 30, "archived@x")];
        let rank = |m: &Message| match m.folder_id {
            1 => pool_rank(Some(FolderKind::Inbox)),
            2 => pool_rank(Some(FolderKind::Archive)),
            _ => pool_rank(Some(FolderKind::Custom)),
        };
        let mut pool = build_search_pool([&all_mail, &label, &inbox], rank);
        pool.sort_by_key(|m| m.uid);
        let kept: Vec<(u32, u32)> = pool.iter().map(|m| (m.folder_id, m.uid)).collect();
        assert_eq!(kept, vec![(1, 10), (2, 22), (3, 30)]);

        // The same Message-ID from someone else is another mail.
        let mut other = copy(3, 31, "only@x");
        other.from_addr = "spam@x".into();
        assert_eq!(build_search_pool([&all_mail, &vec![other]], rank).len(), 4);
    }

    #[test]
    fn a_search_from_the_inbox_spans_all_folders() {
        assert_eq!(default_scope(true), SearchScope::AllFolders);
        assert_eq!(default_scope(false), SearchScope::ThisFolder);
    }
}
