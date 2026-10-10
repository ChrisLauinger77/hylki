//! The command palette (#399): Ctrl+K opens a search over what the app can
//! do now and where it can go, so the keyboard reaches every action and
//! folder without the menus. The app hands it the entries that apply at that
//! moment; typing narrows them, Enter or a click runs one.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::i18n::i18n;

/// One thing the palette offers.
pub struct Entry {
    pub title: String,
    /// What kind of thing it is, or where it is: "Action", an account's
    /// folder path. Searched along with the title.
    pub detail: String,
    /// The shortcut that does the same, shown beside it; empty for none.
    pub key: String,
    pub run: Box<dyn Fn()>,
}

/// Whether every word of `query` is somewhere in `text`, both lower case.
fn matches(text: &str, query: &[String]) -> bool {
    query.iter().all(|w| text.contains(w.as_str()))
}

/// Open the palette over `parent` with `entries`, in the order given.
pub fn show(parent: &impl IsA<gtk::Widget>, entries: Vec<Entry>) {
    let dialog = adw::Dialog::builder()
        .title(i18n("Command Palette"))
        .content_width(560)
        .content_height(480)
        .build();

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(i18n("Type an action, a folder or a tag").as_str()));
    search.set_hexpand(true);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    let haystacks: Vec<String> =
        entries.iter().map(|e| format!("{} {}", e.title, e.detail).to_lowercase()).collect();
    for e in &entries {
        let row = adw::ActionRow::builder()
            .title(gtk::glib::markup_escape_text(&e.title))
            .subtitle(gtk::glib::markup_escape_text(&e.detail))
            .activatable(true)
            .build();
        if !e.key.is_empty() {
            let key = gtk::Label::new(Some(&e.key));
            key.add_css_class("shortcut-key");
            key.set_valign(gtk::Align::Center);
            row.add_suffix(&key);
        }
        list.append(&row);
    }

    let query: Rc<RefCell<Vec<String>>> = Rc::default();
    {
        let query = query.clone();
        list.set_filter_func(move |row| matches(&haystacks[row.index() as usize], &query.borrow()));
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_margin_top(12);
    column.set_margin_bottom(12);
    column.set_margin_start(12);
    column.set_margin_end(12);
    column.append(&search);
    column.append(&scroller);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&column));
    dialog.set_child(Some(&toolbar));

    // The rows the search leaves, in order.
    let shown = |list: &gtk::ListBox| -> Vec<gtk::ListBoxRow> {
        let mut out = Vec::new();
        let mut child = list.first_child();
        while let Some(w) = child {
            if let Ok(row) = w.clone().downcast::<gtk::ListBoxRow>() {
                if row.is_child_visible() {
                    out.push(row);
                }
            }
            child = w.next_sibling();
        }
        out
    };

    {
        let list = list.clone();
        let query = query.clone();
        search.connect_search_changed(move |entry| {
            *query.borrow_mut() = entry.text().to_lowercase().split_whitespace().map(str::to_string).collect();
            list.invalidate_filter();
            match shown(&list).first() {
                Some(first) => list.select_row(Some(first)),
                None => list.unselect_all(),
            }
        });
    }

    let entries = Rc::new(entries);
    {
        let dialog = dialog.clone();
        let entries = entries.clone();
        list.connect_row_activated(move |_, row| {
            let Some(e) = entries.get(row.index() as usize) else { return };
            dialog.close();
            (e.run)();
        });
    }
    {
        let list = list.clone();
        search.connect_activate(move |_| {
            let row = list.selected_row().or_else(|| shown(&list).into_iter().next());
            if let Some(row) = row {
                row.activate();
            }
        });
    }
    // The arrows move through the rows while the typing stays in the search.
    {
        let keys = gtk::EventControllerKey::new();
        let list = list.clone();
        let search_back = search.clone();
        keys.connect_key_pressed(move |_, keyval, _, _| {
            let step = match keyval {
                gtk::gdk::Key::Down => 1,
                gtk::gdk::Key::Up => -1,
                _ => return gtk::glib::Propagation::Proceed,
            };
            let rows = shown(&list);
            if rows.is_empty() {
                return gtk::glib::Propagation::Stop;
            }
            let at = list.selected_row().and_then(|s| rows.iter().position(|r| *r == s));
            let next = match at {
                Some(i) => (i as i32 + step).clamp(0, rows.len() as i32 - 1) as usize,
                None => 0,
            };
            list.select_row(Some(&rows[next]));
            // Focus scrolls the row into view; the typing goes back to
            // the search at once.
            rows[next].grab_focus();
            let at = search_back.position();
            search_back.grab_focus();
            search_back.select_region(at, at);
            gtk::glib::Propagation::Stop
        });
        // Ahead of the entry's own text, which keeps Up and Down for itself.
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        search.add_controller(keys);
    }

    if let Some(first) = shown(&list).first() {
        list.select_row(Some(first));
    }
    dialog.present(Some(parent));
    search.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn every_word_has_to_match() {
        let q = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        assert!(matches("archive action", &q("arch")));
        assert!(matches("inbox work · inbox", &q("work in")));
        assert!(!matches("inbox work · inbox", &q("work sent")));
        assert!(matches("anything", &q("")));
    }
}
