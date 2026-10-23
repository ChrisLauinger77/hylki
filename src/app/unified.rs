//! The unified views (All Inboxes, Starred, Favorites ...): one list merged
//! from many folders, rebuilt only when what it holds changed.

use super::*;

/// A unified view, and the account it is narrowed to, as one number: the
/// key the message list keeps it under.
pub(super) fn view_key_of(view: UnifiedView, account: Option<u32>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{view:?}{account:?}").hash(&mut h);
    h.finish()
}

/// What a list holds, as one number: every message's place, state and
/// tags, whatever order they come in. A few milliseconds for tens of
/// thousands of messages, against a merge and a regrouping many times that.
pub(super) fn content_fp<'a>(msgs: impl Iterator<Item = &'a Message>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut sum = 0u64;
    let mut n = 0u64;
    for m in msgs {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (m.account_id, m.folder_id, m.uid, m.id, m.timestamp).hash(&mut h);
        (m.unread, m.starred, m.answered, m.forwarded).hash(&mut h);
        (
            &m.keywords,
            &m.labels,
            m.subject.len(),
            m.preview.len(),
            m.from_addr.len(),
        )
            .hash(&mut h);
        (m.message_id.len(), m.references.len()).hash(&mut h);
        sum = sum.wrapping_add(h.finish());
        n += 1;
    }
    sum ^ n.rotate_left(32)
}

/// How many of a folder's newest messages a list not in memory shows at
/// once, read from the index while the rest of the folder is.
pub(super) const LIST_NEWEST: usize = 300;

/// How long after an account's folders arrive the unified rows' folders are
/// read into memory: past the start's own rush of work.
pub(super) const PRELOAD_AFTER_SECS: u32 = 5;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    #[test]
    fn view_keys_differ_by_view_and_account() {
        let inbox = UnifiedView::Kind(FolderKind::Inbox);
        assert_eq!(view_key_of(inbox, None), view_key_of(inbox, None));
        assert_ne!(view_key_of(inbox, None), view_key_of(inbox, Some(1)));
        assert_ne!(
            view_key_of(inbox, None),
            view_key_of(UnifiedView::Favorites, None)
        );
    }

    /// The fingerprint ignores order, and (B29) only hashes the lengths of
    /// the text fields: an equal-length edit leaves it unchanged.
    #[test]
    fn content_fingerprint_ignores_order_and_same_length_text() {
        let a = msg(1, 1, 1);
        let mut b = msg(1, 1, 2);
        b.subject = "abc".into();
        let fp = content_fp([&a, &b].into_iter());
        assert_eq!(fp, content_fp([&b, &a].into_iter()));
        let mut b2 = b.clone();
        b2.subject = "xyz".into();
        assert_eq!(fp, content_fp([&a, &b2].into_iter()));
        let mut b3 = b.clone();
        b3.unread = true;
        assert_ne!(fp, content_fp([&a, &b3].into_iter()));
        assert_ne!(content_fp([&a].into_iter()), content_fp(std::iter::empty()));
    }
}
