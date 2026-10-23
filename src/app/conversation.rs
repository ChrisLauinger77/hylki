//! Assembling a conversation for the reader: which members to show, which
//! bodies to fetch, and how worker answers find the message they are about.

use super::*;

/// Whether two rows are the same mail stored twice, rather than two messages.
///
/// Message-ID is the identity, but it is written by whoever sent the mail and
/// spam reuses one across unrelated messages — so sender and timestamp have to
/// agree as well before one copy is allowed to stand for another. Gmail's label
/// copies agree on all three. Subject is deliberately left out: a re-decoded
/// encoded-word can rewrite it under one label and not another.
pub(super) fn same_mail(a: &Message, b: &Message) -> bool {
    !a.message_id.is_empty()
        && a.message_id == b.message_id
        && a.from_addr == b.from_addr
        && a.timestamp == b.timestamp
}

/// Keep one copy of each message, dropping the rest of its labels.
///
/// Gmail exposes labels as IMAP folders, so a single message sits in INBOX,
/// All Mail and Important at once — three folder/UID pairs carrying one
/// Message-ID. A conversation that dedupes on those pairs alone therefore shows
/// every copy: a six-message thread renders as eighteen. On a real cache 1210 of
/// this account's 1212 messages are labelled that way, so this is the normal
/// case for Gmail, not an edge one.
///
/// The copy from the folder the reader is showing wins, so the "from folder X"
/// label and the actions on a message refer to the mail actually on screen. A
/// message with no Message-ID can't be matched this way and is left alone.
pub(super) fn dedupe_label_copies(
    messages: Vec<Message>,
    conv: &[Message],
    shown_folder: u32,
) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    for r in messages {
        if r.message_id.is_empty() {
            out.push(r);
            continue;
        }
        if conv.iter().any(|m| same_mail(m, &r)) {
            continue; // the conversation already holds this mail under some label
        }
        match out.iter().position(|m| same_mail(m, &r)) {
            Some(i) => {
                if out[i].folder_id != shown_folder && r.folder_id == shown_folder {
                    out[i] = r;
                }
            }
            None => out.push(r),
        }
    }
    out
}

/// A conversation member still waiting for its body:
/// `(account_id, message_id, uid, folder_path)`.
pub(super) type MissingBody = (u32, u32, u32, String);

/// One `LoadBodies` request: the `(account_id, folder_path)` it goes to, and the
/// `(message_id, uid)` of every member it covers.
type BodyBatch = ((u32, String), Vec<(u32, u32)>);

/// Group a conversation's missing bodies into one request per (account, folder).
///
/// Every member asked for on its own is a round trip each, and each arrival
/// re-renders the reader; `LoadBodies` fetches a whole UID set at once. Members
/// are kept in the order given — the primary is first, so it is first in its
/// batch and renders first — and a conversation that spans folders (or, in the
/// unified inbox, accounts) splits into one batch per folder rather than losing
/// the members that don't fit the first one.
pub(super) fn batch_bodies_by_folder(to_load: Vec<MissingBody>) -> Vec<BodyBatch> {
    let mut batches: Vec<BodyBatch> = Vec::new();
    for (account_id, message_id, uid, path) in to_load {
        match batches
            .iter_mut()
            .find(|((a, p), _)| *a == account_id && *p == path)
        {
            Some((_, items)) => items.push((message_id, uid)),
            None => batches.push(((account_id, path), vec![(message_id, uid)])),
        }
    }
    batches
}

/// Where to move the reader when its message disappeared from a sync (deleted or
/// moved on another device): whatever now sits in the slot it occupied.
///
/// `None` when that slot cannot be worked out, which is not the same as "start at
/// the top". This used to fall back to the folder's first message, and since
/// deleting prunes the message from the cache before the sync arrives, the
/// lookup fails on exactly that path — so a delete could throw the selection to
/// the top of the folder and scroll the list with it (#19).
pub(super) fn next_after_vanish(
    previous: Option<&Vec<Message>>,
    messages: &[Message],
    cur_uid: u32,
) -> Option<(u32, u32)> {
    let idx = previous?.iter().position(|m| m.uid == cur_uid)?;
    messages
        .get(idx.min(messages.len().checked_sub(1)?))
        .map(|m| (m.account_id, m.id))
}

/// Where a body is kept in the app's RAM cache: (account, folder, UID).
pub(super) type BodyKey = (u32, u32, u32);

/// A message's [`BodyKey`]. The folder and UID are the message's real ones
/// even where its id is not (a member opened out from another folder), so
/// every copy of a message is found where its own body was filed.
pub(super) fn body_key(m: &Message) -> BodyKey {
    (m.account_id, m.folder_id, m.uid)
}

/// Whether a worker event about `message_id` (whose [`AppModel::event_key`]
/// is `key`) is about `m`. Without a key, for a folder the app does not
/// know, the id alone has to do.
pub(super) fn is_event_target(
    m: &Message,
    key: Option<BodyKey>,
    account_id: u32,
    message_id: u32,
) -> bool {
    match key {
        Some(k) => body_key(m) == k,
        None => m.account_id == account_id && m.id == message_id,
    }
}

/// How long a conversation waits for its outstanding bodies before painting what
/// it has. Long enough that a cached conversation — the common case — always
/// arrives whole, short enough that one unreachable message can't hold the
/// reader hostage.
pub(super) const THREAD_BODY_WAIT: std::time::Duration = std::time::Duration::from_millis(1200);

/// How many messages' reply headers are held for joining conversations across
/// folders. Only a bound on memory and rebuild cost — never on correctness: a
/// conversation whose members are all on screen groups without consulting these
/// at all. They matter for the join that runs through a message that isn't
/// shown, most often your own reply in Sent. The newest are kept, since that is
/// where the mail being read lives.
pub(super) const THREAD_LINK_LIMIT: usize = 20_000;

/// How many carried-over bodies to hold before giving up on them. They are
/// meant to be claimed by the very next reload; a build-up means the reloads
/// never came, and these are whole message bodies.
pub(super) const CARRIED_BODY_LIMIT: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{labelled, msg};

    #[test]
    fn one_gmail_message_under_three_labels_joins_the_conversation_once() {
        // INBOX = 1, All Mail = 2, Important = 3 — the same mail in all three,
        // which is how 1210 of one real account's 1212 messages are stored.
        let related = vec![
            labelled(2, 500, "<a@example.com>"),
            labelled(3, 900, "<a@example.com>"),
            labelled(1, 42, "<a@example.com>"),
            labelled(2, 501, "<b@example.com>"),
        ];
        let kept = dedupe_label_copies(related, &[], 1);
        assert_eq!(kept.len(), 2, "two messages, not four copies");
        let a = kept
            .iter()
            .find(|m| m.message_id == "<a@example.com>")
            .unwrap();
        assert_eq!(
            (a.folder_id, a.uid),
            (1, 42),
            "the copy from the folder on screen wins, whatever order it arrived in"
        );
    }

    #[test]
    fn a_label_copy_of_a_message_already_shown_is_dropped() {
        let conv = vec![labelled(1, 42, "<a@example.com>")];
        let kept = dedupe_label_copies(vec![labelled(2, 500, "<a@example.com>")], &conv, 1);
        assert!(kept.is_empty(), "the conversation already holds this mail");
    }

    #[test]
    fn a_reused_message_id_is_not_treated_as_the_same_mail() {
        let mut spam = labelled(2, 900, "<a@example.com>");
        spam.from_addr = "spammer@fake.example".into();
        let mut mine = labelled(1, 42, "<a@example.com>");
        mine.from_addr = "me@real.example".into();

        let kept = dedupe_label_copies(vec![mine, spam], &[], 1);
        assert_eq!(
            kept.len(),
            2,
            "a shared id from another sender is other mail"
        );
    }

    #[test]
    fn messages_without_a_message_id_are_never_merged_together() {
        // Two distinct messages that carry no Message-ID must stay two: there is
        // nothing to match them on, and folding them would lose one.
        let kept = dedupe_label_copies(vec![labelled(1, 7, ""), labelled(1, 8, "")], &[], 1);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn a_conversation_in_one_folder_becomes_a_single_fetch() {
        let batches = batch_bodies_by_folder(vec![
            (1, 10, 100, "INBOX".into()),
            (1, 11, 101, "INBOX".into()),
            (1, 12, 102, "INBOX".into()),
        ]);
        assert_eq!(batches.len(), 1, "one folder is one round trip");
        assert_eq!(batches[0].0, (1, "INBOX".to_string()));
        // Order is the thread's, so the primary is fetched and rendered first.
        assert_eq!(batches[0].1, vec![(10, 100), (11, 101), (12, 102)]);
    }

    #[test]
    fn members_from_other_folders_and_accounts_get_their_own_fetch() {
        // A conversation pulled together across Inbox and Sent, plus a second
        // account: batching must not drop the members that don't share the
        // first one's folder.
        let batches = batch_bodies_by_folder(vec![
            (1, 10, 100, "INBOX".into()),
            (1, 11, 55, "Sent".into()),
            (1, 12, 101, "INBOX".into()),
            (2, 13, 7, "INBOX".into()),
        ]);
        assert_eq!(batches.len(), 3);
        assert_eq!(
            batches[0],
            ((1, "INBOX".to_string()), vec![(10, 100), (12, 101)])
        );
        assert_eq!(batches[1], ((1, "Sent".to_string()), vec![(11, 55)]));
        assert_eq!(batches[2], ((2, "INBOX".to_string()), vec![(13, 7)]));
        let members: usize = batches.iter().map(|(_, items)| items.len()).sum();
        assert_eq!(members, 4, "every member is still requested");
    }

    #[test]
    fn a_vanished_message_never_throws_you_to_the_top() {
        let m = |uid| msg(1, 1, uid);
        let previous = vec![m(10), m(9), m(8), m(7)];
        let after = vec![m(10), m(9), m(7)];
        // The message that vanished sat third; whatever is third now takes over.
        assert_eq!(next_after_vanish(Some(&previous), &after, 8), Some((1, 7)));
        // It sat last: the new last row.
        let after = vec![m(10), m(9)];
        assert_eq!(next_after_vanish(Some(&previous), &after, 7), Some((1, 9)));
        // No slot to go by — deleting prunes the cache, so this is the delete
        // path — and the top of the folder is NOT the answer (#19).
        assert_eq!(next_after_vanish(Some(&previous), &after, 999), None);
        assert_eq!(next_after_vanish(None, &after, 8), None);
        // Nothing left at all.
        assert_eq!(next_after_vanish(Some(&previous), &[], 8), None);
    }

    #[test]
    fn same_mail_needs_id_sender_and_time() {
        let mut a = labelled(1, 1, "<a@x>");
        a.from_addr = "s@x".into();
        a.timestamp = 5;
        let mut b = a.clone();
        b.folder_id = 2;
        b.subject = "other".into();
        assert!(same_mail(&a, &b));
        b.timestamp = 6;
        assert!(!same_mail(&a, &b));
        let mut c = a.clone();
        c.message_id = String::new();
        assert!(!same_mail(&c, &c.clone()));
    }

    #[test]
    fn events_match_by_body_key_or_by_id() {
        let mut m = msg(2, 3, 40);
        m.id = 9;
        assert_eq!(body_key(&m), (2, 3, 40));
        assert!(is_event_target(&m, Some((2, 3, 40)), 7, 7));
        assert!(!is_event_target(&m, Some((2, 3, 41)), 2, 9));
        assert!(is_event_target(&m, None, 2, 9));
        assert!(!is_event_target(&m, None, 2, 40));
    }
}
