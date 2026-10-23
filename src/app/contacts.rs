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
