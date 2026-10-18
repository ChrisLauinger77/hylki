//! In-app Contacts view, shown in the main content area (like the attachments
//! gallery): a searchable, sortable list of every GNOME Contacts entry beside
//! the selected contact's full card, with editing, creation and deletion done
//! right here (writes go through EDS, so GNOME Contacts and CardDAV stay in
//! sync). Anything Hylki doesn't do — linking, photos — is one click away via
//! a contact's "Open in GNOME Contacts".

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::{ContactDetails, ContactEdit, GroupEdit, GroupFormat, Labeled, Member};
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::i18n::{i18n, i18n_f, i18n_noop};

/// Where the people found in the LDAP directories (#307) start in the
/// index space `selected` and `row_map` share with `contacts`: an index this
/// high or higher is `directory_hits[index - DIRECTORY]`.
const DIRECTORY: usize = usize::MAX / 2;

/// At most this many directory entries are listed for one search.
const DIRECTORY_LIMIT: usize = 50;

/// Which side of the list shows: the people, or the groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactsView {
    People,
    Groups,
}

/// The "Show only" filters of the sort and filter button, as the message
/// list has for mail. They narrow the people; groups are not filtered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactFilter {
    Photo,
    Email,
    Phone,
    Address,
    Birthday,
    Company,
    NoGroup,
}

impl ContactFilter {
    pub const ALL: [ContactFilter; 7] = [
        ContactFilter::Photo,
        ContactFilter::Email,
        ContactFilter::Phone,
        ContactFilter::Address,
        ContactFilter::Birthday,
        ContactFilter::Company,
        ContactFilter::NoGroup,
    ];

    fn label(self) -> String {
        match self {
            ContactFilter::Photo => i18n("Has Photo"),
            ContactFilter::Email => i18n("Has Email"),
            ContactFilter::Phone => i18n("Has Phone"),
            ContactFilter::Address => i18n("Has Address"),
            ContactFilter::Birthday => i18n("Has Birthday"),
            ContactFilter::Company => i18n("Has Company"),
            ContactFilter::NoGroup => i18n("Not in a Group"),
        }
    }

    fn bit(self) -> u8 {
        1 << ContactFilter::ALL.iter().position(|f| *f == self).unwrap_or(0)
    }

    /// Whether `c` passes; `grouped` holds every group member's address,
    /// lower case.
    fn passes(self, c: &ContactDetails, grouped: &std::collections::HashSet<String>) -> bool {
        match self {
            ContactFilter::Photo => c.photo.is_some(),
            ContactFilter::Email => !c.emails.is_empty(),
            ContactFilter::Phone => !c.phones.is_empty(),
            ContactFilter::Address => !c.addresses.is_empty(),
            ContactFilter::Birthday => !c.birthday.is_empty(),
            ContactFilter::Company => !c.org.is_empty(),
            ContactFilter::NoGroup => !c.emails.iter().any(|e| grouped.contains(&e.value.to_lowercase())),
        }
    }
}

/// How the contact list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactSort {
    FirstName,
    LastName,
    Email,
}

/// Live handles into the open contact form.
struct ContactForm {
    name: adw::EntryRow,
    nickname: adw::EntryRow,
    org: adw::EntryRow,
    title: adw::EntryRow,
    note: gtk::TextView,
    emails: Rc<RefCell<Vec<(adw::EntryRow, String)>>>,
    phones: Rc<RefCell<Vec<(adw::EntryRow, String)>>>,
    urls: Rc<RefCell<Vec<(adw::EntryRow, String)>>>,
    /// The book a new contact goes to; `None` when editing one (it stays in
    /// its book) or before the books are known.
    book: Option<adw::ComboRow>,
}

/// Live handles into the open group form.
struct GroupForm {
    name: adw::EntryRow,
    hide: adw::SwitchRow,
    /// The members as the form has them, in order.
    members: Rc<RefCell<Vec<Member>>>,
    /// A new group's address book, picked from `books`, those that keep
    /// groups.
    book: Option<adw::ComboRow>,
    books: Vec<crate::contacts::Book>,
}

/// The form open in the detail pane.
enum Editor {
    Contact(ContactForm),
    Group(GroupForm),
}

pub struct ContactsPage {
    contacts: Vec<ContactDetails>,
    query: String,
    sort: ContactSort,
    /// People or groups: the switch above the list.
    view: ContactsView,
    /// The "Show only" filters on, by [`ContactFilter::bit`].
    filters: u8,
    /// The sort and filter button's popover, kept in step with `sort` and
    /// `filters`.
    filter_menu: Option<FilterMenu>,
    /// The address book picker beside the search.
    book_picker: Option<BookPicker>,
    /// The search bar is shown: opened, or kept open by the setting the
    /// message list's search has too.
    search_open: bool,
    search_pinned: bool,
    /// When the bar last closed: the click on the search button that took
    /// focus from an empty search, and closed the bar, must not open it
    /// again (as in the message list).
    search_closed_at: Option<std::time::Instant>,
    /// Index into `contacts` of the contact shown in the detail pane.
    selected: Option<usize>,
    /// List position → index into `contacts` (or a directory hit, see
    /// [`DIRECTORY`]); `None` for a line that is nobody, such as a heading.
    row_map: Vec<Option<usize>>,
    /// The list's lines, shown by a list view that builds rows only for the
    /// lines on screen: a thousand contacts took most of a second to build
    /// as rows, at every visit and every key typed in the search (#477).
    store: gtk::gio::ListStore,
    selection: gtk::SingleSelection,
    /// The row widgets the list view has made, by their list item.
    rows: Rc<RefCell<HashMap<usize, Rc<ListRow>>>>,
    /// One list item per contact, in step with `contacts`. The same item
    /// keeps its row through a search, a sort or a re-read that leaves it
    /// as it was: the view makes rows only for items it has not shown.
    items: Vec<gtk::glib::BoxedAnyObject>,
    /// A load is underway (the page was just opened).
    loading: bool,
    /// The editor form, while open. `editing_target` is the contact or group
    /// being edited, `None` for a brand-new one.
    editor: Option<Editor>,
    editing_target: Option<usize>,
    /// EDS UIDs deleted here but possibly still in EDS's local cache — a
    /// re-read races the CardDAV flush, and without these tombstones a
    /// deleted contact pops right back into the list. A tombstone lives
    /// until a fresh read no longer contains its contact. Keyed by book and
    /// UID: an imported card keeps the UID of the book it came from.
    deleted: std::collections::HashSet<(String, String)>,
    /// The books new contacts can go to, the chosen default first; the
    /// Hylki book is always among them.
    books: Vec<crate::contacts::Book>,
    /// The book the list is narrowed to, or `None` for every book.
    book_filter: Option<String>,
    /// The filter's entries after "All Address Books": (book UID, name).
    filter_books: Vec<(String, String)>,
    /// The people the LDAP directories hold for the search (#307), listed
    /// after the contacts under their own heading. Read-only: they can be
    /// written to and added to Contacts, not edited.
    directory_hits: Vec<ContactDetails>,
    /// Bumped at every change to the search, so an answer to an older one
    /// is dropped.
    directory_generation: u64,
    /// The directories switched on, as (UID, name): the list shows, with
    /// its search box, even with no contacts of its own, and the book
    /// filter offers each after the address books.
    directories: Vec<(String, String)>,
    /// The directories that did not answer the search, listed under the
    /// people found, so a search that finds nobody says why (#307).
    directory_failures: Vec<crate::directory::DirectoryFailure>,
    /// The composer's recipient suggestions (Contacts and mail history),
    /// shared with an open group form, which offers them as people to add.
    /// Read again each time a group form opens; until the first read lands
    /// the form offers the contacts listed here.
    suggestions: Rc<RefCell<Vec<crate::contacts::Suggestion>>>,
    /// A read that came in while a form was open, applied once it closes:
    /// a sync in the background must not throw away what is being typed.
    pending: Option<Vec<ContactDetails>>,
    /// The photos handed to a decoding thread that has not finished them.
    decoding: std::collections::HashSet<u64>,
}

#[derive(Debug)]
pub enum ContactsPageInput {
    /// Fresh EDS read: replace everything (keeps the selection when possible).
    SetContacts(Vec<ContactDetails>),
    /// The page was just opened; a read is on its way.
    SetLoading,
    Query(String),
    SetSort(ContactSort),
    /// The switch above the list moved to people or groups.
    SetView(ContactsView),
    /// A "Show only" filter turned on or off.
    SetFilter(ContactFilter, bool),
    ClearFilters,
    /// The search button, Ctrl+F or `/`: open the search, or close it.
    FocusSearch,
    /// Esc in the search, or focus left it empty: close it and clear it.
    CloseSearch,
    /// The setting that keeps the search field open.
    SetSearchPinned(bool),
    /// The list's selection moved to this position.
    RowSelected(u32),
    /// A right-click on the line for this entry, at (x, y) on the list.
    RowMenu { idx: usize, x: f64, y: f64 },
    Compose(String),
    OpenUrl(String),
    /// Edit the selected contact / a specific one (context menu).
    Edit,
    EditIndex(usize),
    /// The header's "+": a blank editor for a new contact, or a new group
    /// (always in the Hylki book).
    NewContact,
    NewGroup,
    /// Write to everyone in the group at this index.
    ComposeGroup(usize),
    /// Put a person's first address in a group (both indexes into
    /// `contacts`).
    AddToGroup { group: usize, person: usize },
    /// The header's import button: choose `.vcf` files for the Hylki book.
    ImportContacts,
    CancelEdit,
    SaveEdit,
    OpenInGnome(usize),
    DeleteRequest(usize),
    /// The Delete key on the list: ask to delete the contact shown.
    DeleteSelected,
    /// The contact to delete, by book and UID: the list may have been
    /// re-read and re-sorted while the dialog was open.
    DeleteConfirmed { book_uid: String, uid: String },
    /// The card's photo was clicked: expand it in the app lightbox.
    OpenPhoto { name: String, data: Vec<u8> },
    /// The books new contacts can go to, freshly read, default first.
    SetBooks(Vec<crate::contacts::Book>),
    /// The book filter above the list: 0 is every book.
    FilterBook(u32),
    /// The LDAP directories switched on, as (UID, name).
    DirectoriesKnown(Vec<(String, String)>),
    /// The search paused: ask the directories, unless it changed since.
    DirectoryLookup { generation: u64, query: String },
    DirectoryResults { generation: u64, hits: Vec<ContactDetails>, failed: Vec<crate::directory::DirectoryFailure> },
    /// Copy a directory entry into the default address book.
    AddDirectoryHit(usize),
    /// Put this in the search box, as if typed.
    SetSearch(String),
    /// The composer's suggestions, freshly read for a group form.
    SuggestionsLoaded(Vec<crate::contacts::Suggestion>),
    /// Pick this entry of the book filter, as if chosen.
    PickBook(u32),
    /// More list photos are decoded and cached.
    PhotosDecoded,
}

#[derive(Debug)]
pub enum ContactsPageOutput {
    /// Start a message to this address, or these (a group).
    Compose(String),
    /// Start a message with these recipients in Bcc: a group that hides
    /// its members' addresses.
    ComposeBcc(String),
    /// The header's sidebar button (same spot as the message list's).
    ToggleSidebar,
    /// Persist an edit: the patched vCard back to its book.
    SaveContact { book_uid: String, vcard: String },
    /// Create a brand-new contact in the book picked in the editor
    /// (`None`: the default book).
    CreateContact { book_uid: Option<String>, vcard: String },
    /// Write a group: a change to `original`, its stored card, or a new
    /// one when there is none.
    SaveGroup { book_uid: String, format: GroupFormat, original: Option<String>, edit: GroupEdit },
    /// Delete, already confirmed by the user.
    DeleteContact { book_uid: String, uid: String },
    /// `.vcf` files chosen for import into the local Hylki book.
    ImportContacts(Vec<std::path::PathBuf>),
    /// Show the contact's photo in the app lightbox.
    ShowPhoto { name: String, data: Vec<u8> },
    /// Read the composer's suggestions for a group form
    /// ([`ContactsPageInput::SuggestionsLoaded`] brings them).
    WantSuggestions,
}

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

                        // The search, shown by the header's search button,
                        // with the address book it searches beside it: the
                        // message list's search bar and its folder picker.
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

                                // Hidden while there is only one book.
                                #[name = "book_btn"]
                                gtk::MenuButton {
                                    set_valign: gtk::Align::Center,
                                    set_visible: false,
                                    set_tooltip_text: Some(i18n("Choose which address book to search").as_str()),
                                },
                            },
                        },

                        // People and groups, each a list of its own. The
                        // toggles are added in `init`.
                        #[name = "view_switch"]
                        adw::ToggleGroup {
                            set_homogeneous: true,
                            set_margin_bottom: 6,
                            set_margin_start: 10,
                            set_margin_end: 10,
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
            view: ContactsView::People,
            filters: 0,
            filter_menu: None,
            book_picker: None,
            search_open: false,
            search_pinned: false,
            search_closed_at: None,
            selected: None,
            row_map: Vec::new(),
            store: gtk::gio::ListStore::new::<gtk::glib::BoxedAnyObject>(),
            selection: gtk::SingleSelection::builder().autoselect(false).can_unselect(true).build(),
            rows: Rc::new(RefCell::new(HashMap::new())),
            items: Vec::new(),
            loading: true,
            editor: None,
            editing_target: None,
            deleted: std::collections::HashSet::new(),
            books: Vec::new(),
            book_filter: None,
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
        model.book_picker = Some(BookPicker::new(&widgets.book_btn, sender.input_sender()));

        for (name, label) in [("people", i18n("Contacts")), ("groups", i18n("Groups"))] {
            widgets.view_switch.add(adw::Toggle::builder().name(name).label(label.as_str()).build());
        }
        widgets.view_switch.set_active_name(Some("people"));
        {
            let s = sender.input_sender().clone();
            widgets.view_switch.connect_active_name_notify(move |group| {
                let view = match group.active_name().as_deref() {
                    Some("groups") => ContactsView::Groups,
                    _ => ContactsView::People,
                };
                let _ = s.send(ContactsPageInput::SetView(view));
            });
        }

        model.selection.set_model(Some(&model.store));
        {
            let s = sender.input_sender().clone();
            model.selection.connect_selected_notify(move |selection| {
                let pos = selection.selected();
                if pos != gtk::INVALID_LIST_POSITION {
                    let _ = s.send(ContactsPageInput::RowSelected(pos));
                }
            });
        }
        widgets.list.set_model(Some(&model.selection));
        widgets.list.set_factory(Some(&row_factory(&model.rows, sender.input_sender())));

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
                    let _ = s.send(ContactsPageInput::DirectoriesKnown(crate::directory::enabled_names()));
                });
            }

            ContactsPageInput::SetContacts(contacts) if self.editor.is_some() => {
                self.pending = Some(contacts);
            }

            ContactsPageInput::SetContacts(mut contacts) => {
                self.pending = None;
                // Drop tombstoned (locally deleted) entries the read still
                // carries; a tombstone retires once a read comes back clean.
                let incoming: std::collections::HashSet<(String, String)> =
                    contacts.iter().map(|c| (c.book_uid.clone(), c.eds_uid.clone())).collect();
                self.deleted.retain(|key| incoming.contains(key));
                contacts.retain(|c| !self.deleted.contains(&(c.book_uid.clone(), c.eds_uid.clone())));
                // Every visit re-reads the books, and a sync behind it reads
                // them again; unchanged, the list on screen stays (#477).
                if !contacts.is_empty() && same_contacts(&self.contacts, &contacts) {
                    // In the same order, so every index still holds: the
                    // fresh cards (an edit patches them) without new rows.
                    self.contacts = contacts;
                    self.loading = false;
                    self.update_view(widgets, sender);
                    return;
                }
                // Keep the same person selected across a refresh, by identity.
                let on_hit = self.selected.filter(|&i| i >= DIRECTORY);
                let keep = self
                    .selected
                    .and_then(|i| self.contacts.get(i))
                    .map(|c| (c.name.clone(), c.primary_email().map(str::to_string)));
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
                self.refresh_book_filter(widgets);
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
                    self.directory_hits.retain(|c| matches_query(c, query.trim()));
                    // Asked once the typing pauses, not per key.
                    let generation = self.directory_generation;
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                        let _ = s.send(ContactsPageInput::DirectoryLookup { generation, query: typed });
                    });
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
                } else if self.search_closed_at.is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(300)) {
                    self.search_open = true;
                    // Once the revealer has mapped it.
                    let entry = widgets.search.clone();
                    gtk::glib::idle_add_local_once(move || {
                        entry.grab_focus();
                    });
                }
            }

            ContactsPageInput::CloseSearch => {
                let had_focus = widgets.search.has_focus() || widgets.search.focus_child().is_some();
                if !self.search_pinned {
                    self.search_open = false;
                    self.search_closed_at = Some(std::time::Instant::now());
                }
                // A hidden search narrows nothing: its text and its book go.
                let narrowed = !self.query.is_empty() || self.book_filter.is_some();
                self.query.clear();
                widgets.search.set_text("");
                self.directory_generation = self.directory_generation.wrapping_add(1);
                self.directory_hits.clear();
                self.directory_failures.clear();
                if !self.search_pinned {
                    self.book_filter = None;
                    if let Some(picker) = &self.book_picker {
                        picker.pick(0);
                    }
                }
                if narrowed {
                    self.rebuild(widgets, &sender);
                }
                if had_focus {
                    widgets.list.grab_focus();
                }
            }

            ContactsPageInput::SetSearchPinned(on) => {
                self.search_pinned = on;
            }

            ContactsPageInput::SetFilter(filter, on) => {
                let filters = if on { self.filters | filter.bit() } else { self.filters & !filter.bit() };
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
                self.search_open = true;
                if let Some(picker) = &self.book_picker {
                    picker.pick(n as usize);
                }
            }

            ContactsPageInput::DirectoriesKnown(directories) => {
                if directories == self.directories {
                    return;
                }
                self.directories = directories;
                self.refresh_book_filter(widgets);
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
                    let _ = s.send(ContactsPageInput::DirectoryResults { generation, hits, failed });
                });
            }

            ContactsPageInput::DirectoryResults { generation, hits, failed } => {
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
                        .position(|c| c.name == name && c.primary_email().map(str::to_string) == email)
                        .map(|i| DIRECTORY + i);
                }
                // An open editor stays as it is; the list catches up with
                // the next change.
                if self.editor.is_none() {
                    self.rebuild(widgets, &sender);
                }
            }

            ContactsPageInput::AddDirectoryHit(idx) => {
                let Some(c) = idx.checked_sub(DIRECTORY).and_then(|i| self.directory_hits.get(i)) else {
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

            ContactsPageInput::SetView(view) => {
                if self.view != view {
                    self.show_view(widgets, view);
                    // Moving to the other side abandons an open editor.
                    self.editor = None;
                    self.editing_target = None;
                    self.rebuild(widgets, &sender);
                }
            }

            ContactsPageInput::RowSelected(pos) => {
                // Sent from the selection's notify, so it may be stale: the
                // selection moved on (or was cleared) before it came.
                if self.selection.selected() != pos {
                    return;
                }
                let Some(&Some(idx)) = self.row_map.get(pos as usize) else { return };
                if self.selected == Some(idx) && (self.editor.is_none() || self.editing_target == Some(idx)) {
                    return;
                }
                // Moving away abandons an open editor.
                self.editor = None;
                self.editing_target = None;
                self.selected = Some(idx);
                self.render_detail(widgets, &sender);
            }

            ContactsPageInput::Compose(email) => {
                let _ = sender.output(ContactsPageOutput::Compose(email));
            }

            ContactsPageInput::OpenUrl(url) => {
                // Bare "example.org" URLs are common in vCards.
                let full = if url.contains("://") { url } else { format!("https://{url}") };
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
                self.new_on_side(widgets, &sender, ContactsView::People);
                self.render_editor(widgets, &sender);
            }

            ContactsPageInput::NewGroup => {
                self.new_on_side(widgets, &sender, ContactsView::Groups);
                self.render_group_editor(widgets, &sender);
            }

            ContactsPageInput::ComposeGroup(idx) => {
                let Some(g) = self.contacts.get(idx).filter(|g| !g.members.is_empty()) else { return };
                let field = g.members_field();
                let _ = sender.output(if g.hide_addresses {
                    ContactsPageOutput::ComposeBcc(field)
                } else {
                    ContactsPageOutput::Compose(field)
                });
            }

            ContactsPageInput::AddToGroup { group, person } => {
                let (Some(g), Some(p)) = (self.contacts.get(group), self.contacts.get(person)) else { return };
                let Some(email) = p.primary_email() else { return };
                if !g.group_editable() || g.members.iter().any(|m| m.email.eq_ignore_ascii_case(email)) {
                    return;
                }
                let mut members = g.members.clone();
                // In an Apple group the person is named by their card,
                // when it is in the group's book.
                let uid = if p.book_uid == g.book_uid { p.eds_uid.clone() } else { String::new() };
                members.push(Member { name: p.name.clone(), email: email.to_string(), uid, ..Member::default() });
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
                        let output = match self.editing_target.and_then(|i| self.contacts.get(i)) {
                            Some(g) => {
                                edit.kept = g.hidden_members.clone();
                                ContactsPageOutput::SaveGroup {
                                    book_uid: g.book_uid.clone(),
                                    format: g.group.unwrap_or(GroupFormat::Evolution),
                                    original: Some(g.raw_vcard.clone()),
                                    edit,
                                }
                            }
                            None => {
                                let book = form
                                    .book
                                    .as_ref()
                                    .and_then(|row| form.books.get(row.selected() as usize))
                                    .cloned()
                                    .unwrap_or_else(crate::contacts::local_book);
                                ContactsPageOutput::SaveGroup {
                                    book_uid: book.uid,
                                    format: book.groups.unwrap_or(GroupFormat::Evolution),
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
                if edit.name.is_empty() && edit.emails.is_empty() {
                    return; // nothing worth saving
                }
                match self.editing_target.and_then(|i| self.contacts.get(i)) {
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
                        let _ = sender.output(ContactsPageOutput::CreateContact { book_uid, vcard });
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
                if let Some(idx) = self.selected {
                    sender.input(ContactsPageInput::DeleteRequest(idx));
                }
            }

            ContactsPageInput::DeleteRequest(idx) => {
                let Some(c) = self.contacts.get(idx) else { return };
                let win = widgets.page_stack.root().and_downcast::<gtk::Window>();
                let (heading, body) = if c.is_group() {
                    (
                        i18n("Delete Group?"),
                        i18n_f(
                            "{name} is removed from your address book, and for a synced book, from the server too. The people in it are not.",
                            &[("name", &c.name)],
                        ),
                    )
                } else {
                    (
                        i18n("Delete Contact?"),
                        i18n_f(
                            "{name} is removed from your address book, and for a synced book, from the server too.",
                            &[("name", &c.name)],
                        ),
                    )
                };
                let dialog = adw::MessageDialog::new(win.as_ref(), Some(heading.as_str()), Some(&body));
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("delete", &i18n("Delete"));
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let s = sender.input_sender().clone();
                let (book_uid, uid) = (c.book_uid.clone(), c.eds_uid.clone());
                dialog.connect_response(None, move |_, resp| {
                    if resp == "delete" {
                        let _ = s.send(ContactsPageInput::DeleteConfirmed {
                            book_uid: book_uid.clone(),
                            uid: uid.clone(),
                        });
                    }
                });
                dialog.present();
            }

            ContactsPageInput::SetBooks(books) => {
                self.books = books;
                self.refresh_book_filter(widgets);
            }

            ContactsPageInput::PhotosDecoded => {
                for row in self.rows.borrow().values() {
                    row.show_photo();
                }
                self.decoding.retain(|key| crate::avatar::photo_thumbnail(*key).is_none());
            }

            ContactsPageInput::RowMenu { idx, x, y } => {
                let entries = self.row_menu(idx, &sender);
                if !entries.is_empty() {
                    show_context_menu(&widgets.list, x, y, entries);
                }
            }

            ContactsPageInput::FilterBook(index) => {
                let picked = (index as usize)
                    .checked_sub(1)
                    .and_then(|i| self.filter_books.get(i))
                    .map(|(uid, _)| uid.clone());
                // Replacing the filter's model notifies too, after the fact:
                // the same book again is no reason to list everyone anew.
                if picked == self.book_filter {
                    return;
                }
                self.book_filter = picked;
                if self.editor.is_none() {
                    self.selected = None;
                }
                self.rebuild(widgets, &sender);
            }

            ContactsPageInput::OpenPhoto { name, data } => {
                let _ = sender.output(ContactsPageOutput::ShowPhoto { name, data });
            }

            ContactsPageInput::DeleteConfirmed { book_uid, uid } => {
                let Some(idx) =
                    self.contacts.iter().position(|c| c.book_uid == book_uid && c.eds_uid == uid)
                else {
                    return;
                };
                let c = self.contacts.remove(idx);
                self.items.remove(idx);
                for (i, item) in self.items.iter().enumerate() {
                    item.borrow::<Line>().idx.set(Some(i));
                }
                if !c.eds_uid.is_empty() {
                    self.deleted.insert((c.book_uid.clone(), c.eds_uid.clone()));
                    let _ = sender.output(ContactsPageOutput::DeleteContact {
                        book_uid: c.book_uid,
                        uid: c.eds_uid,
                    });
                }
                // The entry leaves the list right now; the EDS delete and the
                // reconciling re-read follow behind.
                self.selected = None;
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

impl ContactsPage {
    /// Whether a contact matches the active search.
    fn matches(&self, c: &ContactDetails) -> bool {
        if c.is_group() != (self.view == ContactsView::Groups) {
            return false;
        }
        if self.book_filter.as_ref().is_some_and(|b| *b != c.book_uid) {
            return false;
        }
        matches_query(c, &self.query)
    }

    /// Whether `c` passes the "Show only" filters on; groups always do.
    fn passes_filters(&self, c: &ContactDetails, grouped: &std::collections::HashSet<String>) -> bool {
        c.is_group()
            || ContactFilter::ALL.iter().all(|f| self.filters & f.bit() == 0 || f.passes(c, grouped))
    }

    /// The addresses of every group's people, lower case, for "Not in a
    /// Group"; empty unless that filter is on.
    fn grouped_addresses(&self) -> std::collections::HashSet<String> {
        if self.filters & ContactFilter::NoGroup.bit() == 0 {
            return std::collections::HashSet::new();
        }
        self.contacts
            .iter()
            .filter(|c| c.is_group())
            .flat_map(|c| c.members.iter().map(|m| m.email.to_lowercase()))
            .collect()
    }

    fn sync_filter_menu(&self) {
        if let Some(menu) = &self.filter_menu {
            menu.sync(self.sort, self.filters);
        }
    }

    /// The entry at `idx`, a directory entry's index included.
    fn entry(&self, idx: usize) -> Option<&ContactDetails> {
        match idx.checked_sub(DIRECTORY) {
            Some(i) => self.directory_hits.get(i),
            None => self.contacts.get(idx),
        }
    }

    /// Someone already in Contacts is not listed again. One found in two
    /// directories stays in both, for when one of them is picked; the list
    /// of every book shows the first only.
    fn drop_known_hits(&mut self) {
        let known: std::collections::HashSet<String> =
            self.contacts.iter().flat_map(|c| c.emails.iter().map(|e| e.value.to_lowercase())).collect();
        self.directory_hits.retain(|c| !c.emails.iter().any(|e| known.contains(&e.value.to_lowercase())));
    }

    /// Fill the book picker from the books known: the writable ones (the
    /// Hylki book always among them) and any others the contacts come from.
    /// The book picked stays picked while it is still there.
    fn refresh_book_filter(&mut self, widgets: &ContactsPageWidgets) {
        let mut books: Vec<(String, String)> =
            self.books.iter().map(|b| (b.uid.clone(), b.name.clone())).collect();
        for c in &self.contacts {
            if !c.book_uid.is_empty() && !books.iter().any(|(uid, _)| *uid == c.book_uid) {
                books.push((c.book_uid.clone(), c.book_name.clone()));
            }
        }
        // A directory lists the people a search finds in it (#307).
        for (uid, name) in &self.directories {
            if !books.iter().any(|(u, _)| u == uid) {
                books.push((uid.clone(), format!("LDAP — {name}")));
            }
        }
        if books == self.filter_books {
            return;
        }
        self.filter_books = books;
        let all = i18n("All Address Books");
        let mut labels: Vec<&str> = vec![all.as_str()];
        labels.extend(self.filter_books.iter().map(|(_, name)| name.as_str()));
        let at = self
            .book_filter
            .as_ref()
            .and_then(|uid| self.filter_books.iter().position(|(u, _)| u == uid))
            .map_or(0, |i| i + 1);
        if at == 0 {
            self.book_filter = None;
        }
        if let Some(picker) = &self.book_picker {
            picker.set_books(&labels, at);
        }
        widgets.book_btn.set_visible(self.filter_books.len() > 1);
    }

    /// Put the switch, the search's hint and the sort button in step with
    /// `view`. The switch notifies after the fact; `SetView` then finds the
    /// side already shown.
    fn show_view(&mut self, widgets: &ContactsPageWidgets, view: ContactsView) {
        self.view = view;
        let groups = view == ContactsView::Groups;
        widgets.view_switch.set_active_name(Some(if groups { "groups" } else { "people" }));
        widgets.search.set_placeholder_text(Some(&if groups { i18n("Search groups") } else { i18n("Search contacts") }));
        // Groups go by name, and the filters are a person's.
        widgets.filter_btn.set_sensitive(!groups);
    }

    /// Ready the list for a new contact or group: the side it will be on,
    /// with nothing selected, so the form is what the pane shows.
    fn new_on_side(&mut self, widgets: &ContactsPageWidgets, sender: &ComponentSender<Self>, view: ContactsView) {
        self.editing_target = None;
        widgets.page_stack.set_visible_child_name("browser");
        if self.view != view {
            self.show_view(widgets, view);
            self.rebuild(widgets, sender);
        }
        self.selected = None;
        self.selection.set_selected(gtk::INVALID_LIST_POSITION);
    }

    /// Move the list selection to the row showing contact `idx` (if visible).
    fn select_row_for(&self, idx: usize) {
        if let Some(pos) = self.row_map.iter().position(|&i| i == Some(idx)) {
            self.selection.set_selected(pos as u32);
        }
    }

    /// Rebuild the list from `contacts` + `query` + `sort`, keep (or default)
    /// the selection, and render its detail card.
    fn rebuild(&mut self, widgets: &ContactsPageWidgets, sender: &ComponentSender<Self>) {
        let started = std::time::Instant::now();
        let empty = self.contacts.is_empty() && self.directories.is_empty();
        widgets.page_stack.set_visible_child_name(if empty { "none" } else { "browser" });

        let grouped = self.grouped_addresses();
        let mut people: Vec<usize> = (0..self.contacts.len())
            .filter(|&i| self.matches(&self.contacts[i]) && self.passes_filters(&self.contacts[i], &grouped))
            .collect();
        people.sort_by_key(|&i| {
            let c = &self.contacts[i];
            let name = c.name.to_lowercase();
            match self.sort {
                _ if c.is_group() => name,
                ContactSort::FirstName => name,
                // Sort by the final word of the name (then the whole name for
                // ties) — how an address book orders by surname.
                ContactSort::LastName => {
                    format!("{} {name}", name.rsplit(' ').next().unwrap_or(&name))
                }
                ContactSort::Email => {
                    c.primary_email().map(str::to_lowercase).unwrap_or(name)
                }
            }
        });
        let groups = self.view == ContactsView::Groups;
        let total = self.contacts.iter().filter(|c| c.is_group() == groups).count();
        let narrowed = !self.query.is_empty() || self.book_filter.is_some() || (!groups && self.filters != 0);
        widgets.count_label.set_label(&if !narrowed {
            total.to_string()
        } else {
            i18n_f("{shown} of {total}", &[("shown", &people.len().to_string()), ("total", &total.to_string())])
        });
        let mut lines: Vec<Line> = Vec::new();
        if groups && total == 0 {
            lines.push(Line::other(
                LineKind::Note,
                i18n("No groups yet. Make one with New Group, under the + button."),
                String::new(),
            ));
        }

        // The directories' answers, after the contacts and by name whatever
        // the sort, under a heading of their own so nobody takes them for
        // contacts. The book filter keeps those of the directory it names.
        let in_filter = |uid: &str| self.book_filter.as_deref().is_none_or(|b| b == uid);
        let mut listed = std::collections::HashSet::new();
        let mut hits = Vec::new();
        for (i, c) in self.directory_hits.iter().enumerate() {
            if groups || !in_filter(&c.book_uid) || !self.passes_filters(c, &grouped) {
                continue;
            }
            let emails: Vec<String> = c.emails.iter().map(|e| e.value.to_lowercase()).collect();
            if emails.iter().any(|e| listed.contains(e)) {
                continue;
            }
            listed.extend(emails);
            // Which directory it is in, after the address.
            let subtitle = match c.primary_email() {
                Some(email) => format!("{email} · {}", c.book_name),
                None => c.book_name.clone(),
            };
            hits.push(Line::entry(DIRECTORY + i, c, subtitle));
        }
        if !hits.is_empty() {
            lines.push(Line::other(LineKind::Heading, i18n("In Directories"), String::new()));
            lines.extend(hits);
        }

        // A directory that did not answer, last and not selectable, so a
        // search that finds nobody in it says why.
        for f in self.directory_failures.iter().filter(|f| !groups && in_filter(&f.uid)) {
            lines.push(Line::other(LineKind::Failure, f.name.clone(), f.message.clone()));
        }

        // A directory picked in the book filter is empty until searched.
        let directory_picked = self
            .book_filter
            .as_ref()
            .is_some_and(|b| self.directories.iter().any(|(uid, _)| uid == b));
        if directory_picked && !groups && people.is_empty() && lines.is_empty() {
            let text = if self.query.trim().chars().count() < crate::directory::MIN_QUERY {
                i18n("Type three letters or more to search the directory.")
            } else {
                i18n("Nobody in the directory matches.")
            };
            lines.push(Line::other(LineKind::Note, text, String::new()));
        }

        let items: Vec<gtk::glib::BoxedAnyObject> = people
            .iter()
            .filter_map(|&idx| self.items.get(idx).cloned())
            .chain(lines.into_iter().map(gtk::glib::BoxedAnyObject::new))
            .collect();
        self.row_map = items.iter().map(|item| item.borrow::<Line>().idx.get()).collect();
        let missing: std::collections::HashSet<u64> = items
            .iter()
            .filter_map(|item| match item.borrow::<Line>().kind {
                LineKind::Entry { photo_key, .. } if photo_key != 0 => Some(photo_key),
                _ => None,
            })
            .filter(|key| !self.decoding.contains(key) && crate::avatar::photo_thumbnail(*key).is_none())
            .collect();
        // Unselected first: the selection would otherwise follow the old
        // lines through the swap and report a position in the new ones.
        self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        self.store.splice(0, self.store.n_items(), &items);

        // Selection: keep it if it survived the filter, else the first match.
        let shown_pos = self
            .selected
            .and_then(|sel| self.row_map.iter().position(|&i| i == Some(sel)))
            .or_else(|| self.row_map.iter().position(Option::is_some));
        match shown_pos {
            Some(pos) => {
                self.selected = self.row_map[pos];
                self.selection.set_selected(pos as u32);
            }
            None => self.selected = None,
        }
        self.render_detail(widgets, sender);
        self.decode_photos(missing, sender);
        tracing::debug!("contacts: listed {} in {:?}", self.row_map.len(), started.elapsed());
    }

    /// The list items for a fresh read: an unchanged line keeps the item it
    /// had, and with it its row.
    fn items_for(&self, contacts: &[ContactDetails]) -> Vec<gtk::glib::BoxedAnyObject> {
        let mut had: HashMap<(&str, &str), &gtk::glib::BoxedAnyObject> = self
            .contacts
            .iter()
            .zip(&self.items)
            .map(|(c, item)| ((c.book_uid.as_str(), c.eds_uid.as_str()), item))
            .collect();
        contacts
            .iter()
            .enumerate()
            .map(|(idx, c)| {
                let subtitle = if c.is_group() {
                    group_byline(c)
                } else {
                    c.primary_email().filter(|e| *e != c.name).unwrap_or_default().to_string()
                };
                let line = Line::entry(idx, c, subtitle);
                match had.remove(&(c.book_uid.as_str(), c.eds_uid.as_str())) {
                    Some(item) if *item.borrow::<Line>() == line => {
                        item.borrow::<Line>().idx.set(Some(idx));
                        item.clone()
                    }
                    _ => gtk::glib::BoxedAnyObject::new(line),
                }
            })
            .collect()
    }

    /// Decode these photos on other threads; the rows waiting for them show
    /// each as it lands.
    fn decode_photos(&mut self, mut keys: std::collections::HashSet<u64>, sender: &ComponentSender<Self>) {
        // Each photo once, however many people share it.
        let photos: Vec<(u64, Vec<u8>)> = self
            .contacts
            .iter()
            .chain(&self.directory_hits)
            .filter_map(|c| {
                let key = line_photo_key(c);
                keys.remove(&key).then(|| Some((key, c.photo.clone()?))).flatten()
            })
            .collect();
        if photos.is_empty() {
            return;
        }
        self.decoding.extend(photos.iter().map(|(key, _)| *key));
        let s = sender.input_sender().clone();
        std::thread::spawn(move || {
            crate::avatar::decode_photo_thumbnails(photos, || {
                let _ = s.send(ContactsPageInput::PhotosDecoded);
            });
        });
    }

    /// The right-click menu for an entry of the list.
    fn row_menu(&self, idx: usize, sender: &ComponentSender<Self>) -> Vec<Vec<MenuEntry>> {
        let s = sender.input_sender();
        if idx >= DIRECTORY {
            let Some(c) = self.entry(idx) else { return Vec::new() };
            let mut entries = Vec::new();
            if let Some(email) = c.primary_email().map(str::to_string) {
                let sc = s.clone();
                entries.push(
                    MenuEntry::new(i18n("New message"), move || {
                        let _ = sc.send(ContactsPageInput::Compose(email.clone()));
                    })
                    .icon("mail-message-new-symbolic"),
                );
            }
            let sa = s.clone();
            entries.push(
                MenuEntry::new(i18n("Add to Contacts"), move || {
                    let _ = sa.send(ContactsPageInput::AddDirectoryHit(idx));
                })
                .icon("contact-new-symbolic"),
            );
            return vec![entries];
        }
        let Some(c) = self.contacts.get(idx) else { return Vec::new() };
        let local = crate::contacts::is_local_book(&c.book_uid);
        let group = c.is_group();
        let (se, sd, sg) = (s.clone(), s.clone(), s.clone());
        let mut first = vec![MenuEntry::new(i18n("Edit"), move || {
            let _ = se.send(ContactsPageInput::EditIndex(idx));
        })
        .icon("document-edit-symbolic")
        .enabled(!group || c.group_editable())];
        if group {
            let sc = s.clone();
            first.push(
                MenuEntry::new(i18n("New message"), move || {
                    let _ = sc.send(ContactsPageInput::ComposeGroup(idx));
                })
                .icon("mail-message-new-symbolic")
                .enabled(!c.members.is_empty()),
            );
        }
        // A person with an address can go in a group they are not in, by name.
        if let Some(email) = c.primary_email().filter(|_| !group) {
            let mut groups: Vec<(usize, &str)> = (0..self.contacts.len())
                .filter(|&g| self.contacts[g].group_editable())
                .filter(|&g| !self.contacts[g].members.iter().any(|m| m.email.eq_ignore_ascii_case(email)))
                .map(|g| (g, self.contacts[g].name.as_str()))
                .collect();
            groups.sort_by_key(|(_, name)| name.to_lowercase());
            if !groups.is_empty() {
                let entries = groups
                    .into_iter()
                    .map(|(g, name)| {
                        let sa = s.clone();
                        MenuEntry::new(name.to_string(), move || {
                            let _ = sa.send(ContactsPageInput::AddToGroup { group: g, person: idx });
                        })
                        .icon("system-users-symbolic")
                    })
                    .collect();
                first.push(MenuEntry::submenu(i18n("Add to Group"), vec![entries]).icon("system-users-symbolic"));
            }
        }
        // GNOME Contacts knows nothing of the Hylki book, nor of groups.
        if !local && !group {
            first.push(
                MenuEntry::new(i18n("Open in GNOME Contacts"), move || {
                    let _ = sg.send(ContactsPageInput::OpenInGnome(idx));
                })
                .icon("adw-external-link-symbolic"),
            );
        }
        vec![
            first,
            vec![MenuEntry::new(i18n("Delete…"), move || {
                let _ = sd.send(ContactsPageInput::DeleteRequest(idx));
            })
            .icon("user-trash-symbolic")],
        ]
    }

    /// Fill the right-hand pane with the selected contact's card.
    fn render_detail(&mut self, widgets: &ContactsPageWidgets, sender: &ComponentSender<Self>) {
        self.editor = None;
        let detail = &widgets.detail_box;
        while let Some(child) = detail.first_child() {
            detail.remove(&child);
        }
        let Some(c) = self.selected.and_then(|i| self.entry(i)) else {
            return;
        };
        let hit = self.selected.filter(|&i| i >= DIRECTORY);
        widgets.edit_btn.set_sensitive(hit.is_none() && (!c.is_group() || c.group_editable()));
        if let Some(idx) = self.selected.filter(|_| c.is_group()) {
            render_group_card(detail, sender, c, idx);
            return;
        }

        // ---- identity header: photo, name, who they are ----
        let avatar = avatar_for(c, 96);
        if let Some(photo) = &c.photo {
            // A real photo expands to the lightbox on click.
            let btn = gtk::Button::new();
            btn.set_child(Some(&avatar));
            btn.add_css_class("flat");
            btn.add_css_class("circular");
            btn.set_halign(gtk::Align::Center);
            btn.set_tooltip_text(Some(i18n("View photo").as_str()));
            let s = sender.input_sender().clone();
            let name = c.name.clone();
            let data = photo.clone();
            btn.connect_clicked(move |_| {
                let _ = s.send(ContactsPageInput::OpenPhoto {
                    name: name.clone(),
                    data: data.clone(),
                });
            });
            detail.append(&btn);
        } else {
            avatar.set_halign(gtk::Align::Center);
            detail.append(&avatar);
        }

        let name = gtk::Label::new(Some(&c.name));
        name.add_css_class("title-1");
        name.set_wrap(true);
        name.set_justify(gtk::Justification::Center);
        name.set_margin_top(12);
        detail.append(&name);

        let mut byline = Vec::new();
        if !c.nickname.is_empty() && c.nickname != c.name {
            byline.push(format!("“{}”", c.nickname));
        }
        match (!c.title.is_empty(), !c.org.is_empty()) {
            (true, true) => byline.push(format!("{} · {}", c.title, c.org)),
            (true, false) => byline.push(c.title.clone()),
            (false, true) => byline.push(c.org.clone()),
            (false, false) => {}
        }
        if !byline.is_empty() {
            let sub = gtk::Label::new(Some(&byline.join("  —  ")));
            sub.add_css_class("dim-label");
            sub.set_wrap(true);
            sub.set_justify(gtk::Justification::Center);
            sub.set_margin_top(4);
            detail.append(&sub);
        }

        // From a directory: it can be kept as a contact, not edited here.
        if let Some(idx) = hit {
            let add = gtk::Button::with_label(&i18n("Add to Contacts"));
            add.add_css_class("pill");
            add.set_halign(gtk::Align::Center);
            add.set_margin_top(12);
            let s = sender.input_sender().clone();
            add.connect_clicked(move |b| {
                b.set_sensitive(false);
                let _ = s.send(ContactsPageInput::AddDirectoryHit(idx));
            });
            detail.append(&add);
        }

        // ---- field groups ----
        if !c.emails.is_empty() {
            let list = group(detail, &i18n("Email"));
            for e in &c.emails {
                let row = labeled_row(e);
                let compose = flat_button(
                    "mail-message-new-symbolic",
                    &i18n("New message"),
                );
                let s = sender.input_sender().clone();
                let addr = e.value.clone();
                compose.connect_clicked(move |_| {
                    let _ = s.send(ContactsPageInput::Compose(addr.clone()));
                });
                row.add_suffix(&copy_button(&e.value));
                row.add_suffix(&compose);
                row.set_activatable(true);
                let s = sender.input_sender().clone();
                let addr = e.value.clone();
                row.connect_activated(move |_| {
                    let _ = s.send(ContactsPageInput::Compose(addr.clone()));
                });
                list.append(&row);
            }
        }

        if !c.phones.is_empty() {
            let list = group(detail, &i18n("Phone"));
            for p in &c.phones {
                let row = labeled_row(p);
                row.add_suffix(&copy_button(&p.value));
                list.append(&row);
            }
        }

        if !c.addresses.is_empty() {
            let list = group(detail, &i18n("Address"));
            for a in &c.addresses {
                let row = labeled_row(a);
                row.add_suffix(&copy_button(&a.value));
                list.append(&row);
            }
        }

        if !c.urls.is_empty() {
            let list = group(detail, &i18n("Website"));
            for url in &c.urls {
                let row = adw::ActionRow::new();
                row.set_title(&gtk::glib::markup_escape_text(url));
                row.set_activatable(true);
                let icon =
                    gtk::Image::from_icon_name("adw-external-link-symbolic");
                icon.add_css_class("dim-label");
                row.add_suffix(&icon);
                let s = sender.input_sender().clone();
                let url = url.clone();
                row.connect_activated(move |_| {
                    let _ = s.send(ContactsPageInput::OpenUrl(url.clone()));
                });
                list.append(&row);
            }
        }

        let mut extras: Vec<Labeled> = Vec::new();
        if !c.birthday.is_empty() {
            extras.push(Labeled { label: i18n("Birthday"), value: c.birthday.clone() });
        }
        if !c.note.is_empty() {
            extras.push(Labeled { label: i18n("Note"), value: c.note.clone() });
        }
        // Where this entry lives: the local book, a synced account, or the
        // directory it was found in.
        if hit.is_some() {
            extras.push(Labeled { label: i18n("Directory"), value: c.book_name.clone() });
        } else if !c.book_name.is_empty() {
            extras.push(Labeled { label: i18n("Address Book"), value: c.book_name.clone() });
        }
        if !extras.is_empty() {
            let list = group(detail, &i18n("Details"));
            for x in &extras {
                let row = adw::ActionRow::new();
                row.set_title(&gtk::glib::markup_escape_text(&x.label));
                row.set_subtitle(&gtk::glib::markup_escape_text(&x.value));
                row.add_css_class("property");
                list.append(&row);
            }
        }
    }

    /// Open the form for the entry at `idx`: a group's, or a contact's. A
    /// group Hylki cannot write stays as it is.
    fn edit(&mut self, widgets: &ContactsPageWidgets, sender: &ComponentSender<Self>, idx: usize) {
        let Some(c) = self.contacts.get(idx) else { return };
        if c.is_group() && !c.group_editable() {
            return;
        }
        let group = c.is_group();
        self.editing_target = Some(idx);
        if group {
            self.render_group_editor(widgets, sender);
        } else {
            self.render_editor(widgets, sender);
        }
    }

    /// Replace the detail pane with the group form for `editing_target`
    /// (blank for a new group, in a book picked from those that keep
    /// groups).
    fn render_group_editor(&mut self, widgets: &ContactsPageWidgets, sender: &ComponentSender<Self>) {
        let detail = &widgets.detail_box;
        while let Some(child) = detail.first_child() {
            detail.remove(&child);
        }
        let blank = ContactDetails::default();
        let c = self.editing_target.and_then(|i| self.contacts.get(i)).unwrap_or(&blank);
        let heading = if self.editing_target.is_some() { i18n("Edit Group") } else { i18n("New Group") };
        editor_bar(detail, sender, &heading);

        let identity = group(detail, &i18n("Group"));
        // A group stays in its book; a new one goes in any that keeps
        // groups, the Hylki book when none is listed yet.
        let mut books: Vec<crate::contacts::Book> = Vec::new();
        let mut book_row = None;
        let mut apple = c.group == Some(GroupFormat::Apple);
        if self.editing_target.is_none() {
            books = self.books.iter().filter(|b| b.groups.is_some()).cloned().collect();
            if books.is_empty() {
                books.push(crate::contacts::local_book());
            }
            let row = adw::ComboRow::new();
            row.set_title(&i18n("Address book"));
            let labels: Vec<&str> = books.iter().map(|b| b.name.as_str()).collect();
            row.set_model(Some(&gtk::StringList::new(&labels)));
            let at = self
                .book_filter
                .as_ref()
                .and_then(|uid| books.iter().position(|b| b.uid == *uid))
                .unwrap_or(0);
            row.set_selected(at as u32);
            row.set_sensitive(books.len() > 1);
            apple = books[at].groups == Some(GroupFormat::Apple);
            identity.append(&row);
            book_row = Some(row);
        } else {
            let book = adw::ActionRow::new();
            book.set_title(&i18n("Address book"));
            book.set_subtitle(&gtk::glib::markup_escape_text(&c.book_name));
            book.add_css_class("property");
            identity.append(&book);
        }
        let name = entry_row(&i18n("Name"), &c.name);
        name.connect_changed(|row| row.remove_css_class("error"));
        identity.append(&name);
        let hide = adw::SwitchRow::new();
        hide.set_title(&i18n("Send as Bcc"));
        hide.set_subtitle(&i18n("Members do not see each other's addresses."));
        hide.set_active(c.hide_addresses);
        identity.append(&hide);

        // People to add are offered as the composer offers recipients: from
        // Contacts and from the mail sent and received. Until that is read,
        // the contacts here stand in.
        if self.suggestions.borrow().is_empty() {
            let contacts = self.contacts.iter().filter(|p| !p.is_group()).flat_map(|p| {
                p.emails.iter().map(|e| crate::contacts::Suggestion {
                    name: p.name.clone(),
                    email: e.value.clone(),
                    from_contacts: true,
                    from_directory: false,
                    score: 0,
                    own: false,
                    group: None,
                })
            });
            self.suggestions.borrow_mut().extend(contacts);
        }
        let _ = sender.output(ContactsPageOutput::WantSuggestions);
        let members = member_editor(detail, &c.members, self.suggestions.clone());
        // An Apple group (iCloud's) holds cards of its own book.
        let note = gtk::Label::new(Some(&i18n(
            "Groups in this address book hold its own contacts: anyone added who is not in it yet is added to it.",
        )));
        note.add_css_class("dim-label");
        note.add_css_class("caption");
        note.set_wrap(true);
        note.set_xalign(0.0);
        note.set_margin_top(6);
        note.set_visible(apple);
        detail.append(&note);
        if let Some(row) = &book_row {
            let formats: Vec<Option<GroupFormat>> = books.iter().map(|b| b.groups).collect();
            let note = note.downgrade();
            row.connect_selected_notify(move |row| {
                let apple = formats.get(row.selected() as usize) == Some(&Some(GroupFormat::Apple));
                if let Some(note) = note.upgrade() {
                    note.set_visible(apple);
                }
            });
        }
        if self.editing_target.is_none() {
            name.grab_focus();
        }
        self.editor = Some(Editor::Group(GroupForm { name, hide, members, book: book_row, books }));
    }

    /// Replace the detail pane with the editor form for `editing_target`
    /// (blank for a new contact).
    fn render_editor(&mut self, widgets: &ContactsPageWidgets, sender: &ComponentSender<Self>) {
        let detail = &widgets.detail_box;
        while let Some(child) = detail.first_child() {
            detail.remove(&child);
        }
        let blank = ContactDetails::default();
        let c = self
            .editing_target
            .and_then(|i| self.contacts.get(i))
            .unwrap_or(&blank);

        let heading = if self.editing_target.is_some() { i18n("Edit Contact") } else { i18n("New Contact") };
        editor_bar(detail, sender, &heading);

        // ---- identity fields ----
        let identity = group(detail, &i18n("Identity"));
        let name = entry_row(&i18n("Name"), &c.name);
        let nickname = entry_row(&i18n("Nickname"), &c.nickname);
        let org = entry_row(&i18n("Organization"), &c.org);
        let title = entry_row(&i18n("Title"), &c.title);
        identity.append(&name);
        identity.append(&nickname);
        identity.append(&org);
        identity.append(&title);

        // ---- the address book: chosen for a new contact, named for one
        // being edited (a contact stays in its book) ----
        let mut book = None;
        if self.editing_target.is_none() && !self.books.is_empty() {
            let row = adw::ComboRow::new();
            row.set_title(&i18n("Address book"));
            let labels: Vec<&str> = self.books.iter().map(|b| b.name.as_str()).collect();
            row.set_model(Some(&gtk::StringList::new(&labels)));
            // The book the list is narrowed to, else the default (first).
            let at = self
                .book_filter
                .as_ref()
                .and_then(|uid| self.books.iter().position(|b| b.uid == *uid))
                .unwrap_or(0);
            row.set_selected(at as u32);
            row.set_sensitive(self.books.len() > 1);
            identity.prepend(&row);
            book = Some(row);
        } else if !c.book_name.is_empty() {
            let row = adw::ActionRow::new();
            row.set_title(&i18n("Address book"));
            row.set_subtitle(&gtk::glib::markup_escape_text(&c.book_name));
            row.add_css_class("property");
            identity.prepend(&row);
        }

        // ---- repeating value groups ----
        let emails = editable_values(
            detail,
            &i18n("Email"),
            &i18n("Add email"),
            &c.emails.iter().map(|l| (l.value.clone(), l.label.clone())).collect::<Vec<_>>(),
        );
        let phones = editable_values(
            detail,
            &i18n("Phone"),
            &i18n("Add phone"),
            &c.phones.iter().map(|l| (l.value.clone(), l.label.clone())).collect::<Vec<_>>(),
        );
        let urls = editable_values(
            detail,
            &i18n("Website"),
            &i18n("Add website"),
            &c.urls.iter().map(|u| (u.clone(), String::new())).collect::<Vec<_>>(),
        );

        // ---- note ----
        let note_list = group(detail, &i18n("Note"));
        let note = gtk::TextView::new();
        note.set_wrap_mode(gtk::WrapMode::WordChar);
        note.buffer().set_text(&c.note);
        note.set_top_margin(8);
        note.set_bottom_margin(8);
        note.set_left_margin(10);
        note.set_right_margin(10);
        note.set_height_request(72);
        let note_row = gtk::ListBoxRow::new();
        note_row.set_activatable(false);
        note_row.set_child(Some(&note));
        note_list.append(&note_row);

        self.editor = Some(Editor::Contact(ContactForm { name, nickname, org, title, note, emails, phones, urls, book }));
    }
}

/// A group's card: who is in it, and a message to them all.
fn render_group_card(detail: &gtk::Box, sender: &ComponentSender<ContactsPage>, c: &ContactDetails, idx: usize) {
    let avatar = avatar_for(c, 96);
    avatar.set_halign(gtk::Align::Center);
    detail.append(&avatar);

    let name = gtk::Label::new(Some(&c.name));
    name.add_css_class("title-1");
    name.set_wrap(true);
    name.set_justify(gtk::Justification::Center);
    name.set_margin_top(12);
    detail.append(&name);

    let sub = gtk::Label::new(Some(&group_byline(c)));
    sub.add_css_class("dim-label");
    sub.set_wrap(true);
    sub.set_justify(gtk::Justification::Center);
    sub.set_margin_top(4);
    detail.append(&sub);

    if !c.members.is_empty() {
        let write = gtk::Button::with_label(&i18n("New Message"));
        write.add_css_class("pill");
        write.add_css_class("suggested-action");
        write.set_halign(gtk::Align::Center);
        write.set_margin_top(12);
        let s = sender.input_sender().clone();
        write.connect_clicked(move |_| {
            let _ = s.send(ContactsPageInput::ComposeGroup(idx));
        });
        detail.append(&write);
    }

    let list = group(detail, &i18n("Members"));
    for m in &c.members {
        let row = adw::ActionRow::new();
        let named = !m.name.trim().is_empty() && m.name != m.email;
        row.set_title(&gtk::glib::markup_escape_text(if named { &m.name } else { &m.email }));
        if named {
            row.set_subtitle(&gtk::glib::markup_escape_text(&m.email));
        }
        let recipient = crate::worker::format_recipient(&m.name, &m.email);
        let compose = flat_button("mail-message-new-symbolic", &i18n("New message"));
        let s = sender.input_sender().clone();
        let to = recipient.clone();
        compose.connect_clicked(move |_| {
            let _ = s.send(ContactsPageInput::Compose(to.clone()));
        });
        row.add_suffix(&copy_button(&m.email));
        row.add_suffix(&compose);
        row.set_activatable(true);
        let s = sender.input_sender().clone();
        row.connect_activated(move |_| {
            let _ = s.send(ContactsPageInput::Compose(recipient.clone()));
        });
        list.append(&row);
    }
    if c.members.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title(&i18n("No one is in this group yet."));
        row.add_css_class("dim-label");
        list.append(&row);
    }

    let details = group(detail, &i18n("Details"));
    if !c.book_name.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title(&i18n("Address Book"));
        row.set_subtitle(&gtk::glib::markup_escape_text(&c.book_name));
        row.add_css_class("property");
        details.append(&row);
    }
    if !c.group_editable() {
        let row = adw::ActionRow::new();
        row.set_title(&i18n("Changed in the app it came from"));
        row.set_subtitle(&i18n("This group names its members by contact, which Hylki does not edit."));
        row.add_css_class("property");
        details.append(&row);
    }
}

fn group_byline(c: &ContactDetails) -> String {
    crate::contacts::group_reach(c.members.len(), c.hide_addresses)
}

/// The sort and filter button's popover, laid out as the message list's
/// (#441): the orders in one column, the "Show only" filters in the other.
/// It stays open as rows are clicked, so an order and several filters can
/// be set in one visit.
struct FilterMenu {
    button: gtk::MenuButton,
    sorts: Vec<(ContactSort, gtk::CheckButton)>,
    filters: Vec<(ContactFilter, gtk::CheckButton)>,
    clear: gtk::Button,
}

impl FilterMenu {
    fn new(button: &gtk::MenuButton, input: &relm4::Sender<ContactsPageInput>) -> Self {
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
        let mut sorts: Vec<(ContactSort, gtk::CheckButton)> = Vec::new();
        for (label, sort) in [
            (i18n_noop("First name"), ContactSort::FirstName),
            (i18n_noop("Last name"), ContactSort::LastName),
            (i18n_noop("Email"), ContactSort::Email),
        ] {
            let radio = gtk::CheckButton::with_label(&i18n(label));
            if let Some((_, first)) = sorts.first() {
                radio.set_group(Some(first));
            }
            radio.set_active(sort == ContactSort::FirstName);
            let s = input.clone();
            radio.connect_toggled(move |r| {
                if r.is_active() {
                    let _ = s.send(ContactsPageInput::SetSort(sort));
                }
            });
            sort_col.append(&radio);
            sorts.push((sort, radio));
        }
        let filter_col = column(&i18n("Show only"));
        let mut filters = Vec::new();
        for f in ContactFilter::ALL {
            let check = gtk::CheckButton::with_label(&f.label());
            let s = input.clone();
            check.connect_toggled(move |c| {
                let _ = s.send(ContactsPageInput::SetFilter(f, c.is_active()));
            });
            filter_col.append(&check);
            filters.push((f, check));
        }
        let clear = gtk::Button::with_label(&i18n("Clear Filters"));
        clear.add_css_class("flat");
        clear.set_halign(gtk::Align::Start);
        clear.set_sensitive(false);
        let s = input.clone();
        clear.connect_clicked(move |_| {
            let _ = s.send(ContactsPageInput::ClearFilters);
        });
        filter_col.append(&clear);

        let columns = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        columns.add_css_class("sort-filter-popover");
        columns.append(&sort_col);
        columns.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        columns.append(&filter_col);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&columns));
        button.set_popover(Some(&popover));
        FilterMenu { button: button.clone(), sorts, filters, clear }
    }

    /// Show the order and the filters on the rows, and tint the button
    /// while any filter is on, so a narrowed list never passes for a short
    /// one.
    fn sync(&self, sort: ContactSort, on: u8) {
        for (s, radio) in &self.sorts {
            if *s == sort && !radio.is_active() {
                radio.set_active(true);
            }
        }
        for (f, check) in &self.filters {
            let active = on & f.bit() != 0;
            if check.is_active() != active {
                check.set_active(active);
            }
        }
        self.clear.set_sensitive(on != 0);
        if on == 0 {
            self.button.remove_css_class("list-filter-active");
            self.button.set_tooltip_text(Some(&i18n("Sort and filter contacts")));
        } else {
            self.button.add_css_class("list-filter-active");
            let names: Vec<String> =
                ContactFilter::ALL.iter().filter(|f| on & f.bit() != 0).map(|f| f.label()).collect();
            self.button.set_tooltip_text(Some(&i18n_f("Filtered: {names}", &[("names", &names.join(", "))])));
        }
    }
}

/// The address book picker beside the search, made as the message list's
/// folder picker: the book's name on the button, a radio per book in its
/// popover. Entry 0 is every book.
struct BookPicker {
    label: gtk::Label,
    list: gtk::Box,
    radios: RefCell<Vec<gtk::CheckButton>>,
    popover: gtk::Popover,
    input: relm4::Sender<ContactsPageInput>,
    /// Set while the radios are made or moved here, not by a click.
    quiet: Rc<Cell<bool>>,
}

impl BookPicker {
    fn new(button: &gtk::MenuButton, input: &relm4::Sender<ContactsPageInput>) -> Self {
        let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let label = gtk::Label::new(Some(&i18n("All Address Books")));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(14);
        face.append(&label);
        face.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
        button.set_child(Some(&face));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("search-scope-popover");
        let popover = gtk::Popover::new();
        popover.set_child(Some(&list));
        button.set_popover(Some(&popover));
        BookPicker {
            label,
            list,
            radios: RefCell::new(Vec::new()),
            popover,
            input: input.clone(),
            quiet: Rc::new(Cell::new(false)),
        }
    }

    /// Offer `labels` (every book first), `at` picked.
    fn set_books(&self, labels: &[&str], at: usize) {
        self.quiet.set(true);
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let mut radios: Vec<gtk::CheckButton> = Vec::new();
        for (i, name) in labels.iter().enumerate() {
            let radio = gtk::CheckButton::with_label(name);
            if let Some(first) = radios.first() {
                radio.set_group(Some(first));
            }
            radio.set_active(i == at);
            let (s, popover, quiet) = (self.input.clone(), self.popover.clone(), self.quiet.clone());
            radio.connect_toggled(move |r| {
                if r.is_active() && !quiet.get() {
                    let _ = s.send(ContactsPageInput::FilterBook(i as u32));
                    popover.popdown();
                }
            });
            self.list.append(&radio);
            radios.push(radio);
        }
        self.label.set_label(labels.get(at).copied().unwrap_or_default());
        self.radios.replace(radios);
        self.quiet.set(false);
    }

    /// Pick entry `at`, as if clicked.
    fn pick(&self, at: usize) {
        let radios = self.radios.borrow();
        if let Some(radio) = radios.get(at) {
            if radio.is_active() {
                return;
            }
            radio.set_active(true);
            self.label.set_label(&radio.label().unwrap_or_default());
        }
    }
}

/// The header's "+": a contact or a group.
fn new_menu(button: &gtk::Button, sender: &ComponentSender<ContactsPage>) {
    let (sc, sg) = (sender.input_sender().clone(), sender.input_sender().clone());
    show_context_menu(
        button,
        (button.width() / 2) as f64,
        button.height() as f64,
        vec![vec![
            MenuEntry::new(i18n("New Contact"), move || {
                let _ = sc.send(ContactsPageInput::NewContact);
            })
            .icon("contact-new-symbolic"),
            MenuEntry::new(i18n("New Group"), move || {
                let _ = sg.send(ContactsPageInput::NewGroup);
            })
            .icon("system-users-symbolic"),
        ]],
    );
}

/// Whether a contact matches a search, already lower case; an empty one
/// matches everyone.
fn matches_query(c: &ContactDetails, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    c.name.to_lowercase().contains(q)
        || c.nickname.to_lowercase().contains(q)
        || c.org.to_lowercase().contains(q)
        || c.emails.iter().any(|e| e.value.to_lowercase().contains(q))
        || c.phones.iter().any(|p| p.value.to_lowercase().contains(q))
        || c.members.iter().any(|m| m.name.to_lowercase().contains(q) || m.email.to_lowercase().contains(q))
}

/// Ask for `.vcf` files to import into the Hylki book, and hand their paths
/// to `done` (never called for a cancelled dialog or no files).
pub fn choose_vcf_files(parent: Option<&gtk::Window>, done: impl Fn(Vec<std::path::PathBuf>) + 'static) {
    let dialog = gtk::FileDialog::builder().title(&i18n("Import Contacts")).build();
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&i18n("vCard files")));
    filter.add_pattern("*.vcf");
    filter.add_pattern("*.vcard");
    filter.add_mime_type("text/vcard");
    filter.add_mime_type("text/x-vcard");
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let any = gtk::FileFilter::new();
    any.set_name(Some(&i18n("All files")));
    any.add_pattern("*");
    filters.append(&any);
    dialog.set_filters(Some(&filters));
    dialog.open_multiple(parent, gtk::gio::Cancellable::NONE, move |res| {
        let Ok(files) = res else { return };
        let paths: Vec<std::path::PathBuf> =
            files.iter::<gtk::gio::File>().flatten().filter_map(|f| f.path()).collect();
        if !paths.is_empty() {
            done(paths);
        }
    });
}

/// The Cancel · title · Save bar over a form (dialog-header layout).
fn editor_bar(detail: &gtk::Box, sender: &ComponentSender<ContactsPage>, heading: &str) {
    let bar = gtk::CenterBox::new();
    let cancel = gtk::Button::with_label(&i18n("Cancel"));
    let s = sender.input_sender().clone();
    cancel.connect_clicked(move |_| {
        let _ = s.send(ContactsPageInput::CancelEdit);
    });
    let save = gtk::Button::with_label(&i18n("Save"));
    save.add_css_class("suggested-action");
    let s = sender.input_sender().clone();
    save.connect_clicked(move |_| {
        let _ = s.send(ContactsPageInput::SaveEdit);
    });
    let heading = gtk::Label::new(Some(heading));
    heading.add_css_class("title-4");
    bar.set_start_widget(Some(&cancel));
    bar.set_center_widget(Some(&heading));
    bar.set_end_widget(Some(&save));
    detail.append(&bar);
}

/// The group form's members: a row each with a remove button, and a row to
/// add someone, by an address or by picking from `people` (the composer's
/// suggestions) as a name or address is typed. Returns the live list read at
/// save time.
fn member_editor(
    detail: &gtk::Box,
    initial: &[Member],
    people: Rc<RefCell<Vec<crate::contacts::Suggestion>>>,
) -> Rc<RefCell<Vec<Member>>> {
    let list = group(detail, &i18n("Members"));
    let members: Rc<RefCell<Vec<Member>>> = Rc::new(RefCell::new(Vec::new()));
    let entry = adw::EntryRow::new();
    entry.set_title(&i18n("Add a person by name or address"));
    entry.set_show_apply_button(true);
    list.append(&entry);
    // The people matching what is typed, under the members.
    let found = gtk::ListBox::new();
    found.add_css_class("boxed-list");
    found.add_css_class("member-matches");
    found.set_selection_mode(gtk::SelectionMode::Single);
    found.set_can_focus(false);
    found.set_margin_top(6);
    found.set_visible(false);
    detail.append(&found);

    // Widgets are held weakly in the handlers they own, so the form is
    // freed when the pane is cleared.
    let add: Rc<dyn Fn(Member)> = {
        let (list, members, entry) = (list.downgrade(), members.clone(), entry.downgrade());
        Rc::new(move |m: Member| {
            let (Some(list), Some(entry)) = (list.upgrade(), entry.upgrade()) else { return };
            if members.borrow().iter().any(|x| x.email.eq_ignore_ascii_case(&m.email)) {
                return;
            }
            let row = adw::ActionRow::new();
            let named = !m.name.trim().is_empty() && m.name != m.email;
            row.set_title(&gtk::glib::markup_escape_text(if named { &m.name } else { &m.email }));
            if named {
                row.set_subtitle(&gtk::glib::markup_escape_text(&m.email));
            }
            let remove = flat_button("user-trash-symbolic", &i18n("Remove"));
            {
                let (list, row, members, email) = (list.downgrade(), row.downgrade(), members.clone(), m.email.clone());
                remove.connect_clicked(move |_| {
                    members.borrow_mut().retain(|x| !x.email.eq_ignore_ascii_case(&email));
                    if let (Some(list), Some(row)) = (list.upgrade(), row.upgrade()) {
                        list.remove(&row);
                    }
                });
            }
            row.add_suffix(&remove);
            list.insert(&row, entry.index());
            members.borrow_mut().push(m);
        })
    };
    for m in initial {
        add(m.clone());
    }

    // Who matches the typing, by name or address and not already in,
    // ranked as the composer ranks them.
    let matches = {
        let (people, members) = (people.clone(), members.clone());
        move |typed: &str| -> Vec<crate::contacts::Suggestion> {
            let q = typed.trim();
            if q.is_empty() {
                return Vec::new();
            }
            let taken: std::collections::HashSet<String> =
                members.borrow().iter().map(|m| m.email.to_lowercase()).collect();
            let mut found: Vec<crate::contacts::Suggestion> = people
                .borrow()
                .iter()
                .filter(|s| s.group.is_none() && !taken.contains(&s.email.to_lowercase()) && s.matches(q))
                .cloned()
                .collect();
            crate::contacts::rank(&mut found, q);
            found.truncate(6);
            found
        }
    };
    let matches = Rc::new(matches);
    // The matches listed, and the one Return takes: the first while a name
    // is typed, none while the text is an address (Return adds what was
    // typed), whichever the arrow keys move to.
    let shown: Rc<RefCell<Vec<crate::contacts::Suggestion>>> = Rc::new(RefCell::new(Vec::new()));
    let chosen: Rc<std::cell::Cell<Option<usize>>> = Rc::new(std::cell::Cell::new(None));
    let highlight = {
        let (found, chosen) = (found.downgrade(), chosen.clone());
        move |at: Option<usize>| {
            chosen.set(at);
            let Some(found) = found.upgrade() else { return };
            match at.and_then(|i| found.row_at_index(i as i32)) {
                Some(row) => found.select_row(Some(&row)),
                None => found.unselect_all(),
            }
        }
    };
    let highlight = Rc::new(highlight);
    // Add a match as a member and start the field over.
    let take = {
        let add = add.clone();
        move |sug: &crate::contacts::Suggestion, entry: &adw::EntryRow| {
            // History may know an address without a name.
            let name = if sug.name == sug.email { String::new() } else { sug.name.clone() };
            add(Member { name, email: sug.email.clone(), ..Member::default() });
            entry.set_text("");
            entry.grab_focus();
        }
    };
    let take = Rc::new(take);
    {
        let (found, matches, shown, highlight, take) =
            (found.downgrade(), matches.clone(), shown.clone(), highlight.clone(), take.clone());
        entry.connect_changed(move |e| {
            let Some(found) = found.upgrade() else { return };
            while let Some(child) = found.first_child() {
                found.remove(&child);
            }
            let text = e.text();
            *shown.borrow_mut() = matches(&text);
            for sug in shown.borrow().iter() {
                let row = adw::ActionRow::new();
                row.set_title(&gtk::glib::markup_escape_text(&sug.name));
                if sug.name != sug.email {
                    row.set_subtitle(&gtk::glib::markup_escape_text(&sug.email));
                }
                // Where the address came from, as in the composer: the
                // address book or mail history.
                let icon = gtk::Image::from_icon_name(if sug.from_contacts {
                    "avatar-default-symbolic"
                } else {
                    "document-open-recent-symbolic"
                });
                icon.add_css_class("dim-label");
                row.add_prefix(&icon);
                row.set_activatable(true);
                // The typing stays in the field while the arrows move here.
                row.set_can_focus(false);
                let (take, entry, sug) = (take.clone(), e.downgrade(), sug.clone());
                row.connect_activated(move |_| {
                    if let Some(entry) = entry.upgrade() {
                        take(&sug, &entry);
                    }
                });
                found.append(&row);
            }
            let any = found.first_child().is_some();
            found.set_visible(any);
            let address = !typed_addresses(&text).is_empty();
            highlight((any && !address).then_some(0));
        });
    }
    // Down and Up move through the matches; Up from the first goes back to
    // what was typed. Escape puts the list away.
    {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let (found, shown, chosen, highlight) = (found.downgrade(), shown.clone(), chosen.clone(), highlight.clone());
        keys.connect_key_pressed(move |_, keyval, _, _| {
            use gtk::glib::Propagation;
            let Some(found) = found.upgrade().filter(|f| f.is_visible()) else { return Propagation::Proceed };
            let last = shown.borrow().len().saturating_sub(1);
            if keyval == gtk::gdk::Key::Down || keyval == gtk::gdk::Key::KP_Down {
                highlight(Some(chosen.get().map_or(0, |i| (i + 1).min(last))));
            } else if keyval == gtk::gdk::Key::Up || keyval == gtk::gdk::Key::KP_Up {
                highlight(chosen.get().and_then(|i| i.checked_sub(1)));
            } else if keyval == gtk::gdk::Key::Escape {
                highlight(None);
                found.set_visible(false);
            } else {
                return Propagation::Proceed;
            }
            Propagation::Stop
        });
        entry.add_controller(keys);
    }
    // Return or the apply button: the match picked, else the addresses typed
    // or pasted.
    let commit = move |e: &adw::EntryRow| {
        let picked = chosen.get().and_then(|i| shown.borrow().get(i).cloned());
        if let Some(sug) = picked.filter(|_| found.is_visible()) {
            take(&sug, e);
            return;
        }
        let typed = typed_addresses(&e.text());
        if typed.is_empty() {
            e.add_css_class("error");
            return;
        }
        for (name, email) in typed {
            // An address typed bare keeps the name Contacts or mail has for it.
            let name = if name.is_empty() || name == email {
                people
                    .borrow()
                    .iter()
                    .find(|s| s.email.eq_ignore_ascii_case(&email) && s.name != s.email)
                    .map(|s| s.name.clone())
                    .unwrap_or_default()
            } else {
                name
            };
            add(Member { name, email, ..Member::default() });
        }
        e.set_text("");
    };
    let commit = Rc::new(commit);
    {
        let commit = commit.clone();
        entry.connect_apply(move |e| commit(e));
    }
    entry.connect_entry_activated(move |e| commit(e));
    entry.connect_changed(|e| e.remove_css_class("error"));
    members
}

/// The whole addresses in a typed or pasted text, as (name, address).
fn typed_addresses(text: &str) -> Vec<(String, String)> {
    crate::worker::parse_recipients(text)
        .into_iter()
        .filter(|(_, email)| email.contains('@') && crate::worker::invalid_recipient(email).is_none())
        .collect()
}

/// Read the editor form back into a `ContactEdit`.
fn collect_edit(e: &ContactForm) -> ContactEdit {
    let values = |rows: &Rc<RefCell<Vec<(adw::EntryRow, String)>>>| -> Vec<Labeled> {
        rows.borrow()
            .iter()
            .filter_map(|(entry, label)| {
                let value = entry.text().trim().to_string();
                (!value.is_empty()).then(|| Labeled { label: label.clone(), value })
            })
            .collect()
    };
    let buffer = e.note.buffer();
    ContactEdit {
        name: e.name.text().trim().to_string(),
        nickname: e.nickname.text().trim().to_string(),
        org: e.org.text().trim().to_string(),
        title: e.title.text().trim().to_string(),
        note: buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .trim()
            .to_string(),
        emails: values(&e.emails),
        phones: values(&e.phones),
        urls: values(&e.urls).into_iter().map(|l| l.value).collect(),
    }
}

/// A titled group of editable value rows with a remove button each and an
/// "add" row at the bottom. Returns the live handle used at save time.
fn editable_values(
    detail: &gtk::Box,
    title: &str,
    add_label: &str,
    initial: &[(String, String)],
) -> Rc<RefCell<Vec<(adw::EntryRow, String)>>> {
    let list = group(detail, title);
    let rows: Rc<RefCell<Vec<(adw::EntryRow, String)>>> = Rc::new(RefCell::new(Vec::new()));

    // The "add" row sits last; value rows are inserted above it.
    let add_row = gtk::ListBoxRow::new();
    add_row.set_activatable(true);
    let add_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    add_box.set_margin_top(10);
    add_box.set_margin_bottom(10);
    add_box.set_margin_start(12);
    let plus = gtk::Image::from_icon_name("list-add-symbolic");
    plus.add_css_class("dim-label");
    let label = gtk::Label::new(Some(add_label));
    label.add_css_class("dim-label");
    add_box.append(&plus);
    add_box.append(&label);
    add_row.set_child(Some(&add_box));

    let field = title.to_string();
    let add_value = {
        let list = list.clone();
        let rows = rows.clone();
        let add_row = add_row.clone();
        move |value: &str, label: String, grab: bool| {
            let entry = adw::EntryRow::new();
            entry.set_title(&if label.is_empty() { field.clone() } else { label.clone() });
            entry.set_text(value);
            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.set_tooltip_text(Some(i18n("Remove").as_str()));
            remove.set_valign(gtk::Align::Center);
            remove.add_css_class("flat");
            {
                let list = list.clone();
                let rows = rows.clone();
                let entry = entry.clone();
                remove.connect_clicked(move |_| {
                    rows.borrow_mut().retain(|(e, _)| e != &entry);
                    list.remove(&entry);
                });
            }
            entry.add_suffix(&remove);
            // Insert above the trailing "add" row.
            list.insert(&entry, add_row.index());
            rows.borrow_mut().push((entry.clone(), label));
            if grab {
                entry.grab_focus();
            }
        }
    };

    for (value, label) in initial {
        add_value(value, label.clone(), false);
    }
    {
        let add_value = add_value.clone();
        let add_row = add_row.clone();
        list.connect_row_activated(move |_, row| {
            if row == &add_row {
                add_value("", String::new(), true);
            }
        });
    }
    list.append(&add_row);
    rows
}

fn entry_row(title: &str, value: &str) -> adw::EntryRow {
    let row = adw::EntryRow::new();
    row.set_title(title);
    row.set_text(value);
    row
}

/// One line of the contact list.
struct Line {
    /// The entry the line shows (see `row_map`); `None` for one that is
    /// nobody and cannot be selected. Moves when a contact before it is
    /// deleted.
    idx: Cell<Option<usize>>,
    title: String,
    subtitle: String,
    kind: LineKind,
}

/// Lines are alike when they show the same thing, wherever they are.
impl PartialEq for Line {
    fn eq(&self, other: &Self) -> bool {
        self.title == other.title && self.subtitle == other.subtitle && self.kind == other.kind
    }
}

#[derive(PartialEq)]
enum LineKind {
    /// A person or a group, with their photo's key (0: none).
    Entry { photo_key: u64, group: bool },
    /// The heading above the people found in the directories.
    Heading,
    /// A directory that did not answer the search.
    Failure,
    /// A dim line in place of an empty list.
    Note,
}

impl Line {
    fn entry(idx: usize, c: &ContactDetails, subtitle: String) -> Self {
        Line {
            idx: Cell::new(Some(idx)),
            title: c.name.clone(),
            subtitle,
            kind: LineKind::Entry { photo_key: line_photo_key(c), group: c.is_group() },
        }
    }

    fn other(kind: LineKind, title: String, subtitle: String) -> Self {
        Line { idx: Cell::new(None), title, subtitle, kind }
    }
}

/// The key a contact's photo is decoded under. A read of the address books
/// works it out off the main thread; a directory entry's is worked out here.
fn line_photo_key(c: &ContactDetails) -> u64 {
    match (&c.photo, c.photo_key) {
        (Some(photo), 0) => crate::contacts::photo_key(photo),
        (_, key) => key,
    }
}

/// The widgets of one row, which the list view binds to line after line.
/// Kept light, since the view holds a couple of hundred: the face is an
/// image in a clipped disc rather than an `adw::Avatar`, and the parts only
/// a heading, a note or a group needs are made when one first shows here.
struct ListRow {
    root: gtk::Box,
    header: gtk::Box,
    /// The circle the face is drawn in; its rounded corners clip a photo.
    disc: gtk::Box,
    face: gtk::Image,
    warning: gtk::Image,
    title: gtk::Label,
    subtitle: gtk::Label,
    /// For a group, or a name with no letter to draw: libadwaita's avatar.
    avatar: RefCell<Option<adw::Avatar>>,
    /// The heading or note text, for those lines.
    banner: RefCell<Option<gtk::Label>>,
    /// The line bound now, for the right-click.
    line: RefCell<Option<gtk::glib::BoxedAnyObject>>,
    /// The photo the face waits for (0: none).
    waiting: Cell<u64>,
}

/// The size of a list face.
const FACE: i32 = 34;

impl ListRow {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // `header`, as in a list box row: the selected row's text turns
        // white by that class (styles.css).
        // The measures of the `adw::ActionRow` the list had before.
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.add_css_class("header");
        header.set_size_request(-1, 50);
        header.set_margin_start(12);
        header.set_margin_end(12);
        let disc = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        disc.add_css_class("contact-face");
        disc.set_overflow(gtk::Overflow::Hidden);
        disc.set_valign(gtk::Align::Center);
        let face = gtk::Image::new();
        face.set_pixel_size(FACE);
        disc.append(&face);
        let warning = gtk::Image::from_icon_name("dialog-warning-symbolic");
        warning.add_css_class("warning");
        // As wide as a face, so the names line up.
        warning.set_size_request(FACE, -1);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
        text.set_valign(gtk::Align::Center);
        text.set_hexpand(true);
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let subtitle = gtk::Label::new(None);
        subtitle.add_css_class("dim-label");
        subtitle.add_css_class("caption");
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&title);
        text.append(&subtitle);
        header.append(&disc);
        header.append(&warning);
        header.append(&text);
        root.append(&header);
        ListRow {
            root,
            header,
            disc,
            face,
            warning,
            title,
            subtitle,
            avatar: RefCell::new(None),
            banner: RefCell::new(None),
            line: RefCell::new(None),
            waiting: Cell::new(0),
        }
    }

    fn bind(&self, item: &gtk::glib::BoxedAnyObject) {
        let line = item.borrow::<Line>();
        self.line.replace(Some(item.clone()));
        self.waiting.set(0);
        let banner = matches!(line.kind, LineKind::Heading | LineKind::Note);
        self.header.set_visible(!banner);
        self.show_banner(banner.then_some(&line));
        if banner {
            return;
        }
        self.title.set_label(&line.title);
        self.subtitle.set_label(&line.subtitle);
        self.subtitle.set_visible(!line.subtitle.is_empty());
        let failure = matches!(line.kind, LineKind::Failure);
        // A failure says why in full; an address fits one line.
        self.subtitle.set_wrap(failure);
        self.subtitle.set_lines(if failure { 3 } else { 1 });
        self.warning.set_visible(failure);
        self.disc.set_visible(!failure);
        if let LineKind::Entry { photo_key, group } = line.kind {
            let initials = (!group).then(|| crate::ui::initials::InitialsPaintable::for_name(&line.title)).flatten();
            match initials {
                Some(paintable) => {
                    self.face.set_paintable(Some(&paintable));
                    self.face.set_visible(true);
                    self.show_avatar(None);
                }
                None => {
                    self.face.set_visible(false);
                    self.show_avatar(Some((&line.title, group)));
                }
            }
            self.waiting.set(photo_key);
            self.show_photo();
        }
    }

    /// Put the awaited photo in the face, once it is decoded.
    fn show_photo(&self) {
        let key = self.waiting.get();
        if key == 0 {
            return;
        }
        if let Some(texture) = crate::avatar::photo_thumbnail(key) {
            if let Some(texture) = texture {
                self.face.set_paintable(Some(&texture));
                self.face.set_visible(true);
                self.show_avatar(None);
            }
            self.waiting.set(0);
        }
    }

    /// libadwaita's avatar for `(name, group)`, in place of the face; `None`
    /// hides it.
    fn show_avatar(&self, who: Option<(&str, bool)>) {
        let mut avatar = self.avatar.borrow_mut();
        let Some((name, group)) = who else {
            if let Some(a) = avatar.as_ref() {
                a.set_visible(false);
            }
            return;
        };
        let a = avatar.get_or_insert_with(|| {
            let a = adw::Avatar::new(FACE, None, true);
            self.disc.append(&a);
            a
        });
        a.set_text(Some(name));
        a.set_show_initials(!group);
        a.set_icon_name(group.then_some("system-users-symbolic"));
        a.set_visible(true);
    }

    /// A heading's or a note's text in place of the row; `None` hides it.
    fn show_banner(&self, line: Option<&Line>) {
        let mut banner = self.banner.borrow_mut();
        let Some(line) = line else {
            if let Some(b) = banner.as_ref() {
                b.set_visible(false);
            }
            return;
        };
        let label = banner.get_or_insert_with(|| {
            let label = gtk::Label::new(None);
            label.add_css_class("dim-label");
            self.root.append(&label);
            label
        });
        let heading = matches!(line.kind, LineKind::Heading);
        label.set_label(&line.title);
        label.set_css_classes(if heading { &["heading", "dim-label"] } else { &["dim-label"] });
        label.set_xalign(if heading { 0.0 } else { 0.5 });
        label.set_wrap(!heading);
        label.set_justify(if heading { gtk::Justification::Left } else { gtk::Justification::Center });
        label.set_margin_top(if heading { 12 } else { 24 });
        label.set_margin_bottom(if heading { 4 } else { 0 });
        label.set_margin_start(12);
        label.set_margin_end(12);
        label.set_visible(true);
    }

    /// The entry the bound line shows.
    fn idx(&self) -> Option<usize> {
        self.line.borrow().as_ref().and_then(|item| item.borrow::<Line>().idx.get())
    }
}

/// The list view's rows: made for the lines on screen and bound to each
/// line scrolled to.
fn row_factory(
    rows: &Rc<RefCell<HashMap<usize, Rc<ListRow>>>>,
    input: &relm4::Sender<ContactsPageInput>,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let key = |obj: &gtk::glib::Object| obj.as_ptr() as usize;
    {
        let (rows, input) = (Rc::downgrade(rows), input.clone());
        factory.connect_setup(move |_, obj| {
            let (Some(rows), Some(item)) = (rows.upgrade(), obj.downcast_ref::<gtk::ListItem>()) else {
                return;
            };
            let row = Rc::new(ListRow::new());
            // Right-click: the quick actions on this entry, built when asked.
            let right_click = gtk::GestureClick::new();
            right_click.set_button(3);
            let (weak, input) = (Rc::downgrade(&row), input.clone());
            right_click.connect_pressed(move |gesture, _, x, y| {
                let (Some(row), Some(widget)) = (weak.upgrade(), gesture.widget()) else { return };
                let Some(idx) = row.idx() else { return };
                let Some(list) = widget.ancestor(gtk::ListView::static_type()) else { return };
                let Some(at) = widget.compute_point(&list, &gtk::graphene::Point::new(x as f32, y as f32)) else {
                    return;
                };
                let _ = input.send(ContactsPageInput::RowMenu { idx, x: at.x().into(), y: at.y().into() });
            });
            row.root.add_controller(right_click);
            item.set_child(Some(&row.root));
            rows.borrow_mut().insert(key(obj), row);
        });
    }
    {
        let rows = Rc::downgrade(rows);
        factory.connect_bind(move |_, obj| {
            let Some(rows) = rows.upgrade() else { return };
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
            let Some(line) = item.item().and_downcast::<gtk::glib::BoxedAnyObject>() else { return };
            let someone = line.borrow::<Line>().idx.get().is_some();
            item.set_selectable(someone);
            item.set_activatable(someone);
            let row = rows.borrow().get(&key(obj)).cloned();
            if let Some(row) = row {
                row.bind(&line);
            }
        });
    }
    {
        let rows = Rc::downgrade(rows);
        factory.connect_teardown(move |_, obj| {
            if let Some(rows) = rows.upgrade() {
                rows.borrow_mut().remove(&key(obj));
            }
        });
    }
    factory
}

/// Whether a fresh read holds what the list already shows. The stored vCard
/// carries everything parsed from it, the photo key a photo kept in a file.
/// Its REV is left out: some books (Microsoft 365's Recent Contacts) stamp
/// every card anew at every sync.
fn same_contacts(shown: &[ContactDetails], read: &[ContactDetails]) -> bool {
    fn card(c: &ContactDetails) -> impl Iterator<Item = &str> {
        c.raw_vcard.lines().filter(|l| !l.starts_with("REV:") && !l.starts_with("REV;"))
    }
    shown.len() == read.len()
        && shown.iter().zip(read).all(|(a, b)| {
            a.book_uid == b.book_uid
                && a.book_name == b.book_name
                && a.eds_uid == b.eds_uid
                && a.name == b.name
                && a.photo_key == b.photo_key
                && card(a).eq(card(b))
        })
}

/// The contact's avatar: their GNOME Contacts photo when they have one, the
/// usual initials disc when not.
fn avatar_for(c: &ContactDetails, size: i32) -> adw::Avatar {
    let avatar = adw::Avatar::new(size, Some(&c.name), !c.is_group());
    if c.is_group() {
        avatar.set_icon_name(Some("system-users-symbolic"));
    }
    if let Some(photo) = &c.photo {
        let bytes = gtk::glib::Bytes::from(photo.as_slice());
        if let Ok(texture) = gtk::gdk::Texture::from_bytes(&bytes) {
            avatar.set_custom_image(Some(&texture));
        }
    }
    avatar
}

/// A titled boxed list appended to the detail column; returns the list.
fn group(detail: &gtk::Box, title: &str) -> gtk::ListBox {
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("heading");
    heading.set_halign(gtk::Align::Start);
    heading.set_margin_top(20);
    heading.set_margin_bottom(6);
    detail.append(&heading);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    detail.append(&list);
    list
}

/// A value row with its (optional) "Home"/"Work" label as the subtitle.
fn labeled_row(l: &Labeled) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&gtk::glib::markup_escape_text(&l.value));
    if !l.label.is_empty() {
        row.set_subtitle(&gtk::glib::markup_escape_text(&l.label));
    }
    row
}

fn flat_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b.add_css_class("flat");
    b
}

/// A suffix button that puts `text` on the clipboard, confirming by briefly
/// swapping its icon for a checkmark.
fn copy_button(text: &str) -> gtk::Button {
    let b = flat_button("edit-copy-symbolic", "Copy");
    let text = text.to_string();
    b.connect_clicked(move |b| {
        b.clipboard().set_text(&text);
        b.set_icon_name("verified-checkmark-symbolic");
        let b = b.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
            b.set_icon_name("edit-copy-symbolic");
        });
    });
    b
}

#[cfg(test)]
mod tests {
    use super::same_contacts;
    use crate::contacts::ContactDetails;

    fn contact(vcard: &str) -> ContactDetails {
        ContactDetails {
            name: "Ann".into(),
            book_uid: "book".into(),
            eds_uid: "ann".into(),
            raw_vcard: vcard.into(),
            ..ContactDetails::default()
        }
    }

    /// A sync that only stamps the cards anew leaves the list as it is; any
    /// real change, a new photo among them, has it listed again (#477).
    #[test]
    fn a_reread_that_only_moves_rev_is_the_same_list() {
        let card = |rev: &str, tel: &str| format!("BEGIN:VCARD\r\nFN:Ann\r\nREV:{rev}\r\nTEL:{tel}\r\nEND:VCARD");
        let shown = vec![contact(&card("2026-10-18T12:23:02Z", "1"))];
        assert!(same_contacts(&shown, &[contact(&card("2026-10-18T12:25:15Z", "1"))]));
        assert!(!same_contacts(&shown, &[contact(&card("2026-10-18T12:25:15Z", "2"))]));
        let mut photo = contact(&card("2026-10-18T12:23:02Z", "1"));
        photo.photo_key = 7;
        assert!(!same_contacts(&shown, &[photo]));
        assert!(!same_contacts(&shown, &[]));
    }
}
