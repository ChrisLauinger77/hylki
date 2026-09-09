//! Settings → Cloud Storage (#144): the Nextcloud-style accounts that
//! "Upload to cloud" in the composer can put files on. Each row is an
//! account; the editor is a dialog with a connection check.

use adw::prelude::*;
use relm4::prelude::*;

use crate::cloud::{self, CloudAccount};
use crate::i18n::{i18n, i18n_f};

pub struct CloudAccounts {
    accounts: Vec<CloudAccount>,
    list: gtk::ListBox,
    toasts: Option<adw::ToastOverlay>,
}

#[derive(Debug)]
pub enum CloudAccountsInput {
    Add,
    Edit(usize),
    Remove(usize),
    /// The editor's Save: `index` is the row being replaced, or none for a
    /// new account. The password is stored only when given.
    Save { index: Option<usize>, account: CloudAccount, password: String },
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
                    set_description: Some(&i18n("Upload a large file to your own Nextcloud, ownCloud or OpenCloud and put a share link in the message instead of an attachment. Sign in with an app password, made under Security in the server's personal settings.")),
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
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            CloudAccountsInput::Add => {
                edit_dialog(self.list.root().and_downcast::<gtk::Window>().as_ref(), None, CloudAccount::empty(), sender.input_sender().clone());
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
            CloudAccountsInput::Save { index, account, password } => {
                if !password.is_empty() {
                    if let Err(e) = crate::config::store_cloud_password(&account.key(), &password) {
                        self.toast(&i18n_f("Could not store the app password in the keyring: {e}", &[("e", &e.to_string())]));
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
            row.set_title(&if a.name.trim().is_empty() { a.base() } else { a.name.clone() });
            let mut sub = format!("{} · {}", a.base(), a.user);
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

/// The account editor: fields, a connection check that signs in with what
/// is typed, and Save.
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
    let name = adw::EntryRow::new();
    name.set_title(&i18n("Name"));
    name.set_text(&account.name);
    let url = adw::EntryRow::new();
    url.set_title(&i18n("Server URL"));
    url.set_text(&account.url);
    let user = adw::EntryRow::new();
    user.set_title(&i18n("User name"));
    user.set_text(&account.user);
    let pass = adw::PasswordEntryRow::new();
    pass.set_title(&if index.is_some() { i18n("App password (leave empty to keep)") } else { i18n("App password") });
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
    for w in [&name, &url, &user] {
        group.add(w);
    }
    group.add(&pass);
    group.add(&folder);
    group.add(&expire);
    group.add(&protect);

    let check_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    check_box.set_margin_top(8);
    let check = gtk::Button::with_label(&i18n("Check Connection"));
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.add_css_class("dim-label");
    check_box.append(&check);
    check_box.append(&status);

    let bx = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bx.set_width_request(420);
    bx.append(&group);
    bx.append(&check_box);
    dialog.set_extra_child(Some(&bx));

    let read = {
        let (name, url, user, folder, expire, protect) =
            (name.clone(), url.clone(), user.clone(), folder.clone(), expire.clone(), protect.clone());
        move || CloudAccount {
            name: name.text().trim().to_string(),
            url: url.text().trim().to_string(),
            user: user.text().trim().to_string(),
            folder: folder.text().trim().to_string(),
            expire_days: expire.value() as u32,
            password: protect.is_active(),
        }
    };

    {
        let read = read.clone();
        let pass = pass.clone();
        let status = status.clone();
        let existing = account.clone();
        check.connect_clicked(move |b| {
            let a = read();
            let pw = match pass.text().to_string() {
                p if !p.is_empty() => p,
                _ => crate::config::load_cloud_password(&existing.key()).unwrap_or_default(),
            };
            if a.url.is_empty() || a.user.is_empty() || pw.is_empty() {
                status.set_label(&i18n("Fill in the server URL, user name and app password first."));
                return;
            }
            b.set_sensitive(false);
            status.set_label(&i18n("Signing in…"));
            let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
            std::thread::spawn(move || {
                let _ = tx.send(cloud::verify(&a, &pw));
            });
            let status = status.clone();
            let b = b.clone();
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(200), move || match rx.try_recv() {
                Ok(Ok(who)) => {
                    status.set_label(&i18n_f("Signed in as {who}.", &[("who", &who)]));
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
        if a.url.is_empty() || a.user.is_empty() {
            return;
        }
        let _ = sender.send(CloudAccountsInput::Save { index, account: a, password: pass.text().to_string() });
    });
    dialog.present();
}
