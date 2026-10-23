//! Cloud links: uploading a file too big to attach and putting its link in the
//! body.

use super::messages::ComposeInput;
use crate::i18n::i18n;
use adw::prelude::*;
use relm4::prelude::*;

pub(super) fn html_escape(s: &str) -> String {
    gtk::glib::markup_escape_text(s).into()
}

/// Which account to upload to, and how the links are made this time:
/// the expiry and the password, seeded from the account's settings and
/// changeable for this upload alone.
pub(super) fn cloud_upload_dialog(
    parent: Option<&gtk::Window>,
    accounts: &[crate::cloud::CloudAccount],
    paths: Vec<std::path::PathBuf>,
    sender: relm4::Sender<ComposeInput>,
) {
    if accounts.is_empty() {
        return;
    }
    let n = paths.len();
    let dialog = adw::MessageDialog::new(
        parent,
        Some(i18n("Upload to Cloud Storage").as_str()),
        Some(
            crate::i18n::ni18n_f(
                "Upload {n} file and put its share link in the message.",
                "Upload {n} files and put their share links in the message.",
                n as u32,
                &[("n", &n.to_string())],
            )
            .as_str(),
        ),
    );
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("upload", &i18n("Upload"));
    dialog.set_default_response(Some("upload"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("upload", adw::ResponseAppearance::Suggested);

    let names: Vec<String> = accounts
        .iter()
        .map(|a| {
            if a.name.trim().is_empty() {
                a.where_shown()
            } else {
                a.name.clone()
            }
        })
        .collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let combo = adw::ComboRow::new();
    combo.set_title(&i18n("Account"));
    combo.set_model(Some(&gtk::StringList::new(&name_refs)));
    combo.set_visible(accounts.len() > 1);

    // This upload's terms, starting from the account's own.
    let expire = adw::SpinRow::with_range(0.0, 365.0, 1.0);
    expire.set_title(&i18n("Links expire after"));
    expire.set_subtitle(&i18n("Days; 0 keeps the link indefinitely"));
    let protect = adw::SwitchRow::new();
    protect.set_title(&i18n("Protect with a password"));
    let password = adw::EntryRow::new();
    password.set_title(&i18n("Download password (empty: generated)"));
    let apply_defaults = {
        let (expire, protect, password) = (expire.clone(), protect.clone(), password.clone());
        let accounts = accounts.to_vec();
        move |i: usize| {
            if let Some(a) = accounts.get(i) {
                // What the service allows for this account: a row it
                // does not is greyed out, with the reason.
                expire.set_sensitive(a.expiry_allowed());
                expire.set_subtitle(&if a.expiry_allowed() {
                    i18n("Days; 0 keeps the link indefinitely")
                } else {
                    a.link_note.clone()
                });
                protect.set_sensitive(a.password_allowed());
                protect.set_subtitle(if a.password_allowed() {
                    ""
                } else {
                    a.link_note.as_str()
                });
                expire.set_value(if a.expiry_allowed() {
                    a.expire_days as f64
                } else {
                    0.0
                });
                protect.set_active(a.password_allowed() && a.password);
                password.set_text("");
            }
        }
    };
    apply_defaults(0);
    password.set_visible(protect.is_active());
    {
        let password = password.clone();
        protect.connect_active_notify(move |p| password.set_visible(p.is_active()));
    }
    {
        let apply_defaults = apply_defaults.clone();
        combo.connect_selected_notify(move |c| apply_defaults(c.selected() as usize));
    }

    let group = adw::PreferencesGroup::new();
    group.add(&combo);
    group.add(&expire);
    group.add(&protect);
    group.add(&password);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bx.set_width_request(400);
    bx.append(&group);
    dialog.set_extra_child(Some(&bx));

    let accounts = accounts.to_vec();
    let paths = std::cell::RefCell::new(Some(paths));
    dialog.connect_response(None, move |_, resp| {
        if resp != "upload" {
            return;
        }
        let (Some(paths), Some(account)) = (
            paths.borrow_mut().take(),
            accounts.get(combo.selected() as usize),
        ) else {
            return;
        };
        let mut account = account.clone();
        account.expire_days = expire.value() as u32;
        account.password = protect.is_active();
        let typed = password.text().trim().to_string();
        let link_password = (account.password && !typed.is_empty()).then_some(typed);
        let _ = sender.send(ComposeInput::CloudUpload {
            paths,
            account,
            link_password,
        });
    });
    dialog.present();
}
