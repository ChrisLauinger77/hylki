//! Send-as aliases (#34) and hidden folders (#239) in the account editor.

use super::*;

/// The alias editor dialog (#34): a small modal for one send-as alias — its
/// identity, and optionally its own SMTP transport.
pub(super) struct AliasDialog {
    pub(super) window: adw::Window,
    pub(super) name_row: adw::EntryRow,
    pub(super) addr_row: adw::EntryRow,
    pub(super) smtp_switch: adw::SwitchRow,
    pub(super) host_row: adw::EntryRow,
    pub(super) port_row: adw::EntryRow,
    pub(super) user_row: adw::EntryRow,
    pub(super) pass_row: adw::PasswordEntryRow,
    pub(super) test_btn: gtk::Button,
    pub(super) test_result: gtk::Label,
}

impl AccountsWindow {
    /// Fill the Hidden Folders list (#239): one row per hidden path, with
    /// a Show button that takes it off the list (Save makes it so).
    pub(super) fn rebuild_hidden_list(
        &self,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let list = &widgets.hidden_list;
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        // An empty boxed-list draws as a bare frame; say "none" instead.
        list.set_visible(!self.hidden_edits.is_empty());
        widgets
            .hidden_empty
            .set_visible(self.hidden_edits.is_empty());
        for (i, path) in self.hidden_edits.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&crate::mutf7::decode(path)));
            let show = gtk::Button::with_label(&i18n("Show"));
            show.set_valign(gtk::Align::Center);
            show.add_css_class("flat");
            show.set_tooltip_text(Some(i18n("Bring this folder back to the sidebar").as_str()));
            let s = sender.input_sender().clone();
            show.connect_clicked(move |_| {
                let _ = s.send(AccountsInput::UnhideFolder(i));
            });
            row.add_suffix(&show);
            list.append(&row);
        }
    }

    /// Rebuild the editor's send-as alias list from `alias_edits` (#34).
    pub(super) fn rebuild_alias_list(&self, list: &gtk::ListBox, sender: &ComponentSender<Self>) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        // The server's identities (#346), less the account's own plain one
        // (its address, under no name or the account's) and any address an
        // alias here already sends as.
        let (own_name, own) = self
            .editing
            .and_then(|i| self.accounts.get(i))
            .map(|a| (a.name.clone(), a.email.clone()))
            .unwrap_or_default();
        let server: Vec<&(String, String)> = self
            .server_identities
            .iter()
            .filter(|(name, addr)| {
                !(addr.eq_ignore_ascii_case(&own)
                    && (name.is_empty() || name.eq_ignore_ascii_case(addr) || *name == own_name))
            })
            .filter(|(_, addr)| {
                !self
                    .alias_edits
                    .iter()
                    .any(|a| split_identity(&a.identity).1.eq_ignore_ascii_case(addr))
            })
            .collect();
        // An empty boxed-list draws as a bare frame; hide it until there is a row.
        list.set_visible(!self.alias_edits.is_empty() || !server.is_empty());

        for (i, alias) in self.alias_edits.iter().enumerate() {
            let row = adw::ActionRow::new();
            row.set_activatable(true);
            row.set_title(&gtk::glib::markup_escape_text(&alias.identity));
            row.set_subtitle(&gtk::glib::markup_escape_text(&if alias.has_own_smtp() {
                i18n_f("Sends through {host}", &[("host", &alias.smtp_host)])
            } else {
                i18n("Sends through this account")
            }));

            // A dim pencil says "activate to edit"; the trash button removes.
            let edit = gtk::Image::from_icon_name("document-edit-symbolic");
            edit.add_css_class("dim-label");
            row.add_suffix(&edit);

            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.set_valign(gtk::Align::Center);
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some(i18n("Remove this alias").as_str()));
            let ri = sender.input_sender().clone();
            remove.connect_clicked(move |_| {
                let _ = ri.send(AccountsInput::AliasRemove(i));
            });
            row.add_suffix(&remove);

            list.append(&row);
        }

        for (name, addr) in server {
            let row = adw::ActionRow::new();
            let title = if name.is_empty() || name.eq_ignore_ascii_case(addr) {
                addr.clone()
            } else {
                format!("{name} <{addr}>")
            };
            row.set_title(&gtk::glib::markup_escape_text(&title));
            row.set_subtitle(&i18n(
                "Kept on the server; change it in your provider's webmail.",
            ));
            list.append(&row);
        }
    }

    /// Close the alias dialog, if one is open.
    pub(super) fn close_alias_dialog(&mut self) {
        if let Some(d) = self.alias_dialog.take() {
            d.window.close();
        }
    }

    /// Open the modal alias editor (#34), prefilled from `alias`. A JMAP
    /// account's server sends every message itself (#346), so its aliases
    /// have no SMTP server of their own to set.
    pub(super) fn open_alias_dialog(
        &mut self,
        root: &adw::Bin,
        alias: &AliasConfig,
        jmap: bool,
        sender: &ComponentSender<Self>,
    ) {
        self.close_alias_dialog();

        let window = adw::Window::builder()
            .modal(true)
            .default_width(440)
            .title(if self.alias_editing.is_some() {
                i18n("Edit Alias")
            } else {
                i18n("Add Alias")
            })
            .build();
        // Parent to whatever window the embedded panel currently sits in.
        window.set_transient_for(root.root().and_downcast::<gtk::Window>().as_ref());

        let cancel = gtk::Button::with_label(&i18n("Cancel"));
        let save = gtk::Button::with_label(&i18n("Save"));
        save.add_css_class("suggested-action");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.pack_start(&cancel);
        header.pack_end(&save);

        let (name, addr) = split_identity(&alias.identity);
        let identity_group = adw::PreferencesGroup::new();
        identity_group.set_description(Some(
            "The composer's From menu offers this address, and replies to \
             mail sent to it answer from it.",
        ));
        let name_row = adw::EntryRow::builder()
            .title(&i18n("Display name (optional)"))
            .build();
        name_row.set_text(&name);
        let addr_row = adw::EntryRow::builder()
            .title(&i18n("Email address"))
            .build();
        addr_row.set_text(&addr);
        identity_group.add(&name_row);
        identity_group.add(&addr_row);

        let smtp_group = adw::PreferencesGroup::new();
        smtp_group.set_title(&i18n("Sending"));
        let smtp_switch = adw::SwitchRow::builder()
            .title(&i18n("Own SMTP server"))
            .subtitle(
                "Send through the alias's own mail provider, with its own \
                 sign-in — instead of this account's server. Needed when the \
                 account's provider (e.g. Gmail) rewrites the sender address.",
            )
            .build();
        smtp_switch.set_active(alias.has_own_smtp());
        let host_row = adw::EntryRow::builder().title(&i18n("SMTP server")).build();
        host_row.set_text(&alias.smtp_host);
        let port_row = adw::EntryRow::builder().title(&i18n("SMTP port")).build();
        port_row.set_text(&alias.smtp_port.to_string());
        let user_row = adw::EntryRow::builder()
            .title(&i18n("SMTP username"))
            .build();
        user_row.set_text(&alias.smtp_username);
        let pass_row = adw::PasswordEntryRow::builder()
            .title(&i18n("SMTP password"))
            .build();
        pass_row.set_text(&alias.smtp_password);
        smtp_group.add(&smtp_switch);
        smtp_group.add(&host_row);
        smtp_group.add(&port_row);
        smtp_group.add(&user_row);
        smtp_group.add(&pass_row);

        let test_btn = gtk::Button::with_label(&i18n("Test SMTP"));
        test_btn.set_halign(gtk::Align::Start);
        let test_result = gtk::Label::new(None);
        test_result.set_visible(false);
        test_result.set_halign(gtk::Align::Start);
        test_result.set_xalign(0.0);
        test_result.set_wrap(true);
        let test_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        test_box.append(&test_btn);
        test_box.append(&test_result);

        // The SMTP fields (and their test) only apply with the switch on.
        for target in [
            host_row.upcast_ref::<gtk::Widget>(),
            port_row.upcast_ref(),
            user_row.upcast_ref(),
            pass_row.upcast_ref(),
            test_box.upcast_ref(),
        ] {
            smtp_switch
                .bind_property("active", target, "visible")
                .sync_create()
                .build();
        }

        let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
        content.set_margin_top(24);
        content.set_margin_bottom(24);
        content.set_margin_start(24);
        content.set_margin_end(24);
        content.append(&identity_group);
        if !jmap {
            content.append(&smtp_group);
            content.append(&test_box);
        }

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(640)
            .child(&content)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroller));
        window.set_content(Some(&toolbar));

        let w = window.clone();
        cancel.connect_clicked(move |_| w.close());
        let s = sender.input_sender().clone();
        save.connect_clicked(move |_| {
            let _ = s.send(AccountsInput::AliasDialogSave);
        });
        let s = sender.input_sender().clone();
        test_btn.connect_clicked(move |_| {
            let _ = s.send(AccountsInput::AliasDialogTest);
        });
        let s = sender.input_sender().clone();
        window.connect_close_request(move |_| {
            let _ = s.send(AccountsInput::AliasDialogClosed);
            gtk::glib::Propagation::Proceed
        });

        window.present();
        self.alias_dialog = Some(AliasDialog {
            window,
            name_row,
            addr_row,
            smtp_switch,
            host_row,
            port_row,
            user_row,
            pass_row,
            test_btn,
            test_result,
        });
    }
}
