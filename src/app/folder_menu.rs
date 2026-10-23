//! The folder context menu's dialogs: new, rename, delete, empty.

use super::*;

impl AppModel {
    /// Prompt for a new custom folder name and create it under `account_id`.
    pub(super) fn prompt_new_folder(&self, account_id: u32, sender: &ComponentSender<Self>) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(i18n("New Folder").as_str()),
            Some(i18n("Create a new folder for this account.").as_str()),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("ok", &i18n("Create"));
        dialog.set_default_response(Some("ok"));
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some(i18n("Folder name").as_str()));
        entry.set_activates_default(true);
        dialog.set_extra_child(Some(&entry));
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "ok" {
                let name = entry.text().to_string();
                if !name.trim().is_empty() {
                    s.input(AppMsg::CreateFolder { account_id, name });
                }
            }
        });
        dialog.present();
    }

    /// Prompt for a folder's new name and rename it in place.
    pub(super) fn prompt_rename_folder(
        &self,
        account_id: u32,
        name: String,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(&format!("Rename {name:?}")),
            Some(i18n("Sub-folders keep their place under the new name.").as_str()),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("ok", &i18n("Rename"));
        dialog.set_default_response(Some("ok"));
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let entry = gtk::Entry::new();
        entry.set_text(&name);
        entry.select_region(0, -1);
        entry.set_activates_default(true);
        dialog.set_extra_child(Some(&entry));
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "ok" {
                let new_name = entry.text().to_string();
                if !new_name.trim().is_empty() {
                    s.input(AppMsg::RenameFolderTo {
                        account_id,
                        path: path.clone(),
                        new_name,
                    });
                }
            }
        });
        dialog.present();
    }

    /// Rename a custom folder's leaf in place: same parent, new (encoded)
    /// name, the same optimistic machinery as a drag-and-drop move.
    pub(super) fn rename_folder_to(&mut self, account_id: u32, path: String, new_name: String) {
        let delim = self.folder_delimiter(account_id);
        let new_name = new_name.trim();
        // The path's parent (everything up to and including the last
        // delimiter) stays; only the leaf changes.
        let parent = match path.rfind(delim) {
            Some(at) => &path[..=at],
            None => "",
        };
        let new_path = format!("{parent}{}", crate::mutf7::encode(new_name));
        if new_path == path {
            return;
        }
        if self
            .folders
            .get(&account_id)
            .is_some_and(|fs| fs.iter().any(|f| f.path == new_path))
        {
            self.notifications.emit(NotifyInput::Push {
                text: i18n_f(
                    "A folder named {new_name} is already there.",
                    &[("new_name", &format!("{new_name:?}"))],
                ),
                error: true,
                connectivity: false,
            });
            return;
        }
        self.apply_folder_rename(account_id, path, new_path, Some(i18n("Rename Folder")));
    }

    /// Confirm deleting a custom folder (contents moved to Trash). Deliberately
    /// outside the undo history (#200): the mailbox itself is gone from the
    /// server, so the dialog says as much instead of promising a way back.
    pub(super) fn confirm_delete_folder(
        &self,
        account_id: u32,
        name: String,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(&format!("Delete “{name}”?")),
            Some(
                i18n(
                    "Its messages are moved to Trash and the folder is removed from the \
                      server. This cannot be undone.",
                )
                .as_str(),
            ),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("delete", &i18n("Delete"));
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "delete" {
                s.input(AppMsg::DeleteFolder {
                    account_id,
                    path: path.clone(),
                });
            }
        });
        dialog.present();
    }

    /// Empty Trash / Empty Junk (#152): there is no undo, so always ask.
    pub(super) fn confirm_empty_folder(
        &self,
        account_id: u32,
        folder_id: u32,
        name: String,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(&i18n_f("Empty “{name}”?", &[("name", &name)])),
            Some(
                i18n("Every message in it will be erased from the server. This can’t be undone.")
                    .as_str(),
            ),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("empty", &i18n("Empty"));
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "empty" {
                s.input(AppMsg::EmptyFolder {
                    account_id,
                    folder_id,
                    path: path.clone(),
                });
            }
        });
        dialog.present();
    }

    /// Empty every unified account's Trash or Junk (#413). One folder asks
    /// the way its own row does; more name each account they belong to.
    pub(super) fn confirm_empty_all(&self, kind: FolderKind, sender: &ComponentSender<Self>) {
        let targets: Vec<(u32, u32, String, String, String)> = self
            .accounts
            .iter()
            .filter(|a| self.in_unified(a.id))
            .filter_map(|a| {
                let f = self.folder_of_kind(a.id, kind)?;
                Some((a.id, f.id, f.name.clone(), f.path.clone(), a.email.clone()))
            })
            .collect();
        match targets.as_slice() {
            [] => {}
            [(account_id, folder_id, name, path, _)] => {
                self.confirm_empty_folder(
                    *account_id,
                    *folder_id,
                    name.clone(),
                    path.clone(),
                    sender,
                );
            }
            _ => {
                let heading = if kind == FolderKind::Trash {
                    i18n("Empty Trash in Every Account?")
                } else {
                    i18n("Empty Junk in Every Account?")
                };
                let accounts: Vec<&str> = targets.iter().map(|t| t.4.as_str()).collect();
                let body = format!(
                    "{}\n\n{}",
                    i18n("Every message in them will be erased from the server. This can’t be undone."),
                    accounts.join("\n")
                );
                let dialog =
                    adw::MessageDialog::new(Some(&self.window), Some(&heading), Some(&body));
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("empty", &i18n("Empty"));
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                dialog.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
                let s = sender.clone();
                dialog.connect_response(None, move |_, resp| {
                    if resp == "empty" {
                        for (account_id, folder_id, _, path, _) in &targets {
                            s.input(AppMsg::EmptyFolder {
                                account_id: *account_id,
                                folder_id: *folder_id,
                                path: path.clone(),
                            });
                        }
                    }
                });
                dialog.present();
            }
        }
    }
}
