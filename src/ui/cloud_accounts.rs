//! Settings → Cloud Storage (#144): the Nextcloud, Dropbox and Seafile
//! accounts that "Upload to cloud" in the composer can put files on. Each
//! row is an account; the editor is a dialog whose fields follow the kind,
//! with a connection check (a browser sign-in, for Dropbox).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::cloud::{self, CloudAccount, CloudKind};
use crate::i18n::{i18n, i18n_f};

/// How a kind is named in the editor and the list.
pub fn kind_label(kind: CloudKind) -> String {
    match kind {
        CloudKind::Nextcloud => i18n("Nextcloud, ownCloud or OpenCloud"),
        CloudKind::Dropbox => i18n("Dropbox"),
        CloudKind::Seafile => i18n("Seafile"),
        CloudKind::GoogleDrive => i18n("Google Drive"),
        CloudKind::OneDrive => i18n("OneDrive"),
    }
}

pub struct CloudAccounts {
    accounts: Vec<CloudAccount>,
    list: gtk::ListBox,
    toasts: Option<adw::ToastOverlay>,
}

#[derive(Debug)]
pub enum CloudAccountsInput {
    Add,
    /// The editor for a new account of that kind.
    AddOf(CloudKind),
    Edit(usize),
    Remove(usize),
    /// The editor's Save: `index` is the row being replaced, or none for a
    /// new account. The password is stored only when given.
    Save { index: Option<usize>, account: CloudAccount, password: String },
    /// A sign-in the editor finished after closing went wrong.
    Failed(String),
}

#[relm4::component(pub)]
impl SimpleComponent for CloudAccounts {
    type Init = ();
    type Input = CloudAccountsInput;
    type Output = ();

    view! {
        #[name = "toasts"]
        adw::ToastOverlay {
            #[wrap(Some)]
            set_child = &adw::PreferencesPage {
                add = &adw::PreferencesGroup {
                    set_title: &i18n("Cloud storage"),
                    set_description: Some(&i18n("Upload a large file to Nextcloud, ownCloud, OpenCloud, Google Drive, OneDrive, Dropbox or Seafile and put a share link in the message instead of an attachment.")),
                    #[wrap(Some)]
                    set_header_suffix = &gtk::Button {
                        set_label: &i18n("Add Account…"),
                        set_valign: gtk::Align::Center,
                        set_margin_start: 24,
                        connect_clicked => CloudAccountsInput::Add,
                    },
                    #[name = "list"]
                    gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                    #[name = "empty"]
                    gtk::Label {
                        set_label: &i18n("No cloud accounts yet."),
                        add_css_class: "dim-label",
                        set_margin_top: 12,
                    },
                },
            },
        }
    }

    fn init(_init: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let widgets = view_output!();
        let mut model = CloudAccounts {
            accounts: cloud::load_accounts(),
            list: widgets.list.clone(),
            toasts: Some(widgets.toasts.clone()),
        };
        model.rebuild(&sender);
        widgets.empty.set_visible(model.accounts.is_empty());
        // VIREO_SHOWCASE_EDIT_CLOUD=<index> opens that account's editor
        // for a capture (demo only); "add", "add:dropbox" or
        // "add:seafile" opens the Add dialog on that kind.
        if let Ok(what) = std::env::var("VIREO_SHOWCASE_EDIT_CLOUD") {
            if std::env::var_os("VIREO_DEMO").is_some() {
                let s = sender.input_sender().clone();
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    let _ = s.send(match what.as_str() {
                        "add" | "add:nextcloud" => CloudAccountsInput::AddOf(CloudKind::Nextcloud),
                        "add:dropbox" => CloudAccountsInput::AddOf(CloudKind::Dropbox),
                        "add:seafile" => CloudAccountsInput::AddOf(CloudKind::Seafile),
                        "add:google-drive" => CloudAccountsInput::AddOf(CloudKind::GoogleDrive),
                        "add:onedrive" => CloudAccountsInput::AddOf(CloudKind::OneDrive),
                        i => CloudAccountsInput::Edit(i.parse().unwrap_or(0)),
                    });
                });
            }
        }
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            CloudAccountsInput::Add => sender.input(CloudAccountsInput::AddOf(CloudKind::Nextcloud)),
            CloudAccountsInput::AddOf(kind) => {
                let mut a = CloudAccount::empty();
                a.kind = kind;
                edit_dialog(self.list.root().and_downcast::<gtk::Window>().as_ref(), None, a, sender.input_sender().clone());
            }
            CloudAccountsInput::Edit(i) => {
                if let Some(a) = self.accounts.get(i).cloned() {
                    edit_dialog(self.list.root().and_downcast::<gtk::Window>().as_ref(), Some(i), a, sender.input_sender().clone());
                }
            }
            CloudAccountsInput::Remove(i) => {
                if i < self.accounts.len() {
                    let a = self.accounts.remove(i);
                    crate::config::delete_cloud_password(&a.key());
                    cloud::save_accounts(&self.accounts);
                    self.rebuild(&sender);
                    self.toast(&i18n_f("Removed {name}", &[("name", &a.name)]));
                }
            }
            CloudAccountsInput::Failed(e) => self.toast(&e),
            CloudAccountsInput::Save { index, account, password } => {
                if !password.is_empty() && account.has_secret() {
                    if let Err(e) = crate::config::store_cloud_password(&account.key(), &password) {
                        self.toast(&i18n_f("Could not store the sign-in in the keyring: {e}", &[("e", &e.to_string())]));
                    }
                }
                match index {
                    Some(i) if i < self.accounts.len() => {
                        // A changed server or user name moves the keyring
                        // entry: drop the old one.
                        if self.accounts[i].key() != account.key() {
                            crate::config::delete_cloud_password(&self.accounts[i].key());
                        }
                        self.accounts[i] = account;
                    }
                    _ => self.accounts.push(account),
                }
                cloud::save_accounts(&self.accounts);
                self.rebuild(&sender);
            }
        }
    }
}

impl CloudAccounts {
    fn toast(&self, text: &str) {
        if let Some(t) = &self.toasts {
            t.add_toast(adw::Toast::new(text));
        }
    }

    fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for (i, a) in self.accounts.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(&if a.name.trim().is_empty() { a.where_shown() } else { a.name.clone() });
            let mut sub = format!("{} · {}", a.where_shown(), a.user);
            if a.expire_days > 0 {
                sub.push_str(&format!(" · {}", i18n_f("links expire after {n} days", &[("n", &a.expire_days.to_string())])));
            }
            if a.password {
                sub.push_str(&format!(" · {}", i18n("password-protected")));
            }
            row.set_subtitle(&sub);
            row.add_prefix(&gtk::Image::from_icon_name("co.hyprlab.Vireo-cloud-symbolic"));
            let edit = gtk::Button::from_icon_name("co.hyprlab.Vireo-document-edit-symbolic");
            edit.add_css_class("flat");
            edit.set_valign(gtk::Align::Center);
            edit.set_tooltip_text(Some(&i18n("Edit")));
            let s = sender.input_sender().clone();
            edit.connect_clicked(move |_| {
                let _ = s.send(CloudAccountsInput::Edit(i));
            });
            row.add_suffix(&edit);
            let rm = gtk::Button::from_icon_name("co.hyprlab.Vireo-user-trash-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            rm.set_tooltip_text(Some(&i18n("Remove")));
            let s = sender.input_sender().clone();
            rm.connect_clicked(move |_| {
                let _ = s.send(CloudAccountsInput::Remove(i));
            });
            row.add_suffix(&rm);
            self.list.append(&row);
        }
        self.list.set_visible(!self.accounts.is_empty());
        if let Some(next) = self.list.next_sibling() {
            next.set_visible(self.accounts.is_empty());
        }
    }
}

/// The account editor: a kind, the fields that kind needs, a connection
/// check that signs in with what is typed (a browser sign-in, for
/// Dropbox), and Save.
fn edit_dialog(
    parent: Option<&gtk::Window>,
    index: Option<usize>,
    account: CloudAccount,
    sender: relm4::Sender<CloudAccountsInput>,
) {
    let heading = if index.is_some() { i18n("Edit Cloud Account") } else { i18n("Add Cloud Account") };
    let dialog = adw::MessageDialog::new(parent, Some(&heading), None);
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("save", &i18n("Save"));
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);

    let group = adw::PreferencesGroup::new();
    let kinds: Vec<String> = CloudKind::ALL.iter().map(|k| kind_label(*k)).collect();
    let kind_refs: Vec<&str> = kinds.iter().map(String::as_str).collect();
    let kind = adw::ComboRow::new();
    kind.set_title(&i18n("Service"));
    kind.set_model(Some(&gtk::StringList::new(&kind_refs)));
    kind.set_selected(CloudKind::ALL.iter().position(|k| *k == account.kind).unwrap_or(0) as u32);
    // The kind is chosen when the account is made; afterwards the
    // sign-in and the keyring entry belong to it.
    kind.set_sensitive(index.is_none());
    // What the account is called in the list; optional, the server or
    // service stands in when empty. Titled per kind with an example, so
    // it does not read as asking for the user's own name.
    let name = adw::EntryRow::new();
    name.set_text(&account.name);
    let url = adw::EntryRow::new();
    url.set_title(&i18n("Server URL"));
    url.set_text(&account.url);
    let user = adw::EntryRow::new();
    user.set_text(&account.user);
    let pass = adw::PasswordEntryRow::new();
    let code = adw::EntryRow::new();
    code.set_title(&i18n("Two-step verification code, if the account uses it"));
    code.set_input_purpose(gtk::InputPurpose::Digits);
    let seafile_hint = gtk::Label::new(Some(&i18n(
        "Sign in with your Seafile password. If the account uses two-step verification, also enter the current code from your authenticator app: Vireo turns it into an API token once and keeps that instead of the password. A token obtained another way can be pasted in the password field.",
    )));
    seafile_hint.set_wrap(true);
    seafile_hint.set_xalign(0.0);
    seafile_hint.add_css_class("dim-label");
    seafile_hint.add_css_class("caption");
    seafile_hint.set_margin_top(6);
    seafile_hint.set_margin_start(12);
    seafile_hint.set_margin_end(12);
    // Google Drive and OneDrive: which GNOME Online Accounts account.
    let goa_accounts = crate::goa::list_files_accounts();
    let goa_row = adw::ComboRow::new();
    goa_row.set_title(&i18n("Online account"));
    let goa_hint = gtk::Label::new(None);
    goa_hint.set_wrap(true);
    goa_hint.set_xalign(0.0);
    goa_hint.add_css_class("dim-label");
    goa_hint.add_css_class("caption");
    goa_hint.set_margin_top(6);
    goa_hint.set_margin_start(12);
    goa_hint.set_margin_end(12);
    let app_key = adw::EntryRow::new();
    app_key.set_title(&i18n("Dropbox app key"));
    app_key.set_text(&account.client_id);
    let app_key_hint = gtk::Label::new(None);
    app_key_hint.set_wrap(true);
    app_key_hint.set_xalign(0.0);
    app_key_hint.add_css_class("dim-label");
    app_key_hint.add_css_class("caption");
    app_key_hint.set_margin_top(6);
    app_key_hint.set_margin_start(12);
    app_key_hint.set_margin_end(12);
    let library = adw::EntryRow::new();
    library.set_title(&i18n("Library"));
    library.set_text(&account.library);
    let folder = adw::EntryRow::new();
    folder.set_title(&i18n("Upload folder"));
    folder.set_text(&account.folder);
    let expire = adw::SpinRow::with_range(0.0, 365.0, 1.0);
    expire.set_title(&i18n("Links expire after"));
    expire.set_subtitle(&i18n("Days; 0 keeps the link"));
    expire.set_value(account.expire_days as f64);
    let protect = adw::SwitchRow::new();
    protect.set_title(&i18n("Protect links with a password"));
    protect.set_subtitle(&i18n("A download password is made for each file and shown to you, to pass on separately"));
    protect.set_active(account.password);
    group.add(&kind);
    group.add(&name);
    group.add(&url);
    group.add(&user);
    group.add(&pass);
    group.add(&code);
    group.add(&goa_row);
    group.add(&app_key);
    group.add(&library);
    group.add(&folder);
    group.add(&expire);
    group.add(&protect);

    let check_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    check_box.set_margin_top(8);
    let check = gtk::Button::with_label(&i18n("Check Connection"));
    check.set_valign(gtk::Align::Start);
    // Google Drive's own sign-in, beside the check, for when GOA cannot
    // serve Drive.
    let google = gtk::Button::with_label(&i18n("Sign in with Google…"));
    google.set_valign(gtk::Align::Start);
    google.set_visible(false);
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.add_css_class("dim-label");
    check_box.append(&check);
    check_box.append(&google);
    check_box.append(&status);

    let bx = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bx.set_width_request(420);
    bx.append(&group);
    bx.append(&app_key_hint);
    bx.append(&goa_hint);
    bx.append(&seafile_hint);
    bx.append(&check_box);
    dialog.set_extra_child(Some(&bx));

    // A sign-in that made a secret of its own leaves it here until Save:
    // the Dropbox refresh token (with the account's e-mail and name), or
    // the Seafile API token from a two-step sign-in.
    let dropbox_login: Rc<RefCell<Option<(String, String, String)>>> = Rc::new(RefCell::new(None));

    let selected_kind = {
        let kind = kind.clone();
        move || CloudKind::ALL.get(kind.selected() as usize).copied().unwrap_or_default()
    };

    // The GOA accounts the picker currently lists (those of the kind's
    // provider), in row order.
    let goa_listed: Rc<RefCell<Vec<crate::goa::GoaFilesAccount>>> = Rc::new(RefCell::new(Vec::new()));
    let fill_goa = {
        let (goa_row, goa_hint, goa_accounts, goa_listed) =
            (goa_row.clone(), goa_hint.clone(), goa_accounts.clone(), goa_listed.clone());
        let existing_goa = account.goa_id.clone();
        move |k: CloudKind| {
            let Some(provider) = k.goa_provider() else { return };
            let listed: Vec<crate::goa::GoaFilesAccount> =
                goa_accounts.iter().filter(|a| a.provider_type == provider).cloned().collect();
            let labels: Vec<String> = listed
                .iter()
                .map(|a| if a.files_enabled { a.email.clone() } else { i18n_f("{email} (Files is off)", &[("email", &a.email)]) })
                .collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            goa_row.set_model(Some(&gtk::StringList::new(&refs)));
            if let Some(i) = listed.iter().position(|a| a.id == existing_goa) {
                goa_row.set_selected(i as u32);
            }
            goa_row.set_sensitive(!listed.is_empty());
            let service = kind_label(k);
            let mut hint = if listed.is_empty() {
                i18n_f(
                    "No {provider} account in GNOME Online Accounts yet. Add one under Settings, Online Accounts; {service} then signs in through it, and nothing more is needed here.",
                    &[("provider", if k == CloudKind::GoogleDrive { "Google" } else { "Microsoft 365" }), ("service", &service)],
                )
            } else {
                i18n_f(
                    "{service} signs in through the GNOME Online Accounts account chosen above; there is no password to enter. An account marked Files is off still works here, but you may want to turn Files on for it under Settings, Online Accounts.",
                    &[("service", &service)],
                )
            };
            if k == CloudKind::GoogleDrive {
                hint.push(' ');
                hint.push_str(&if cloud::google_client_available() {
                    i18n("Some systems build GNOME Online Accounts without Google Drive access (Fedora does); the check then says so, and Sign in with Google below signs Vireo in directly, for its own uploads only.")
                } else {
                    i18n("Some systems build GNOME Online Accounts without Google Drive access (Fedora does). Then sign in with Google directly: put a Google OAuth client of your own in ~/.config/vireo/oauth.toml under [google] (see the README), and a Sign in with Google button appears here.")
                });
            }
            goa_hint.set_label(&hint);
            *goa_listed.borrow_mut() = listed;
        }
    };

    // The fields each kind wants.
    let apply_kind = {
        let (name, url, user, pass, code, seafile_hint, app_key, app_key_hint, library, check, protect, expire, goa_row, goa_hint) = (
            name.clone(),
            url.clone(),
            user.clone(),
            pass.clone(),
            code.clone(),
            seafile_hint.clone(),
            app_key.clone(),
            app_key_hint.clone(),
            library.clone(),
            check.clone(),
            protect.clone(),
            expire.clone(),
            goa_row.clone(),
            goa_hint.clone(),
        );
        let editing = index.is_some();
        let fill_goa = fill_goa.clone();
        let google = google.clone();
        move |k: CloudKind| {
            let dropbox = k == CloudKind::Dropbox;
            let goa = k.via_goa();
            google.set_visible(k == CloudKind::GoogleDrive && cloud::google_client_available());
            url.set_visible(!dropbox && !goa);
            user.set_visible(!dropbox && !goa);
            pass.set_visible(!dropbox && !goa);
            app_key.set_visible(dropbox);
            app_key_hint.set_visible(dropbox);
            goa_row.set_visible(goa);
            goa_hint.set_visible(goa);
            library.set_visible(k == CloudKind::Seafile);
            code.set_visible(k == CloudKind::Seafile);
            seafile_hint.set_visible(k == CloudKind::Seafile);
            // Google Drive has no link expiry or password to offer.
            expire.set_sensitive(k.can_expire());
            expire.set_subtitle(&if k.can_expire() { i18n("Days; 0 keeps the link") } else { i18n("Not offered by Google Drive") });
            protect.set_sensitive(k.can_password());
            if goa {
                fill_goa(k);
                check.set_label(&i18n("Check Connection"));
                protect.set_subtitle(&if k.can_password() {
                    i18n("A download password is made for each file and shown to you, to pass on separately. OneDrive allows link passwords and expiry dates with a Microsoft 365 subscription or OneDrive for Business.")
                } else {
                    i18n("Not offered by Google Drive")
                });
                name.set_title(&if k == CloudKind::GoogleDrive {
                    i18n("Account name, such as Work Drive (optional)")
                } else {
                    i18n("Account name, such as Work OneDrive (optional)")
                });
                return;
            }
            match k {
                CloudKind::Nextcloud => {
                    name.set_title(&i18n("Account name, such as Work Nextcloud (optional)"));
                    user.set_title(&i18n("User name"));
                    pass.set_title(&if editing { i18n("App password (leave empty to keep)") } else { i18n("App password") });
                    protect.set_subtitle(&i18n("A download password is made for each file and shown to you, to pass on separately"));
                    check.set_label(&i18n("Check Connection"));
                }
                CloudKind::Seafile => {
                    name.set_title(&i18n("Account name, such as Team Seafile (optional)"));
                    user.set_title(&i18n("E-mail"));
                    pass.set_title(&if editing { i18n("Password (leave empty to keep)") } else { i18n("Password") });
                    protect.set_subtitle(&i18n("A download password is made for each file and shown to you, to pass on separately"));
                    check.set_label(&i18n("Check Connection"));
                }
                // Handled above, before the return.
                CloudKind::GoogleDrive | CloudKind::OneDrive => {}
                CloudKind::Dropbox => {
                    name.set_title(&i18n("Account name, such as Personal Dropbox (optional)"));
                    protect.set_subtitle(&i18n("A download password is made for each file and shown to you, to pass on separately. Dropbox allows link passwords and expiry dates on paid plans only."));
                    check.set_label(&i18n("Connect with Dropbox…"));
                    app_key_hint.set_label(&i18n_f(
                        "Dropbox only lets an app sign in when it is registered, so make one for yourself (it takes a minute and stays private):\n\
                         1. Open dropbox.com/developers/apps, signed in to your Dropbox, and press Create app.\n\
                         2. Choose Scoped access, then App folder (Vireo sees only its own folder under Apps) or Full Dropbox (uploads go to the folder named above).\n\
                         3. Give the app a name that no one else has used, such as Vireo for your name, and press Create app.\n\
                         4. On the Permissions tab, tick account_info.read, files.content.write and sharing.write, then press Submit.\n\
                         5. On the Settings tab, under OAuth 2, add the redirect URI {uri} and press Add.\n\
                         6. Copy the App key from the top of the Settings tab into the field above.\n\
                         The app may stay in Development status; that allows your own account. Leave the field empty to use the app key this build was made with, when it has one.",
                        &[("uri", &format!("http://localhost:{}/", crate::oauth::DROPBOX_REDIRECT_PORT))],
                    ));
                }
            }
        }
    };
    apply_kind(account.kind);
    {
        let apply_kind = apply_kind.clone();
        let status = status.clone();
        let selected_kind = selected_kind.clone();
        kind.connect_selected_notify(move |_| {
            apply_kind(selected_kind());
            status.set_label("");
        });
    }

    let read = {
        let (name, url, user, app_key, library, folder, expire, protect) = (
            name.clone(),
            url.clone(),
            user.clone(),
            app_key.clone(),
            library.clone(),
            folder.clone(),
            expire.clone(),
            protect.clone(),
        );
        let selected_kind = selected_kind.clone();
        let dropbox_login = dropbox_login.clone();
        let existing = account.clone();
        let (goa_row, goa_listed) = (goa_row.clone(), goa_listed.clone());
        move || {
            let kind = selected_kind();
            // Google Drive signed in directly: this session's sign-in, or
            // the account's existing one when nothing was picked anew.
            let direct_google = kind == CloudKind::GoogleDrive
                && (dropbox_login.borrow().is_some()
                    || (existing.kind == CloudKind::GoogleDrive
                        && existing.goa_id.is_empty()
                        && !existing.user.is_empty()
                        && goa_listed.borrow().is_empty()));
            let goa = if kind.via_goa() && !direct_google {
                goa_listed.borrow().get(goa_row.selected() as usize).map(|a| (a.id.clone(), a.email.clone()))
            } else {
                None
            };
            let user = match (kind, dropbox_login.borrow().as_ref()) {
                (CloudKind::Dropbox | CloudKind::GoogleDrive, Some((_, email, _))) => email.clone(),
                (CloudKind::Dropbox, None) if existing.kind == CloudKind::Dropbox => existing.user.clone(),
                (CloudKind::Dropbox, None) => String::new(),
                (CloudKind::GoogleDrive, None) if direct_google => existing.user.clone(),
                (CloudKind::GoogleDrive | CloudKind::OneDrive, _) => {
                    goa.as_ref().map(|(_, e)| e.clone()).unwrap_or_default()
                }
                _ => user.text().trim().to_string(),
            };
            CloudAccount {
                name: name.text().trim().to_string(),
                kind,
                url: url.text().trim().to_string(),
                user,
                folder: folder.text().trim().to_string(),
                library: library.text().trim().to_string(),
                client_id: app_key.text().trim().to_string(),
                goa_id: goa.map(|(id, _)| id).unwrap_or_default(),
                expire_days: if kind.can_expire() { expire.value() as u32 } else { 0 },
                password: kind.can_password() && protect.is_active(),
            }
        }
    };

    if (account.kind == CloudKind::Dropbox || (account.kind == CloudKind::GoogleDrive && account.goa_id.is_empty()))
        && !account.user.is_empty()
    {
        status.set_label(&i18n_f("Connected as {who}.", &[("who", &account.user)]));
    }

    // Sign in with Google: the browser flow, then the refresh token waits
    // in the same slot as Dropbox's until Save.
    {
        let status = status.clone();
        let dropbox_login = dropbox_login.clone();
        let name = name.clone();
        let check = check.clone();
        google.connect_clicked(move |b| {
            status.set_label(&i18n("Waiting for the sign-in in your browser…"));
            b.set_sensitive(false);
            check.set_sensitive(false);
            let (tx, rx) = std::sync::mpsc::channel::<Result<(String, String, String), String>>();
            std::thread::spawn(move || {
                let _ = tx.send(cloud::google_connect());
            });
            let (status, b, check, dropbox_login, name) = (status.clone(), b.clone(), check.clone(), dropbox_login.clone(), name.clone());
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                let r = match rx.try_recv() {
                    Ok(r) => r,
                    Err(std::sync::mpsc::TryRecvError::Empty) => return gtk::glib::ControlFlow::Continue,
                    Err(_) => Err(String::new()),
                };
                match r {
                    Ok((refresh, email, who)) => {
                        status.set_label(&i18n_f("Signed in as {who}.", &[("who", &who)]));
                        if name.text().trim().is_empty() {
                            name.set_text("Google Drive");
                        }
                        *dropbox_login.borrow_mut() = Some((refresh, email, who));
                    }
                    Err(e) if !e.is_empty() => status.set_label(&e),
                    Err(_) => {}
                }
                b.set_sensitive(true);
                check.set_sensitive(true);
                gtk::glib::ControlFlow::Break
            });
        });
    }

    {
        let read = read.clone();
        let pass = pass.clone();
        let status = status.clone();
        let existing = account.clone();
        let dropbox_login = dropbox_login.clone();
        let name = name.clone();
        let code = code.clone();
        let selected_kind = selected_kind.clone();
        check.connect_clicked(move |b| {
            let a = read();
            enum Job {
                Verify(CloudAccount, String),
                Dropbox(CloudAccount),
                SeafileCode(CloudAccount, String, String),
            }
            let job = if a.kind.via_goa() {
                let direct = a.kind == CloudKind::GoogleDrive && a.goa_id.is_empty();
                let secret = if direct {
                    match dropbox_login.borrow().as_ref() {
                        Some((refresh, _, _)) => refresh.clone(),
                        None => crate::config::load_cloud_password(&existing.key()).unwrap_or_default(),
                    }
                } else {
                    String::new()
                };
                if a.goa_id.is_empty() && secret.is_empty() {
                    status.set_label(&if a.kind == CloudKind::GoogleDrive {
                        i18n("Choose an online account first, or sign in with Google.")
                    } else {
                        i18n("Choose an online account first.")
                    });
                    return;
                }
                status.set_label(&i18n("Signing in…"));
                Job::Verify(a, secret)
            } else if a.kind == CloudKind::Dropbox {
                if cloud::dropbox_client_id(&a).is_empty() {
                    status.set_label(&i18n("Enter the app key of a Dropbox app first."));
                    return;
                }
                status.set_label(&i18n("Waiting for the sign-in in your browser…"));
                Job::Dropbox(a)
            } else {
                let pw = match pass.text().to_string() {
                    p if !p.is_empty() => p,
                    _ => crate::config::load_cloud_password(&existing.key()).unwrap_or_default(),
                };
                if a.url.is_empty() || a.user.is_empty() || pw.is_empty() {
                    status.set_label(&if a.kind == CloudKind::Seafile {
                        i18n("Fill in the server URL, e-mail and password first.")
                    } else {
                        i18n("Fill in the server URL, user name and app password first.")
                    });
                    return;
                }
                status.set_label(&i18n("Signing in…"));
                let otp = code.text().trim().to_string();
                if a.kind == CloudKind::Seafile && !otp.is_empty() {
                    Job::SeafileCode(a, pw, otp)
                } else {
                    Job::Verify(a, pw)
                }
            };
            b.set_sensitive(false);
            let (tx, rx) = std::sync::mpsc::channel::<Result<(String, Option<(String, String, String)>), String>>();
            std::thread::spawn(move || {
                let r = match job {
                    Job::Verify(a, pw) => cloud::verify(&a, &pw).map(|who| (who, None)),
                    Job::Dropbox(a) => cloud::dropbox_connect(&a)
                        .map(|(refresh, email, who)| (format!("{who} ({email})"), Some((refresh, email, who)))),
                    Job::SeafileCode(a, pw, otp) => cloud::seafile_login_with_code(&a, &pw, &otp)
                        .map(|(token, who)| (who.clone(), Some((token, a.user.clone(), who)))),
                };
                let _ = tx.send(r);
            });
            let status = status.clone();
            let b = b.clone();
            let dropbox_login = dropbox_login.clone();
            let name = name.clone();
            let selected_kind_now = selected_kind.clone();
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(200), move || match rx.try_recv() {
                Ok(Ok((who, login))) => {
                    status.set_label(&i18n_f("Signed in as {who}.", &[("who", &who)]));
                    if let Some(l) = login {
                        if name.text().trim().is_empty() && selected_kind_now() == CloudKind::Dropbox {
                            name.set_text("Dropbox");
                        }
                        *dropbox_login.borrow_mut() = Some(l);
                    }
                    b.set_sensitive(true);
                    gtk::glib::ControlFlow::Break
                }
                Ok(Err(e)) => {
                    status.set_label(&e);
                    b.set_sensitive(true);
                    gtk::glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
                Err(_) => {
                    b.set_sensitive(true);
                    gtk::glib::ControlFlow::Break
                }
            });
        });
    }

    dialog.connect_response(None, move |_, resp| {
        if resp != "save" {
            return;
        }
        let a = read();
        let secret = if a.kind.via_goa() {
            if a.kind == CloudKind::GoogleDrive && a.goa_id.is_empty() {
                match dropbox_login.borrow().as_ref() {
                    Some((refresh, _, _)) => refresh.clone(),
                    None if !a.user.is_empty() => String::new(),
                    None => return,
                }
            } else {
                if a.goa_id.is_empty() {
                    return;
                }
                String::new()
            }
        } else if a.kind == CloudKind::Dropbox {
            // Without a sign-in there is nothing to save; a re-opened
            // account keeps its token when none was made anew.
            match dropbox_login.borrow().as_ref() {
                Some((refresh, _, _)) => refresh.clone(),
                None if !a.user.is_empty() => String::new(),
                None => return,
            }
        } else {
            if a.url.is_empty() || a.user.is_empty() {
                return;
            }
            let otp = code.text().trim().to_string();
            match dropbox_login.borrow().as_ref() {
                // The token a two-step sign-in made.
                Some((token, _, _)) if a.kind == CloudKind::Seafile => token.clone(),
                // A code typed but never checked: exchange it now, and
                // save once the token is here.
                _ if a.kind == CloudKind::Seafile && !otp.is_empty() && !pass.text().is_empty() => {
                    let pw = pass.text().to_string();
                    let sender = sender.clone();
                    std::thread::spawn(move || {
                        let _ = sender.send(match cloud::seafile_login_with_code(&a, &pw, &otp) {
                            Ok((token, _)) => CloudAccountsInput::Save { index, account: a, password: token },
                            Err(e) => CloudAccountsInput::Failed(e),
                        });
                    });
                    return;
                }
                _ => pass.text().to_string(),
            }
        };
        let _ = sender.send(CloudAccountsInput::Save { index, account: a, password: secret });
    });
    dialog.present();
}
