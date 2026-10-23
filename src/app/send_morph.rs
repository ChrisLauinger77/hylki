//! A reply sent from the split pane turning into its card in the
//! conversation, and the card standing in until the Sent copy arrives.

use super::*;

/// A reply on its way from the split pane into its conversation: its card,
/// the composer it was written in, and the conversation it answers.
pub(super) struct MorphPending {
    pub(super) message: Message,
    pub(super) composer: gtk::Widget,
    pub(super) conversation: (u32, u32),
}

/// The card a reply sent from the split pane has in its conversation until
/// its copy is read back from Sent (send_morph.rs).
pub(super) struct Provisional {
    pub(super) message: Message,
    /// The Message-ID it answers.
    pub(super) answers: String,
}

impl Provisional {
    /// Whether `m` is this reply's own copy: from the same account, dated
    /// when this was sent (by this machine's clock either way), and
    /// answering the same message.
    pub(super) fn copied_by(&self, m: &Message) -> bool {
        m.uid != 0
            && m.account_id == self.message.account_id
            && m.timestamp >= self.message.timestamp - 30
            && m.references.split_whitespace().any(|x| x == self.answers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    /// B2 in the map: an empty `answers` never matches, since an empty
    /// reference is never in a whitespace split.
    #[test]
    fn a_provisional_card_is_copied_by_its_sent_copy() {
        let mut sent = msg(1, 5, 0);
        sent.timestamp = 1000;
        let p = Provisional {
            message: sent.clone(),
            answers: "parent@x".into(),
        };
        let mut copy = msg(1, 9, 77);
        copy.timestamp = 970;
        copy.references = "root@x parent@x".into();
        assert!(p.copied_by(&copy));
        copy.timestamp = 969;
        assert!(!p.copied_by(&copy));
        copy.timestamp = 2000;
        copy.uid = 0;
        assert!(!p.copied_by(&copy));
        copy.uid = 77;
        copy.account_id = 2;
        assert!(!p.copied_by(&copy));
        let empty = Provisional {
            message: sent,
            answers: String::new(),
        };
        copy.account_id = 1;
        assert!(!empty.copied_by(&copy));
    }
}
