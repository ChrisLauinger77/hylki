//! Accounts panel: manage all mail accounts (add / edit / remove / reorder).
//!
//! Not a window of its own: the panel is embedded behind the "Accounts" tab
//! of the combined Accounts & Preferences window (see `ui/preferences.rs`).
//! It uses an `AdwNavigationView` with two pages: a list of accounts (drag rows
//! to set the sidebar order) and a reusable editor form pushed on top.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{
    split_identity, AccountConfig, AliasConfig, NamedSignature, OAuthSettings, Protocol,
};
use crate::i18n::{i18n, i18n_f, i18n_noop};
use crate::ui::rich_editor::{self, RichEditor};
use crate::ui::sender_row::{SenderRow, SenderRowOutput};
use crate::worker::{self, ConnTest};

mod account_list;
mod aliases;
mod editor;
mod filters;
mod form;
mod form_page;
mod messages;
mod oauth;
mod providers;
mod signatures;
mod smart_folders;
mod tags;

use account_list::{importable_goa_accounts, importable_of};
use aliases::AliasDialog;
pub use editor::AccountSecrets;
pub(crate) use form::non_ellipsizing_factory;
use form::{
    age_labels, apply_protocol, auto_empty_labels, clear_editor, fill_editor, form_protocol,
    import_avatar_file, left_justify_wrapping_labels, mount_editor, parse_color, read_account,
    row_matches, set_connection_editable, show_quota, trimmed, wrap_subtitle, CLEAN_LISTS_DAYS,
    REPLY_COPIES_HINT, SENT_COPY_HINT,
};
pub use messages::{AccountsCmd, AccountsInit, AccountsInput, AccountsOutput, TagProposal};
pub(crate) use oauth::open_online_accounts;
use oauth::sign_in;
use providers::provider_at;
pub(crate) use providers::{brand_for_goa, provider_factory, Provider, PROVIDERS};
use signatures::signature_label;

const DEFAULT_COLOR: &str = "#3584e4";

pub struct AccountsWindow {
    /// Accounts in display order.
    accounts: Vec<AccountConfig>,
    /// Each account's live folder list (path, display name), for the editor's
    /// Special Folders combos — pushed by the app, keyed by account email.
    folders_by_email: std::collections::HashMap<String, Vec<(String, String)>>,
    /// Allowed-senders and blocklist rows (moved here from Settings).
    senders: relm4::factory::FactoryVecDeque<SenderRow>,
    sender_addrs: Vec<String>,
    blacklist: relm4::factory::FactoryVecDeque<SenderRow>,
    blacklist_addrs: Vec<String>,
    /// Filter rules (#47), managed on this tab.
    filter_rules: Vec<crate::config::FilterRule>,
    filters_list: Option<gtk::ListBox>,
    /// The filters page's search text, read by the list's filter function.
    filters_query: std::rc::Rc<std::cell::RefCell<String>>,
    /// The open filter or tag page's Save: reads the form and hands the
    /// result back, true when it went through (the page then pops). The
    /// settings window's leave-editor prompt saves through it too.
    form_save: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn() -> bool>>>>,
    /// The senders page's search text, shared by both lists.
    senders_query: std::rc::Rc<std::cell::RefCell<String>>,
    /// Tags (#71), managed on this tab too — the rules' "Tag with" names them.
    tags: Vec<crate::config::Tag>,
    tags_list: Option<gtk::ListBox>,
    /// Smart folders (#417): saved searches the sidebar lists.
    smart_folders: Vec<crate::config::SmartFolder>,
    smart_list: Option<gtk::ListBox>,
    /// The tag finder's button: its spinner turns while the mailboxes are
    /// being read, and its label says so.
    find_tags_spinner: Option<gtk::Spinner>,
    /// The Apply Now button's spinner and label, turned on while a manual
    /// filter run is under way (#198).
    apply_filters_spinner: Option<gtk::Spinner>,
    apply_filters_label: Option<gtk::Label>,
    /// Whether a manual filter run is under way; a second press does nothing.
    filters_applying: bool,
    find_tags_label: Option<gtk::Label>,
    tag_scanning: bool,
    /// Paths behind the currently-open editor's folder combos (index 0 in the
    /// combo is "Automatic"; entry N here is combo index N + 1).
    folder_paths: Vec<String>,
    /// Paths behind the "Save a copy of sent mail in" combo (#199). A separate list from
    /// `folder_paths`: this one offers every folder, the Inbox included,
    /// because a destination takes nothing away from the folder it names.
    /// Index 0 in the combo is "Sent folder"; entry N here is index N + 1.
    sent_copy_paths: Vec<String>,
    /// Index being edited; `None` while adding a new account.
    editing: Option<usize>,
    /// Emoji currently chosen in the editor (`None` → use initials).
    emoji: Option<String>,
    /// Avatar picture chosen in the editor (#162): its file name under the
    /// avatars directory.
    avatar: Option<String>,
    /// The preview circle's color, as a stylesheet on the display.
    preview_css: gtk::CssProvider,
    /// The account list's per-row disc colors (`.acct-list-color-N`),
    /// rewritten on every rebuild.
    list_css: gtk::CssProvider,
    /// The "Circle shows" toggle: a picture, or initials/emoji. Only the
    /// chosen side is saved; the other side's choice stays in the editor
    /// so flipping back and forth loses nothing.
    picture_mode: bool,
    /// What the account editor held when it opened, so leaving an untouched
    /// one asks nothing. `None` while no editor is open — or when something
    /// filled the form without recording it, where the safe answer is that
    /// it was touched.
    editor_seed: Option<String>,
    /// WYSIWYG editor for the account signature.
    /// The signature's rich editor — a WebKit view, so it is created when
    /// an account's editor first opens rather than with the panel.
    sig_editor: Option<RichEditor>,
    /// "Edit HTML" / "Preview" at the end of the signature editor's
    /// toolbar, put there when the editor is made.
    sig_source_btn: gtk::Button,
    /// The account's signatures as edited (#445). The one shown is in
    /// `sig_editor` and is read back into its entry when another is shown
    /// or the account is saved.
    sig_edits: Vec<NamedSignature>,
    /// Index into `sig_edits` of the signature in the editor.
    sig_shown: usize,
    /// Which signature new messages start with.
    sig_default: Option<usize>,
    /// A signature was added, deleted, renamed, made the default, or had
    /// its text changed before another was shown: the WebView's own dirty
    /// flag only knows about the one it holds now.
    sig_touched: bool,
    /// The shown signature is being read back: the editor still holds it,
    /// so another switch waits for that to land.
    sig_reading: bool,
    /// The email value the label field currently mirrors, so the label auto-fills
    /// from the email until the user customizes it.
    label_synced: String,
    /// GNOME Online Accounts mail accounts available to import (not yet in Hylki).
    goa: Vec<crate::goa::GoaMailAccount>,
    /// Refresh token captured from a successful OAuth sign-in, applied on save.
    pending_oauth_refresh: Option<String>,
    /// Counts the times the editor opened, so a keyring answer for an
    /// editor since replaced is dropped.
    editor_session: u64,
    /// The keyring's answer for the account in the editor, once it came:
    /// Save fills the fields left empty from it.
    editor_secrets: Option<AccountSecrets>,
    /// A keyring read for the editor is out.
    secrets_reading: bool,
    /// Save was pressed before the keyring answered, and runs again when it
    /// does: the keyring may be waiting on its user (#466).
    save_waiting: bool,
    /// The editor's Microsoft sign-in may search the organization's
    /// directory (#410): saved with those scopes, or just approved for them.
    /// Save keeps asking for them, or the next sign-in would drop them.
    directory_granted: bool,
    /// What the directory switch is known to show, so the notify that
    /// filling the editor sets off is not taken for the user turning it on.
    directory_on: bool,
    /// The same two for the sensitivity label names (#411).
    labels_granted: bool,
    labels_on: bool,
    /// The send-as aliases being edited for the account in the editor (#34).
    /// Committed to the account on Save.
    alias_edits: Vec<AliasConfig>,
    /// The open account's identities on its JMAP server (#346), shown
    /// under the aliases and not editable here.
    server_identities: Vec<(String, String)>,
    /// The account's hidden folders (#239) as the editor holds them: the
    /// saved list, less whatever the user has brought back since opening.
    hidden_edits: Vec<String>,
    /// Index into `alias_edits` open in the alias dialog; `None` while adding.
    alias_editing: Option<usize>,
    /// The open alias editor dialog and its fields, if any.
    alias_dialog: Option<AliasDialog>,
}

#[relm4::component(pub)]
impl Component for AccountsWindow {
    type Init = AccountsInit;
    type Input = AccountsInput;
    type Output = AccountsOutput;
    type CommandOutput = AccountsCmd;

    view! {
        adw::Bin {
            #[wrap(Some)]
            #[name = "nav"]
            set_child = &adw::NavigationView {

                // ---- list page ----
                add = &adw::NavigationPage {
                    set_title: &i18n("Accounts"),
                    set_tag: Some("list"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        // No header of its own: the combined settings window's
                        // shared header (with the view switcher) sits above.

                        #[wrap(Some)]
                        #[name = "list_stack"]
                        set_content = &gtk::Stack {
                            // Sections switch outright, and only the shown
                            // one is measured.
                            set_transition_type: gtk::StackTransitionType::None,
                            set_hhomogeneous: false,
                            set_vhomogeneous: false,

                            add_named[Some("accounts")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Mail Accounts"),
                                    set_description: Some(
                                        i18n("Drag to reorder them in the sidebar.").as_str()
                                    ),
                                    // At the header's end, as the Tags and
                                    // Cloud Storage panels place theirs.
                                    #[wrap(Some)]
                                    set_header_suffix = &gtk::Box {
                                        set_orientation: gtk::Orientation::Horizontal,
                                        set_spacing: 6,
                                        set_valign: gtk::Align::Start,
                                        set_halign: gtk::Align::End,
                                        set_margin_start: 24,
                                        // An account just added in GNOME
                                        // Settings, without waiting for GOA
                                        // to say so.
                                        #[name = "goa_refresh_btn"]
                                        gtk::Button {
                                            set_icon_name: "view-refresh-symbolic",
                                            set_tooltip_text: Some(&i18n("Check GNOME Online Accounts for new accounts")),
                                            connect_clicked => AccountsInput::RefreshGoa,
                                        },
                                        gtk::Button {
                                            set_label: &i18n("Add Account…"),
                                            set_size_request: (130, -1),
                                            connect_clicked => AccountsInput::AddAccount,
                                        },
                                    },

                                    #[name = "accounts_list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                        connect_row_activated[sender] => move |_, row| {
                                            sender.input_sender().emit(AccountsInput::EditAccount(row.index() as usize));
                                        },
                                    },
                                },

                                #[name = "goa_group"]
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("GNOME Online Accounts"),
                                    set_description: Some(
                                        i18n("Mail accounts from GNOME Settings. Switch one on to use it.").as_str()
                                    ),
                                    set_visible: false,

                                    #[name = "goa_list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },

                            add_named[Some("tags")] = &adw::PreferencesPage {
                                // Tags (#71): a name and color per keyword.
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Tags"),
                                    set_description: Some(
                                        i18n("Tags are stored on the server as IMAP keywords, so Thunderbird and other clients show them too.\n\nDrag to reorder. The first nine are mapped to the 1–9 keys.").as_str()
                                    ),
                                    #[wrap(Some)]
                                    // Stacked like the Cloud Storage panel's
                                    // buttons: a column at the header's end.
                                    set_header_suffix = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 12,
                                        set_valign: gtk::Align::Start,
                                        set_halign: gtk::Align::End,
                                        set_margin_start: 24,
                                        // Both 130px wide (filling the column,
                                        // so they always share one width), and
                                        // the finder's label change moves nothing.
                                        gtk::Button {
                                            set_label: &i18n("Add Tag…"),
                                            set_size_request: (130, -1),
                                            connect_clicked => AccountsInput::AddTag,
                                        },
                                        // The tag finder: read every mailbox
                                        // for keywords already in use and
                                        // offer them as tags.
                                        #[name = "find_tags_btn"]
                                        gtk::Button {
                                            set_tooltip_text: Some(i18n("Look through every mailbox for tags other clients have set").as_str()),
                                            set_size_request: (130, -1),
                                            connect_clicked => AccountsInput::FindTags,
                                            // A spinner inside the button
                                            // while the mailboxes are read.
                                            gtk::Box {
                                                set_spacing: 6,
                                                set_halign: gtk::Align::Center,
                                                #[name = "find_tags_spinner"]
                                                gtk::Spinner {
                                                    set_visible: false,
                                                },
                                                #[name = "find_tags_label"]
                                                gtk::Label {
                                                    set_label: &i18n("Find Tags…"),
                                                },
                                            },
                                        },
                                    },

                                    #[name = "tags_list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },

                            add_named[Some("smart")] = &adw::PreferencesPage {
                                // Smart folders (#417): saved searches.
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Smart Folders"),
                                    set_description: Some(
                                        i18n("Lists the messages that match its conditions. Shown in the sidebar under the unified rows.").as_str()
                                    ),
                                    #[wrap(Some)]
                                    set_header_suffix = &gtk::Button {
                                        set_label: &i18n("Add Smart Folder…"),
                                        set_valign: gtk::Align::Start,
                                        set_margin_start: 24,
                                        connect_clicked => AccountsInput::AddSmartFolder,
                                    },

                                    #[name = "smart_list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },

                            add_named[Some("filters")] = &adw::PreferencesPage {
                                // A search box over the long lists (#141): rows
                                // that do not match the text are hidden.
                                add = &adw::PreferencesGroup {
                                    #[name = "filters_search"]
                                    gtk::SearchEntry {
                                        set_placeholder_text: Some(i18n("Search filters").as_str()),
                                        connect_search_changed[sender] => move |entry| {
                                            sender.input_sender().emit(AccountsInput::SearchFilters(entry.text().to_string()));
                                        },
                                    },
                                },

                                // Mail hygiene (moved from Settings): filters,
                                // the remote-content allow list, the blocklist.
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Filters"),
                                    set_description: Some(
                                        i18n("File or tag incoming mail automatically. Filters run on each Inbox as it syncs; Apply Now runs them over the mail already there.").as_str()
                                    ),
                                    // At the header's end, like the other
                                    // panels' buttons.
                                    #[wrap(Some)]
                                    set_header_suffix = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 6,
                                        set_valign: gtk::Align::Start,
                                        set_halign: gtk::Align::End,
                                        set_margin_start: 24,
                                        gtk::Button {
                                            set_label: &i18n("Add Filter…"),
                                            set_size_request: (130, -1),
                                            connect_clicked => AccountsInput::AddFilter,
                                        },
                                        // Rules normally meet mail as it
                                        // arrives; this holds them up against
                                        // what is already in the Inbox (#198).
                                        #[name = "apply_filters_btn"]
                                        gtk::Button {
                                            set_size_request: (130, -1),
                                            set_tooltip_text: Some(
                                                i18n("Run every rule over the mail already in your inboxes").as_str()
                                            ),
                                            connect_clicked => AccountsInput::ApplyFilters,
                                            // A spinner inside the button
                                            // while the run is under way, as
                                            // the tag finder has.
                                            gtk::Box {
                                                set_spacing: 6,
                                                set_halign: gtk::Align::Center,
                                                #[name = "apply_filters_spinner"]
                                                gtk::Spinner {
                                                    set_visible: false,
                                                },
                                                #[name = "apply_filters_label"]
                                                gtk::Label {
                                                    set_label: &i18n("Apply Now"),
                                                },
                                            },
                                        },
                                    },

                                    #[name = "filters_list"]
                                    gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },

                            add_named[Some("senders")] = &adw::PreferencesPage {
                                // A search box over the long lists (#141): rows
                                // that do not match the text are hidden.
                                add = &adw::PreferencesGroup {
                                    #[name = "senders_search"]
                                    gtk::SearchEntry {
                                        set_placeholder_text: Some(i18n("Search senders").as_str()),
                                        connect_search_changed[sender] => move |entry| {
                                            sender.input_sender().emit(AccountsInput::SearchSenders(entry.text().to_string()));
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Allowed Senders"),
                                    set_description: Some(
                                        i18n("Messages from these senders load remote content \
                                         automatically.").as_str()
                                    ),

                                    #[name = "add_sender_row"]
                                    adw::EntryRow {
                                        set_title: &i18n("Email address"),
                                        set_input_purpose: gtk::InputPurpose::Email,
                                        set_show_apply_button: false,
                                        connect_entry_activated[sender] => move |row| {
                                            sender.input_sender().emit(AccountsInput::AddSenderText(row.text().to_string()));
                                            row.set_text("");
                                        },

                                        add_suffix = &gtk::Button {
                                            set_icon_name: "list-add-symbolic",
                                            set_tooltip_text: Some(i18n("Allow this sender").as_str()),
                                            set_valign: gtk::Align::Center,
                                            add_css_class: "flat",
                                            connect_clicked[sender, add_sender_row] => move |_| {
                                                sender.input_sender().emit(AccountsInput::AddSenderText(
                                                    add_sender_row.text().to_string(),
                                                ));
                                                add_sender_row.set_text("");
                                            },
                                        },
                                    },

                                    #[local_ref]
                                    senders_box -> gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        add_css_class: "sender-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Blocked Senders"),
                                    set_description: Some(
                                        i18n("Their mail is moved to Trash on arrival. Enter an address, or a domain like \"example.com\" to apply to all senders from that domain.").as_str()
                                    ),

                                    #[name = "add_blacklist_row"]
                                    adw::EntryRow {
                                        set_title: &i18n("Address or domain"),
                                        set_show_apply_button: false,
                                        connect_entry_activated[sender] => move |row| {
                                            sender.input_sender().emit(AccountsInput::AddBlacklistText(row.text().to_string()));
                                            row.set_text("");
                                        },

                                        add_suffix = &gtk::Button {
                                            set_icon_name: "list-add-symbolic",
                                            set_tooltip_text: Some(i18n("Block this sender").as_str()),
                                            set_valign: gtk::Align::Center,
                                            add_css_class: "flat",
                                            connect_clicked[sender, add_blacklist_row] => move |_| {
                                                sender.input_sender().emit(AccountsInput::AddBlacklistText(
                                                    add_blacklist_row.text().to_string(),
                                                ));
                                                add_blacklist_row.set_text("");
                                            },
                                        },
                                    },

                                    #[local_ref]
                                    blacklist_box -> gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        add_css_class: "sender-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },
                        },
                    },
                },

                // ---- editor page ---- (taken out of the view until an
                // editor opens: see `mount_editor`)
                #[name = "editor_page"]
                add = &adw::NavigationPage {
                    set_title: &i18n("Account"),
                    set_tag: Some("editor"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        add_top_bar = &adw::HeaderBar {
                            // The window's close button lives here while the
                            // editor is up (the shared header hides), so it
                            // is never out of reach.
                            set_show_end_title_buttons: true,
                            pack_end = &gtk::Button {
                                set_label: &i18n("Save"),
                                add_css_class: "suggested-action",
                                connect_clicked => AccountsInput::Save,
                            },
                            // Left of Save, only while editing an existing
                            // account; asks before removing.
                            #[name = "remove_btn"]
                            pack_end = &gtk::Button {
                                set_label: &i18n("Remove"),
                                add_css_class: "destructive-action",
                                set_visible: false,
                                connect_clicked => AccountsInput::RemoveCurrent,
                            },
                        },

                        #[wrap(Some)]
                        set_content = &adw::PreferencesPage {
                            // GNOME Online Accounts owns this account's servers and
                            // credentials; Hylki only mirrors them. Saying so where
                            // the greyed-out fields are is worth more than leaving
                            // the user to work out why they can't type.
                            // GNOME Online Accounts owns this account's servers and
                            // credentials; Hylki only mirrors them, and can hide it
                            // locally. Both facts belong together, above the fields
                            // they explain.
                            // The provider's mark over the form, following
                            // the Provider picker (a protocol tile for manual
                            // IMAP and custom OAuth). Filled in apply_provider.
                            add = &adw::PreferencesGroup {
                                #[name = "provider_mark"]
                                gtk::Box {
                                    set_halign: gtk::Align::Center,
                                    set_height_request: 56,
                                    set_margin_bottom: 6,
                                },
                            },

                            #[name = "goa_banner"]
                            add = &adw::PreferencesGroup {
                                set_visible: false,
                                set_title: &i18n("GNOME Online Account"),

                                #[name = "goa_enabled_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Show in Hylki"),
                                    set_subtitle: &i18n("Off returns it to the import list. It stays in GNOME Online Accounts."),
                                    connect_active_notify[sender] => move |row| {
                                        sender.input_sender().emit(AccountsInput::ToggleCurrentEnabled(row.is_active()));
                                    },
                                },

                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 12,
                                    set_halign: gtk::Align::Start,
                                    set_margin_top: 12,

                                    gtk::Label {
                                        set_label: &i18n("Managed by GNOME Online Accounts. Its address, servers and password are changed in Settings \u{2192} Online Accounts."),
                                        set_xalign: 0.0,
                                        set_halign: gtk::Align::Start,
                                        set_wrap: true,
                                        add_css_class: "dim-label",
                                    },

                                    gtk::Button {
                                        set_label: &i18n("Open Online Accounts\u{2026}"),
                                        set_halign: gtk::Align::Start,
                                        connect_clicked => AccountsInput::OpenOnlineAccounts,
                                    },
                                },
                            },

                            // The mailbox's storage (#298): shown once the
                            // server has said how much is in use, never for
                            // one that keeps no limit or cannot say.
                            #[name = "quota_group"]
                            add = &adw::PreferencesGroup {
                                set_visible: false,

                                #[name = "quota_row"]
                                adw::ActionRow {
                                    set_title: &i18n("Storage"),
                                    add_css_class: "property",

                                    #[name = "quota_bar"]
                                    add_suffix = &gtk::LevelBar {
                                        set_valign: gtk::Align::Center,
                                        set_width_request: 160,
                                        set_min_value: 0.0,
                                        set_max_value: 1.0,
                                    },
                                },
                            },

                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Mail Account"),

                                // The nickname first: it is the one thing
                                // most edits are for, and it names the
                                // account wherever it is listed, the address
                                // standing in while there is none. Where it
                                // shows up is said in the title, the way the
                                // other rows carry their hints.
                                #[name = "label_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Nickname"),
                                },

                                // Then the provider; the rest of the form
                                // adapts (server fields vs. OAuth sign-in).
                                #[name = "provider_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Provider"),
                                    connect_selected_notify => AccountsInput::ProviderChanged,
                                },
                                #[name = "name_row"]
                                adw::EntryRow { set_title: &i18n("Display Name") },
                                #[name = "email_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Email Address"),
                                    set_input_purpose: gtk::InputPurpose::Email,
                                },
                                #[name = "protocol_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Incoming Protocol"),
                                    connect_selected_notify => AccountsInput::ProtocolChanged,
                                },
                                #[name = "host_row"]
                                adw::EntryRow { set_title: &i18n("Incoming Server") },
                                #[name = "port_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Port (IMAP 993 / POP3 995)"),
                                    set_input_purpose: gtk::InputPurpose::Digits,
                                },
                                #[name = "smtp_row"]
                                adw::EntryRow { set_title: &i18n("SMTP Server (optional)") },
                                #[name = "smtp_port_row"]
                                adw::EntryRow {
                                    set_title: &i18n("SMTP Port (default 587)"),
                                    set_input_purpose: gtk::InputPurpose::Digits,
                                },
                                #[name = "user_row"]
                                adw::EntryRow { set_title: &i18n("Username") },
                                #[name = "pass_row"]
                                adw::PasswordEntryRow { set_title: &i18n("Password") },
                                // JMAP with a bearer token in place of the
                                // username and password (#356).
                                #[name = "jmap_token_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Sign in with an API token"),
                                    set_subtitle: &i18n("For servers, like Fastmail, that take a token instead of a password."),
                                    set_visible: false,
                                    connect_active_notify => AccountsInput::ProtocolChanged,
                                },

                                // ---- OAuth fields (shown when Authentication is an OAuth option) ----
                                #[name = "oauth_client_id_row"]
                                adw::EntryRow {
                                    set_title: &i18n("OAuth Client ID"),
                                    set_visible: false,
                                },
                                #[name = "oauth_secret_row"]
                                adw::PasswordEntryRow {
                                    set_title: &i18n("OAuth Client Secret (optional)"),
                                    set_visible: false,
                                },
                                #[name = "oauth_auth_url_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Authorization URL"),
                                    set_visible: false,
                                },
                                #[name = "oauth_token_url_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Token URL"),
                                    set_visible: false,
                                },
                                #[name = "oauth_scope_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Scopes (space-separated)"),
                                    set_visible: false,
                                },
                                // Microsoft's own sign-in, made with another app
                                // registration (#329): one an organization has
                                // approved, say. Empty rows keep Hylki's.
                                #[name = "ms_advanced"]
                                adw::ExpanderRow {
                                    set_title: &i18n("Advanced"),
                                    set_subtitle: &i18n("Sign in with another app registration"),
                                    set_visible: false,
                                    add_row: ms_client_row = &adw::EntryRow {
                                        set_title: &i18n("Client ID"),
                                    },
                                    add_row: ms_tenant_row = &adw::EntryRow {
                                        set_title: &i18n("Tenant (common, organizations, consumers or an ID)"),
                                    },
                                    add_row: ms_scopes_row = &adw::EntryRow {
                                        set_title: &i18n("Scopes (space-separated)"),
                                    },
                                    add_row: ms_redirect_row = &adw::EntryRow {
                                        set_title: &i18n("Redirect URI"),
                                        // A redirect of its own signs in in
                                        // Hylki's window, not the browser.
                                        connect_changed[signin = oauth_signin_btn.clone()] => move |row| {
                                            signin.set_label(&if row.text().trim().is_empty() {
                                                i18n("Sign In with Browser")
                                            } else {
                                                i18n("Sign In")
                                            });
                                        },
                                    },
                                },
                                #[name = "oauth_signin_btn"]
                                gtk::Button {
                                    set_label: &i18n("Sign In with Browser"),
                                    set_halign: gtk::Align::Start,
                                    set_margin_top: 16,
                                    set_visible: false,
                                    add_css_class: "suggested-action",
                                    connect_clicked => AccountsInput::OAuthSignIn,
                                },
                                #[name = "oauth_status"]
                                gtk::Label {
                                    set_visible: false,
                                    set_halign: gtk::Align::Start,
                                    set_xalign: 0.0,
                                    set_wrap: true,
                                },

                                // Shown for Google/Microsoft when no built-in/own OAuth
                                // client is available: point the user at GNOME Online
                                // Accounts (the only sign-in path for these providers).
                                #[name = "goa_hint"]
                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 12,
                                    set_margin_top: 8,
                                    set_visible: false,

                                    gtk::Label {
                                        set_wrap: true,
                                        set_xalign: 0.0,
                                        add_css_class: "dim-label",
                                        set_label: &i18n("Google and Microsoft sign in through GNOME Online Accounts. Sign in there, then reopen this window and switch the account on under GNOME Online Accounts."),
                                    },
                                    gtk::Button {
                                        set_label: &i18n("Open Online Accounts…"),
                                        set_halign: gtk::Align::Start,
                                        add_css_class: "suggested-action",
                                        connect_clicked => AccountsInput::OpenOnlineAccounts,
                                    },
                                },

                                #[name = "smtp_separate_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Separate SMTP credentials"),
                                    set_subtitle: &i18n("Off uses the credentials above."),
                                },
                                #[name = "smtp_user_row"]
                                adw::EntryRow { set_title: &i18n("SMTP Username") },
                                #[name = "smtp_pass_row"]
                                adw::PasswordEntryRow { set_title: &i18n("SMTP Password") },
                                #[name = "tls_mismatch_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Accept a certificate for another name"),
                                    set_subtitle: &i18n("For shared hosting. Turn on only when the connection test says the names differ."),
                                },

                                // Off keeps a mailbox read on its own out of
                                // the merged lists (#267); its own section,
                                // the tray and notifications are unchanged.
                                #[name = "in_unified_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Show in All Inboxes"),
                                    set_subtitle: &i18n("Also in the unified Starred, Sent, Drafts, Archive, Filters and Tags."),
                                    set_active: true,
                                },

                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 6,
                                    set_margin_top: 16,

                                    #[name = "test_btn"]
                                    gtk::Button {
                                        set_label: &i18n("Test Connection"),
                                        set_halign: gtk::Align::Start,
                                        connect_clicked => AccountsInput::TestConnection,
                                    },
                                    #[name = "test_result"]
                                    gtk::Label {
                                        set_visible: false,
                                        set_halign: gtk::Align::Start,
                                        set_xalign: 0.0,
                                        set_wrap: true,
                                        // The text can be copied out (#246):
                                        // an error is often something to
                                        // paste into a bug report.
                                        set_selectable: true,
                                    },
                                },
                            },

                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Appearance"),

                                // The sidebar circle as it will look, as the
                                // group's first row (a non-row child would
                                // land below the rows): the accent color,
                                // and the picture, emoji or initials it
                                // shows. Not a button — it neither activates
                                // nor selects.
                                adw::PreferencesRow {
                                    set_activatable: false,
                                    set_selectable: false,
                                    set_can_focus: false,
                                    #[wrap(Some)]
                                    set_child = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 2,
                                        set_halign: gtk::Align::Center,
                                        set_margin_top: 10,
                                        set_margin_bottom: 10,
                                        set_margin_start: 10,
                                        set_margin_end: 10,

                                        #[name = "preview_disc"]
                                        gtk::Box {
                                            add_css_class: "account-circle",
                                            add_css_class: "account-preview-disc",
                                            set_size_request: (72, 72),
                                            set_halign: gtk::Align::Center,
                                            set_hexpand: false,
                                            set_overflow: gtk::Overflow::Hidden,
                                        },
                                    },
                                },

                                // The accent stands on its own: it colors
                                // the circle, and marks the account's
                                // folders and mail elsewhere.
                                adw::ActionRow {
                                    set_title: &i18n("Account accent color"),
                                    #[name = "color_btn"]
                                    add_suffix = &gtk::ColorDialogButton {
                                        set_valign: gtk::Align::Center,
                                        set_dialog: &gtk::ColorDialog::new(),
                                        connect_rgba_notify[sender] => move |_| {
                                            sender.input_sender().emit(AccountsInput::RefreshPreview);
                                        },
                                    },
                                },

                                // This mailbox's own Gravatar (#189), ahead
                                // of everything below — when the address has
                                // one. Off by default: the lookup tells a
                                // third party the address is in use here.
                                #[name = "gravatar_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Use my Gravatar"),
                                    set_subtitle: &i18n("Sends gravatar.com a hash of the address."),
                                    connect_active_notify[sender] => move |row| {
                                        sender.input_sender().emit(AccountsInput::SetOwnGravatar(row.is_active()));
                                    },
                                },

                                // What the circle shows (#162): initials or
                                // an emoji, or a picture. The row below
                                // follows the choice.
                                adw::ActionRow {
                                    set_title: &i18n("Circle shows"),
                                    add_suffix = &gtk::Box {
                                        add_css_class: "linked",
                                        set_valign: gtk::Align::Center,
                                        #[name = "mode_glyph_btn"]
                                        gtk::ToggleButton {
                                            set_label: &i18n("Initials or emoji"),
                                            set_active: true,
                                        },
                                        #[name = "mode_picture_btn"]
                                        gtk::ToggleButton {
                                            set_label: &i18n("Picture"),
                                            set_group: Some(&mode_glyph_btn),
                                            connect_toggled[sender] => move |b| {
                                                sender.input_sender().emit(AccountsInput::SetPictureMode(b.is_active()));
                                            },
                                        },
                                    },
                                },

                                #[name = "emoji_row"]
                                adw::ActionRow {
                                    set_title: &i18n("Emoji"),
                                    set_subtitle: &i18n("Instead of initials."),

                                    #[name = "emoji_btn"]
                                    add_suffix = &gtk::MenuButton {
                                        set_valign: gtk::Align::Center,
                                        set_label: &i18n("Choose…"),
                                        #[wrap(Some)]
                                        set_popover = &gtk::EmojiChooser {
                                            connect_emoji_picked[sender] => move |_, text| {
                                                sender.input_sender().emit(AccountsInput::SetEmoji(text.to_string()));
                                            },
                                        },
                                    },
                                    #[name = "emoji_clear_btn"]
                                    add_suffix = &gtk::Button {
                                        set_valign: gtk::Align::Center,
                                        set_label: &i18n("Use initials"),
                                        set_visible: false,
                                        connect_clicked => AccountsInput::ClearGlyph,
                                    },
                                },

                                #[name = "picture_row"]
                                adw::ActionRow {
                                    set_title: &i18n("Picture"),
                                    set_visible: false,
                                    add_suffix = &gtk::Button {
                                        set_valign: gtk::Align::Center,
                                        set_label: &i18n("Choose…"),
                                        connect_clicked => AccountsInput::PickAvatar,
                                    },
                                    #[name = "avatar_clear_btn"]
                                    add_suffix = &gtk::Button {
                                        set_valign: gtk::Align::Center,
                                        set_label: &i18n("Remove"),
                                        set_visible: false,
                                        connect_clicked => AccountsInput::ClearGlyph,
                                    },
                                },
                            },

                            // Send-as aliases (#34): extra From identities the
                            // composer offers, and replies to an alias answer
                            // from it. Each alias sends through this account's
                            // SMTP, or — for a forwarded mailbox whose provider
                            // would rewrite the sender — through its own.
                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Send-as aliases"),
                                set_description: Some(
                                    i18n("Extra addresses this account can send as.").as_str()
                                ),

                                // At the header's end, styled like the Add
                                // Filter…, Add Tag… and cloud Add Account…
                                // buttons.
                                #[wrap(Some)]
                                set_header_suffix = &gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_valign: gtk::Align::Start,
                                    set_halign: gtk::Align::End,
                                    set_margin_start: 24,
                                    gtk::Button {
                                        set_label: &i18n("Add Alias…"),
                                        set_size_request: (130, -1),
                                        connect_clicked => AccountsInput::AliasAdd,
                                    },
                                },

                                #[name = "aliases_list"]
                                gtk::ListBox {
                                    add_css_class: "boxed-list",
                                    set_selection_mode: gtk::SelectionMode::None,
                                    connect_row_activated[sender] => move |_, row| {
                                        sender.input_sender().emit(AccountsInput::AliasEdit(row.index() as usize));
                                    },
                                },
                            },

                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Signatures"),
                                set_description: Some(
                                    i18n("With more than one, the composer's signature button allows you to pick between them.")
                                        .as_str()
                                ),
                                // A designed signature usually exists as HTML
                                // already (#120): bring it in from its file.
                                // Its source is edited in place, from the
                                // editor's own toolbar. Stacked and 130px
                                // wide, like Add Alias… and the other
                                // groups' buttons.
                                #[wrap(Some)]
                                set_header_suffix = &gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 6,
                                    set_valign: gtk::Align::Start,
                                    set_halign: gtk::Align::End,
                                    set_margin_start: 24,
                                    gtk::Button {
                                        set_label: &i18n("Add Signature"),
                                        set_size_request: (130, -1),
                                        connect_clicked => AccountsInput::SigAdd,
                                    },
                                    gtk::Button {
                                        set_label: &i18n("Import File…"),
                                        set_size_request: (130, -1),
                                        connect_clicked => AccountsInput::SignatureImport,
                                    },
                                },

                                // Which signature the editor below holds,
                                // and its name: shown once there are two.
                                #[name = "sig_choice_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Signature"),
                                    set_visible: false,
                                    connect_selected_notify => AccountsInput::SigShow,
                                },
                                #[name = "sig_name_row"]
                                adw::EntryRow {
                                    set_title: &i18n("Name"),
                                    set_visible: false,
                                    connect_changed => AccountsInput::SigRename,
                                    add_suffix = &gtk::Button {
                                        set_icon_name: "user-trash-symbolic",
                                        set_valign: gtk::Align::Center,
                                        set_tooltip_text: Some(&i18n("Delete Signature")),
                                        add_css_class: "flat",
                                        connect_clicked => AccountsInput::SigDelete,
                                    },
                                },
                                #[name = "sig_default_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Start messages with this signature"),
                                    connect_active_notify => AccountsInput::SigDefault,
                                },

                                #[name = "sig_holder"]
                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_margin_top: 6,
                                },
                            },

                            // Per-account push override (#91): some servers
                            // mishandle IDLE, and one bad account shouldn't
                            // cost the good ones their instant delivery.
                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Syncing"),

                                #[name = "push_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Instant new mail (push)"),
                                    set_subtitle: &i18n("Turn off for servers that stall on push connections."),
                                },

                                // Auto-empty (#140): mail past the chosen age in
                                // Junk / Trash is deleted for good at each sync.
                                #[name = "empty_junk_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Empty Junk automatically"),
                                },
                                #[name = "empty_trash_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Empty Trash automatically"),
                                },
                                // Mailing-list mail past the chosen age goes
                                // to the Trash (#407).
                                #[name = "clean_lists_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Move list mail to Trash"),
                                    set_subtitle: &i18n("Inbox mail with an unsubscribe link older than this gets trashed unless it's starred."),
                                },
                            },

                            // What a Microsoft 365 account can use from
                            // Microsoft Graph, in one place: the
                            // directory's suggestions (#410) and names for
                            // sensitivity labels (#411). Each asks for its
                            // own access when turned on.
                            #[name = "graph_group"]
                            add = &adw::PreferencesGroup {
                                set_visible: false,
                                set_title: &i18n("Microsoft 365"),

                                #[name = "graph_directory_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Search the organization directory"),
                                    set_subtitle: &i18n("Suggests people from your organization as you type."),
                                    connect_active_notify[sender] => move |row| {
                                        sender.input_sender().emit(AccountsInput::SetGraphDirectory(row.is_active()));
                                    },
                                },
                                #[name = "directory_only_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Suggest only from this account"),
                                    set_subtitle: &i18n("Suggestions come from the directory and this account's mail only."),
                                },
                                #[name = "graph_labels_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Show sensitivity label names"),
                                    set_subtitle: &i18n("Shows a label's name when a message gives only its ID."),
                                    connect_active_notify[sender] => move |row| {
                                        sender.input_sender().emit(AccountsInput::SetGraphLabels(row.is_active()));
                                    },
                                },
                                // How the last request for access went.
                                #[name = "graph_status"]
                                gtk::Label {
                                    set_visible: false,
                                    set_halign: gtk::Align::Start,
                                    set_xalign: 0.0,
                                    set_wrap: true,
                                    set_selectable: true,
                                    set_margin_top: 8,
                                },
                            },

                            // Its own folder order, or Settings → Sidebar's.
                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Folder Order"),
                                set_description: Some(
                                    i18n("Also in the right-click menu of its Folders heading.").as_str()
                                ),

                                #[name = "folder_sort_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Sort by"),
                                },
                            },

                            // Manual special-folder mapping (#82): for servers
                            // whose Sent/Trash/… aren't detected, pin each role
                            // to one of the account's real folders. "Save
                            // copies in" (#199) rides with them because it is
                            // read as a refinement of Sent, but it is a
                            // different kind of thing: a role says what a
                            // folder *is*, and relabelling the Inbox costs the
                            // account its inbox (#136), which is why the Inbox
                            // is offered in that row and in none of the others.
                            add = &adw::PreferencesGroup {
                                set_title: &i18n("Special Folders"),
                                set_description: Some(
                                    i18n("Follows the server's own markings unless you pick a folder. Nothing already on the server moves.").as_str()
                                ),

                                #[name = "folder_sent_row"]
                                adw::ComboRow { set_title: &i18n("Sent") },
                                #[name = "folder_sent_copy_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Save a copy of sent mail in"),
                                    set_subtitle: &i18n(SENT_COPY_HINT),
                                },
                                // Replies beside what they answer (#468), as
                                // Evolution offers. Microsoft 365 files its
                                // own copy and POP3 has no folders, so the
                                // row is for IMAP and JMAP only.
                                #[name = "reply_copies_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Save a copy of replies in"),
                                    set_subtitle: &i18n(REPLY_COPIES_HINT),
                                },
                                // Gmail files a copy of anything sent through
                                // its SMTP, so Hylki's append makes a second
                                // one. Off by default: a server that does not
                                // do it, paired with a Hylki that has stopped
                                // appending, keeps no sent mail at all.
                                #[name = "server_saves_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Server saves its own copy of sent mail"),
                                    set_subtitle: &i18n("For Gmail and others that file sent mail themselves."),
                                    connect_active_notify[sender] => move |row| {
                                        sender.input_sender().emit(AccountsInput::SetServerSavesSent(row.is_active()));
                                    },
                                },
                                #[name = "folder_drafts_row"]
                                adw::ComboRow { set_title: &i18n("Drafts") },
                                // Nothing on a server marks a Templates folder
                                // (#360): one called Templates is found, any
                                // other is pointed at here.
                                #[name = "folder_templates_row"]
                                adw::ComboRow { set_title: &i18n("Templates") },
                                #[name = "folder_trash_row"]
                                adw::ComboRow { set_title: &i18n("Trash") },
                                #[name = "folder_junk_row"]
                                adw::ComboRow { set_title: &i18n("Junk") },
                                #[name = "folder_archive_row"]
                                adw::ComboRow { set_title: &i18n("Archive") },
                            },

                            // Hidden folders (#239): what "Hide Folder" in the
                            // sidebar took out of sight, each with the way back.
                            #[name = "hidden_group"]
                            adw::PreferencesGroup {
                                set_title: &i18n("Hidden Folders"),
                                set_description: Some(
                                    i18n("Hidden folders are not synced. Hide one from its right-click menu in the sidebar.").as_str()
                                ),
                                #[name = "hidden_list"]
                                gtk::ListBox {
                                    add_css_class: "boxed-list",
                                    set_selection_mode: gtk::SelectionMode::None,
                                },
                                #[name = "hidden_empty"]
                                gtk::Label {
                                    set_label: &i18n("No hidden folders."),
                                    add_css_class: "dim-label",
                                    set_xalign: 0.0,
                                },
                            },

                            // OpenPGP (#133): which of the user's keys this
                            // account signs and decrypts with.
                            add = &adw::PreferencesGroup {
                                set_title: &i18n("OpenPGP"),
                                set_description: Some(
                                    i18n("Keys are managed under OpenPGP in the sidebar.").as_str()
                                ),
                                #[name = "pgp_key_row"]
                                adw::ComboRow {
                                    set_title: &i18n("Key"),
                                    set_subtitle: &i18n("Automatic uses the key whose address matches."),
                                },
                                // For an account whose mail is always signed
                                // (#267): the composer's toggle starts on,
                                // and still turns it off for one message.
                                #[name = "sign_default_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Sign messages by default"),
                                },
                                // Protected headers (#493): on, an encrypted
                                // message says "..." where its subject would be.
                                #[name = "hide_subject_row"]
                                adw::SwitchRow {
                                    set_title: &i18n("Hide the subject of encrypted mail"),
                                    set_subtitle: &i18n("Only the recipients see the subject. Others see \"...\"."),
                                },
                            },

                            // For a GOA-imported account this removes it from
                            // Hylki only — it stays in GNOME Online Accounts and
                            // returns to the import list.
                            add = &adw::PreferencesGroup {
                                // Not for a GNOME Online Account: GNOME holds its
                                // sign-in, and Google and Microsoft have no password.
                                #[name = "keyring_note"]
                                gtk::Label {
                                    set_wrap: true,
                                    set_xalign: 0.0,
                                    add_css_class: "dim-label",
                                    add_css_class: "caption",
                                    set_label: &i18n("Your password is kept in the system keyring, never in plain text."),
                                },
                            },
                        },
                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let t_init = std::time::Instant::now();
        let goa = importable_goa_accounts(&init.accounts);
        let sig_source_btn = gtk::Button::with_label(&i18n("Edit HTML"));
        sig_source_btn.add_css_class("flat");
        sig_source_btn.set_valign(gtk::Align::Center);
        sig_source_btn.set_tooltip_text(Some(&i18n("Edit the signature's HTML source")));
        {
            let s = sender.input_sender().clone();
            sig_source_btn.connect_clicked(move |_| {
                let _ = s.send(AccountsInput::SignatureSourceToggle);
            });
        }

        let senders = relm4::factory::FactoryVecDeque::builder()
            .launch(gtk::ListBox::new())
            .forward(sender.input_sender(), |out| match out {
                SenderRowOutput::Remove(addr) => AccountsInput::RemoveSenderRow(addr),
            });
        let blacklist = relm4::factory::FactoryVecDeque::builder()
            .launch(gtk::ListBox::new())
            .forward(sender.input_sender(), |out| match out {
                SenderRowOutput::Remove(addr) => AccountsInput::RemoveBlacklistRow(addr),
            });

        let mut model = AccountsWindow {
            accounts: init.accounts,
            editing: None,
            emoji: None,
            avatar: None,
            preview_css: gtk::CssProvider::new(),
            list_css: gtk::CssProvider::new(),
            picture_mode: false,
            editor_seed: None,
            sig_editor: None,
            sig_source_btn,
            sig_edits: Vec::new(),
            sig_shown: 0,
            sig_default: None,
            sig_touched: false,
            sig_reading: false,
            label_synced: String::new(),
            goa,
            pending_oauth_refresh: None,
            editor_session: 0,
            editor_secrets: None,
            secrets_reading: false,
            save_waiting: false,
            directory_granted: false,
            directory_on: false,
            labels_granted: false,
            labels_on: false,
            alias_edits: Vec::new(),
            server_identities: Vec::new(),
            hidden_edits: Vec::new(),
            alias_editing: None,
            alias_dialog: None,
            folders_by_email: std::collections::HashMap::new(),
            folder_paths: Vec::new(),
            sent_copy_paths: Vec::new(),
            senders,
            sender_addrs: Vec::new(),
            blacklist,
            blacklist_addrs: Vec::new(),
            filter_rules: init.filters,
            filters_list: None,
            filters_query: Default::default(),
            form_save: Default::default(),
            senders_query: Default::default(),
            tags: init.tags,
            tags_list: None,
            smart_folders: init.smart_folders,
            smart_list: None,
            find_tags_spinner: None,
            apply_filters_spinner: None,
            apply_filters_label: None,
            filters_applying: false,
            find_tags_label: None,
            tag_scanning: false,
        };
        {
            let mut guard = model.senders.guard();
            for addr in &init.allowed_senders {
                model.sender_addrs.push(addr.clone());
                guard.push_back(addr.clone());
            }
        }
        {
            let mut guard = model.blacklist.guard();
            for addr in &init.blacklist {
                model.blacklist_addrs.push(addr.clone());
                guard.push_back(addr.clone());
            }
        }

        let senders_box = model.senders.widget();
        let blacklist_box = model.blacklist.widget();
        // An empty boxed list still draws its frame: a stray line under the
        // entry that adds to it (#129). Shown only once it has a row.
        senders_box.set_visible(!model.sender_addrs.is_empty());
        blacklist_box.set_visible(!model.blacklist_addrs.is_empty());
        let widgets = view_output!();
        // Narrowing to the directory means nothing without it.
        widgets
            .graph_directory_row
            .bind_property("active", &widgets.directory_only_row, "sensitive")
            .sync_create()
            .build();
        // Long enough for the hint, short enough that the combo's own value
        // is not what gets shortened instead.
        wrap_subtitle(&widgets.folder_sent_copy_row, 34);
        wrap_subtitle(&widgets.reply_copies_row, 30);
        // Left-justify the editor's wrapping labels (its group descriptions):
        // libadwaita 1.9 renders a group description fill-justified when its
        // text does not naturally fill the label's width, stretching the word
        // gaps (most visible on Special Folders, whose description spans the
        // full width). Single-line row labels are untouched.
        left_justify_wrapping_labels(&widgets.editor_page.clone().upcast());
        // The editor's sixty-odd rows stay out of the widget tree until an
        // editor opens, so the Settings window's first layout skips them.
        widgets.nav.remove(&widgets.editor_page);
        {
            // Leaving the editor page (however it was left) lets its
            // signature view go; see ReleaseSignatureEditor.
            let s = sender.clone();
            widgets.nav.connect_popped(move |_, page| {
                if page.tag().is_some_and(|t| t == "editor") {
                    s.input_sender().emit(AccountsInput::ReleaseSignatureEditor);
                }
            });
        }
        tracing::debug!(
            "settings window: accounts view built in {:?}",
            t_init.elapsed()
        );
        model.filters_list = Some(widgets.filters_list.clone());
        {
            let q = model.filters_query.clone();
            widgets
                .filters_list
                .set_filter_func(move |row| row_matches(row, &q.borrow()));
            let q = model.senders_query.clone();
            senders_box.set_filter_func(move |row| row_matches(row, &q.borrow()));
            let q = model.senders_query.clone();
            blacklist_box.set_filter_func(move |row| row_matches(row, &q.borrow()));
        }
        model.rebuild_filter_rows(&sender);
        model.tags_list = Some(widgets.tags_list.clone());
        model.find_tags_spinner = Some(widgets.find_tags_spinner.clone());
        model.apply_filters_spinner = Some(widgets.apply_filters_spinner.clone());
        model.apply_filters_label = Some(widgets.apply_filters_label.clone());
        model.find_tags_label = Some(widgets.find_tags_label.clone());
        model.rebuild_tag_rows(&sender);
        model.smart_list = Some(widgets.smart_list.clone());
        model.rebuild_smart_rows(&sender);
        let t_list = std::time::Instant::now();
        model.rebuild_account_list(&widgets.accounts_list, &sender);
        tracing::debug!(
            "settings window: accounts list built in {:?}",
            t_list.elapsed()
        );
        model.rebuild_goa_list(&widgets.goa_list, &sender);
        widgets.goa_group.set_visible(!model.goa.is_empty());
        widgets
            .protocol_row
            .set_model(Some(&gtk::StringList::new(&["IMAP", "POP3"])));

        // The Provider dropdown picks both the sign-in method and (for known
        // providers) the servers. The default popup ellipsizes items; a factory
        // whose labels don't lets the list widen to the full option text.
        let provider_labels: Vec<&str> = PROVIDERS.iter().map(|p| p.label).collect();
        widgets
            .provider_row
            .set_model(Some(&gtk::StringList::new(&provider_labels)));
        widgets.provider_row.set_factory(Some(&provider_factory()));

        // Push override choices mirror AccountConfig::push (None / Some(true)
        // / Some(false), in that order).
        widgets.push_row.set_model(Some(&gtk::StringList::new(&[
            i18n("Follow Settings").as_str(),
            i18n("On").as_str(),
            i18n("Off").as_str(),
        ])));
        widgets
            .push_row
            .set_list_factory(Some(&non_ellipsizing_factory()));
        let sort_labels: Vec<String> = std::iter::once(i18n("Follow Settings"))
            .chain(crate::config::FolderSort::ALL.iter().map(|s| s.label()))
            .collect();
        let sort_labels: Vec<&str> = sort_labels.iter().map(String::as_str).collect();
        widgets
            .folder_sort_row
            .set_model(Some(&gtk::StringList::new(&sort_labels)));
        widgets
            .folder_sort_row
            .set_list_factory(Some(&non_ellipsizing_factory()));
        // The chosen value whole: "Follow Settings" cut to "Follow …" says nothing.
        widgets
            .folder_sort_row
            .set_factory(Some(&non_ellipsizing_factory()));
        let empty_labels = auto_empty_labels();
        let empty_labels: Vec<&str> = empty_labels.iter().map(String::as_str).collect();
        for row in [&widgets.empty_junk_row, &widgets.empty_trash_row] {
            row.set_model(Some(&gtk::StringList::new(&empty_labels)));
            row.set_list_factory(Some(&non_ellipsizing_factory()));
        }
        let list_labels = age_labels(CLEAN_LISTS_DAYS);
        let list_labels: Vec<&str> = list_labels.iter().map(String::as_str).collect();
        widgets
            .clean_lists_row
            .set_model(Some(&gtk::StringList::new(&list_labels)));
        widgets
            .clean_lists_row
            .set_list_factory(Some(&non_ellipsizing_factory()));
        let reply_labels: Vec<String> = crate::config::ReplyCopies::ALL
            .iter()
            .map(|c| c.label())
            .collect();
        let reply_labels: Vec<&str> = reply_labels.iter().map(String::as_str).collect();
        widgets
            .reply_copies_row
            .set_model(Some(&gtk::StringList::new(&reply_labels)));
        widgets
            .reply_copies_row
            .set_list_factory(Some(&non_ellipsizing_factory()));
        widgets
            .reply_copies_row
            .set_factory(Some(&non_ellipsizing_factory()));

        // Show the SMTP credential fields only when the toggle is on.
        widgets
            .smtp_separate_row
            .bind_property("active", &widgets.smtp_user_row, "visible")
            .sync_create()
            .build();
        widgets
            .smtp_separate_row
            .bind_property("active", &widgets.smtp_pass_row, "visible")
            .sync_create()
            .build();

        // Auto-fill the label from the email as it's typed (until customized).
        let es = sender.clone();
        widgets
            .email_row
            .connect_changed(move |_| es.input_sender().emit(AccountsInput::EmailChanged));

        // The preview circle follows the name, label and email (its
        // initials) as they are typed; its color is a stylesheet.
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &model.preview_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            gtk::style_context_add_provider_for_display(
                &display,
                &model.list_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        for row in [&widgets.name_row, &widgets.label_row, &widgets.email_row] {
            let s = sender.clone();
            row.connect_changed(move |_| s.input_sender().emit(AccountsInput::RefreshPreview));
        }

        // Tell the combined settings window when the editor subpage is up —
        // every way in or out (Save, back button, swipe) lands here.
        {
            let s = sender.output_sender().clone();
            let form_save = model.form_save.clone();
            widgets.nav.connect_visible_page_notify(move |nav| {
                let tag = nav.visible_page().and_then(|p| p.tag());
                let editor = match tag.as_deref() {
                    Some("editor") => Some("accounts"),
                    Some("filter") => Some("filters"),
                    Some("tag") => Some("tags"),
                    Some("smart") => Some("smart"),
                    _ => None,
                };
                if !matches!(editor, Some("filters") | Some("tags") | Some("smart")) {
                    form_save.borrow_mut().take();
                }
                let _ = s.send(AccountsOutput::EditorOpen(editor));
            });
        }

        tracing::debug!(
            "settings window: accounts panel init {:?}",
            t_init.elapsed()
        );
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            AccountsInput::ShowPage(id) => {
                widgets.list_stack.set_visible_child_name(&id);
            }
            AccountsInput::RefreshGoa => {
                // Off the main thread: GOA answers over D-Bus, and a busy
                // goa-daemon is slow to.
                widgets.goa_refresh_btn.set_sensitive(false);
                sender.oneshot_command(async move {
                    let listed = tokio::task::spawn_blocking(crate::goa::list_mail_accounts)
                        .await
                        .unwrap_or_default();
                    AccountsCmd::GoaListed(listed)
                });
            }
            AccountsInput::CloseEditor => {
                if widgets
                    .nav
                    .visible_page()
                    .and_then(|p| p.tag())
                    .is_some_and(|t| t == "editor" || t == "filter" || t == "tag")
                {
                    widgets.nav.pop();
                }
            }
            AccountsInput::ReleaseSignatureEditor => {
                let editing = widgets
                    .nav
                    .visible_page()
                    .and_then(|p| p.tag())
                    .is_some_and(|t| t == "editor");
                if !editing {
                    if let Some(editor) = self.sig_editor.take() {
                        // Kept for the next editor, which takes it in.
                        editor.toolbar_end().remove(&self.sig_source_btn);
                        widgets.sig_holder.remove(&editor.widget);
                    }
                }
            }
            AccountsInput::DebugEditLabel(text) => widgets.label_row.set_text(&text),
            AccountsInput::DebugProvider(label) => {
                if let Some(i) = PROVIDERS.iter().position(|p| p.label == label) {
                    widgets.provider_row.set_selected(i as u32);
                }
            }

            AccountsInput::LeaveRequest(page) => {
                match widgets.nav.visible_page().and_then(|p| p.tag()).as_deref() {
                    // The account editor knows what it was opened with, so it
                    // can tell an edited form from an untouched one. The
                    // signature lives in a WebView and only answers when
                    // asked, so that answer arrives separately.
                    Some("editor") => {
                        if self.editor_touched(widgets) {
                            sender.input_sender().emit(AccountsInput::LeaveVerdict {
                                page,
                                touched: true,
                            });
                        } else {
                            let s = sender.clone();
                            self.sig_editor(widgets).is_dirty(move |touched| {
                                s.input_sender()
                                    .emit(AccountsInput::LeaveVerdict { page, touched });
                            });
                        }
                    }
                    // Nothing records what a filter or a tag was opened with,
                    // so those are always worth asking about.
                    Some("filter") | Some("tag") => {
                        sender.input_sender().emit(AccountsInput::LeaveVerdict {
                            page,
                            touched: true,
                        })
                    }
                    _ => sender.input_sender().emit(AccountsInput::LeaveVerdict {
                        page,
                        touched: false,
                    }),
                }
            }

            AccountsInput::LeaveVerdict { page, touched } => {
                if touched {
                    let _ = sender.output(AccountsOutput::LeaveNeedsPrompt(page));
                    return;
                }
                // Nothing to lose: close the editor and let the window move on.
                sender.input_sender().emit(AccountsInput::CloseEditor);
                let _ = sender.output(AccountsOutput::LeftEditor(page));
            }

            AccountsInput::SaveOpenPage => {
                match widgets.nav.visible_page().and_then(|p| p.tag()).as_deref() {
                    Some("editor") => sender.input_sender().emit(AccountsInput::Save),
                    Some("filter") | Some("tag") => {
                        let saved = self.form_save.borrow().as_ref().is_some_and(|f| f());
                        if saved {
                            widgets.nav.pop();
                        }
                    }
                    _ => {}
                }
            }
            AccountsInput::SearchFilters(text) => {
                *self.filters_query.borrow_mut() = text.to_lowercase();
                widgets.filters_list.invalidate_filter();
            }
            AccountsInput::SearchSenders(text) => {
                *self.senders_query.borrow_mut() = text.to_lowercase();
                self.senders.widget().invalidate_filter();
                self.blacklist.widget().invalidate_filter();
            }
            AccountsInput::AddAccount => {
                self.editing = None;
                self.new_editor_session();
                self.emoji = None;
                self.avatar = None;
                widgets.gravatar_row.set_active(false);
                self.picture_mode = false;
                widgets.mode_glyph_btn.set_active(true);
                self.label_synced = String::new();
                self.pending_oauth_refresh = None;
                self.directory_granted = false;
                self.directory_on = false;
                self.labels_granted = false;
                self.labels_on = false;
                self.close_alias_dialog();
                self.alias_edits.clear();
                self.server_identities.clear();
                self.rebuild_alias_list(&widgets.aliases_list, &sender);
                clear_editor(widgets);
                self.show_push_row(widgets);
                self.populate_folder_combos(widgets, None);
                set_connection_editable(widgets, true);
                widgets.goa_banner.set_visible(false);
                widgets.keyring_note.set_visible(true);
                self.apply_provider(widgets);
                self.load_signatures(widgets, Vec::new(), None);
                widgets.color_btn.set_rgba(&parse_color(DEFAULT_COLOR));
                self.refresh_preview(widgets);
                widgets.remove_btn.set_visible(false);
                // A prior GOA edit may have hidden the provider picker.
                widgets.provider_row.set_visible(true);
                self.editor_seed = Some(self.editor_fingerprint(widgets));
                mount_editor(widgets);
                widgets.nav.push_by_tag("editor");
            }

            AccountsInput::EditAccountByEmail(email) => {
                let Some(i) = self.accounts.iter().position(|a| a.email == email) else {
                    return;
                };
                let editor_up = widgets
                    .nav
                    .visible_page()
                    .and_then(|p| p.tag())
                    .is_some_and(|tag| tag == "editor");
                if editor_up {
                    if self.editing == Some(i) {
                        return;
                    }
                    // Another account's editor is up: back to the list first,
                    // since a page can't be pushed while it is in the stack.
                    widgets.nav.pop();
                }
                sender.input_sender().emit(AccountsInput::EditAccount(i));
            }

            AccountsInput::EditAccount(i) => {
                let Some(acc) = self.accounts.get(i).cloned() else {
                    return;
                };
                self.editing = Some(i);
                self.new_editor_session();
                self.pending_oauth_refresh = None;
                self.directory_granted = acc.goa_id.is_none()
                    && acc.oauth
                    && acc
                        .oauth_settings
                        .as_ref()
                        .is_some_and(|s| crate::oauth::has_directory_scopes(&s.scopes));
                // On without the scopes would only be refused.
                self.directory_on = acc.graph_directory && self.directory_granted;
                widgets.graph_directory_row.set_active(self.directory_on);
                widgets
                    .directory_only_row
                    .set_active(acc.graph_directory_only);
                self.labels_granted = acc.goa_id.is_none()
                    && acc.oauth
                    && acc.oauth_settings.as_ref().is_some_and(|s| {
                        crate::oauth::has_scopes(&s.scopes, &crate::oauth::MICROSOFT_LABEL_SCOPES)
                    });
                self.labels_on = acc.graph_label_names && self.labels_granted;
                widgets.graph_labels_row.set_active(self.labels_on);
                self.close_alias_dialog();
                self.alias_edits = acc.aliases.clone();
                self.server_identities.clear();
                self.rebuild_alias_list(&widgets.aliases_list, &sender);
                self.hidden_edits = acc.hidden_folders.clone();
                self.rebuild_hidden_list(widgets, &sender);
                fill_editor(widgets, &acc);
                self.show_push_row(widgets);
                // Hidden until this account's server answers; a page opened
                // before it must not show the last account's figure.
                show_quota(widgets, None);
                let _ = sender.output(AccountsOutput::WantQuota(acc.email.clone()));
                let _ = sender.output(AccountsOutput::WantIdentities(acc.email.clone()));
                // The secrets come from the keyring now, off the main thread,
                // and land in the fields when they arrive (Save waits for
                // them should it come first).
                if AccountSecrets::needed(&acc) {
                    self.read_secrets(&acc, &sender);
                }
                self.populate_folder_combos(widgets, Some(&acc));
                self.apply_provider(widgets);
                // Label mirrors the email until customized.
                self.label_synced = acc.email.clone();
                let (signatures, default) = acc.signature_set();
                self.load_signatures(widgets, signatures, default);
                widgets
                    .color_btn
                    .set_rgba(&parse_color(acc.color.as_deref().unwrap_or(DEFAULT_COLOR)));
                self.emoji = acc.emoji.clone();
                self.avatar = acc.avatar.clone();
                widgets.gravatar_row.set_active(acc.gravatar);
                self.picture_mode = acc.avatar.is_some();
                if self.picture_mode {
                    widgets.mode_picture_btn.set_active(true);
                } else {
                    widgets.mode_glyph_btn.set_active(true);
                }
                self.refresh_preview(widgets);
                // GOA accounts: no "Remove" (it lives in the system) — offer an
                // enable/disable toggle and a shortcut to Online Accounts instead.
                let is_goa = acc.goa_id.is_some();
                set_connection_editable(widgets, !is_goa);
                widgets.goa_banner.set_visible(is_goa);
                widgets.keyring_note.set_visible(!is_goa);
                // GOA accounts get the same Remove flow — it removes the account
                // from Hylki only (back to the import list); GNOME keeps it.
                widgets.remove_btn.set_visible(true);
                // GNOME owns a GOA account's connection outright, so the server
                // and credential section isn't shown at all — only what Hylki
                // owns (name, label, color, signature, aliases) plus the email
                // for identification. `apply_provider` re-shows what applies the
                // next time a native account or the add-form opens the editor.
                widgets.provider_row.set_visible(!is_goa);
                if is_goa {
                    for w in [
                        widgets.protocol_row.upcast_ref::<gtk::Widget>(),
                        widgets.host_row.upcast_ref(),
                        widgets.port_row.upcast_ref(),
                        widgets.smtp_row.upcast_ref(),
                        widgets.smtp_port_row.upcast_ref(),
                        widgets.user_row.upcast_ref(),
                        widgets.pass_row.upcast_ref(),
                        widgets.smtp_separate_row.upcast_ref(),
                        widgets.tls_mismatch_row.upcast_ref(),
                        widgets.smtp_user_row.upcast_ref(),
                        widgets.smtp_pass_row.upcast_ref(),
                        widgets.test_btn.upcast_ref(),
                        widgets.oauth_signin_btn.upcast_ref(),
                        widgets.oauth_status.upcast_ref(),
                        widgets.goa_hint.upcast_ref(),
                    ] {
                        w.set_visible(false);
                    }
                    // The Google/Microsoft "use GNOME" guidance hid these; a GOA
                    // account edits its display fields right here.
                    widgets.name_row.set_visible(true);
                    widgets.email_row.set_visible(true);
                }
                widgets.goa_enabled_row.set_active(acc.enabled);
                // While Mail is switched off in GNOME Settings the account is
                // paused from there, not from here — say so where the toggle is.
                widgets
                    .goa_enabled_row
                    .set_sensitive(!acc.goa_mail_disabled);
                widgets.goa_enabled_row.set_subtitle(if acc.goa_mail_disabled {
                    "Paused: Mail is switched off for this account in GNOME Settings \u{2192} Online Accounts."
                } else {
                    "Switching this off returns the account to the import list — it \
                     stays in GNOME Online Accounts."
                });
                self.editor_seed = Some(self.editor_fingerprint(widgets));
                mount_editor(widgets);
                widgets.nav.push_by_tag("editor");
            }

            AccountsInput::EmailChanged => {
                let email = trimmed(&widgets.email_row);
                let label = widgets.label_row.text().to_string();
                // Mirror while the label is still tracking the email (or empty);
                // once the user types a custom label, stop.
                if label.is_empty() || label == self.label_synced {
                    widgets.label_row.set_text(&email);
                }
                self.label_synced = email;
            }

            AccountsInput::MoveRow { from, to } => {
                if from < self.accounts.len() {
                    let acc = self.accounts.remove(from);
                    let to = to.min(self.accounts.len());
                    self.accounts.insert(to, acc);
                    self.rebuild_account_list(&widgets.accounts_list, &sender);
                    let emails = self.accounts.iter().map(|a| a.email.clone()).collect();
                    let _ = sender.output(AccountsOutput::Reordered(emails));
                }
            }

            AccountsInput::ToggleEnabled { index, enabled } => {
                // A GNOME Online Account switched off here isn't paused — it is
                // un-imported: it drops out of Hylki (the config entry and its
                // stored copies go) and returns to the "GNOME Online Accounts"
                // list below, ready to import again. The account itself stays
                // in GNOME untouched.
                if !enabled && self.accounts.get(index).is_some_and(|a| a.goa_id.is_some()) {
                    self.unimport_goa(index, widgets, &sender);
                    return;
                }
                if let Some(acc) = self.accounts.get_mut(index) {
                    if acc.enabled != enabled {
                        acc.enabled = enabled;
                        let email = acc.email.clone();
                        let _ = sender.output(AccountsOutput::EnabledChanged { email, enabled });
                    }
                }
            }

            AccountsInput::ToggleCurrentEnabled(enabled) => {
                if let Some(i) = self.editing {
                    // Same un-import semantics as the list toggle; the editor
                    // page closes since its account is no longer in Hylki.
                    if !enabled && self.accounts.get(i).is_some_and(|a| a.goa_id.is_some()) {
                        self.unimport_goa(i, widgets, &sender);
                        widgets.nav.pop();
                        return;
                    }
                    if let Some(acc) = self.accounts.get_mut(i) {
                        if acc.enabled != enabled {
                            acc.enabled = enabled;
                            let email = acc.email.clone();
                            let _ =
                                sender.output(AccountsOutput::EnabledChanged { email, enabled });
                            self.rebuild_account_list(&widgets.accounts_list, &sender);
                        }
                    }
                }
            }

            AccountsInput::ImportGoa(index) => {
                if let Some(g) = self.goa.get(index).cloned() {
                    // Password-based providers: pull the password now. OAuth
                    // providers: authenticate with a GOA token at connect time.
                    // Either way the worker asks GOA again when it connects, so an
                    // account still works if this read comes back empty.
                    let (password, oauth) = if g.password_based {
                        (
                            crate::goa::mail_passwords(&g.id).0.unwrap_or_default(),
                            false,
                        )
                    } else {
                        (String::new(), true)
                    };
                    let account = g.to_config(password, oauth);
                    self.goa.remove(index);
                    self.accounts.push(account.clone());
                    self.rebuild_account_list(&widgets.accounts_list, &sender);
                    self.rebuild_goa_list(&widgets.goa_list, &sender);
                    widgets.goa_group.set_visible(!self.goa.is_empty());
                    let _ = sender.output(AccountsOutput::ImportGoa(Box::new(account)));
                }
            }

            AccountsInput::SetEmoji(text) => {
                self.emoji = Some(text);
                self.refresh_preview(widgets);
            }

            AccountsInput::ClearGlyph => {
                if self.picture_mode {
                    self.avatar = None;
                } else {
                    self.emoji = None;
                }
                self.refresh_preview(widgets);
            }

            AccountsInput::SetServerSavesSent(on) => {
                widgets.folder_sent_copy_row.set_sensitive(!on);
                widgets.reply_copies_row.set_sensitive(!on);
            }

            AccountsInput::SetFolderSort { email, sort } => {
                if let Some(i) = self.accounts.iter().position(|a| a.email == email) {
                    self.accounts[i].folder_sort = sort;
                    if self.editing == Some(i) {
                        widgets
                            .folder_sort_row
                            .set_selected(sort.map_or(0, |s| s.index() + 1));
                    }
                }
            }

            AccountsInput::SetOwnGravatar(on) => {
                // Look it up as soon as it is asked for, so the preview can
                // answer rather than waiting for the account to be saved.
                let address = trimmed(&widgets.email_row);
                if on && !address.is_empty() && crate::avatar::wants_own_gravatar(&address) {
                    sender.oneshot_command(async move {
                        let outcome = tokio::task::spawn_blocking({
                            let address = address.clone();
                            move || crate::avatar::fetch_own_gravatar(&address)
                        })
                        .await
                        .unwrap_or(crate::avatar::FetchOutcome::Retry);
                        AccountsCmd::OwnGravatar {
                            email: address,
                            outcome,
                        }
                    });
                }
                self.refresh_preview(widgets);
            }

            AccountsInput::SetPictureMode(on) => {
                if self.picture_mode != on {
                    self.picture_mode = on;
                    self.refresh_preview(widgets);
                }
            }

            AccountsInput::RefreshPreview => self.refresh_preview(widgets),

            AccountsInput::PickAvatar => {
                let dialog = gtk::FileDialog::builder()
                    .title(&i18n("Choose a Picture"))
                    .build();
                let filter = gtk::FileFilter::new();
                filter.set_name(Some(&i18n("Images")));
                filter.add_pixbuf_formats();
                let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                dialog.set_default_filter(Some(&filter));
                // The panel is a Bin inside the Settings window; the
                // dialog wants that window.
                let parent = root.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                let s = sender.clone();
                dialog.open(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    match import_avatar_file(&path) {
                        Ok(name) => s.input_sender().emit(AccountsInput::SetAvatar(name)),
                        Err(e) => tracing::warn!("could not import avatar {}: {e}", path.display()),
                    }
                });
            }

            AccountsInput::SetAvatar(name) => {
                self.avatar = Some(name);
                self.refresh_preview(widgets);
            }

            AccountsInput::TestConnection => {
                let account = read_account(widgets, self.saved_emoji(), self.saved_avatar());
                widgets.test_btn.set_sensitive(false);
                widgets.test_result.set_visible(true);
                widgets.test_result.set_css_classes(&["dim-label"]);
                widgets.test_result.set_label(&i18n("Testing…"));
                sender.oneshot_command(async move {
                    let r = tokio::task::spawn_blocking(move || {
                        worker::test_connection_blocking(account)
                    })
                    .await
                    .unwrap_or_else(|_| ConnTest {
                        incoming: Err("test could not run".into()),
                        smtp: Err("test could not run".into()),
                    });
                    AccountsCmd::Test(r)
                });
            }

            AccountsInput::ProtocolChanged => {
                apply_protocol(widgets);
                self.show_push_row(widgets);
                self.refresh_provider_mark(widgets);
            }
            AccountsInput::ProviderChanged => {
                // Editing a GOA account: the provider dropdown is hidden and the
                // connection section deliberately not shown — but filling the
                // editor sets the dropdown, whose notify lands here right after
                // EditAccount and would undo that. GNOME owns those fields;
                // leave them hidden.
                let editing_goa = self
                    .editing
                    .and_then(|i| self.accounts.get(i))
                    .is_some_and(|a| a.goa_id.is_some());
                if !editing_goa {
                    self.apply_provider(widgets);
                }
                self.show_push_row(widgets);
            }

            AccountsInput::OAuthSignIn => {
                let settings = self.oauth_settings_from_form(widgets);
                if settings.client_id.trim().is_empty()
                    || settings.auth_url.is_empty()
                    || settings.token_url.is_empty()
                {
                    widgets.oauth_status.set_visible(true);
                    widgets.oauth_status.set_css_classes(&["error"]);
                    widgets.oauth_status.set_label(&i18n(
                        "Enter a client ID (and endpoints for a custom provider) first",
                    ));
                    return;
                }
                widgets.oauth_signin_btn.set_sensitive(false);
                widgets.oauth_status.set_visible(true);
                widgets.oauth_status.set_css_classes(&["dim-label"]);
                let in_window = !settings.redirect_uri.trim().is_empty();
                widgets.oauth_status.set_label(&if in_window {
                    i18n("Complete the sign-in in the window that opened.")
                } else {
                    i18n("Opening browser… complete sign-in there.")
                });
                // The address typed so far names the identity broker's
                // account, where one is in charge of sign-ins.
                let user =
                    Some(widgets.email_row.text().trim().to_string()).filter(|u| !u.is_empty());
                sign_in(root, &sender, settings, user, false, AccountsCmd::OAuth);
            }

            AccountsInput::SetGraphDirectory(on) => {
                if on == self.directory_on {
                    return;
                }
                self.directory_on = on;
                widgets.graph_status.set_visible(false);
                // Turning it off keeps the scopes: they cost nothing, and
                // turning it on again then asks nothing.
                if !on || self.directory_granted {
                    return;
                }
                let mut settings = self.oauth_settings_from_form(widgets);
                if settings.client_id.trim().is_empty()
                    || !settings.token_url.contains("microsoftonline")
                {
                    return;
                }
                settings.scopes = crate::oauth::with_directory_scopes(&settings.scopes);
                widgets.graph_directory_row.set_sensitive(false);
                widgets.oauth_signin_btn.set_sensitive(false);
                widgets.graph_status.set_visible(true);
                widgets.graph_status.set_css_classes(&["dim-label"]);
                widgets
                    .graph_status
                    .set_label(&if settings.redirect_uri.trim().is_empty() {
                        i18n("Opening browser… approve access to the directory there.")
                    } else {
                        i18n("Approve access to the directory in the window that opened.")
                    });
                // Straight to the account being edited: Microsoft then asks
                // only for what it has not approved yet.
                let user =
                    Some(widgets.email_row.text().trim().to_string()).filter(|u| !u.is_empty());
                sign_in(
                    root,
                    &sender,
                    settings,
                    user,
                    true,
                    AccountsCmd::DirectoryConsent,
                );
            }

            AccountsInput::SetGraphLabels(on) => {
                if on == self.labels_on {
                    return;
                }
                self.labels_on = on;
                widgets.graph_status.set_visible(false);
                // Off keeps the scope, as the directory's does.
                if !on || self.labels_granted {
                    return;
                }
                let mut settings = self.oauth_settings_from_form(widgets);
                if settings.client_id.trim().is_empty()
                    || !settings.token_url.contains("microsoftonline")
                {
                    return;
                }
                settings.scopes = crate::oauth::with_scopes(
                    &settings.scopes,
                    &crate::oauth::MICROSOFT_LABEL_SCOPES,
                );
                widgets.graph_labels_row.set_sensitive(false);
                widgets.oauth_signin_btn.set_sensitive(false);
                widgets.graph_status.set_visible(true);
                widgets.graph_status.set_css_classes(&["dim-label"]);
                widgets
                    .graph_status
                    .set_label(&if settings.redirect_uri.trim().is_empty() {
                        i18n("Opening browser… approve access to the sensitivity labels there.")
                    } else {
                        i18n("Approve access to the sensitivity labels in the window that opened.")
                    });
                let user =
                    Some(widgets.email_row.text().trim().to_string()).filter(|u| !u.is_empty());
                sign_in(
                    root,
                    &sender,
                    settings,
                    user,
                    true,
                    AccountsCmd::LabelsConsent,
                );
            }

            AccountsInput::OpenOnlineAccounts => open_online_accounts(),

            AccountsInput::Save => {
                // Pull the signature HTML out of the editor first (async), then
                // finish saving in SaveWithSig.
                let s = sender.clone();
                self.read_signature(widgets, move |html| {
                    s.input_sender().emit(AccountsInput::SaveWithSig(html))
                });
            }

            AccountsInput::SetFolderChoices(map) => {
                self.folders_by_email = map;
            }

            AccountsInput::SignatureSourceToggle => {
                if self.sig_reading {
                    return;
                }
                // The view loads a new document either way, which forgets
                // that the old one was edited.
                let s = sender.clone();
                self.sig_editor(widgets).is_dirty(move |dirty| {
                    s.input_sender().emit(AccountsInput::SigTextTouched(dirty))
                });
                let s = sender.clone();
                if self.sig_editor(widgets).source_kind().is_some() {
                    self.sig_editor(widgets).extract_source(move |text| {
                        s.input_sender().emit(AccountsInput::SignatureToRich(text))
                    });
                } else {
                    self.sig_editor(widgets).extract_html(move |html| {
                        s.input_sender()
                            .emit(AccountsInput::SignatureToSource(html))
                    });
                }
            }
            AccountsInput::SignatureToSource(html) => {
                self.sig_editor(widgets)
                    .set_source(rich_editor::SourceKind::Html, &html);
                self.sync_source_button(widgets);
            }
            AccountsInput::SignatureToRich(text) => {
                self.sig_editor(widgets)
                    .set_html(&rich_editor::signature_from_source(&text, None));
                self.sync_source_button(widgets);
            }
            AccountsInput::SignatureImport => {
                let dialog = gtk::FileDialog::new();
                dialog.set_title(&i18n("Import Signature"));
                let filter = gtk::FileFilter::new();
                filter.set_name(Some(&i18n("HTML and text files")));
                filter.add_mime_type("text/html");
                filter.add_mime_type("text/plain");
                filter.add_suffix("html");
                filter.add_suffix("htm");
                filter.add_suffix("txt");
                let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                dialog.set_default_filter(Some(&filter));
                let parent = root.root().and_downcast::<gtk::Window>();
                let s = sender.input_sender().clone();
                dialog.open(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                    if let Some(path) = res.ok().and_then(|f| f.path()) {
                        let _ = s.send(AccountsInput::SignatureImportFile(path));
                    }
                });
            }
            AccountsInput::SignatureImportFile(path) => match std::fs::read(&path) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    self.sig_editor(widgets)
                        .set_html(&rich_editor::signature_from_source(&text, path.parent()));
                    self.sync_source_button(widgets);
                }
                Err(e) => {
                    let host = root.root().and_downcast::<gtk::Window>();
                    let dialog = adw::MessageDialog::new(
                        host.as_ref(),
                        Some(i18n("Could Not Read the File").as_str()),
                        Some(&format!("{}\n{e}", path.display())),
                    );
                    dialog.add_response("ok", &i18n("OK"));
                    dialog.present();
                }
            },

            AccountsInput::SigShow => {
                let pick = widgets.sig_choice_row.selected() as usize;
                if self.sig_reading || pick == self.sig_shown || pick >= self.sig_edits.len() {
                    return;
                }
                self.read_back_signature(widgets, &sender, false);
            }
            AccountsInput::SigAdd => {
                if !self.sig_reading {
                    self.read_back_signature(widgets, &sender, true);
                }
            }
            AccountsInput::SigDelete => {
                if self.sig_reading || self.sig_edits.len() < 2 {
                    return;
                }
                let gone = self.sig_shown;
                self.sig_edits.remove(gone);
                self.sig_default = match self.sig_default {
                    Some(d) if d == gone => None,
                    Some(d) if d > gone => Some(d - 1),
                    d => d,
                };
                self.sig_touched = true;
                self.show_signature(widgets, gone.saturating_sub(1));
            }
            AccountsInput::SigRename => {
                let name = widgets.sig_name_row.text().to_string();
                let shown = self.sig_shown;
                let Some(sig) = self.sig_edits.get_mut(shown) else {
                    return;
                };
                if sig.name == name {
                    return;
                }
                sig.name = name;
                self.sig_touched = true;
                // Setting the model's entry moves the chooser off it; its
                // notify finds it back on the shown one and does nothing.
                let label = signature_label(&self.sig_edits, shown);
                if let Some(list) = widgets
                    .sig_choice_row
                    .model()
                    .and_downcast::<gtk::StringList>()
                {
                    list.splice(shown as u32, 1, &[label.as_str()]);
                    widgets.sig_choice_row.set_selected(shown as u32);
                }
            }
            AccountsInput::SigDefault => {
                let on = widgets.sig_default_row.is_active();
                let shown = Some(self.sig_shown);
                let default = match (on, self.sig_default == shown) {
                    (true, false) => shown,
                    (false, true) => None,
                    _ => return,
                };
                self.sig_default = default;
                self.sig_touched = true;
            }
            AccountsInput::SigTextTouched(dirty) => self.sig_touched |= dirty,
            AccountsInput::SigStored { html, add } => {
                self.sig_reading = false;
                if let Some(shown) = self.sig_edits.get_mut(self.sig_shown) {
                    shown.html = html;
                }
                let next = if add {
                    let n = self.sig_edits.len() + 1;
                    self.sig_edits.push(NamedSignature {
                        name: i18n_f("Signature {n}", &[("n", &n.to_string())]),
                        html: String::new(),
                    });
                    self.sig_touched = true;
                    self.sig_edits.len() - 1
                } else {
                    widgets.sig_choice_row.selected() as usize
                };
                self.show_signature(widgets, next.min(self.sig_edits.len().saturating_sub(1)));
                if add {
                    widgets.sig_name_row.grab_focus();
                }
            }

            AccountsInput::SaveWithSig(sig_html) => {
                widgets.host_row.remove_css_class("error");
                let mut account = read_account(widgets, self.saved_emoji(), self.saved_avatar());
                account.aliases = self.alias_edits.clone();
                account.hidden_folders = self.hidden_edits.clone();
                account.folder_roles = self.read_folder_roles(widgets);
                account.sent_copy_path = self.read_sent_copy_path(widgets);
                account.server_saves_sent = widgets.server_saves_row.is_active();
                if let Some(shown) = self.sig_edits.get_mut(self.sig_shown) {
                    shown.html = sig_html;
                }
                let (signatures, default) = self.saved_signatures();
                account.set_signatures(signatures, default);

                // Editing preserves the enabled state; GOA accounts keep their
                // (GOA-driven) OAuth mechanism regardless of the Authentication combo.
                let editing_orig = self.editing.and_then(|i| self.accounts.get(i)).cloned();
                if let Some(orig) = &editing_orig {
                    account.enabled = orig.enabled;
                    // The one-time look for Exchange's non-mail folders
                    // (#239) stays done, or a folder brought back here
                    // would be hidden again at the next listing.
                    account.folders_seeded = orig.folders_seeded;
                    account.goa_id = orig.goa_id.clone();
                    account.goa_mail_disabled = orig.goa_mail_disabled;
                    account.goa_enabled_before_mail_disabled =
                        orig.goa_enabled_before_mail_disabled;
                    if orig.goa_id.is_some() {
                        account.oauth = orig.oauth;
                        account.oauth_settings = orig.oauth_settings.clone();
                        // GNOME Online Accounts is the source of truth for these;
                        // Hylki keeps only what it owns (display name, signature,
                        // color, emoji, label).
                        account.email = orig.email.clone();
                        account.protocol = orig.protocol;
                        account.imap_host = orig.imap_host.clone();
                        account.imap_port = orig.imap_port;
                        account.smtp_host = orig.smtp_host.clone();
                        account.smtp_port = orig.smtp_port;
                        account.username = orig.username.clone();
                        account.password = orig.password.clone();
                        account.smtp_separate = orig.smtp_separate;
                        account.smtp_username = orig.smtp_username.clone();
                        account.smtp_password = orig.smtp_password.clone();
                        account.security = orig.security.clone();
                    }
                }

                // Only once Microsoft has approved the scopes; an account from
                // GNOME Online Accounts cannot have them.
                account.graph_directory &= account.goa_id.is_none() && self.directory_granted;
                account.graph_label_names &= account.goa_id.is_none() && self.labels_granted;

                // Native account: authentication comes from the provider dropdown.
                let is_oauth = provider_at(widgets.provider_row.selected()).is_oauth();
                if account.goa_id.is_none() {
                    if is_oauth {
                        account.oauth = true;
                        account.oauth_settings = Some(self.oauth_settings_from_form(widgets));
                        if account.username.trim().is_empty() {
                            account.username = account.email.clone();
                        }
                        // A fresh sign-in supplies a refresh token; otherwise keep
                        // the one already in the keyring (edit without re-signing).
                        if let Some(rt) = self.pending_oauth_refresh.clone() {
                            account.oauth_refresh = rt;
                        }
                    } else {
                        account.oauth = false;
                        account.oauth_settings = None;
                    }
                }

                // Validation.
                let oauth_ready = if account.oauth && account.goa_id.is_none() {
                    let has_client = account
                        .oauth_settings
                        .as_ref()
                        .is_some_and(|s| !s.client_id.trim().is_empty());
                    let signed_in = self.pending_oauth_refresh.is_some()
                        || editing_orig.as_ref().is_some_and(|o| o.oauth);
                    if !has_client || !signed_in {
                        widgets.oauth_status.set_visible(true);
                        widgets.oauth_status.set_css_classes(&["error"]);
                        widgets
                            .oauth_status
                            .set_label(&i18n("Enter a client ID and sign in before saving"));
                    }
                    has_client && signed_in
                } else {
                    true
                };
                // Saved before the keyring answered, or with the field left
                // as it was: the stored secrets stand. Not read yet, they
                // are waited for, and Save runs again once they are here.
                if AccountSecrets::needed(&account) {
                    match &self.editor_secrets {
                        Some(secrets) => secrets.apply(&mut account),
                        None => {
                            if !self.secrets_reading {
                                self.read_secrets(&account, &sender);
                            }
                            self.save_waiting = true;
                            return;
                        }
                    }
                }
                let password_ok = account.oauth || !account.password.is_empty();
                // A GOA account's connection fields were all restored from the
                // original above (GNOME owns them; a Graph account rightly has
                // no IMAP host at all) — validating them would only block the
                // fields Hylki does own: label, signature, color, aliases.
                let is_goa_edit = account.goa_id.is_some();
                if !is_goa_edit
                    && (account.imap_host.is_empty()
                        || account.username.is_empty()
                        || !password_ok
                        || !oauth_ready
                        || (account.smtp_separate
                            && (account.smtp_username.is_empty()
                                || account.smtp_password.is_empty())))
                {
                    widgets.host_row.add_css_class("error");
                    return;
                }
                self.pending_oauth_refresh = None;

                let original_email = self
                    .editing
                    .and_then(|i| self.accounts.get(i))
                    .map(|a| a.email.clone());
                match self.editing {
                    Some(i) if i < self.accounts.len() => self.accounts[i] = account.clone(),
                    _ => self.accounts.push(account.clone()),
                }
                self.rebuild_account_list(&widgets.accounts_list, &sender);
                let _ = sender.output(AccountsOutput::Saved {
                    original_email,
                    account: Box::new(account),
                });
                widgets.nav.pop();
            }

            AccountsInput::RemoveCurrent => {
                // Confirm before this destructive, keyring-clearing action.
                let Some(i) = self.editing else { return };
                let Some(account) = self.accounts.get(i) else {
                    return;
                };
                let name = if account.name.trim().is_empty() {
                    account.email.clone()
                } else {
                    account.name.clone()
                };
                let body = if account.goa_id.is_some() {
                    format!(
                        "Remove {name} from Hylki? It stays in GNOME Online Accounts \
                         and can be imported again from the list below. Mail on the \
                         server is not affected."
                    )
                } else {
                    format!(
                        "Remove {name} from Hylki? Its saved password is deleted from \
                         the keyring. Mail on the server is not affected."
                    )
                };
                // The panel is embedded — dialogs parent to whatever window
                // it currently sits in (the combined settings window).
                let host = root.root().and_downcast::<gtk::Window>();
                let dialog = adw::MessageDialog::new(
                    host.as_ref(),
                    Some(i18n("Remove Account?").as_str()),
                    Some(&body),
                );
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("remove", &i18n("Remove"));
                dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let s = sender.clone();
                dialog.connect_response(None, move |_, resp| {
                    if resp == "remove" {
                        s.input_sender().emit(AccountsInput::ConfirmRemove);
                    }
                });
                dialog.present();
            }
            AccountsInput::ConfirmRemove => {
                if let Some(i) = self.editing {
                    if i < self.accounts.len() {
                        let email = self.accounts[i].email.clone();
                        self.accounts.remove(i);
                        self.rebuild_account_list(&widgets.accounts_list, &sender);
                        let _ = sender.output(AccountsOutput::Removed { email });
                        // A removed GOA import returns to the list below (and a
                        // repaired-away broken entry re-offers its GOA account).
                        self.goa = importable_goa_accounts(&self.accounts);
                        self.rebuild_goa_list(&widgets.goa_list, &sender);
                        widgets.goa_group.set_visible(!self.goa.is_empty());
                    }
                }
                widgets.nav.pop();
            }

            AccountsInput::AliasAdd => {
                self.alias_editing = None;
                let jmap = form_protocol(widgets) == Protocol::Jmap;
                self.open_alias_dialog(root, &AliasConfig::default(), jmap, &sender);
            }

            AccountsInput::AliasEdit(i) => {
                let Some(alias) = self.alias_edits.get(i).cloned() else {
                    return;
                };
                self.alias_editing = Some(i);
                let jmap = form_protocol(widgets) == Protocol::Jmap;
                self.open_alias_dialog(root, &alias, jmap, &sender);
            }

            AccountsInput::UnhideFolder(i) => {
                if i < self.hidden_edits.len() {
                    self.hidden_edits.remove(i);
                    self.rebuild_hidden_list(widgets, &sender);
                }
            }
            AccountsInput::AliasRemove(i) => {
                if i < self.alias_edits.len() {
                    // The keyring entry (if the alias had its own SMTP) is
                    // dropped on Save, when the removal actually takes effect.
                    self.alias_edits.remove(i);
                    self.rebuild_alias_list(&widgets.aliases_list, &sender);
                }
            }

            AccountsInput::AliasDialogSave => {
                let Some(d) = self.alias_dialog.as_ref() else {
                    return;
                };
                for row in [&d.addr_row, &d.host_row, &d.user_row] {
                    row.remove_css_class("error");
                }
                d.pass_row.remove_css_class("error");

                let name = trimmed(&d.name_row);
                let addr = trimmed(&d.addr_row);
                let own_smtp = d.smtp_switch.is_active();
                let host = trimmed(&d.host_row);
                let port: u16 = trimmed(&d.port_row).parse().unwrap_or(587);
                let user = trimmed(&d.user_row);
                let pass = d.pass_row.text().to_string();

                // The address must look like one, and must not shadow the
                // account's own address or another alias — the send path picks
                // an alias's transport by matching the From address, so a
                // duplicate would make "which server sends this?" ambiguous.
                let account_email = trimmed(&widgets.email_row);
                let duplicate = addr.eq_ignore_ascii_case(&account_email)
                    || self.alias_edits.iter().enumerate().any(|(i, a)| {
                        Some(i) != self.alias_editing && a.address().eq_ignore_ascii_case(&addr)
                    });
                let mut bad = false;
                if addr.is_empty() || !addr.contains('@') || duplicate {
                    d.addr_row.add_css_class("error");
                    bad = true;
                }
                if own_smtp {
                    if host.is_empty() {
                        d.host_row.add_css_class("error");
                        bad = true;
                    }
                    if user.is_empty() {
                        d.user_row.add_css_class("error");
                        bad = true;
                    }
                    if pass.is_empty() {
                        d.pass_row.add_css_class("error");
                        bad = true;
                    }
                }
                if bad {
                    return;
                }

                let alias = AliasConfig {
                    identity: if name.is_empty() {
                        addr.clone()
                    } else {
                        format!("{name} <{addr}>")
                    },
                    smtp_host: if own_smtp { host } else { String::new() },
                    smtp_port: port,
                    smtp_username: if own_smtp { user } else { String::new() },
                    smtp_password: if own_smtp { pass } else { String::new() },
                };
                match self.alias_editing {
                    Some(i) if i < self.alias_edits.len() => self.alias_edits[i] = alias,
                    _ => self.alias_edits.push(alias),
                }
                self.alias_editing = None;
                self.close_alias_dialog();
                self.rebuild_alias_list(&widgets.aliases_list, &sender);
            }

            AccountsInput::AliasDialogTest => {
                let Some(d) = self.alias_dialog.as_ref() else {
                    return;
                };
                let alias = AliasConfig {
                    identity: trimmed(&d.addr_row),
                    smtp_host: trimmed(&d.host_row),
                    smtp_port: trimmed(&d.port_row).parse().unwrap_or(587),
                    smtp_username: trimmed(&d.user_row),
                    smtp_password: d.pass_row.text().to_string(),
                };
                let email = trimmed(&widgets.email_row);
                d.test_btn.set_sensitive(false);
                d.test_result.set_visible(true);
                d.test_result.set_css_classes(&["dim-label"]);
                d.test_result.set_label(&i18n("Testing…"));
                sender.oneshot_command(async move {
                    let r = tokio::task::spawn_blocking(move || {
                        worker::test_alias_smtp_blocking(email, alias)
                    })
                    .await
                    .unwrap_or_else(|_| Err("test could not run".into()));
                    AccountsCmd::AliasTested(r)
                });
            }

            AccountsInput::AliasDialogClosed => {
                // The notification is queued behind the close, so by the time it
                // arrives a replacement dialog may already be open — only clear
                // state when the dialog we track is really the one that closed.
                if self
                    .alias_dialog
                    .as_ref()
                    .is_none_or(|d| !d.window.is_visible())
                {
                    self.alias_dialog = None;
                    self.alias_editing = None;
                }
            }

            AccountsInput::AddSenderText(text) => {
                let addr = text.trim().to_lowercase();
                if !addr.is_empty() && !self.sender_addrs.contains(&addr) {
                    self.sender_addrs.push(addr.clone());
                    self.senders.guard().push_back(addr.clone());
                    self.senders.widget().set_visible(true);
                    let _ = sender.output(AccountsOutput::AddSender(addr));
                }
            }
            AccountsInput::RemoveSenderRow(addr) => {
                if let Some(pos) = self.sender_addrs.iter().position(|a| *a == addr) {
                    self.sender_addrs.remove(pos);
                    self.senders.guard().remove(pos);
                    self.senders
                        .widget()
                        .set_visible(!self.sender_addrs.is_empty());
                    let _ = sender.output(AccountsOutput::RemoveSender(addr));
                }
            }
            AccountsInput::AddBlacklistText(text) => {
                let addr = text.trim().to_lowercase();
                if !addr.is_empty() && !self.blacklist_addrs.contains(&addr) {
                    self.blacklist_addrs.push(addr.clone());
                    self.blacklist.guard().push_back(addr.clone());
                    self.blacklist.widget().set_visible(true);
                    let _ = sender.output(AccountsOutput::AddBlacklist(addr));
                }
            }
            AccountsInput::RemoveBlacklistRow(addr) => {
                if let Some(pos) = self.blacklist_addrs.iter().position(|a| *a == addr) {
                    self.blacklist_addrs.remove(pos);
                    self.blacklist.guard().remove(pos);
                    self.blacklist
                        .widget()
                        .set_visible(!self.blacklist_addrs.is_empty());
                    let _ = sender.output(AccountsOutput::RemoveBlacklist(addr));
                }
            }
            AccountsInput::AddFilter => {
                self.open_filter_page(&widgets.nav, &sender, None, None);
            }
            AccountsInput::AddFilterFrom(rule) => {
                self.open_filter_page(&widgets.nav, &sender, None, Some(rule));
            }
            AccountsInput::ApplyFilters => {
                if !self.filters_applying {
                    self.set_filters_applying(true);
                    let _ = sender.output(AccountsOutput::ApplyFilters);
                }
            }
            AccountsInput::FiltersApplying(on) => self.set_filters_applying(on),
            AccountsInput::EditFilter(i) => {
                if let Some(rule) = self.filter_rules.get(i).cloned() {
                    self.open_filter_page(&widgets.nav, &sender, Some((i, rule)), None);
                }
            }
            AccountsInput::FilterEdited(i, rule) => {
                if let Some(slot) = self.filter_rules.get_mut(i) {
                    if *slot != rule {
                        *slot = rule;
                        self.rebuild_filter_rows(&sender);
                        let _ =
                            sender.output(AccountsOutput::SetFilters(self.filter_rules.clone()));
                    }
                }
            }
            AccountsInput::RemoveFilter(i) => {
                if i < self.filter_rules.len() {
                    self.filter_rules.remove(i);
                    self.rebuild_filter_rows(&sender);
                    let _ = sender.output(AccountsOutput::SetFilters(self.filter_rules.clone()));
                }
            }
            AccountsInput::FilterAdded(rule) => {
                self.filter_rules.push(rule);
                self.rebuild_filter_rows(&sender);
                let _ = sender.output(AccountsOutput::SetFilters(self.filter_rules.clone()));
            }
            AccountsInput::AddSmartFolder => self.open_smart_page(&widgets.nav, &sender, None),
            AccountsInput::EditSmartFolder(i) => {
                if let Some(f) = self.smart_folders.get(i).cloned() {
                    self.open_smart_page(&widgets.nav, &sender, Some((i, f)));
                }
            }
            AccountsInput::RemoveSmartFolder(i) => {
                if i < self.smart_folders.len() {
                    self.smart_folders.remove(i);
                    self.rebuild_smart_rows(&sender);
                    let _ =
                        sender.output(AccountsOutput::SetSmartFolders(self.smart_folders.clone()));
                }
            }
            AccountsInput::SmartFolderSaved(at, folder) => {
                match at.filter(|i| *i < self.smart_folders.len()) {
                    Some(i) => self.smart_folders[i] = folder,
                    None => self.smart_folders.push(folder),
                }
                self.rebuild_smart_rows(&sender);
                let _ = sender.output(AccountsOutput::SetSmartFolders(self.smart_folders.clone()));
            }
            AccountsInput::SetSmartFolders(folders) => {
                if folders != self.smart_folders {
                    self.smart_folders = folders;
                    self.rebuild_smart_rows(&sender);
                }
            }
            AccountsInput::AddTag => self.open_tag_page(&widgets.nav, &sender, None),
            AccountsInput::EditTag(i) => {
                if let Some(tag) = self.tags.get(i).cloned() {
                    self.open_tag_page(&widgets.nav, &sender, Some((i, tag)));
                }
            }
            AccountsInput::RemoveTag(i) => {
                if i < self.tags.len() {
                    self.tags.remove(i);
                    self.rebuild_tag_rows(&sender);
                    // The rules name tags by keyword; a rule whose tag is gone
                    // shows the bare keyword until it is edited.
                    self.rebuild_filter_rows(&sender);
                    let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
                }
            }
            AccountsInput::MoveTag { from, to } => {
                if from < self.tags.len() && from != to {
                    let tag = self.tags.remove(from);
                    let to = to.min(self.tags.len());
                    self.tags.insert(to, tag);
                    self.rebuild_tag_rows(&sender);
                    let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
                }
            }
            AccountsInput::TagAdded(tag) => {
                self.tags.push(tag);
                self.rebuild_tag_rows(&sender);
                let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
            }
            AccountsInput::TagEdited(i, tag) => {
                if let Some(slot) = self.tags.get_mut(i) {
                    if *slot != tag {
                        *slot = tag;
                        self.rebuild_tag_rows(&sender);
                        self.rebuild_filter_rows(&sender);
                        let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
                    }
                }
            }
            AccountsInput::FindTags => {
                if !self.tag_scanning {
                    self.set_tag_scanning(true);
                    let _ = sender.output(AccountsOutput::FindTags);
                }
            }
            AccountsInput::ServerIdentities { email, identities } => {
                let open = self
                    .editing
                    .and_then(|i| self.accounts.get(i))
                    .is_some_and(|a| a.email.eq_ignore_ascii_case(&email));
                if open && self.server_identities != identities {
                    self.server_identities = identities;
                    self.rebuild_alias_list(&widgets.aliases_list, &sender);
                }
            }
            AccountsInput::Quota { email, quota } => {
                let open = self
                    .editing
                    .and_then(|i| self.accounts.get(i))
                    .is_some_and(|a| a.email.eq_ignore_ascii_case(&email));
                if open {
                    show_quota(widgets, quota);
                }
            }
            AccountsInput::TagScanning(on) => self.set_tag_scanning(on),
            AccountsInput::TagFindings(found) => {
                self.set_tag_scanning(false);
                let parent = root.root().and_downcast::<gtk::Window>();
                self.show_tag_findings(parent.as_ref(), found, &sender);
            }
            AccountsInput::ImportTags(tags) => {
                let mut added = 0;
                for t in tags {
                    if !self
                        .tags
                        .iter()
                        .any(|x| x.keyword.eq_ignore_ascii_case(&t.keyword))
                    {
                        self.tags.push(t);
                        added += 1;
                    }
                }
                if added > 0 {
                    self.rebuild_tag_rows(&sender);
                    self.rebuild_filter_rows(&sender);
                    let _ = sender.output(AccountsOutput::SetTags(self.tags.clone()));
                }
            }
        }
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        result: AccountsCmd,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match result {
            AccountsCmd::OwnGravatar { email, outcome } => {
                // Cached for every other circle in the app as well; the
                // preview only redraws while that address is still in the
                // editor, and the accounts list redraws its circles so one
                // that landed after the list was built shows there too.
                let found = crate::avatar::cache_own_gravatar(&email, outcome);
                if found && trimmed(&widgets.email_row).eq_ignore_ascii_case(&email) {
                    self.refresh_preview(widgets);
                }
                if found {
                    self.rebuild_account_list(&widgets.accounts_list, &sender);
                }
            }

            AccountsCmd::GoaListed(listed) => {
                widgets.goa_refresh_btn.set_sensitive(true);
                self.goa = importable_of(listed, &self.accounts);
                self.rebuild_goa_list(&widgets.goa_list, &sender);
                widgets.goa_group.set_visible(!self.goa.is_empty());
            }

            AccountsCmd::Secrets(session, secrets) => {
                if session != self.editor_session {
                    return;
                }
                self.secrets_reading = false;
                self.editor_secrets = Some(secrets.clone());
                let editor_up = widgets
                    .nav
                    .visible_page()
                    .and_then(|p| p.tag())
                    .is_some_and(|tag| tag == "editor");
                // Queued, so it runs once the fields below are filled.
                if std::mem::take(&mut self.save_waiting) && editor_up {
                    sender.input_sender().emit(AccountsInput::Save);
                }
                // Still editing that account: fill in what the user hasn't
                // typed over meanwhile.
                let Some(i) = self.editing else { return };
                if self
                    .accounts
                    .get(i)
                    .is_none_or(|a| a.email != secrets.email)
                {
                    return;
                }
                // The passwords arrive after the editor opened, so they would
                // read as an edit the user never made. Re-take the seed below
                // when nothing has actually been typed yet.
                let untouched = !self.editor_touched(widgets);
                if let Some(acc) = self.accounts.get_mut(i) {
                    secrets.apply(acc);
                }
                if widgets.pass_row.text().is_empty() {
                    widgets.pass_row.set_text(&secrets.password);
                }
                if widgets.smtp_pass_row.text().is_empty() {
                    widgets.smtp_pass_row.set_text(&secrets.smtp_password);
                }
                for al in self.alias_edits.iter_mut() {
                    if al.smtp_password.is_empty() {
                        if let Some((_, pw)) =
                            secrets.aliases.iter().find(|(a, _)| *a == al.address())
                        {
                            al.smtp_password = pw.clone();
                        }
                    }
                }
                if untouched {
                    self.editor_seed = Some(self.editor_fingerprint(widgets));
                }
            }
            AccountsCmd::Test(result) => {
                let line = |label: &str, r: &Result<(), String>| {
                    match r {
                    Ok(()) => format!("✓ {label}: connected"),
                    Err(e) if crate::worker::is_hostname_mismatch(e) => format!(
                        "✗ {label}: {e}\n   {}",
                        i18n("The certificate is for another name. If this server is the right one, turn on “Accept a certificate for another name” above and test again.")
                    ),
                    Err(e) => format!("✗ {label}: {e}"),
                }
                };
                let protocol = form_protocol(widgets);
                let incoming_label = match protocol {
                    Protocol::Pop3 => "POP3",
                    Protocol::Jmap => "JMAP",
                    _ => "IMAP",
                };
                // A JMAP account sends through the server: no SMTP line.
                let text = if protocol == Protocol::Jmap {
                    line(incoming_label, &result.incoming)
                } else {
                    format!(
                        "{}\n{}",
                        line(incoming_label, &result.incoming),
                        line("SMTP", &result.smtp),
                    )
                };
                let class = if result.incoming.is_ok() && result.smtp.is_ok() {
                    "success"
                } else {
                    "error"
                };
                widgets.test_result.set_label(&text);
                widgets.test_result.set_css_classes(&[class]);
                widgets.test_btn.set_sensitive(true);
            }
            AccountsCmd::OAuth(result) => {
                widgets.oauth_signin_btn.set_sensitive(true);
                widgets.oauth_status.set_visible(true);
                match result {
                    Ok((refresh, who)) => {
                        self.pending_oauth_refresh = Some(refresh);
                        if let Some((name, address)) = who {
                            if !address.is_empty() {
                                widgets.email_row.set_text(&address);
                            }
                            if !name.is_empty() && widgets.name_row.text().trim().is_empty() {
                                widgets.name_row.set_text(&name);
                            }
                        }
                        widgets.oauth_status.set_css_classes(&["success"]);
                        widgets
                            .oauth_status
                            .set_label(&i18n("✓ Signed in. Save the account to finish."));
                    }
                    Err(e) => {
                        widgets.oauth_status.set_css_classes(&["error"]);
                        widgets
                            .oauth_status
                            .set_label(&i18n_f("Sign-in failed: {e}", &[("e", &(e).to_string())]));
                    }
                }
            }
            AccountsCmd::DirectoryConsent(result) => {
                widgets.graph_directory_row.set_sensitive(true);
                widgets.oauth_signin_btn.set_sensitive(true);
                widgets.graph_status.set_visible(true);
                match self.take_consent(widgets, result) {
                    Ok(()) => {
                        self.directory_granted = true;
                        widgets.graph_status.set_css_classes(&["success"]);
                        widgets.graph_status.set_label(&i18n(
                            "Access to the directory approved. Save the account to finish.",
                        ));
                    }
                    Err(e) => {
                        self.directory_on = false;
                        widgets.graph_directory_row.set_active(false);
                        widgets.graph_status.set_css_classes(&["error"]);
                        widgets.graph_status.set_label(&i18n_f(
                            "Access to the directory was not approved: {e}",
                            &[("e", &e)],
                        ));
                    }
                }
            }
            AccountsCmd::LabelsConsent(result) => {
                widgets.graph_labels_row.set_sensitive(true);
                widgets.oauth_signin_btn.set_sensitive(true);
                widgets.graph_status.set_visible(true);
                match self.take_consent(widgets, result) {
                    Ok(()) => {
                        self.labels_granted = true;
                        widgets.graph_status.set_css_classes(&["success"]);
                        widgets
                            .graph_status
                            .set_label(&i18n("Access to the sensitivity labels approved. Save the account to finish."));
                    }
                    Err(e) => {
                        self.labels_on = false;
                        widgets.graph_labels_row.set_active(false);
                        widgets.graph_status.set_css_classes(&["error"]);
                        widgets.graph_status.set_label(&i18n_f(
                            "Access to the sensitivity labels was not approved: {e}",
                            &[("e", &e)],
                        ));
                    }
                }
            }
            AccountsCmd::AliasTested(result) => {
                let Some(d) = self.alias_dialog.as_ref() else {
                    return;
                };
                d.test_btn.set_sensitive(true);
                d.test_result.set_visible(true);
                match result {
                    Ok(()) => {
                        d.test_result.set_css_classes(&["success"]);
                        d.test_result.set_label(&i18n("✓ SMTP: connected"));
                    }
                    Err(e) => {
                        d.test_result.set_css_classes(&["error"]);
                        d.test_result
                            .set_label(&i18n_f("✗ SMTP: {e}", &[("e", &(e).to_string())]));
                    }
                }
            }
        }
    }
}
