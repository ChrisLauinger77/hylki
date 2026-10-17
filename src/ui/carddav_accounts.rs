//! Settings → CardDAV Accounts (#408): address books on a CardDAV server,
//! set up without GNOME Online Accounts or Evolution. They are Evolution
//! Data Server collections, so this page lists the ones Evolution made as
//! well, and the editor writes the same source Evolution would.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::carddav::{self, Account, MailOffer, Status};
use crate::i18n::{i18n, i18n_f, ni18n_f};

pub struct CardDavAccounts {
    accounts: Vec<(Account, Status)>,
    /// Mail accounts whose address books can be added in one go.
    offers: Vec<MailOffer>,
    offers_group: adw::PreferencesGroup,
    offers_list: gtk::ListBox,
    list: gtk::ListBox,
    empty: gtk::Label,
    add_btn: gtk::Button,
    toasts: adw::ToastOverlay,
    nav: adw::NavigationView,
    editor_page: adw::NavigationPage,
    editor_slot: adw::Bin,
    remove_btn: gtk::Button,
    /// Which account the editor shows (none for a new one).
    editing: Option<usize>,
    /// What the editor's Save does: reads the form and sends `Save`;
    /// answers whether it accepted the form.
    save_action: Rc<RefCell<Option<Rc<dyn Fn() -> bool>>>>,
}

#[derive(Debug)]
pub enum CardDavInput {
    /// Ask EDS for the accounts again.
    Load,
    Loaded(Result<(Vec<(Account, Status)>, Vec<MailOffer>), String>),
    Add,
    /// Add the address books of the mail account offered at this index.
    AddOffer(usize),
    Edit(usize),
    ToggleEnabled { index: usize, enabled: bool },
    RemoveCurrent,
    ConfirmRemove(usize),
    Remove(usize),
    SaveClicked,
    CloseEditor,
    Save { account: Account, password: String },
    /// The keyring has no password for the offered mail account.
    OfferNeedsPassword(MailOffer),
    /// A save or removal went through (or not); `found` is a saved
    /// account as it stood once EDS had looked at the server.
    Done { error: Option<String>, found: Option<(Account, Status)> },
}

#[derive(Debug)]
pub enum CardDavOutput {
    /// The editor page is up (or gone): the settings window hides its
    /// shared header meanwhile and asks before moving on.
    EditorOpen(bool),
    /// An account's address books changed: Contacts reads them again.
    BooksChanged,
}

#[relm4::component(pub)]
impl SimpleComponent for CardDavAccounts {
    type Init = ();
    type Input = CardDavInput;
    type Output = CardDavOutput;

    view! {
        adw::Bin {
            #[wrap(Some)]
            #[name = "nav"]
            set_child = &adw::NavigationView {
                add = &adw::NavigationPage {
                    set_title: &i18n("CardDAV Accounts"),
                    set_tag: Some("list"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        #[wrap(Some)]
                        #[name = "toasts"]
                        set_content = &adw::ToastOverlay {
                            #[wrap(Some)]
                            set_child = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("CardDAV accounts"),
                                    set_description: Some(&i18n("The address books on these servers are listed in Contacts and suggested in the composer, and changes made in Hylki are sent back. They are kept by Evolution Data Server, so an account set up in Evolution is listed here too. Google and Microsoft 365 address books are added in GNOME Online Accounts.")),
                                    #[wrap(Some)]
                                    #[name = "add_btn"]
                                    set_header_suffix = &gtk::Button {
                                        set_label: &i18n("Add Account…"),
                                        set_valign: gtk::Align::Start,
                                        connect_clicked => CardDavInput::Add,
                                    },
                                    #[name = "list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                        set_margin_top: 16,
                                        set_visible: false,
                                        connect_row_activated[sender] => move |_, row| {
                                            sender.input(CardDavInput::Edit(row.index() as usize));
                                        },
                                    },
                                    #[name = "empty"]
                                    gtk::Label {
                                        set_label: &i18n("No CardDAV accounts yet."),
                                        add_css_class: "dim-label",
                                        set_wrap: true,
                                        set_margin_top: 12,
                                    },
                                },
                                #[name = "offers_group"]
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("From your mail accounts"),
                                    set_description: Some(&i18n("These mail accounts keep contacts too, under the same sign-in. Adding them uses the password the mail account already has.")),
                                    set_visible: false,
                                    #[name = "offers_list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },
                        },
                    },
                },

                #[name = "editor_page"]
                add = &adw::NavigationPage {
                    set_title: &i18n("CardDAV Account"),
                    set_tag: Some("editor"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        add_top_bar = &adw::HeaderBar {
                            set_show_end_title_buttons: true,
                            pack_end = &gtk::Button {
                                set_label: &i18n("Save"),
                                add_css_class: "suggested-action",
                                connect_clicked => CardDavInput::SaveClicked,
                            },
                            #[name = "remove_btn"]
                            pack_end = &gtk::Button {
                                set_label: &i18n("Remove"),
                                add_css_class: "destructive-action",
                                set_visible: false,
                                connect_clicked => CardDavInput::RemoveCurrent,
                            },
                        },

                        #[wrap(Some)]
                        set_content = &gtk::ScrolledWindow {
                            set_hscrollbar_policy: gtk::PolicyType::Never,
                            #[wrap(Some)]
                            set_child = &adw::Clamp {
                                set_maximum_size: 640,
                                set_tightening_threshold: 480,
                                set_margin_top: 24,
                                set_margin_bottom: 24,
                                set_margin_start: 24,
                                set_margin_end: 24,
                                #[wrap(Some)]
                                #[name = "editor_slot"]
                                set_child = &adw::Bin {},
                            },
                        },
                    },
                },
            },
        }
    }

    fn init(_init: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let widgets = view_output!();
        let model = CardDavAccounts {
            accounts: Vec::new(),
            offers: Vec::new(),
            offers_group: widgets.offers_group.clone(),
            offers_list: widgets.offers_list.clone(),
            list: widgets.list.clone(),
            empty: widgets.empty.clone(),
            add_btn: widgets.add_btn.clone(),
            toasts: widgets.toasts.clone(),
            nav: widgets.nav.clone(),
            editor_page: widgets.editor_page.clone(),
            editor_slot: widgets.editor_slot.clone(),
            remove_btn: widgets.remove_btn.clone(),
            editing: None,
            save_action: Rc::new(RefCell::new(None)),
        };
        {
            let s = sender.output_sender().clone();
            let save_action = model.save_action.clone();
            let slot = widgets.editor_slot.clone();
            widgets.nav.connect_visible_page_notify(move |nav| {
                let editor = nav.visible_page().and_then(|p| p.tag()).is_some_and(|t| t == "editor");
                if !editor {
                    *save_action.borrow_mut() = None;
                    slot.set_child(None::<&gtk::Widget>);
                }
                let _ = s.send(CardDavOutput::EditorOpen(editor));
            });
        }
        // HYLKI_SHOWCASE_EDIT_CARDDAV=<index>|add|offer<index> opens that
        // account's editor, a new one, or presses an offer's button, for a
        // capture (demo only).
        if let Ok(what) = std::env::var("HYLKI_SHOWCASE_EDIT_CARDDAV") {
            if std::env::var_os("HYLKI_DEMO").is_some() {
                let s = sender.input_sender().clone();
                // After the window opens on the page and the page has read
                // the accounts.
                gtk::glib::timeout_add_seconds_local_once(7, move || {
                    let _ = s.send(match what.as_str() {
                        "add" => CardDavInput::Add,
                        o if o.starts_with("offer") => CardDavInput::AddOffer(o[5..].parse().unwrap_or(0)),
                        i => CardDavInput::Edit(i.parse().unwrap_or(0)),
                    });
                });
            }
        }
        // Read each time the page is shown: an account's sign-in may have
        // gone through, or failed, since.
        {
            let s = sender.input_sender().clone();
            root.connect_map(move |_| {
                let _ = s.send(CardDavInput::Load);
            });
        }
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            CardDavInput::Load => {
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let listed = carddav::list().map(|accounts| {
                        let offers = carddav::mail_offers(&accounts);
                        let accounts = accounts
                            .into_iter()
                            .map(|a| {
                                let status = carddav::status(&a);
                                (a, status)
                            })
                            .collect();
                        (accounts, offers)
                    });
                    let _ = s.send(CardDavInput::Loaded(listed));
                });
            }
            CardDavInput::Loaded(result) => {
                match result {
                    Ok((accounts, offers)) => {
                        self.accounts = accounts;
                        self.offers = offers;
                        self.add_btn.set_sensitive(true);
                        self.empty.set_label(&i18n("No CardDAV accounts yet."));
                    }
                    Err(_) => {
                        self.accounts.clear();
                        self.offers.clear();
                        self.add_btn.set_sensitive(false);
                        self.empty.set_label(&i18n("CardDAV accounts are kept by Evolution Data Server, which is not installed. Install the evolution-data-server package, which runs without GNOME, and open this page again."));
                    }
                }
                self.rebuild(&sender);
            }
            CardDavInput::Add => self.open_editor(None, Account::empty(), &sender),
            CardDavInput::AddOffer(i) => {
                let Some(offer) = self.offers.get(i).cloned() else { return };
                self.toast(&i18n_f("Looking for address books on {name}…", &[("name", &offer.account().title())]));
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    match carddav::add_offer(&offer) {
                        Some(saved) => report_save(&s, saved, true),
                        // No password kept for the mail account: the form
                        // asks for it, filled in otherwise.
                        None => {
                            let _ = s.send(CardDavInput::OfferNeedsPassword(offer));
                        }
                    }
                });
            }
            CardDavInput::OfferNeedsPassword(offer) => {
                self.open_editor(None, offer.account(), &sender);
            }
            CardDavInput::Edit(i) => {
                if let Some((a, _)) = self.accounts.get(i).cloned() {
                    self.open_editor(Some(i), a, &sender);
                }
            }
            CardDavInput::ToggleEnabled { index, enabled } => {
                let Some((a, _)) = self.accounts.get_mut(index) else { return };
                if a.enabled == enabled {
                    return;
                }
                a.enabled = enabled;
                let a = a.clone();
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let error = carddav::set_enabled(&a, enabled).err();
                    let _ = s.send(CardDavInput::Done { error, found: None });
                });
            }
            CardDavInput::RemoveCurrent => {
                if let Some(i) = self.editing {
                    sender.input(CardDavInput::ConfirmRemove(i));
                }
            }
            CardDavInput::ConfirmRemove(i) => {
                let Some((a, _)) = self.accounts.get(i) else { return };
                let parent = relm4::main_application().active_window();
                let dialog = adw::MessageDialog::new(
                    parent.as_ref(),
                    Some(&i18n_f("Remove {name}?", &[("name", &a.title())])),
                    Some(&i18n("The account and its address books are removed from this computer, so Evolution and other apps that use them stop seeing them too. Nothing on the server changes.")),
                );
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("remove", &i18n("Remove"));
                dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let s = sender.input_sender().clone();
                dialog.connect_response(None, move |_, response| {
                    if response == "remove" {
                        let _ = s.send(CardDavInput::Remove(i));
                    }
                });
                dialog.present();
            }
            CardDavInput::Remove(i) => {
                let Some((a, _)) = self.accounts.get(i).cloned() else { return };
                if self.editing == Some(i) {
                    self.editing = None;
                    self.close_editor();
                }
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let error = carddav::remove(&a).err();
                    let _ = s.send(CardDavInput::Done { error, found: None });
                });
            }
            CardDavInput::SaveClicked => {
                let action = self.save_action.borrow().clone();
                if let Some(f) = action {
                    if f() {
                        self.close_editor();
                    }
                }
            }
            CardDavInput::CloseEditor => self.close_editor(),
            CardDavInput::Save { account, password } => {
                self.toast(&i18n_f("Looking for address books on {name}…", &[("name", &account.title())]));
                let s = sender.input_sender().clone();
                std::thread::spawn(move || {
                    let password = (!password.is_empty()).then_some(password);
                    let saved = carddav::save(&account, password.as_deref());
                    if account.enabled {
                        report_save(&s, saved, password.is_some());
                    } else {
                        let _ = s.send(CardDavInput::Done { error: saved.err(), found: None });
                    }
                });
            }
            CardDavInput::Done { error, found } => {
                if let Some(e) = error {
                    self.toast(&e);
                }
                if let Some((a, status)) = found {
                    let n = a.books.len() as u32;
                    match status.problem() {
                        Some(problem) => self.toast(&i18n_f("{name}: {problem}", &[("name", &a.title()), ("problem", &problem)])),
                        None if n == 0 => self.toast(&i18n_f("No address books were found on {name} yet.", &[("name", &a.title())])),
                        None => self.toast(&ni18n_f(
                            "Found {n} address book on {name}",
                            "Found {n} address books on {name}",
                            n,
                            &[("n", &n.to_string()), ("name", &a.title())],
                        )),
                    }
                }
                let _ = sender.output(CardDavOutput::BooksChanged);
                sender.input(CardDavInput::Load);
            }
        }
    }
}

impl CardDavAccounts {
    fn toast(&self, text: &str) {
        let toast = adw::Toast::new(text);
        toast.set_timeout(6);
        self.toasts.add_toast(toast);
    }

    fn open_editor(&mut self, index: Option<usize>, account: Account, sender: &ComponentSender<Self>) {
        self.editor_page.set_title(&if index.is_some() { i18n("Edit Account") } else { i18n("Add Account") });
        self.editing = index;
        self.remove_btn.set_visible(index.is_some());
        let (form, save) = build_editor(account, sender.input_sender().clone());
        self.editor_slot.set_child(Some(&form));
        *self.save_action.borrow_mut() = Some(save);
        if !self.nav.visible_page().and_then(|p| p.tag()).is_some_and(|t| t == "editor") {
            self.nav.push_by_tag("editor");
        }
    }

    fn close_editor(&self) {
        if self.nav.visible_page().and_then(|p| p.tag()).is_some_and(|t| t == "editor") {
            self.nav.pop();
        }
    }

    fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for (i, (a, status)) in self.accounts.iter().enumerate() {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(true);
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("account-list-row");
            let icon = gtk::Image::from_icon_name("folder-remote-symbolic");
            icon.set_pixel_size(24);
            hbox.append(&icon);
            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
            vbox.set_hexpand(true);
            vbox.set_valign(gtk::Align::Center);
            let title = gtk::Label::new(Some(&a.title()));
            title.set_halign(gtk::Align::Start);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.add_css_class("account-name");
            // The address books found, or why there are none.
            let problem = if a.enabled { status.problem() } else { None };
            let sub = match (&problem, a.books.is_empty()) {
                (Some(p), _) => p.clone(),
                (None, false) => a.books.join(", "),
                (None, true) if !a.enabled => a.url.clone(),
                (None, true) => i18n("Looking for address books…"),
            };
            let subtitle = gtk::Label::new(Some(&sub));
            subtitle.set_halign(gtk::Align::Start);
            subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
            subtitle.add_css_class("account-email");
            subtitle.set_tooltip_text(Some(&sub));
            if problem.is_some() {
                subtitle.add_css_class("error");
            }
            vbox.append(&title);
            vbox.append(&subtitle);
            hbox.append(&vbox);
            let toggle = gtk::Switch::new();
            toggle.set_valign(gtk::Align::Center);
            toggle.set_tooltip_text(Some(&i18n("Use this account's address books")));
            toggle.set_active(a.enabled);
            let s = sender.input_sender().clone();
            toggle.connect_state_set(move |_, state| {
                let _ = s.send(CardDavInput::ToggleEnabled { index: i, enabled: state });
                gtk::glib::Propagation::Proceed
            });
            hbox.append(&toggle);
            let next = gtk::Image::from_icon_name("go-next-symbolic");
            next.add_css_class("dim-label");
            hbox.append(&next);
            row.set_child(Some(&hbox));
            self.list.append(&row);
        }
        self.list.set_visible(!self.accounts.is_empty());
        self.empty.set_visible(self.accounts.is_empty());

        while let Some(child) = self.offers_list.first_child() {
            self.offers_list.remove(&child);
        }
        for (i, offer) in self.offers.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&offer.email));
            let host = carddav::parse_url(&offer.url).map(|u| u.host).unwrap_or_default();
            let mut sub = format!("{} · {host}", offer.provider);
            if offer.online_accounts {
                sub = i18n_f(
                    "{provider} contacts are synced through GNOME Online Accounts already, maybe for this account under another address.",
                    &[("provider", offer.provider)],
                );
            }
            row.set_subtitle(&gtk::glib::markup_escape_text(&sub));
            row.set_subtitle_lines(3);
            let add = gtk::Button::with_label(&i18n("Add Address Books"));
            add.set_valign(gtk::Align::Center);
            let s = sender.input_sender().clone();
            add.connect_clicked(move |b| {
                b.set_sensitive(false);
                let _ = s.send(CardDavInput::AddOffer(i));
            });
            row.add_suffix(&add);
            self.offers_list.append(&row);
        }
        self.offers_group.set_visible(!self.offers.is_empty());
    }
}

/// After a save: show the account at once, wait for EDS to look at the
/// server, and say what it found. Blocking; `signing_in` when a new
/// password was given.
fn report_save(s: &relm4::Sender<CardDavInput>, saved: Result<String, String>, signing_in: bool) {
    let uid = match saved {
        Ok(uid) => uid,
        Err(e) => {
            let _ = s.send(CardDavInput::Done { error: Some(e), found: None });
            return;
        }
    };
    // The list shows the account at once, not only once the server has
    // answered.
    let _ = s.send(CardDavInput::Load);
    let found = carddav::await_books(&uid, std::time::Duration::from_secs(30), signing_in).map(|a| {
        let status = carddav::status(&a);
        (a, status)
    });
    let _ = s.send(CardDavInput::Done { error: None, found });
}

/// The account editor's form, and what the page's Save does with it.
fn build_editor(account: Account, sender: relm4::Sender<CardDavInput>) -> (gtk::Widget, Rc<dyn Fn() -> bool>) {
    let server = adw::PreferencesGroup::new();
    server.set_title(&i18n("Server"));
    server.set_description(Some(&i18n("The server's address is enough for most services, such as nextcloud.example.org; Hylki finds the address books on it. For iCloud, use contacts.icloud.com.")));
    let name = adw::EntryRow::new();
    name.set_title(&i18n("Name (optional)"));
    name.set_text(&account.name);
    let url = adw::EntryRow::new();
    url.set_title(&i18n("Server"));
    url.set_text(&account.url);
    url.set_input_purpose(gtk::InputPurpose::Url);
    server.add(&name);
    server.add(&url);

    let signin = adw::PreferencesGroup::new();
    signin.set_title(&i18n("Sign-in"));
    signin.set_description(Some(&i18n("Services that sign in with two steps want an app password made for Hylki: iCloud, Fastmail and Nextcloud with two-factor sign-in among them.")));
    let user = adw::EntryRow::new();
    user.set_title(&i18n("User name"));
    user.set_text(&account.user);
    let pass = adw::PasswordEntryRow::new();
    pass.set_title(&i18n("Password"));
    // Asked off the main thread: the keyring may wait on its user (#466).
    if !account.uid.is_empty() {
        let (a, pass) = (account.clone(), pass.clone());
        crate::config::keyring_then(
            move || carddav::has_password(&a),
            move |saved| {
                if saved {
                    pass.set_title(&i18n("Password (saved; type to replace it)"));
                }
            },
        );
    }
    signin.add(&user);
    signin.add(&pass);
    // Return in the last field saves, as the button does.
    {
        let sender = sender.clone();
        pass.connect_entry_activated(move |_| {
            let _ = sender.send(CardDavInput::SaveClicked);
        });
    }
    // A new account starts at its server, the one field always needed, or
    // at the password when the rest came filled in from a mail account.
    if account.uid.is_empty() {
        let (url, pass) = (url.clone(), pass.clone());
        gtk::glib::idle_add_local_once(move || {
            if url.text().is_empty() {
                url.grab_focus();
            } else {
                pass.grab_focus();
            }
        });
    }

    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_xalign(0.0);
    error.set_visible(false);

    let form = gtk::Box::new(gtk::Orientation::Vertical, 24);
    form.append(&server);
    form.append(&signin);
    form.append(&error);

    let new = account.uid.is_empty();
    let save: Rc<dyn Fn() -> bool> = Rc::new(move || {
        let fail = |text: &str, row: &adw::EntryRow| {
            error.set_label(text);
            error.set_visible(true);
            row.grab_focus();
            false
        };
        if let Err(e) = carddav::parse_url(&url.text()) {
            return fail(&e, &url);
        }
        if user.text().trim().is_empty() {
            return fail(&i18n("Enter the user name."), &user);
        }
        if new && pass.text().is_empty() {
            return fail(&i18n("Enter the password."), pass.upcast_ref());
        }
        let mut a = account.clone();
        a.name = name.text().trim().to_string();
        a.url = url.text().trim().to_string();
        a.user = user.text().trim().to_string();
        let _ = sender.send(CardDavInput::Save { account: a, password: pass.text().to_string() });
        true
    });
    (form.upcast(), save)
}
