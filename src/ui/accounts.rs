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
mod fields;
mod filters;
mod form;
mod form_page;
mod messages;
mod navigation;
mod oauth;
mod providers;
mod save;
mod senders;
mod setup;
mod signatures;
mod smart_folders;
mod tags;

use account_list::importable_goa_accounts;
use aliases::AliasDialog;
pub use editor::AccountSecrets;
pub(crate) use form::non_ellipsizing_factory;
use form::{
    apply_protocol, clear_editor, fill_editor, form_protocol, import_avatar_file, mount_editor,
    parse_color, read_account, set_connection_editable, show_quota, trimmed, REPLY_COPIES_HINT,
    SENT_COPY_HINT,
};
pub use messages::{AccountsCmd, AccountsInit, AccountsInput, AccountsOutput, TagProposal};
pub(crate) use oauth::open_online_accounts;
use providers::provider_at;
pub(crate) use providers::{brand_for_goa, provider_factory, Provider, PROVIDERS};
use setup::{connect_editor_open, fill_editor_choices, prepare_editor_page};

const DEFAULT_COLOR: &str = "#3584e4";

/// The open form page's Save (see `AccountsWindow::form_save`).
type FormSave = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn() -> bool>>>>;

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
    form_save: FormSave,
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
        let mut model = AccountsWindow::new(init, &sender);

        let senders_box = model.senders.widget();
        let blacklist_box = model.blacklist.widget();
        // An empty boxed list still draws its frame: a stray line under the
        // entry that adds to it (#129). Shown only once it has a row.
        senders_box.set_visible(!model.sender_addrs.is_empty());
        blacklist_box.set_visible(!model.blacklist_addrs.is_empty());
        let widgets = view_output!();
        prepare_editor_page(&widgets, &sender);
        tracing::debug!(
            "settings window: accounts view built in {:?}",
            t_init.elapsed()
        );
        model.fill_lists(&widgets, &sender);
        fill_editor_choices(&widgets);
        model.connect_editor(&widgets, &sender);
        connect_editor_open(&widgets, &sender, model.form_save.clone());

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
            AccountsInput::RefreshGoa => self.on_refresh_goa(widgets, sender),
            AccountsInput::CloseEditor => self.on_close_editor(widgets),
            AccountsInput::ReleaseSignatureEditor => self.on_release_signature_editor(widgets),
            AccountsInput::DebugEditLabel(text) => widgets.label_row.set_text(&text),
            AccountsInput::DebugProvider(label) => self.on_debug_provider(label, widgets),
            AccountsInput::LeaveRequest(page) => self.on_leave_request(page, widgets, sender),
            AccountsInput::LeaveVerdict { page, touched } => {
                self.on_leave_verdict(page, touched, sender)
            }
            AccountsInput::SaveOpenPage => self.on_save_open_page(widgets, sender),
            AccountsInput::SearchFilters(text) => self.on_search_filters(text, widgets),
            AccountsInput::SearchSenders(text) => self.on_search_senders(text),
            AccountsInput::AddAccount => self.on_add_account(widgets, sender),
            AccountsInput::EditAccountByEmail(email) => {
                self.on_edit_account_by_email(email, widgets, sender)
            }
            AccountsInput::EditAccount(i) => self.on_edit_account(i, widgets, sender),
            AccountsInput::EmailChanged => self.on_email_changed(widgets),
            AccountsInput::MoveRow { from, to } => self.on_move_row(from, to, widgets, sender),
            AccountsInput::ToggleEnabled { index, enabled } => {
                self.on_toggle_enabled(index, enabled, widgets, sender)
            }
            AccountsInput::ToggleCurrentEnabled(enabled) => {
                self.on_toggle_current_enabled(enabled, widgets, sender)
            }
            AccountsInput::ImportGoa(index) => self.on_import_goa(index, widgets, sender),
            AccountsInput::SetEmoji(text) => self.on_set_emoji(text, widgets),
            AccountsInput::ClearGlyph => self.on_clear_glyph(widgets),
            AccountsInput::SetServerSavesSent(on) => self.on_set_server_saves_sent(on, widgets),
            AccountsInput::SetFolderSort { email, sort } => {
                self.on_set_folder_sort(email, sort, widgets)
            }
            AccountsInput::SetOwnGravatar(on) => self.on_set_own_gravatar(on, widgets, sender),
            AccountsInput::SetPictureMode(on) => self.on_set_picture_mode(on, widgets),
            AccountsInput::RefreshPreview => self.refresh_preview(widgets),
            AccountsInput::PickAvatar => self.on_pick_avatar(sender, root),
            AccountsInput::SetAvatar(name) => self.on_set_avatar(name, widgets),
            AccountsInput::TestConnection => self.on_test_connection(widgets, sender),
            AccountsInput::ProtocolChanged => self.on_protocol_changed(widgets),
            AccountsInput::ProviderChanged => self.on_provider_changed(widgets),
            AccountsInput::OAuthSignIn => self.on_oauth_sign_in(widgets, sender, root),
            AccountsInput::SetGraphDirectory(on) => {
                self.on_set_graph_directory(on, widgets, sender, root)
            }
            AccountsInput::SetGraphLabels(on) => {
                self.on_set_graph_labels(on, widgets, sender, root)
            }
            AccountsInput::OpenOnlineAccounts => open_online_accounts(),
            AccountsInput::Save => self.on_save(widgets, sender),
            AccountsInput::SetFolderChoices(map) => self.on_set_folder_choices(map),
            AccountsInput::SignatureSourceToggle => {
                self.on_signature_source_toggle(widgets, sender)
            }
            AccountsInput::SignatureToSource(html) => self.on_signature_to_source(html, widgets),
            AccountsInput::SignatureToRich(text) => self.on_signature_to_rich(text, widgets),
            AccountsInput::SignatureImport => self.on_signature_import(sender, root),
            AccountsInput::SignatureImportFile(path) => {
                self.on_signature_import_file(path, widgets, root)
            }
            AccountsInput::SigShow => self.on_sig_show(widgets, sender),
            AccountsInput::SigAdd => self.on_sig_add(widgets, sender),
            AccountsInput::SigDelete => self.on_sig_delete(widgets),
            AccountsInput::SigRename => self.on_sig_rename(widgets),
            AccountsInput::SigDefault => self.on_sig_default(widgets),
            AccountsInput::SigTextTouched(dirty) => self.sig_touched |= dirty,
            AccountsInput::SigStored { html, add } => self.on_sig_stored(html, add, widgets),
            AccountsInput::SaveWithSig(sig_html) => {
                self.on_save_with_sig(sig_html, widgets, sender)
            }
            AccountsInput::RemoveCurrent => self.on_remove_current(sender, root),
            AccountsInput::ConfirmRemove => self.on_confirm_remove(widgets, sender),
            AccountsInput::AliasAdd => self.on_alias_add(widgets, sender, root),
            AccountsInput::AliasEdit(i) => self.on_alias_edit(i, widgets, sender, root),
            AccountsInput::UnhideFolder(i) => self.on_unhide_folder(i, widgets, sender),
            AccountsInput::AliasRemove(i) => self.on_alias_remove(i, widgets, sender),
            AccountsInput::AliasDialogSave => self.on_alias_dialog_save(widgets, sender),
            AccountsInput::AliasDialogTest => self.on_alias_dialog_test(widgets, sender),
            AccountsInput::AliasDialogClosed => self.on_alias_dialog_closed(),
            AccountsInput::AddSenderText(text) => self.on_add_sender_text(text, sender),
            AccountsInput::RemoveSenderRow(addr) => self.on_remove_sender_row(addr, sender),
            AccountsInput::AddBlacklistText(text) => self.on_add_blacklist_text(text, sender),
            AccountsInput::RemoveBlacklistRow(addr) => self.on_remove_blacklist_row(addr, sender),
            AccountsInput::AddFilter => self.on_add_filter(widgets, sender),
            AccountsInput::AddFilterFrom(rule) => self.on_add_filter_from(rule, widgets, sender),
            AccountsInput::ApplyFilters => self.on_apply_filters(sender),
            AccountsInput::FiltersApplying(on) => self.set_filters_applying(on),
            AccountsInput::EditFilter(i) => self.on_edit_filter(i, widgets, sender),
            AccountsInput::FilterEdited(i, rule) => self.on_filter_edited(i, rule, sender),
            AccountsInput::RemoveFilter(i) => self.on_remove_filter(i, sender),
            AccountsInput::FilterAdded(rule) => self.on_filter_added(rule, sender),
            AccountsInput::AddSmartFolder => self.open_smart_page(&widgets.nav, &sender, None),
            AccountsInput::EditSmartFolder(i) => self.on_edit_smart_folder(i, widgets, sender),
            AccountsInput::RemoveSmartFolder(i) => self.on_remove_smart_folder(i, sender),
            AccountsInput::SmartFolderSaved(at, folder) => {
                self.on_smart_folder_saved(at, folder, sender)
            }
            AccountsInput::SetSmartFolders(folders) => self.on_set_smart_folders(folders, sender),
            AccountsInput::AddTag => self.open_tag_page(&widgets.nav, &sender, None),
            AccountsInput::EditTag(i) => self.on_edit_tag(i, widgets, sender),
            AccountsInput::RemoveTag(i) => self.on_remove_tag(i, sender),
            AccountsInput::MoveTag { from, to } => self.on_move_tag(from, to, sender),
            AccountsInput::TagAdded(tag) => self.on_tag_added(tag, sender),
            AccountsInput::TagEdited(i, tag) => self.on_tag_edited(i, tag, sender),
            AccountsInput::FindTags => self.on_find_tags(sender),
            AccountsInput::ServerIdentities { email, identities } => {
                self.on_server_identities(email, identities, widgets, sender)
            }
            AccountsInput::Quota { email, quota } => self.on_quota(email, quota, widgets),
            AccountsInput::TagScanning(on) => self.set_tag_scanning(on),
            AccountsInput::TagFindings(found) => self.on_tag_findings(found, sender, root),
            AccountsInput::ImportTags(tags) => self.on_import_tags(tags, sender),
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
                self.on_own_gravatar_fetched(email, outcome, widgets, sender)
            }
            AccountsCmd::GoaListed(listed) => self.on_goa_listed(listed, widgets, sender),
            AccountsCmd::Secrets(session, secrets) => {
                self.on_secrets_read(session, secrets, widgets, sender)
            }
            AccountsCmd::Test(result) => self.on_connection_tested(result, widgets),
            AccountsCmd::OAuth(result) => self.on_signed_in(result, widgets),
            AccountsCmd::DirectoryConsent(result) => self.on_directory_consent(result, widgets),
            AccountsCmd::LabelsConsent(result) => self.on_labels_consent(result, widgets),
            AccountsCmd::AliasTested(result) => self.on_alias_tested(result),
        }
    }
}
