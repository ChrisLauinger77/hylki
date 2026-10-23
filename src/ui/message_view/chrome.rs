//! Per-account colour classes for the GTK chrome around the page.

use adw::prelude::*;
use relm4::prelude::*;

/// Style rules for a [`DisplayCss`](crate::ui::DisplayCss), each named by a
/// class. A message's own colors used to be the one rule a provider held, so
/// every message opened reloaded it and restyled the whole window (#323);
/// kept side by side, they leave a class change on one widget to move
/// between them.
pub(super) struct ClassRules {
    css: crate::ui::DisplayCss,
    rules: std::cell::RefCell<std::collections::BTreeMap<String, String>>,
}

impl ClassRules {
    pub(super) fn new() -> Self {
        ClassRules {
            css: crate::ui::DisplayCss::new(),
            rules: Default::default(),
        }
    }

    /// Add or replace rules, as (selector, declarations).
    pub(super) fn set(&self, new: Vec<(String, String)>) {
        let mut rules = self.rules.borrow_mut();
        rules.extend(new);
        self.css.load(
            rules
                .iter()
                .map(|(s, d)| format!("{s} {{ {d} }}\n"))
                .collect(),
        );
    }
}

/// Give `widget` the class `class` in place of any other starting with
/// `prefix`. Nothing is touched when it already wears it.
pub(super) fn wear_class(widget: &impl IsA<gtk::Widget>, prefix: &str, class: &str) {
    if widget.has_css_class(class) {
        return;
    }
    for c in widget.css_classes() {
        if c.starts_with(prefix) {
            widget.remove_css_class(&c);
        }
    }
    widget.add_css_class(class);
}
