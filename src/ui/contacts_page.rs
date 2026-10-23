//! In-app Contacts view, shown in the main content area (like the attachments
//! gallery): a searchable, sortable list of every GNOME Contacts entry beside
//! the selected contact's full card, with editing, creation and deletion done
//! right here (writes go through EDS, so GNOME Contacts and CardDAV stay in
//! sync). Anything Hylki doesn't do — linking, photos — is one click away via
//! a contact's "Open in GNOME Contacts".

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::{ContactDetails, ContactEdit, GroupEdit, GroupFormat, Member};
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::ui::context_menu::show_context_menu;

mod actions;
mod detail;
mod forms;
mod list;
mod member_editor;
mod menus;
mod rows;
mod types;

use detail::member_for;
use member_editor::collect_edit;
use menus::{book_label_factory, confirm, matches_query, new_menu, FilterMenu};
use rows::{is_member, row_factory, same_contacts, DragKeys};

pub use menus::choose_vcf_files;
pub use rows::dragged_contacts;
pub use types::*;

#[cfg(test)]
mod tests;

#[relm4::component(pub)]
impl Component for ContactsPage {
    /// The app's compose revealer, mounted over the detail pane.
    type Init = gtk::Revealer;
    type Input = ContactsPageInput;
    type Output = ContactsPageOutput;
    type CommandOutput = ();

    view! {
        #[name = "page_stack"]
        gtk::Stack {
            set_transition_type: gtk::StackTransitionType::Crossfade,

            // No contacts at all (or still loading the first read).
            add_named[Some("none")] = &adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    add_css_class: "flat",
                    #[wrap(Some)]
                    set_title_widget = &gtk::Label {
                        set_label: &i18n("Contacts"),
                        add_css_class: "pane-title",
                    },
                    pack_start = &gtk::Button {
                        set_icon_name: "sidebar-show-symbolic",
                        set_tooltip_text: Some(i18n("Toggle sidebar").as_str()),
                        add_css_class: "flat",
                        connect_clicked[sender] => move |_| {
                            let _ = sender.output(ContactsPageOutput::ToggleSidebar);
                        },
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "document-open-symbolic",
                        set_tooltip_text: Some(i18n("Import vCard files into the Hylki address book").as_str()),
                        add_css_class: "flat",
                        connect_clicked => ContactsPageInput::ImportContacts,
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "list-add-symbolic",
                        set_tooltip_text: Some(i18n("New contact or group").as_str()),
                        add_css_class: "flat",
                        connect_clicked[sender] => move |b| new_menu(b, &sender),
                    },
                },

                #[wrap(Some)]
                set_content = &adw::StatusPage {
                    set_icon_name: Some("x-office-address-book-symbolic"),
                    #[watch]
                    set_title: &if model.loading { i18n("Loading Contacts…") } else { i18n("No Contacts") },
                    #[watch]
                    set_description: Some(if model.loading {
                        String::new()
                    } else {
                        i18n("Add a contact with the + button, or import vCard files. Contacts are kept in Hylki's own address book, and the address books of GNOME Contacts are listed here too.")
                    }.as_str()),
                },
            },

            // Each pane carries its own header (like the mail view), so the
            // divider between them runs the full height of the window; the
            // paned handle lets the list grow, with its launch width as the
            // floor.
            add_named[Some("browser")] = &gtk::Paned {
                set_orientation: gtk::Orientation::Horizontal,
                set_wide_handle: false,
                set_position: crate::config::load_contacts_pane_width(),
                set_resize_start_child: false,
                set_shrink_start_child: false,
                set_resize_end_child: true,
                set_shrink_end_child: false,
                // Remember the width the user drags to. Debounced: one write
                // once the drag settles (see the mail view's list pane).
                connect_position_notify[
                    pending = std::rc::Rc::new(std::cell::RefCell::new(
                        None::<gtk::glib::SourceId>,
                    ))
                ] => move |p| {
                    let pos = p.position();
                    if let Some(id) = pending.borrow_mut().take() {
                        id.remove();
                    }
                    let armed = pending.clone();
                    *pending.borrow_mut() = Some(gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(600),
                        move || {
                            *armed.borrow_mut() = None;
                            crate::config::save_contacts_pane_width(pos);
                        },
                    ));
                },

                #[wrap(Some)]
                set_start_child = &adw::ToolbarView {
                    add_top_bar = &adw::HeaderBar {
                        add_css_class: "flat",
                        // Left pane: no window controls (the detail pane's
                        // header carries the window's close button).
                        set_show_start_title_buttons: false,
                        set_show_end_title_buttons: false,
                        #[wrap(Some)]
                        set_title_widget = &gtk::Label {
                            set_label: "",
                        },
                        // Leftmost, mirroring the pane it acts on — same spot
                        // as in the message list's header.
                        pack_start = &gtk::Button {
                            set_icon_name: "sidebar-show-symbolic",
                            set_tooltip_text: Some(i18n("Toggle sidebar").as_str()),
                            add_css_class: "flat",
                            connect_clicked[sender] => move |_| {
                                let _ = sender.output(ContactsPageOutput::ToggleSidebar);
                            },
                        },
                        // Beside it, as in the message list's header.
                        pack_start = &gtk::Button {
                            set_icon_name: "system-search-symbolic",
                            set_tooltip_text: Some(i18n("Search contacts (Ctrl+F)").as_str()),
                            add_css_class: "flat",
                            connect_clicked => ContactsPageInput::FocusSearch,
                        },
                        // pack_end packs right-to-left: sort and filter at the
                        // far right (matching the message list), then the
                        // count, then import, then "+", first on the right.
                        #[name = "filter_btn"]
                        pack_end = &gtk::MenuButton {
                            set_icon_name: "filter-symbolic",
                            set_tooltip_text: Some(i18n("Sort and filter contacts").as_str()),
                            set_valign: gtk::Align::Center,
                            add_css_class: "flat",
                        },
                        #[name = "count_label"]
                        pack_end = &gtk::Label {
                            set_valign: gtk::Align::Center,
                            add_css_class: "list-count",
                        },
                        pack_end = &gtk::Button {
                            set_icon_name: "document-open-symbolic",
                            set_tooltip_text: Some(i18n("Import vCard files into the Hylki address book").as_str()),
                            add_css_class: "flat",
                            connect_clicked => ContactsPageInput::ImportContacts,
                        },
                        pack_end = &gtk::Button {
                            set_icon_name: "list-add-symbolic",
                            set_tooltip_text: Some(i18n("New contact or group").as_str()),
                            add_css_class: "flat",
                            connect_clicked[sender] => move |b| new_menu(b, &sender),
                        },
                    },

                    #[wrap(Some)]
                    set_content = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_width_request: 280,

                        // The search, shown by the header's search button:
                        // the message list's search bar.
                        gtk::Revealer {
                            set_transition_type: gtk::RevealerTransitionType::SlideDown,
                            #[watch]
                            set_reveal_child: model.search_open || model.search_pinned,

                            gtk::Box {
                                set_spacing: 6,
                                add_css_class: "list-search-row",

                                #[name = "search"]
                                gtk::SearchEntry {
                                    set_hexpand: true,
                                    set_width_chars: 3,
                                    set_placeholder_text: Some(i18n("Search contacts").as_str()),
                                    connect_search_changed[sender] => move |e| {
                                        sender.input(ContactsPageInput::Query(e.text().to_string()));
                                    },
                                    connect_stop_search[sender] => move |_| {
                                        sender.input(ContactsPageInput::CloseSearch);
                                    },
                                    // Leaving it empty closes it too.
                                    add_controller = gtk::EventControllerFocus {
                                        connect_leave[sender] => move |ctl| {
                                            let Some(entry) = ctl.widget().and_downcast::<gtk::SearchEntry>() else { return };
                                            let sender = sender.clone();
                                            crate::ui::close_if_focus_left_row(&entry, move || {
                                                sender.input(ContactsPageInput::CloseSearch);
                                            });
                                        },
                                    },
                                },

                            },
                        },

                        // Which address book the list shows, while the
                        // sidebar does not list them: every one, or one.
                        #[name = "book_filter"]
                        gtk::DropDown {
                            set_margin_top: 6,
                            set_margin_bottom: 6,
                            set_margin_start: 10,
                            set_margin_end: 10,
                            set_visible: false,
                            set_tooltip_text: Some(i18n("Address book").as_str()),
                        },

                        gtk::ScrolledWindow {
                            set_vexpand: true,
                            set_hscrollbar_policy: gtk::PolicyType::Never,

                            #[name = "list"]
                            gtk::ListView {
                                add_css_class: "navigation-sidebar",
                                add_css_class: "contacts-listbox",
                            },
                        },
                    },
                },

                #[wrap(Some)]
                #[name = "detail_overlay"]
                set_end_child = &gtk::Overlay {
                    // The app mounts its contacts compose slot over this in
                    // init, so "New message" slides down right here.
                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                    set_hexpand: true,

                    add_top_bar = &adw::HeaderBar {
                        add_css_class: "flat",
                        set_show_start_title_buttons: false,
                        // Top-left of the card pane: edit the shown contact.
                        #[name = "edit_btn"]
                        pack_start = &gtk::Button {
                            set_icon_name: "document-edit-symbolic",
                            set_tooltip_text: Some(i18n("Edit contact").as_str()),
                            add_css_class: "flat",
                            connect_clicked => ContactsPageInput::Edit,
                        },
                        #[wrap(Some)]
                        set_title_widget = &gtk::Label {
                            set_label: &i18n("Contacts"),
                            add_css_class: "pane-title",
                        },
                    },

                    #[wrap(Some)]
                    set_content = &gtk::ScrolledWindow {
                        set_hexpand: true,
                        set_vexpand: true,
                        set_hscrollbar_policy: gtk::PolicyType::Never,
                        add_css_class: "contact-detail-pane",

                        adw::Clamp {
                            set_maximum_size: 560,
                            set_tightening_threshold: 420,

                            #[name = "detail_box"]
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_margin_top: 24,
                                set_margin_bottom: 32,
                                set_margin_start: 18,
                                set_margin_end: 18,
                            },
                        },
                    },
                    },
                },
            },
        }
    }

    fn init(
        compose_slot: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let mut model = ContactsPage {
            contacts: Vec::new(),
            query: String::new(),
            sort: ContactSort::FirstName,
            scope: ContactScope::All,
            tree: None,
            drag: Rc::new(RefCell::new(DragKeys::default())),
            filters: 0,
            filter_menu: None,
            book_picker: false,
            picker_labels: Vec::new(),
            picker_guard: Rc::new(std::cell::Cell::new(false)),
            search_open: false,
            search_pinned: false,
            search_closed_at: None,
            selected: None,
            multi: Vec::new(),
            row_map: Vec::new(),
            store: gtk::gio::ListStore::new::<gtk::glib::BoxedAnyObject>(),
            selection: gtk::MultiSelection::new(None::<gtk::gio::ListStore>),
            rows: Rc::new(RefCell::new(HashMap::new())),
            items: Vec::new(),
            loading: true,
            editor: None,
            editing_target: None,
            deleted: std::collections::HashSet::new(),
            books: Vec::new(),
            filter_books: Vec::new(),
            directory_hits: Vec::new(),
            directory_generation: 0,
            directories: Vec::new(),
            directory_failures: Vec::new(),
            suggestions: Rc::new(RefCell::new(Vec::new())),
            pending: None,
            decoding: std::collections::HashSet::new(),
        };
        let widgets = view_output!();
        widgets.detail_overlay.add_overlay(&compose_slot);

        // Delete on the list asks to remove the contact being looked at. The
        // controller sits on the list, not the page, so Delete inside the
        // search box or the editor still edits text.
        {
            let keys = gtk::EventControllerKey::new();
            let s = sender.input_sender().clone();
            keys.connect_key_pressed(move |_, keyval, _, _| {
                if !matches!(keyval, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete) {
                    return gtk::glib::Propagation::Proceed;
                }
                let _ = s.send(ContactsPageInput::DeleteSelected);
                gtk::glib::Propagation::Stop
            });
            widgets.list.add_controller(keys);
        }

        model.filter_menu = Some(FilterMenu::new(&widgets.filter_btn, sender.input_sender()));

        // The dropdown's own label gives way in a narrow pane; its list
        // shows each name whole.
        widgets
            .book_filter
            .set_factory(Some(&book_label_factory(true)));
        widgets
            .book_filter
            .set_list_factory(Some(&book_label_factory(false)));
        {
            let s = sender.input_sender().clone();
            let guard = model.picker_guard.clone();
            widgets.book_filter.connect_selected_notify(move |d| {
                if !guard.get() {
                    let _ = s.send(ContactsPageInput::PickBook(d.selected()));
                }
            });
        }

        model.selection.set_model(Some(&model.store));
        {
            let s = sender.input_sender().clone();
            model.selection.connect_selection_changed(move |_, _, _| {
                let _ = s.send(ContactsPageInput::SelectionChanged);
            });
        }
        widgets.list.set_model(Some(&model.selection));
        widgets.list.set_factory(Some(&row_factory(
            &model.rows,
            sender.input_sender(),
            &model.drag,
        )));

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match message {
            ContactsPageInput::SetLoading => {
                self.loading = true;
                if self.contacts.is_empty() && self.directories.is_empty() {
                    widgets.page_stack.set_visible_child_name("none");
                }
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let _ = s.send(ContactsPageInput::DirectoriesKnown(
                        crate::directory::enabled_names(),
                    ));
                });
            }

            ContactsPageInput::SetContacts(contacts) if self.editor.is_some() => {
                self.pending = Some(contacts);
            }

            ContactsPageInput::SetContacts(mut contacts) => {
                self.pending = None;
                // Drop tombstoned (locally deleted) entries the read still
                // carries; a tombstone retires once a read comes back clean.
                let incoming: std::collections::HashSet<(String, String)> = contacts
                    .iter()
                    .map(|c| (c.book_uid.clone(), c.eds_uid.clone()))
                    .collect();
                self.deleted.retain(|key| incoming.contains(key));
                contacts.retain(|c| {
                    !self
                        .deleted
                        .contains(&(c.book_uid.clone(), c.eds_uid.clone()))
                });
                // Every visit re-reads the books, and a sync behind it reads
                // them again; unchanged, the list on screen stays (#477).
                if !contacts.is_empty() && same_contacts(&self.contacts, &contacts) {
                    // In the same order, so every index still holds: the
                    // fresh cards (an edit patches them) without new rows.
                    self.contacts = contacts;
                    self.loading = false;
                    // Their photos too, which the rows have as thumbnails.
                    self.decode_photos(Default::default(), &sender);
                    self.update_view(widgets, sender);
                    return;
                }
                // Keep the same person selected across a refresh, by identity.
                let on_hit = self.selected.filter(|&i| i >= DIRECTORY);
                let keep = self
                    .selected
                    .and_then(|i| self.contacts.get(i))
                    .map(|c| (c.name.clone(), c.primary_email().map(str::to_string)));
                // And those selected together, the same way.
                let identity = |c: &ContactDetails| {
                    (
                        c.book_uid.clone(),
                        c.eds_uid.clone(),
                        c.name.clone(),
                        c.primary_email().map(str::to_string),
                    )
                };
                let multi: Vec<_> = self
                    .multi
                    .iter()
                    .filter_map(|&i| self.contacts.get(i))
                    .map(identity)
                    .collect();
                self.multi = contacts
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| multi.contains(&identity(c)))
                    .map(|(i, _)| i)
                    .collect();
                self.items = self.items_for(&contacts);
                self.contacts = contacts;
                self.loading = false;
                self.editor = None;
                self.editing_target = None;
                self.selected = keep
                    .and_then(|(name, email)| {
                        self.contacts.iter().position(|c| {
                            c.name == name && c.primary_email().map(str::to_string) == email
                        })
                    })
                    .or(on_hit);
                // One just added from a directory is a contact now.
                self.drop_known_hits();
                if self.selected.is_some_and(|i| self.entry(i).is_none()) {
                    self.selected = None;
                }
                self.refresh_book_filter();
                self.rebuild(widgets, &sender);
            }

            ContactsPageInput::Query(q) if q.to_lowercase() == self.query => {}

            ContactsPageInput::Query(q) => {
                self.query = q.to_lowercase();
                self.directory_generation = self.directory_generation.wrapping_add(1);
                let typed = q.trim().to_string();
                if typed.chars().count() < crate::directory::MIN_QUERY {
                    self.directory_hits.clear();
                    self.directory_failures.clear();
                } else {
                    // What the directories found before narrows with the
                    // search until they answer this one.
                    let query = self.query.clone();
                    self.directory_hits
                        .retain(|c| matches_query(c, query.trim()));
                    // Asked once the typing pauses, not per key.
                    let generation = self.directory_generation;
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(300),
                        move || {
                            let _ = s.send(ContactsPageInput::DirectoryLookup {
                                generation,
                                query: typed,
                            });
                        },
                    );
                }
                if self.selected.is_some_and(|i| self.entry(i).is_none()) {
                    self.selected = None;
                }
                self.rebuild(widgets, &sender);
            }

            ContactsPageInput::SetSearch(text) => {
                self.search_open = true;
                widgets.search.set_text(&text);
            }

            ContactsPageInput::FocusSearch => {
                if self.search_pinned {
                    widgets.search.grab_focus();
                } else if self.search_open {
                    sender.input(ContactsPageInput::CloseSearch);
                } else if self
                    .search_closed_at
                    .is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(300))
                {
                    self.search_open = true;
                    // Once the revealer has mapped it.
                    let entry = widgets.search.clone();
                    gtk::glib::idle_add_local_once(move || {
                        entry.grab_focus();
                    });
                }
            }

            ContactsPageInput::CloseSearch => {
                let had_focus =
                    widgets.search.has_focus() || widgets.search.focus_child().is_some();
                if !self.search_pinned {
                    self.search_open = false;
                    self.search_closed_at = Some(std::time::Instant::now());
                }
                // A hidden search narrows nothing: its text goes.
                let narrowed = !self.query.is_empty();
                self.query.clear();
                widgets.search.set_text("");
                self.directory_generation = self.directory_generation.wrapping_add(1);
                self.directory_hits.clear();
                self.directory_failures.clear();
                if narrowed {
                    self.rebuild(widgets, &sender);
                }
                if had_focus {
                    widgets.list.grab_focus();
                }
            }

            ContactsPageInput::SetBookPicker(on) => {
                if self.book_picker != on {
                    self.book_picker = on;
                    self.sync_book_picker(widgets);
                }
            }

            ContactsPageInput::SetSearchPinned(on) => {
                self.search_pinned = on;
            }

            ContactsPageInput::SetFilter(filter, on) => {
                let filters = if on {
                    self.filters | filter.bit()
                } else {
                    self.filters & !filter.bit()
                };
                if filters != self.filters {
                    self.filters = filters;
                    self.sync_filter_menu();
                    self.rebuild(widgets, &sender);
                }
            }

            ContactsPageInput::ClearFilters => {
                if self.filters != 0 {
                    self.filters = 0;
                    self.sync_filter_menu();
                    self.rebuild(widgets, &sender);
                }
            }

            ContactsPageInput::SuggestionsLoaded(suggestions) => {
                *self.suggestions.borrow_mut() = suggestions;
            }

            ContactsPageInput::PickBook(n) => {
                let scope = match (n as usize)
                    .checked_sub(1)
                    .and_then(|i| self.filter_books.get(i))
                {
                    Some((uid, _)) => ContactScope::Book(uid.clone()),
                    None => ContactScope::All,
                };
                sender.input(ContactsPageInput::SetScope(scope));
            }

            ContactsPageInput::DirectoriesKnown(directories) => {
                if directories == self.directories {
                    return;
                }
                self.directories = directories;
                self.refresh_book_filter();
                self.publish_tree(&sender);
                if !self.directories.is_empty() && self.contacts.is_empty() && !self.loading {
                    widgets.page_stack.set_visible_child_name("browser");
                }
            }

            ContactsPageInput::DirectoryLookup { generation, query } => {
                if generation != self.directory_generation {
                    return;
                }
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let (hits, failed) = crate::directory::search_contacts(&query, DIRECTORY_LIMIT);
                    let _ = s.send(ContactsPageInput::DirectoryResults {
                        generation,
                        hits,
                        failed,
                    });
                });
            }

            ContactsPageInput::DirectoryResults {
                generation,
                hits,
                failed,
            } => {
                if generation != self.directory_generation {
                    return;
                }
                self.directory_failures = failed;
                // The entry shown may be at another place in the new list.
                let shown = self
                    .selected
                    .filter(|&i| i >= DIRECTORY)
                    .and_then(|i| self.entry(i))
                    .map(|c| (c.name.clone(), c.primary_email().map(str::to_string)));
                self.directory_hits = hits;
                self.drop_known_hits();
                if let Some((name, email)) = shown {
                    self.selected = self
                        .directory_hits
                        .iter()
                        .position(|c| {
                            c.name == name && c.primary_email().map(str::to_string) == email
                        })
                        .map(|i| DIRECTORY + i);
                }
                // An open editor stays as it is; the list catches up with
                // the next change.
                if self.editor.is_none() {
                    self.rebuild(widgets, &sender);
                }
            }

            ContactsPageInput::AddDirectoryHit(idx) => {
                let Some(c) = idx
                    .checked_sub(DIRECTORY)
                    .and_then(|i| self.directory_hits.get(i))
                else {
                    return;
                };
                // A card of its own, not the directory's, whose UID is the
                // directory's name for the entry.
                let edit = ContactEdit {
                    name: c.name.clone(),
                    nickname: c.nickname.clone(),
                    org: c.org.clone(),
                    title: c.title.clone(),
                    note: c.note.clone(),
                    emails: c.emails.clone(),
                    phones: c.phones.clone(),
                    urls: c.urls.clone(),
                    addresses: c.postal.clone(),
                    birthday: c.birthday_raw.clone(),
                    photo: c.photo_bytes().map_or(
                        crate::contacts::PhotoEdit::Keep,
                        crate::contacts::PhotoEdit::Set,
                    ),
                };
                let _ = sender.output(ContactsPageOutput::CreateContact {
                    book_uid: None,
                    vcard: crate::contacts::new_vcard(&edit),
                });
            }

            ContactsPageInput::SetSort(sort) => {
                if self.sort != sort {
                    self.sort = sort;
                    self.sync_filter_menu();
                    self.rebuild(widgets, &sender);
                }
            }

            ContactsPageInput::SetScope(scope) => {
                if self.scope != scope {
                    self.scope = scope;
                    // Another list abandons an open editor and what was
                    // selected in this one.
                    self.editor = None;
                    self.editing_target = None;
                    self.selected = None;
                    self.multi.clear();
                    self.rebuild(widgets, &sender);
                }
            }

            ContactsPageInput::DropContacts { keys, on, moving } => {
                let people: Vec<usize> = keys
                    .iter()
                    .filter_map(|(book, uid)| {
                        self.contacts
                            .iter()
                            .position(|c| !c.is_group() && c.book_uid == *book && c.eds_uid == *uid)
                    })
                    .collect();
                match on {
                    DropOn::Book(to) if moving => {
                        let keys: Vec<(String, String)> = people
                            .iter()
                            .map(|&i| &self.contacts[i])
                            .filter(|c| self.can_move(c, &to))
                            .map(|c| (c.book_uid.clone(), c.eds_uid.clone()))
                            .collect();
                        self.ask_move(widgets, &sender, keys, to, None);
                    }
                    // A copy leaves the contact where it was: no question.
                    DropOn::Book(to) => {
                        let writable = self.books.iter().any(|b| b.uid == to);
                        let cards: Vec<String> = people
                            .iter()
                            .map(|&i| &self.contacts[i])
                            .filter(|c| writable && c.book_uid != to)
                            .map(crate::contacts::moved_vcard)
                            .collect();
                        if !cards.is_empty() {
                            let _ = sender.output(ContactsPageOutput::CopyContacts { to, cards });
                        }
                    }
                    DropOn::Group { book, uid } => {
                        let Some(g) = self
                            .contacts
                            .iter()
                            .position(|c| c.is_group() && c.book_uid == book && c.eds_uid == uid)
                        else {
                            return;
                        };
                        self.add_to_group(&sender, g, &people);
                    }
                }
            }

            ContactsPageInput::Tree(action) => {
                let group = |book: &str, uid: &str| {
                    self.contacts
                        .iter()
                        .position(|c| c.is_group() && c.book_uid == book && c.eds_uid == uid)
                };
                match action {
                    TreeAction::NewContact(book) => {
                        sender.input(ContactsPageInput::SetScope(ContactScope::Book(book)));
                        sender.input(ContactsPageInput::NewContact);
                    }
                    TreeAction::NewGroup(book) => {
                        sender.input(ContactsPageInput::SetScope(ContactScope::Book(book)));
                        sender.input(ContactsPageInput::NewGroup);
                    }
                    TreeAction::EditGroup { book, uid } => {
                        if let Some(idx) = group(&book, &uid) {
                            sender.input(ContactsPageInput::SetScope(ContactScope::Group {
                                book,
                                uid,
                            }));
                            sender.input(ContactsPageInput::EditIndex(idx));
                        }
                    }
                    TreeAction::DeleteGroup { book, uid } => {
                        if let Some(idx) = group(&book, &uid) {
                            sender.input(ContactsPageInput::DeleteRequest(idx));
                        }
                    }
                    TreeAction::RenameBook(uid) => self.ask_book_name(widgets, &sender, uid),
                    TreeAction::ComposeGroup { book, uid } => {
                        if let Some(idx) = group(&book, &uid) {
                            sender.input(ContactsPageInput::ComposeGroup(idx));
                        }
                    }
                }
            }

            ContactsPageInput::RemoveFromGroup(people) => {
                let ContactScope::Group { book, uid } = &self.scope else {
                    return;
                };
                let Some(g) = self
                    .contacts
                    .iter()
                    .find(|c| c.is_group() && c.book_uid == *book && c.eds_uid == *uid)
                else {
                    return;
                };
                let Some(format) = g.group else { return };
                let gone: Vec<&ContactDetails> = people
                    .iter()
                    .filter_map(|&i| self.contacts.get(i))
                    .collect();
                let members: Vec<Member> = g
                    .members
                    .iter()
                    .filter(|m| !gone.iter().any(|p| is_member(g, m, p)))
                    .cloned()
                    .collect();
                if members.len() == g.members.len() {
                    return;
                }
                let edit = GroupEdit {
                    name: g.name.clone(),
                    members,
                    hide_addresses: g.hide_addresses,
                    kept: g.hidden_members.clone(),
                };
                let _ = sender.output(ContactsPageOutput::SaveGroup {
                    book_uid: g.book_uid.clone(),
                    format,
                    original: Some(g.raw_vcard.clone()),
                    edit,
                });
            }

            ContactsPageInput::SelectionChanged => {
                // Read as the selection is now: the change that sent this
                // may have been followed by others.
                let picked = self.picked_entries();
                match picked.as_slice() {
                    [] => {}
                    [idx] => {
                        let idx = *idx;
                        if self.multi.is_empty()
                            && self.selected == Some(idx)
                            && (self.editor.is_none() || self.editing_target == Some(idx))
                        {
                            return;
                        }
                        // Moving away abandons an open editor.
                        self.multi.clear();
                        self.editor = None;
                        self.editing_target = None;
                        self.selected = Some(idx);
                        self.render_detail(widgets, &sender);
                    }
                    many => {
                        // A directory's entry is not a contact to act on.
                        let keys: Vec<usize> = many
                            .iter()
                            .copied()
                            .filter(|&i| i < self.contacts.len())
                            .collect();
                        if keys == self.multi && self.editor.is_none() {
                            return;
                        }
                        self.multi = keys;
                        self.editor = None;
                        self.editing_target = None;
                        self.selected = None;
                        self.render_detail(widgets, &sender);
                    }
                }
            }

            ContactsPageInput::SelectFirst(n) => {
                let entries: Vec<u32> = self
                    .row_map
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| i.is_some())
                    .map(|(pos, _)| pos as u32)
                    .take(n as usize)
                    .collect();
                self.selection.unselect_all();
                for pos in entries {
                    self.selection.select_item(pos, false);
                }
            }

            ContactsPageInput::BulkAddToGroup(g) => {
                let chosen = self.chosen();
                self.add_to_group(&sender, g, &chosen);
            }

            ContactsPageInput::BulkMoveRequest(to) => {
                let keys: Vec<(String, String)> = self
                    .movable(&to)
                    .into_iter()
                    .map(|i| {
                        (
                            self.contacts[i].book_uid.clone(),
                            self.contacts[i].eds_uid.clone(),
                        )
                    })
                    .collect();
                self.ask_move(widgets, &sender, keys, to, None);
            }

            ContactsPageInput::MoveRequest { idx, to } => {
                let Some(c) = self.contacts.get(idx).filter(|c| self.can_move(c, &to)) else {
                    return;
                };
                let key = (c.book_uid.clone(), c.eds_uid.clone());
                self.ask_move(widgets, &sender, vec![key], to, None);
            }

            ContactsPageInput::EditMoveConfirmed { to, key, vcard } => {
                self.forget(std::slice::from_ref(&key));
                let _ = sender.output(ContactsPageOutput::MoveContacts {
                    to,
                    cards: vec![(key.0, key.1, vcard)],
                });
                self.editor = None;
                self.editing_target = None;
                self.rebuild(widgets, &sender);
            }

            ContactsPageInput::BulkMoveConfirmed { to, keys } => {
                let cards: Vec<(String, String, String)> = keys
                    .iter()
                    .filter_map(|(book, uid)| {
                        self.contacts
                            .iter()
                            .find(|c| c.book_uid == *book && c.eds_uid == *uid)
                    })
                    .map(|c| {
                        (
                            c.book_uid.clone(),
                            c.eds_uid.clone(),
                            crate::contacts::moved_vcard(c),
                        )
                    })
                    .collect();
                if cards.is_empty() {
                    return;
                }
                // They leave their old books' lists now; the re-read after
                // the move shows them in the new one.
                self.forget(&keys);
                let _ = sender.output(ContactsPageOutput::MoveContacts { to, cards });
                self.rebuild(widgets, &sender);
            }

            ContactsPageInput::WriteFailed => {
                self.deleted.clear();
            }

            ContactsPageInput::BulkDeleteRequest => {
                let chosen = self.chosen();
                if chosen.is_empty() {
                    return;
                }
                let keys: Vec<(String, String)> = chosen
                    .iter()
                    .map(|&i| {
                        (
                            self.contacts[i].book_uid.clone(),
                            self.contacts[i].eds_uid.clone(),
                        )
                    })
                    .collect();
                let synced = chosen
                    .iter()
                    .any(|&i| self.synced(&self.contacts[i].book_uid));
                let groups = chosen.iter().all(|&i| self.contacts[i].is_group());
                let n = chosen.len() as u32;
                let count = n.to_string();
                let args = [("n", count.as_str())];
                let (heading, body) = match (groups, synced) {
                    (true, true) => (
                        ni18n_f("Delete {n} Group?", "Delete {n} Groups?", n, &args),
                        ni18n_f(
                            "This will remove {n} group from your address books locally and on the server. The contacts are not removed.",
                            "This will remove {n} groups from your address books locally and on the server. The contacts are not removed.",
                            n,
                            &args,
                        ),
                    ),
                    (true, false) => (
                        ni18n_f("Delete {n} Group?", "Delete {n} Groups?", n, &args),
                        ni18n_f(
                            "This will remove {n} group from your address book. The contacts are not removed.",
                            "This will remove {n} groups from your address books. The contacts are not removed.",
                            n,
                            &args,
                        ),
                    ),
                    (false, true) => (
                        ni18n_f("Delete {n} Contact?", "Delete {n} Contacts?", n, &args),
                        ni18n_f(
                            "This will remove {n} contact from your address books locally and on the server.",
                            "This will remove {n} contacts from your address books locally and on the server.",
                            n,
                            &args,
                        ),
                    ),
                    (false, false) => (
                        ni18n_f("Delete {n} Contact?", "Delete {n} Contacts?", n, &args),
                        ni18n_f(
                            "This will remove {n} contact from your address book.",
                            "This will remove {n} contacts from your address books.",
                            n,
                            &args,
                        ),
                    ),
                };
                let s = sender.input_sender().clone();
                confirm(widgets, &heading, &body, &i18n("Delete"), true, move || {
                    let _ = s.send(ContactsPageInput::DeleteConfirmed(keys.clone()));
                });
            }

            ContactsPageInput::Compose(email) => {
                let _ = sender.output(ContactsPageOutput::Compose(email));
            }

            ContactsPageInput::OpenUrl(url) => {
                // Bare "example.org" URLs are common in vCards.
                let full = if url.contains("://") {
                    url
                } else {
                    format!("https://{url}")
                };
                crate::ui::launch::open_link(&full, None);
            }

            ContactsPageInput::Edit => {
                if let Some(idx) = self.selected.filter(|&i| i < DIRECTORY) {
                    self.edit(widgets, &sender, idx);
                }
            }

            ContactsPageInput::EditIndex(idx) => {
                if idx < self.contacts.len() {
                    self.selected = Some(idx);
                    self.select_row_for(idx);
                    self.edit(widgets, &sender, idx);
                }
            }

            ContactsPageInput::NewContact => {
                self.new_form(widgets);
                self.render_editor(widgets, &sender);
            }

            ContactsPageInput::NewGroup => {
                self.new_form(widgets);
                self.render_group_editor(widgets, &sender);
            }

            ContactsPageInput::ComposeGroup(idx) => {
                let Some(g) = self
                    .contacts
                    .get(idx)
                    .filter(|g| g.reachable().next().is_some())
                else {
                    return;
                };
                let field = g.members_field();
                let _ = sender.output(if g.hide_addresses {
                    ContactsPageOutput::ComposeBcc(field)
                } else {
                    ContactsPageOutput::Compose(field)
                });
            }

            ContactsPageInput::AddToGroup { group, person } => {
                let (Some(g), Some(p)) = (self.contacts.get(group), self.contacts.get(person))
                else {
                    return;
                };
                let Some(member) = member_for(g, p, true) else {
                    return;
                };
                let mut members = g.members.clone();
                members.push(member);
                let edit = GroupEdit {
                    name: g.name.clone(),
                    members,
                    hide_addresses: g.hide_addresses,
                    kept: g.hidden_members.clone(),
                };
                let Some(format) = g.group else { return };
                let _ = sender.output(ContactsPageOutput::SaveGroup {
                    book_uid: g.book_uid.clone(),
                    format,
                    original: Some(g.raw_vcard.clone()),
                    edit,
                });
            }

            ContactsPageInput::ImportContacts => {
                let parent = relm4::main_application().active_window();
                let s = sender.clone();
                choose_vcf_files(parent.as_ref(), move |paths| {
                    let _ = s.output(ContactsPageOutput::ImportContacts(paths));
                });
            }

            ContactsPageInput::CancelEdit => {
                self.editor = None;
                self.editing_target = None;
                if let Some(contacts) = self.pending.take() {
                    sender.input(ContactsPageInput::SetContacts(contacts));
                }
                if self.contacts.is_empty() && self.directories.is_empty() {
                    widgets.page_stack.set_visible_child_name("none");
                }
                self.render_detail(widgets, &sender);
            }

            ContactsPageInput::SaveEdit => {
                let editor = match &self.editor {
                    Some(Editor::Contact(form)) => form,
                    Some(Editor::Group(form)) => {
                        let mut edit = GroupEdit {
                            name: form.name.text().trim().to_string(),
                            members: form.members.borrow().clone(),
                            hide_addresses: form.hide.is_active(),
                            ..GroupEdit::default()
                        };
                        // A group is known by its name alone.
                        if edit.name.is_empty() {
                            form.name.add_css_class("error");
                            form.name.grab_focus();
                            return;
                        }
                        let picked = form
                            .book
                            .as_ref()
                            .and_then(|row| form.books.get(row.selected() as usize))
                            .cloned();
                        let output = match self.editing_target.and_then(|i| self.contacts.get(i)) {
                            // Saved in another book: made there, then removed
                            // from its own.
                            Some(g) if picked.as_ref().is_some_and(|b| b.uid != g.book_uid) => {
                                let to = picked.unwrap_or_else(crate::contacts::local_book);
                                let format = to.groups.unwrap_or(GroupFormat::Evolution);
                                edit.members = self.members_for(
                                    edit.members,
                                    Some(&g.book_uid),
                                    &to.uid,
                                    format,
                                );
                                let key = (g.book_uid.clone(), g.eds_uid.clone());
                                self.forget(&[key.clone()]);
                                let _ = sender.output(ContactsPageOutput::MoveGroup {
                                    from: key.0,
                                    uid: key.1,
                                    to: to.uid,
                                    format,
                                    edit,
                                });
                                self.editor = None;
                                self.editing_target = None;
                                self.rebuild(widgets, &sender);
                                return;
                            }
                            Some(g) => {
                                let format = g.group.unwrap_or(GroupFormat::Evolution);
                                edit.members = self.members_for(
                                    edit.members,
                                    Some(&g.book_uid),
                                    &g.book_uid,
                                    format,
                                );
                                edit.kept = g.hidden_members.clone();
                                ContactsPageOutput::SaveGroup {
                                    book_uid: g.book_uid.clone(),
                                    format,
                                    original: Some(g.raw_vcard.clone()),
                                    edit,
                                }
                            }
                            None => {
                                let book = picked.unwrap_or_else(crate::contacts::local_book);
                                let format = book.groups.unwrap_or(GroupFormat::Evolution);
                                edit.members =
                                    self.members_for(edit.members, None, &book.uid, format);
                                ContactsPageOutput::SaveGroup {
                                    book_uid: book.uid,
                                    format,
                                    original: None,
                                    edit,
                                }
                            }
                        };
                        let _ = sender.output(output);
                        self.editor = None;
                        self.editing_target = None;
                        self.render_detail(widgets, &sender);
                        return;
                    }
                    None => return,
                };
                let edit = collect_edit(editor);
                // Anything at all is a contact; an empty form is nothing.
                let empty = [
                    &edit.name,
                    &edit.nickname,
                    &edit.org,
                    &edit.title,
                    &edit.note,
                    &edit.birthday,
                ]
                .iter()
                .all(|v| v.is_empty())
                    && edit.emails.is_empty()
                    && edit.phones.is_empty()
                    && edit.urls.is_empty()
                    && edit.addresses.is_empty()
                    && !matches!(edit.photo, crate::contacts::PhotoEdit::Set(_));
                if empty {
                    return;
                }
                let picked = editor
                    .book
                    .as_ref()
                    .and_then(|row| self.books.get(row.selected() as usize));
                match self.editing_target.and_then(|i| self.contacts.get(i)) {
                    // Saved in another book: moved there, with the edits,
                    // once the move is confirmed. The form stays till then.
                    Some(c) if picked.is_some_and(|b| b.uid != c.book_uid) => {
                        let to = picked.map(|b| b.uid.clone()).unwrap_or_default();
                        if !self.can_move(c, &to) {
                            return;
                        }
                        let mut moved = c.clone();
                        moved.raw_vcard = crate::contacts::patched_vcard(&c.raw_vcard, &edit);
                        moved.photo = match &edit.photo {
                            crate::contacts::PhotoEdit::Keep => c.photo_bytes(),
                            crate::contacts::PhotoEdit::Remove => None,
                            crate::contacts::PhotoEdit::Set(bytes) => Some(bytes.clone()),
                        };
                        moved.photo_key =
                            moved.photo.as_deref().map_or(0, crate::contacts::photo_key);
                        let vcard = crate::contacts::moved_vcard(&moved);
                        let key = (c.book_uid.clone(), c.eds_uid.clone());
                        self.ask_move(widgets, &sender, vec![key], to, Some(vcard));
                        return;
                    }
                    Some(c) => {
                        let vcard = crate::contacts::patched_vcard(&c.raw_vcard, &edit);
                        let _ = sender.output(ContactsPageOutput::SaveContact {
                            book_uid: c.book_uid.clone(),
                            vcard,
                        });
                    }
                    None => {
                        let vcard = crate::contacts::new_vcard(&edit);
                        let book_uid = editor
                            .book
                            .as_ref()
                            .and_then(|row| self.books.get(row.selected() as usize))
                            .map(|b| b.uid.clone());
                        let _ =
                            sender.output(ContactsPageOutput::CreateContact { book_uid, vcard });
                    }
                }
                // The app re-reads EDS and pushes SetContacts; until then the
                // card shows the pre-edit state rather than a half-saved one.
                self.editor = None;
                self.editing_target = None;
                self.render_detail(widgets, &sender);
            }

            ContactsPageInput::OpenInGnome(idx) => {
                if let Some(c) = self.contacts.get(idx) {
                    crate::ui::contacts_browser::launch_gnome_contacts_for(&c.eds_uid);
                }
            }

            ContactsPageInput::DeleteSelected => {
                // Not while an editor is open: that contact is being changed.
                if self.editor.is_some() {
                    return;
                }
                if !self.multi.is_empty() {
                    sender.input(ContactsPageInput::BulkDeleteRequest);
                } else if let Some(idx) = self.selected {
                    sender.input(ContactsPageInput::DeleteRequest(idx));
                }
            }

            ContactsPageInput::DeleteRequest(idx) => {
                let Some(c) = self.contacts.get(idx) else {
                    return;
                };
                let synced = self.synced(&c.book_uid);
                let name = [("name", c.name.as_str())];
                let (heading, body) = if c.is_group() {
                    let body = if synced {
                        i18n_f(
                            "This will remove the group {name} from your address book locally and on the server. The contacts are not removed.",
                            &name,
                        )
                    } else {
                        i18n_f("This will remove the group {name} from your address book. The contacts are not removed.", &name)
                    };
                    (i18n("Delete Group?"), body)
                } else {
                    let body = if synced {
                        i18n_f(
                            "This will remove the contact {name} from your address book locally and on the server.",
                            &name,
                        )
                    } else {
                        i18n_f(
                            "This will remove the contact {name} from your address book.",
                            &name,
                        )
                    };
                    (i18n("Delete Contact?"), body)
                };
                let s = sender.input_sender().clone();
                let keys = vec![(c.book_uid.clone(), c.eds_uid.clone())];
                confirm(widgets, &heading, &body, &i18n("Delete"), true, move || {
                    let _ = s.send(ContactsPageInput::DeleteConfirmed(keys.clone()));
                });
            }

            ContactsPageInput::SetBooks(books) => {
                self.books = books;
                self.refresh_book_filter();
                self.publish_tree(&sender);
            }

            ContactsPageInput::PhotosDecoded => {
                for row in self.rows.borrow().values() {
                    row.show_photo();
                }
                self.decoding
                    .retain(|key| crate::avatar::photo_thumbnail(*key).is_none());
            }

            ContactsPageInput::RowMenu { idx, x, y } => {
                let entries = self.row_menu(idx, &sender);
                if !entries.is_empty() {
                    show_context_menu(&widgets.list, x, y, entries);
                }
            }

            ContactsPageInput::OpenPhoto { name, data } => {
                let _ = sender.output(ContactsPageOutput::ShowPhoto { name, data });
            }

            ContactsPageInput::DeleteConfirmed(keys) => {
                let gone = self.forget(&keys);
                if !gone.is_empty() {
                    let _ = sender.output(ContactsPageOutput::DeleteContacts(gone));
                }
                // The entries leave the list right now; the EDS delete and
                // the reconciling re-read follow behind.
                self.editor = None;
                self.editing_target = None;
                self.rebuild(widgets, &sender);
            }
        }
        // Overriding `update_with_view` replaces the generated body that ran
        // `update_view` after every message; without this the `#[watch]`
        // bindings above (the "Loading Contacts…" / "No Contacts" face) only
        // ever hold their `init` values, and an empty address book looked
        // like a load that never finished.
        self.update_view(widgets, sender);
    }
}

impl ContactsPage {}
