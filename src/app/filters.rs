//! Message filters: which rules match a message, and running them over a
//! folder as it syncs.

use super::*;

/// The rules among `rules` that match `m`, as indexes in order. The list
/// rarely carries a body (the demo's does); the preview is what a sync
/// brings for every message, so it stands in for an empty body.
pub(super) fn rules_matching(
    rules: &[&config::FilterRule],
    m: &Message,
    body_hits: &[String],
    contacts: Option<&std::collections::HashSet<String>>,
) -> Vec<usize> {
    let recipients = [m.to.as_str(), m.cc.as_str()]
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let input = config::FilterInput {
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
        body_hits,
        contacts,
    };
    rules
        .iter()
        .enumerate()
        .filter(|(_, r)| r.matches(&input))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::demo::demo_filters;
    use crate::app::test_support::msg;

    #[test]
    fn filter_rules_read_recipients_and_the_preview() {
        use crate::config::{FilterField, FilterMatch};
        let mut rules = demo_filters();
        rules[0].field = FilterField::Recipients;
        rules[0].matcher = FilterMatch::Contains;
        rules[0].value = "carol".into();
        rules[1].field = FilterField::Body;
        rules[1].value = "invoice".into();
        let rules: Vec<&config::FilterRule> = rules.iter().collect();
        let mut m = msg(1, 1, 1);
        m.cc = "Carol <carol@x>".into();
        m.preview = "Your invoice".into();
        assert_eq!(rules_matching(&rules, &m, &[], None), vec![0, 1]);
        m.body = "nothing here".into();
        assert_eq!(rules_matching(&rules, &m, &[], None), vec![0]);
        m.subject = "Your order".into();
        assert_eq!(rules_matching(&rules, &m, &[], None), vec![0, 2]);
    }
}
