//! The message list's header: its Focus Mode fold, the sort and filter
//! popover, and the wide/narrow switch.

use super::*;

/// How wide the list pane has to be dragged before Automatic lays the list
/// out on one line, and how far back before it returns to cards (#334).
pub(super) const LIST_WIDE_ON: i32 = 600;

pub(super) const LIST_WIDE_OFF: i32 = 560;

/// The message list header's controls that Focus Mode folds into a ⋯ menu:
/// each sits in a revealer so it can slide away, and the toggles are kept so
/// the menu can flip them (their own handlers then do the filtering).
pub(super) struct ListHeaderWidgets {
    pub(super) revealers: Vec<gtk::Revealer>,
    pub(super) overflow: gtk::Revealer,
    pub(super) filter: SortFilterPopover,
    pub(super) sort: gtk::gio::SimpleAction,
}

/// The orders the list header offers, by their keys in `SortOrder`. The
/// column headings can set more (#443); none of these is ticked then.
pub(super) const SORT_ORDERS: [(&str, &str); 6] = [
    (i18n_noop("Date (Newest first)"), "date_newest"),
    (i18n_noop("Date (Oldest first)"), "date_oldest"),
    (i18n_noop("Sender (A–Z)"), "sender"),
    (i18n_noop("Subject (A–Z)"), "subject"),
    (i18n_noop("Unread first"), "unread"),
    (i18n_noop("Starred first"), "flagged"),
];

/// The list header's sort and filter button (#441): the sort orders in one
/// column of its popover, the quick filters in the other.
pub(super) struct SortFilterPopover {
    pub(super) button: gtk::MenuButton,
    pub(super) filters: Vec<(QuickFilter, gtk::CheckButton)>,
    pub(super) clear: gtk::Button,
}

impl SortFilterPopover {
    /// The popover stays open as rows are clicked, so an order and several
    /// filters can be set in one visit.
    pub(super) fn new(
        button: gtk::MenuButton,
        sort: &gtk::gio::SimpleAction,
        sender: &relm4::Sender<AppMsg>,
    ) -> Self {
        let column = |title: &str| {
            let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
            col.add_css_class("sort-filter-column");
            let caption = gtk::Label::new(Some(title));
            caption.set_xalign(0.0);
            caption.add_css_class("dim-label");
            caption.add_css_class("caption-heading");
            col.append(&caption);
            col
        };

        let sort_col = column(&i18n("Sort by"));
        let mut radios: Vec<(&'static str, gtk::CheckButton)> = Vec::new();
        for (label, key) in SORT_ORDERS {
            let radio = gtk::CheckButton::with_label(&i18n(label));
            if let Some((_, first)) = radios.first() {
                radio.set_group(Some(first));
            }
            let action = sort.clone();
            radio.connect_toggled(move |r| {
                let current = action.state().and_then(|v| v.str().map(String::from));
                if r.is_active() && current.as_deref() != Some(key) {
                    action.activate(Some(&key.to_variant()));
                }
            });
            sort_col.append(&radio);
            radios.push((key, radio));
        }
        // The headings and the ⋯ menu sort through the same action, so the
        // radios follow its state wherever the change came from.
        let show_sort = move |key: Option<String>| {
            let key = key.unwrap_or_default();
            match radios.iter().find(|(k, _)| *k == key) {
                Some((_, r)) => r.set_active(true),
                None => radios.iter().for_each(|(_, r)| r.set_active(false)),
            }
        };
        show_sort(sort.state().and_then(|v| v.str().map(String::from)));
        sort.connect_state_notify(move |a| {
            show_sort(a.state().and_then(|v| v.str().map(String::from)))
        });

        let filter_col = column(&i18n("Show only"));
        let mut filters = Vec::new();
        for f in QuickFilter::ALL {
            let check = gtk::CheckButton::with_label(&f.label());
            let s = sender.clone();
            check.connect_toggled(move |c| s.emit(AppMsg::SetQuickFilter(f, c.is_active())));
            filter_col.append(&check);
            filters.push((f, check));
        }
        let clear = gtk::Button::with_label(&i18n("Clear Filters"));
        clear.add_css_class("flat");
        clear.set_halign(gtk::Align::Start);
        clear.set_sensitive(false);
        let s = sender.clone();
        clear.connect_clicked(move |_| s.emit(AppMsg::ClearQuickFilters));
        filter_col.append(&clear);

        let columns = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        columns.add_css_class("sort-filter-popover");
        columns.append(&sort_col);
        columns.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        columns.append(&filter_col);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&columns));
        button.set_popover(Some(&popover));
        Self {
            button,
            filters,
            clear,
        }
    }

    /// Show `filters` on the checks, and tint the button while any is on
    /// so a narrowed list never passes for an empty folder.
    pub(super) fn sync(&self, filters: QuickFilters) {
        for (f, check) in &self.filters {
            if check.is_active() != filters.has(*f) {
                check.set_active(filters.has(*f));
            }
        }
        self.clear.set_sensitive(!filters.is_empty());
        if filters.is_empty() {
            self.button.remove_css_class("list-filter-active");
            self.button
                .set_tooltip_text(Some(&i18n("Sort and filter messages")));
        } else {
            self.button.add_css_class("list-filter-active");
            let names: Vec<String> = QuickFilter::ALL
                .iter()
                .filter(|f| filters.has(**f))
                .map(|f| f.label())
                .collect();
            self.button.set_tooltip_text(Some(&i18n_f(
                "Filtered: {names}",
                &[("names", &names.join(", "))],
            )));
        }
    }
}
