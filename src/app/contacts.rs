//! Contacts: the address books, syncing them, and importing vCards.

use super::*;

/// Sync the address books that sync with a server, after `delay_ms` and
/// off the main thread, and read the contacts again if any did (#408).
/// Unless `force`d, a book synced in the last few minutes is left alone.
pub(super) fn sync_contact_books(sender: &ComponentSender<AppModel>, force: bool, delay_ms: u64) {
    let s = sender.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        if crate::carddav::sync_books(force) {
            s.input(AppMsg::ContactBooksSynced);
        }
    });
}

/// Read the contacts, the books new ones can go to and the Hylki book's
/// size off the main thread, after `delay_ms` (EDS takes a moment to flush
/// a write to the cache the read goes through).
pub(super) fn reload_contacts(sender: &ComponentSender<AppModel>, delay_ms: u64) {
    let s = sender.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        if demo_mode() {
            s.input(AppMsg::ContactsLoaded(crate::contacts::demo_contacts()));
            s.input(AppMsg::ContactBooksLoaded {
                books: crate::contacts::demo_books(),
                local_count: 1,
            });
            return;
        }
        let contacts = crate::contacts::read_contact_details();
        crate::avatar::retain_photo_thumbnails(&contacts.iter().map(|c| c.photo_key).collect());
        s.input(AppMsg::ContactsLoaded(contacts));
        s.input(AppMsg::ContactBooksLoaded {
            books: crate::contacts::writable_books(),
            local_count: crate::local_contacts::count(),
        });
    });
}

/// The notification text for a finished contacts import, and whether it is an error.
pub(super) fn import_summary(o: &crate::local_contacts::ImportOutcome) -> (String, bool) {
    if o.added + o.updated == 0 {
        return (
            i18n("No contacts were found in the selected files"),
            o.errors > 0,
        );
    }
    let counts = [
        ("added", o.added.to_string()),
        ("updated", o.updated.to_string()),
        ("skipped", o.skipped.to_string()),
        ("errors", o.errors.to_string()),
    ];
    let args: Vec<(&str, &str)> = counts.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let text = if o.skipped + o.errors > 0 {
        i18n_f(
            "Contacts imported: {added} new, {updated} updated, {skipped} skipped, {errors} damaged",
            &args,
        )
    } else {
        i18n_f("Contacts imported: {added} new, {updated} updated", &args)
    };
    (text, false)
}

impl AppModel {
    /// The window a dialog opened from Settings belongs to: Settings while
    /// it is open, else the main window.
    pub(super) fn dialog_parent(&self) -> Option<gtk::Window> {
        let prefs = self
            .prefs
            .as_ref()
            .map(|p| p.widget().clone().upcast::<gtk::Window>());
        prefs
            .filter(|w| w.is_visible())
            .or_else(|| Some(self.window.clone().upcast()))
    }

    /// Dialog to add an email to GNOME Contacts (choosing the address book).
    /// Which books can take it is asked of EDS first, off the UI thread:
    /// that opens every book, and a remote one can go to its server (#315).
    pub(super) fn show_add_contact_dialog(
        &self,
        name: &str,
        email: &str,
        sender: &ComponentSender<Self>,
    ) {
        let (name, email) = (name.to_string(), email.to_string());
        let input = sender.input_sender().clone();
        std::thread::spawn(move || {
            let books = crate::contacts::writable_books();
            let _ = input.send(AppMsg::AddContactBooks { name, email, books });
        });
    }

    pub(super) fn add_contact_dialog(
        &self,
        name: &str,
        email: &str,
        books: Vec<crate::contacts::Book>,
        sender: &ComponentSender<Self>,
    ) {
        if books.is_empty() || email.trim().is_empty() {
            self.notifications.emit(NotifyInput::Push {
                text: i18n("No address book available to add contacts"),
                error: true,
                connectivity: false,
            });
            return;
        }

        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(i18n("Add to Contacts").as_str()),
            None,
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("add", &i18n("Add"));
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("add"));
        dialog.set_close_response("cancel");

        let form = gtk::ListBox::new();
        form.add_css_class("boxed-list");
        form.set_selection_mode(gtk::SelectionMode::None);
        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Name"));
        name_row.set_text(name);
        let email_row = adw::EntryRow::new();
        email_row.set_title(&i18n("Email"));
        email_row.set_text(email);
        let book_row = adw::ComboRow::new();
        book_row.set_title(&i18n("Address book"));
        let labels: Vec<&str> = books.iter().map(|b| b.name.as_str()).collect();
        book_row.set_model(Some(&gtk::StringList::new(&labels)));
        form.append(&name_row);
        form.append(&email_row);
        form.append(&book_row);
        dialog.set_extra_child(Some(&form));

        let input = sender.input_sender().clone();
        dialog.connect_response(None, move |_, resp| {
            if resp != "add" {
                return;
            }
            let name = name_row.text().trim().to_string();
            let email = email_row.text().trim().to_string();
            let idx = book_row.selected() as usize;
            let Some(book) = books.get(idx).cloned() else {
                return;
            };
            if email.is_empty() {
                return;
            }
            // Writing talks to EDS over D-Bus (blocking) — do it off the UI thread.
            let input = input.clone();
            std::thread::spawn(move || {
                let result = crate::contacts::add_or_merge(&book.uid, &name, &email);
                let _ = input.send(AppMsg::ContactAdded(result));
            });
        });
        dialog.present();
    }

    /// Show Contacts in place of the mail panes, read afresh.
    pub(super) fn show_contacts_view(&mut self, sender: &ComponentSender<Self>) {
        self.close_sidebar_peek();
        self.showing_outbox = false;
        self.leave_gallery();
        self.showing_contacts = true;
        // Read EDS off the UI thread (SQLite + photo decoding); the page
        // shows its loading face until the list lands.
        self.contacts_page.emit(ContactsPageInput::SetLoading);
        reload_contacts(sender, 0);
        if !demo_mode() {
            sync_contact_books(sender, false, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_import_summary_counts() {
        use crate::local_contacts::ImportOutcome;
        let none = ImportOutcome {
            errors: 1,
            ..Default::default()
        };
        assert_eq!(
            import_summary(&none),
            (
                "No contacts were found in the selected files".to_string(),
                true
            )
        );
        let ok = ImportOutcome {
            added: 2,
            updated: 1,
            ..Default::default()
        };
        assert_eq!(
            import_summary(&ok),
            ("Contacts imported: 2 new, 1 updated".to_string(), false)
        );
        let mixed = ImportOutcome {
            added: 1,
            skipped: 3,
            ..Default::default()
        };
        assert_eq!(
            import_summary(&mixed).0,
            "Contacts imported: 1 new, 0 updated, 3 skipped, 0 damaged"
        );
    }
}
