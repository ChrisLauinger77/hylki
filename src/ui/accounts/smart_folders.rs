//! Smart folders (#417): saved searches, their list and their editor.

use super::*;

/// One condition of the smart folder editor (#417): the filter editor's
/// rows, and a tag chooser for Tagged.
struct SmartCondRows {
    group: adw::PreferencesGroup,
    remove: gtk::Button,
    field: adw::ComboRow,
    matcher: adw::ComboRow,
    value: adw::EntryRow,
    tag: adw::ComboRow,
}

impl AccountsWindow {
    /// The Smart Folders list (#417): a row per folder, saying where it
    /// looks, activating to edit, with a remove button.
    pub(super) fn rebuild_smart_rows(&self, sender: &ComponentSender<Self>) {
        let Some(list) = &self.smart_list else { return };
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        list.set_visible(!self.smart_folders.is_empty());
        for (i, f) in self.smart_folders.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_activatable(true);
            let s = sender.clone();
            row.connect_activated(move |_| {
                s.input_sender().emit(AccountsInput::EditSmartFolder(i))
            });
            row.set_title(&gtk::glib::markup_escape_text(&f.name));
            let account = if f.account_email.is_empty() {
                i18n("All accounts")
            } else {
                f.account_email.clone()
            };
            let folder = f
                .folder
                .map_or_else(|| i18n("All but Trash and Junk"), Self::smart_kind_label);
            row.set_subtitle(&gtk::glib::markup_escape_text(&format!(
                "{account} · {folder}"
            )));
            row.add_prefix(&gtk::Image::from_icon_name("folder-saved-search-symbolic"));
            let rm = gtk::Button::from_icon_name("user-trash-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            rm.set_tooltip_text(Some(i18n("Remove smart folder").as_str()));
            let s = sender.clone();
            rm.connect_clicked(move |_| s.input_sender().emit(AccountsInput::RemoveSmartFolder(i)));
            row.add_suffix(&rm);
            list.append(&row);
        }
    }

    fn smart_kind_label(k: crate::config::SmartFolderKind) -> String {
        use crate::config::SmartFolderKind::*;
        match k {
            Inbox => i18n("Inbox"),
            Sent => i18n("Sent"),
            Drafts => i18n("Drafts"),
            Archive => i18n("Archive"),
            Junk => i18n("Junk"),
            Trash => i18n("Trash"),
        }
    }

    fn smart_field_label(f: crate::config::SmartField) -> String {
        use crate::config::SmartField::*;
        match f {
            Starred => i18n("Starred"),
            NotStarred => i18n("Not starred"),
            Unread => i18n("Unread"),
            Read => i18n("Read"),
            Attachment => i18n("Has attachments"),
            Tagged => i18n("Tagged"),
            Body => i18n("Message preview"),
            other => other
                .filter_field()
                .map(Self::field_label)
                .unwrap_or_default(),
        }
    }

    /// The smart folder editor (#417): a name, where it looks, and its
    /// conditions, each a group of its own as in the filter editor. With
    /// `edit` it opens on that folder and saves over it.
    pub(super) fn open_smart_page(
        &self,
        nav: &adw::NavigationView,
        sender: &ComponentSender<Self>,
        edit: Option<(usize, crate::config::SmartFolder)>,
    ) {
        use crate::config::{
            FilterMatch, SmartCondition, SmartField, SmartFolder, SmartFolderKind,
        };
        let (title, verb) = if edit.is_some() {
            (i18n("Edit Smart Folder"), i18n("Save"))
        } else {
            (i18n("Add Smart Folder"), i18n("Add Smart Folder"))
        };
        let emails: Vec<String> = self.accounts.iter().map(|a| a.email.clone()).collect();

        let page_box = gtk::Box::new(gtk::Orientation::Vertical, 24);
        page_box.set_margin_top(24);
        page_box.set_margin_bottom(24);
        page_box.set_margin_start(12);
        page_box.set_margin_end(12);
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(600);
        clamp.set_tightening_threshold(400);
        clamp.set_child(Some(&page_box));
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scrolled.set_vexpand(true);
        scrolled.set_child(Some(&clamp));

        let where_group = adw::PreferencesGroup::new();
        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Name"));
        where_group.add(&name_row);
        let account_row = adw::ComboRow::new();
        account_row.set_title(&i18n("Account"));
        {
            let all = i18n("All accounts");
            let mut names: Vec<&str> = vec![all.as_str()];
            names.extend(emails.iter().map(|e| e.as_str()));
            account_row.set_model(Some(&gtk::StringList::new(&names)));
        }
        account_row.set_subtitle(&i18n("Those in the unified section."));
        where_group.add(&account_row);
        let folder_row = adw::ComboRow::new();
        folder_row.set_title(&i18n("Folders"));
        {
            let mut names = vec![i18n("All but Trash and Junk")];
            names.extend(
                SmartFolderKind::ALL
                    .iter()
                    .map(|k| Self::smart_kind_label(*k)),
            );
            let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            folder_row.set_model(Some(&gtk::StringList::new(&refs)));
        }
        where_group.add(&folder_row);

        // The conditions: Where, then Match and Text for the fields that
        // look at text, or the tag for Tagged.
        let conds: std::rc::Rc<std::cell::RefCell<Vec<SmartCondRows>>> = Default::default();
        let field_names: Vec<String> = SmartField::ALL
            .iter()
            .map(|f| Self::smart_field_label(*f))
            .collect();
        let match_names: Vec<String> = FilterMatch::ALL
            .iter()
            .map(|m| Self::match_label(*m))
            .collect();
        let tags = self.tags.clone();
        let tag_names: Vec<String> = std::iter::once(i18n("Any tag"))
            .chain(tags.iter().map(|t| t.name.clone()))
            .collect();
        let make_cond = {
            let tags = tags.clone();
            move |init: Option<&SmartCondition>| -> SmartCondRows {
                let group = adw::PreferencesGroup::new();
                let remove = gtk::Button::from_icon_name("user-trash-symbolic");
                remove.add_css_class("flat");
                remove.set_valign(gtk::Align::Center);
                remove.set_tooltip_text(Some(i18n("Remove condition").as_str()));
                group.set_header_suffix(Some(&remove));
                let field = adw::ComboRow::new();
                field.set_title(&i18n("Where"));
                let refs: Vec<&str> = field_names.iter().map(|s| s.as_str()).collect();
                field.set_model(Some(&gtk::StringList::new(&refs)));
                let matcher = adw::ComboRow::new();
                matcher.set_title(&i18n("Match"));
                matcher.set_subtitle(&i18n("Separate alternatives with commas."));
                let refs: Vec<&str> = match_names.iter().map(|s| s.as_str()).collect();
                matcher.set_model(Some(&gtk::StringList::new(&refs)));
                let value = adw::EntryRow::new();
                value.set_title(&i18n("Text to match"));
                let tag = adw::ComboRow::new();
                tag.set_title(&i18n("Tag"));
                let refs: Vec<&str> = tag_names.iter().map(|s| s.as_str()).collect();
                tag.set_model(Some(&gtk::StringList::new(&refs)));
                if let Some(c) = init {
                    if let Some(i) = SmartField::ALL.iter().position(|f| *f == c.field) {
                        field.set_selected(i as u32);
                    }
                    if let Some(i) = FilterMatch::ALL.iter().position(|m| *m == c.matcher) {
                        matcher.set_selected(i as u32);
                    }
                    if c.field == SmartField::Tagged {
                        if let Some(i) = tags
                            .iter()
                            .position(|t| t.keyword.eq_ignore_ascii_case(&c.value))
                        {
                            tag.set_selected(i as u32 + 1);
                        }
                    } else {
                        value.set_text(&c.value);
                    }
                }
                let explain = {
                    let (matcher, value, tag) = (matcher.clone(), value.clone(), tag.clone());
                    move |field: &adw::ComboRow| {
                        let chosen = SmartField::ALL.get(field.selected() as usize).copied();
                        let text = chosen.is_some_and(|f| f.takes_text());
                        let body = chosen == Some(SmartField::Body);
                        if body {
                            matcher.set_selected(0);
                        }
                        matcher.set_visible(text);
                        matcher.set_sensitive(!body);
                        value.set_visible(text);
                        tag.set_visible(chosen == Some(SmartField::Tagged));
                        field.set_subtitle(&match chosen {
                            Some(SmartField::Body) => {
                                i18n("The preview each message has in the list")
                            }
                            Some(SmartField::ReplyTo) => {
                                i18n("From address when no Reply-To is set")
                            }
                            Some(SmartField::FromInContacts | SmartField::FromNotInContacts) => {
                                i18n("Looks in the system address books and the Hylki book")
                            }
                            _ => String::new(),
                        });
                    }
                };
                explain(&field);
                field.connect_selected_notify(explain);
                group.add(&field);
                group.add(&matcher);
                group.add(&value);
                group.add(&tag);
                SmartCondRows {
                    group,
                    remove,
                    field,
                    matcher,
                    value,
                    tag,
                }
            }
        };

        let add_group = adw::PreferencesGroup::new();
        let add_row = adw::ActionRow::new();
        add_row.set_title(&i18n("Add Condition"));
        add_row.set_activatable(true);
        add_row.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
        add_group.add(&add_row);
        let combine_row = adw::ComboRow::new();
        combine_row.set_title(&i18n("Condition matching"));
        combine_row.set_model(Some(&gtk::StringList::new(&[
            i18n("All must match").as_str(),
            i18n("Any one may match").as_str(),
        ])));
        let combine_group = adw::PreferencesGroup::new();
        combine_group.add(&combine_row);
        combine_group.set_visible(false);

        let relabel = {
            let (conds, combine_row, combine_group) =
                (conds.clone(), combine_row.clone(), combine_group.clone());
            move || {
                let conds = conds.borrow();
                let any = combine_row.selected() == 1;
                let several = conds.len() > 1;
                for (i, c) in conds.iter().enumerate() {
                    c.group
                        .set_title(&i18n_f("Condition {n}", &[("n", &(i + 1).to_string())]));
                    c.field.set_title(&if i == 0 {
                        i18n("Where")
                    } else if any {
                        i18n("Or where")
                    } else {
                        i18n("And where")
                    });
                    c.remove.set_visible(several);
                }
                combine_group.set_visible(several);
            }
        };
        {
            let relabel = relabel.clone();
            combine_row.connect_selected_notify(move |_| relabel());
        }
        let add_cond = {
            let (conds, page_box, where_group, relabel) = (
                conds.clone(),
                page_box.clone(),
                where_group.clone(),
                relabel.clone(),
            );
            let (form_save, nav) = (self.form_save.clone(), nav.clone());
            move |init: Option<&SmartCondition>| {
                let c = make_cond(init);
                let after: gtk::Widget = conds
                    .borrow()
                    .last()
                    .map(|last| last.group.clone().upcast())
                    .unwrap_or_else(|| where_group.clone().upcast());
                page_box.insert_child_after(&c.group, Some(&after));
                {
                    let (conds, page_box, relabel, group) = (
                        conds.clone(),
                        page_box.clone(),
                        relabel.clone(),
                        c.group.clone(),
                    );
                    c.remove.connect_clicked(move |_| {
                        let gone = {
                            let mut conds = conds.borrow_mut();
                            if conds.len() < 2 {
                                return;
                            }
                            conds
                                .iter()
                                .position(|c| c.group == group)
                                .map(|i| conds.remove(i))
                        };
                        if let Some(gone) = gone {
                            page_box.remove(&gone.group);
                            relabel();
                        }
                    });
                }
                {
                    let (form_save, nav) = (form_save.clone(), nav.clone());
                    c.value.connect_entry_activated(move |_| {
                        if form_save.borrow().as_ref().is_some_and(|save| save()) {
                            nav.pop();
                        }
                    });
                }
                conds.borrow_mut().push(c);
                relabel();
            }
        };
        {
            let add_cond = add_cond.clone();
            add_row.connect_activated(move |_| add_cond(None));
        }

        let edit_index = edit.as_ref().map(|(i, _)| *i);
        let id = edit
            .as_ref()
            .map_or_else(crate::config::new_smart_folder_id, |(_, f)| f.id.clone());
        if let Some((_, f)) = &edit {
            name_row.set_text(&f.name);
            if let Some(i) = emails
                .iter()
                .position(|e| e.eq_ignore_ascii_case(&f.account_email))
            {
                account_row.set_selected(i as u32 + 1);
            }
            if let Some(i) = f
                .folder
                .and_then(|k| SmartFolderKind::ALL.iter().position(|x| *x == k))
            {
                folder_row.set_selected(i as u32 + 1);
            }
            combine_row.set_selected(u32::from(f.any));
        }

        page_box.append(&where_group);
        page_box.append(&add_group);
        page_box.append(&combine_group);
        match &edit {
            Some((_, f)) if !f.conditions.is_empty() => {
                for c in &f.conditions {
                    add_cond(Some(c));
                }
            }
            _ => add_cond(None),
        }

        let s = sender.clone();
        self.push_form_content(
            nav,
            "smart",
            &title,
            &verb,
            scrolled.upcast_ref(),
            vec![name_row.clone()],
            move || {
                let name = name_row.text().trim().to_string();
                // A text condition with no text says nothing and is dropped;
                // the others need none.
                let conditions: Vec<SmartCondition> = conds
                    .borrow()
                    .iter()
                    .filter_map(|c| {
                        let field =
                            SmartField::ALL[c.field.selected() as usize % SmartField::ALL.len()];
                        let value = if field == SmartField::Tagged {
                            match c.tag.selected() {
                                0 => String::new(),
                                i => tags
                                    .get(i as usize - 1)
                                    .map(|t| t.keyword.clone())
                                    .unwrap_or_default(),
                            }
                        } else if field.takes_text() {
                            let v = c.value.text().trim().to_string();
                            if v.is_empty() {
                                return None;
                            }
                            v
                        } else {
                            String::new()
                        };
                        Some(SmartCondition {
                            field,
                            matcher: FilterMatch::ALL
                                [c.matcher.selected() as usize % FilterMatch::ALL.len()],
                            value,
                        })
                    })
                    .collect();
                if name.is_empty() || conditions.is_empty() {
                    return false;
                }
                let account_email = match account_row.selected() {
                    0 => String::new(),
                    i => emails.get(i as usize - 1).cloned().unwrap_or_default(),
                };
                let folder = match folder_row.selected() {
                    0 => None,
                    i => SmartFolderKind::ALL.get(i as usize - 1).copied(),
                };
                let folder = SmartFolder {
                    id: id.clone(),
                    name,
                    account_email,
                    folder,
                    any: combine_row.selected() == 1,
                    conditions,
                };
                s.input_sender()
                    .emit(AccountsInput::SmartFolderSaved(edit_index, folder));
                true
            },
        );
    }
}
