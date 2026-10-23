//! Smart folders (#417): saved searches over the index, shown as folders.

use super::*;

/// Most messages one smart folder (#417) lists per account.
pub(super) const SMART_VIEW_LIMIT: usize = 5000;

/// What a smart folder's conditions need besides the message itself, made
/// on the main thread and taken to the one that reads the index.
pub(super) struct SmartContext {
    /// The tags' keywords, lower case.
    pub(super) tags: Vec<String>,
    /// The tags themselves, for the Gmail labels that stand for them (#374).
    pub(super) tag_list: Vec<config::Tag>,
    pub(super) contacts: Option<std::sync::Arc<std::collections::HashSet<String>>>,
}

impl SmartContext {
    pub(super) fn matches(&self, folder: &config::SmartFolder, m: &Message) -> bool {
        let recipients = [m.to.as_str(), m.cc.as_str()]
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        folder.matches(&config::SmartInput {
            text: config::FilterInput {
                from_addr: &m.from_addr,
                from_name: &m.from_name,
                subject: &m.subject,
                recipients: &recipients,
                reply_to: &m.reply_to,
                body: if m.body.is_empty() {
                    &m.preview
                } else {
                    &m.body
                },
                body_hits: &[],
                contacts: self.contacts.as_deref(),
            },
            starred: m.starred,
            unread: m.unread,
            attachment: m.has_attachment,
            keywords: &m.tag_keywords(&self.tag_list),
            tags: &self.tags,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    #[test]
    fn smart_folders_read_recipients_and_fall_back_to_the_preview() {
        use crate::config::{FilterMatch, SmartCondition, SmartField, SmartFolder};
        let ctx = SmartContext {
            tags: Vec::new(),
            tag_list: Vec::new(),
            contacts: None,
        };
        let folder = |field, value: &str| SmartFolder {
            id: "s".into(),
            name: "S".into(),
            account_email: String::new(),
            folder: None,
            any: false,
            conditions: vec![SmartCondition {
                field,
                matcher: FilterMatch::Contains,
                value: value.into(),
            }],
        };
        let mut m = msg(1, 1, 1);
        m.cc = "carol@x".into();
        m.preview = "quarterly numbers".into();
        assert!(ctx.matches(&folder(SmartField::Recipients, "carol"), &m));
        assert!(ctx.matches(&folder(SmartField::Body, "quarterly"), &m));
        m.body = "something else".into();
        assert!(!ctx.matches(&folder(SmartField::Body, "quarterly"), &m));
    }
}
