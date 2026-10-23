//! The settings window (#141): a category sidebar beside a stack of pages.
//! Most pages are built here; Accounts (`ui/accounts.rs`), OpenPGP, Cloud
//! Storage, Translation and Contacts are components of their own hosted in
//! the stack.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{
    AppTheme, ClockStyle, DateStyle, LauncherBadge, MessageTheme, ReaderToolbar, ToolbarItem,
    ToolbarSide, TrayIcon,
};
use crate::i18n::{i18n, i18n_f, i18n_noop, ni18n_f};
use crate::ui::chip_flow::ChipFlow;
use std::rc::Rc;

mod alerts;
mod appearance;
mod chips;
mod choices;
mod composing;
mod contacts;
mod init_layout;
mod init_message_rows;
mod init_rows;
mod messages;
mod navigation;
mod reading;
mod rows;
mod search;
mod sidebar_page;
mod spelling;
mod system;

use chips::{ColumnEditor, ToolbarEditor};
pub use choices::language_choices;
pub(crate) use choices::{
    contacts_placement_from_index, contacts_placement_index, contacts_placement_model,
};
use choices::{
    files_action_index, files_large_index, link_browser_index, placement_from_index,
    placement_index, sound_index, sound_labels, APP_THEMES, CLOCK_STYLES, DATE_STYLES,
    FETCH_INTERVALS, KEEP_DOWNLOADS_DAYS, LAUNCHER_BADGES, MESSAGE_THEMES, RENDERERS, TRAY_ICONS,
};
pub use messages::{PrefInit, PrefInput, PrefOutput};
use navigation::{side_page, SIDE_PAGES};
use rows::{compose_font_setup, middle_ellipsize, widen_combo_value, widen_page};
pub use search::NOT_SEARCHED;
use search::{scroll_to, SettingsSearch};
use spelling::{rebuild_personal_words, SpellLangRows};
use system::{hide_edit_icon, link_group_description, nautilus_status_text, storage_text};

// ---- Preferences window -----------------------------------------------------

pub struct Preferences {
    /// Mirrors the notifications switch, so the "show sender and subject" row
    /// below it can grey out when nothing is being posted at all.
    notifications: bool,
    /// The notification buttons, kept whole so each switch can hand the
    /// app the full set (#244).
    notification_buttons: crate::config::NotificationButtons,
    /// The new-mail sound (#292): on or off, and which one.
    sound: crate::config::NewMailSound,
    /// The custom sound file, a copy in the data directory.
    custom_sound: Option<std::path::PathBuf>,
    show_unified: bool,
    /// The unified Starred / Sent / Drafts switches, kept whole so each
    /// toggle can hand the app the full set.
    unified_kinds: crate::config::UnifiedKinds,
    /// The unified rows' unread-chip switches, likewise kept whole.
    unified_chips: crate::config::UnifiedChips,
    /// The Printing switches, kept whole so each toggle can hand the app
    /// the full set (#359).
    print_options: crate::config::PrintOptions,
    /// Mirrors the "Accounts in the sidebar" switch, which the main menu
    /// can flip too.
    show_accounts: bool,
    /// The icon rail's fold-up switches, kept whole so each toggle can hand
    /// the app the full set.
    rail_fold: crate::config::RailFold,
    /// The browsers the links combo offers, in the order it lists them
    /// (empty inside the Flatpak sandbox, which cannot see them).
    browsers: Vec<crate::ui::launch::Browser>,
    /// The reader toolbar layout being edited (the app applies every drop).
    toolbar: ReaderToolbar,
    /// Focus Mode's switches, kept whole so each toggle hands the app the
    /// full set (the main menu and Ctrl+Shift+F flip the master switch too).
    focus: crate::config::FocusMode,
    /// The three drop zones of the toolbar editor, filled from `toolbar`.
    toolbar_editor: Option<ToolbarEditor>,
    /// The single-line columns being edited (#334), and their editor.
    list_columns: Vec<crate::config::ListColumn>,
    column_editor: Option<ColumnEditor>,
    /// Message List → Columns, which the list's headings open to (#443).
    columns_group: Option<gtk::Widget>,
    /// Whether the list can be on one line at all, which the columns are
    /// for.
    list_single_possible: bool,
    /// Whether swipe actions are on (the reverse switch follows it).
    swipe_enabled: bool,
    /// Mirrors the threading switch, so the "threaded message list" row below
    /// it can grey out when conversations aren't grouped at all.
    threading: bool,
    /// Mirrors the expandable-conversations switch — "expand by default" only
    /// means anything while conversations can expand in the list at all.
    thread_expansion: bool,
    /// Mirrors the list-palette switch, so the hover row under it can grey
    /// out when there is no palette to open.
    list_palette: bool,
    /// Mirrors the card-actions mode: only its hidden-behind-a-toggle choice
    /// has a ⋯, so only there can the ⋯ stand in for a menu.
    card_actions_hover: bool,
    /// Mirrors the sender-avatars switch: with no circles drawn at all,
    /// whose face they would show doesn't arise (#189).
    avatars: bool,
    /// The content stack (one child per category, plus the accounts panel
    /// in its "accounts" slot), driven by the sidebar (#141).
    panels_stack: Option<gtk::Stack>,
    /// The accounts panel's slot, for the fresh panel each open brings.
    accounts_slot: Option<adw::Bin>,
    /// Pages taken out of the stack until after the window's first paint:
    /// only the page shown is laid out and styled for it, the rest come
    /// back a moment later (or at once, should one be asked for first).
    deferred_pages: std::cell::RefCell<Vec<(String, gtk::Widget)>>,
    /// The sidebar list, for selecting a category from update().
    side_list: Option<gtk::ListBox>,
    /// The settings search in the sidebar (#260).
    search: Option<SettingsSearch>,
    /// The content pane's page, whose title names the chosen category.
    content_page: Option<adw::NavigationPage>,
    /// The split view, to bring the content forward when collapsed.
    split: Option<adw::NavigationSplitView>,
    /// The accounts component, told which of its pages the sidebar chose.
    accounts_sender: relm4::Sender<crate::ui::accounts::AccountsInput>,
    /// The content header bar; hidden while the accounts editor subpage is
    /// open, whose own header takes over.
    host_header: Option<adw::HeaderBar>,
    /// Settings → Sidebar's Contacts placement row, kept in step with the
    /// same row in Settings → Contacts.
    contacts_placement_row: Option<adw::ComboRow>,
    /// The OpenPGP page (#133), kept alive with the window.
    pgp_keys: Option<Controller<crate::ui::pgp_keys::PgpKeys>>,
    /// (name, address) per enabled account and alias, the rows of the
    /// "Send new messages from" combo after its first (#157).
    identities: Vec<(String, String)>,
    /// The Cloud Storage page (#144), likewise.
    cloud: Option<Controller<crate::ui::cloud_accounts::CloudAccounts>>,
    /// The Contacts page (#408): every address book, CardDAV accounts and
    /// LDAP directories (#307) among them, likewise.
    contacts: Option<Controller<crate::ui::contact_sources::ContactSources>>,
    /// A page about one address book is pushed over the Contacts list.
    contacts_open: bool,
    /// The account editor is up in the accounts slot: leaving it for another
    /// category asks about the unsaved changes first.
    editor_open: bool,
    /// Which side page's editor is up: "accounts", "cloud" or "contacts".
    editor_page: &'static str,
    /// What downloaded mail takes up (#394): message bodies, attachment
    /// files and the whole cache file, in bytes; `None` while measuring.
    storage: Option<(u64, u64, u64)>,
    /// The GNOME Files extension (#188): installed, loaded, loader present.
    nautilus: crate::nautilus_ext::State,
    /// The terminal command that installs the nautilus-python package on
    /// this machine, for the copyable row; `None` on a distribution whose
    /// package manager is not known.
    nautilus_cmd: Option<&'static str>,
    /// What files handed in from Files open into, and the size limit.
    files: crate::config::FilesPrefs,
    /// Those rows, for the app to move when a hand-off dialog's "always
    /// do this" changes the preference while the window is up. Set only
    /// when they differ: a combo row set to its current value is a no-op,
    /// but one set from `#[watch]` on every update hung GTK's list-item
    /// manager in the pre-warmed (unmapped) window.
    files_rows: Option<(adw::ComboRow, adw::ComboRow, adw::SpinRow)>,
    /// Composing → Spelling → Languages, kept to follow a change made from
    /// a composer's right-click menu (#365).
    spell_langs: Option<SpellLangRows>,
}

/// The `sender` the view's handlers report through. Setting a row's initial
/// value fires the same signal a change by hand does: a combo row's model
/// going in moves its selection to the first entry, and the saved value then
/// moves it again. Built hidden a moment after startup (`AppMsg::PrewarmSettings`),
/// the window so reported every choice twice, the first time wrong: the tray
/// item was taken down and published again for each, which Cinnamon's status
/// applet answered by crashing (#275). Nothing is reported until `ready` is
/// set, at the end of `init`, once the saved values are in.
#[derive(Clone)]
struct ViewSender {
    inner: ComponentSender<Preferences>,
    ready: Rc<std::cell::Cell<bool>>,
}

impl ViewSender {
    fn input(&self, msg: PrefInput) {
        if self.ready.get() {
            self.inner.input(msg);
        } else {
            tracing::debug!("settings window: {msg:?} while the rows are being set, not reported");
        }
    }

    fn output(&self, msg: PrefOutput) -> Result<(), PrefOutput> {
        self.inner.output(msg)
    }
}

#[relm4::component(pub)]
impl Component for Preferences {
    type Init = PrefInit;
    type Input = PrefInput;
    type Output = PrefOutput;
    type CommandOutput = ();

    view! {
        adw::Window {
            set_modal: false,
            set_default_width: 920,
            // The same size every time: the two-pane layout (#141) fits its
            // sidebar at this height, and nothing is remembered from a resize.
            set_default_height: 848,
            set_title: Some(i18n("Settings").as_str()),
            // Closing hides: the window is kept and shown again next time.
            set_hide_on_close: true,

            connect_close_request[sender] => move |_| {
                let _ = sender.output(PrefOutput::Closed);
                gtk::glib::Propagation::Proceed
            },

            // One window, two panes (#141): a sidebar of categories on the
            // left, the chosen category's groups on the right. The account
            // pages (accounts, tags, filters, senders) belong to the
            // AccountsWindow component, handed in via PrefInit with its own
            // sub-navigation for the account editor; while that editor is
            // open the content header hides, so the editor's own back/Save
            // header takes over. Narrow, the split collapses to one pane
            // (breakpoint in init) and the sidebar becomes the first page.
            #[wrap(Some)]
            #[name = "split"]
            set_content = &adw::NavigationSplitView {
                set_min_sidebar_width: 180.0,
                set_max_sidebar_width: 250.0,
                set_sidebar_width_fraction: 0.27,

                #[wrap(Some)]
                set_sidebar = &adw::NavigationPage {
                    set_title: &i18n("Settings"),

                    #[wrap(Some)]
                    #[name = "side_toolbar"]
                    set_child = &adw::ToolbarView {
                        #[name = "side_header"]
                        add_top_bar = &adw::HeaderBar {
                            set_show_end_title_buttons: false,
                        },

                        #[wrap(Some)]
                        #[name = "side_scroller"]
                        set_content = &gtk::ScrolledWindow {
                            set_hscrollbar_policy: gtk::PolicyType::Never,

                            #[wrap(Some)]
                            #[name = "side_list"]
                            set_child = &gtk::ListBox {
                                add_css_class: "navigation-sidebar",
                                add_css_class: "settings-side",
                                set_selection_mode: gtk::SelectionMode::Single,
                                connect_row_selected[sender] => move |_, row| {
                                    if let Some(id) = row
                                        .map(|r| r.widget_name().to_string())
                                        .and_then(|n| n.strip_prefix("page:").map(str::to_string))
                                    {
                                        sender.input(PrefInput::SelectPage(id));
                                    }
                                },
                            },
                        },
                    },
                },

                #[wrap(Some)]
                #[name = "content_page"]
                set_content = &adw::NavigationPage {
                    set_title: &i18n("General"),

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                        #[name = "host_header"]
                        add_top_bar = &adw::HeaderBar {
                            set_show_start_title_buttons: false,
                        },

                        #[wrap(Some)]
                        #[name = "panels_stack"]
                        set_content = &gtk::Stack {
                            // Sections switch outright, no fade.
                            set_transition_type: gtk::StackTransitionType::None,
                            // Only the shown page is measured: homogeneous, the
                            // stack measured every page's rows on every layout
                            // pass, which was most of the window's first paint.
                            set_hhomogeneous: false,
                            set_vhomogeneous: false,

                            #[name = "accounts_slot"]
                            add_named[Some("accounts")] = &adw::Bin {},

                            // The OpenPGP key manager (#133), its own component.
                            #[name = "pgp_slot"]
                            add_named[Some("openpgp")] = &adw::Bin {},

                            // Cloud attachment accounts (#144), its own component.
                            #[name = "cloud_slot"]
                            add_named[Some("cloud")] = &adw::Bin {},

                            // Message translation (#327), built by its own module.
                            #[name = "translation_slot"]
                            add_named[Some("translation")] = &adw::Bin {},

                            // Every address book (#408), its own component.
                            #[name = "contacts_slot"]
                            add_named[Some("contacts")] = &adw::Bin {},

                            add_named[Some("general")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("General"),

                                    #[name = "fetch_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Check for new mail"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFetchInterval(row.selected()));
                                        },
                                    },

                                    #[name = "push_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Instant new mail (push)"),
                                        set_subtitle: &i18n("Microsoft 365 accounts check at the interval above."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetPush(row.is_active()));
                                        },
                                    },

                                    #[name = "metered_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Save data on metered connections"),
                                        set_subtitle: &i18n("New mail still arrives. Nothing else is fetched ahead."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetMeteredSaveData(row.is_active()));
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Notifications"),

                                    #[name = "notifications_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Desktop notifications"),
                                        set_subtitle: &i18n("For new mail and errors, while the window is in the background."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleNotifications(row.is_active()));
                                        },
                                    },

                                    #[name = "notification_content_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.notifications,
                                        set_title: &i18n("Show sender and subject"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetNotificationContent(row.is_active()));
                                        },
                                    },

                                    // A sound with each new-mail notification
                                    // (#292): a built-in one or a file of the
                                    // user's own.
                                    #[name = "sound_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.notifications,
                                        set_title: &i18n("Sound for new mail"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleSound(row.is_active()));
                                        },
                                    },

                                    #[name = "sound_choice_row"]
                                    adw::ComboRow {
                                        #[watch]
                                        set_sensitive: model.notifications && model.sound.enabled,
                                        set_title: &i18n("Sound"),
                                        add_suffix = &gtk::Button {
                                            set_icon_name: "media-playback-start-symbolic",
                                            set_tooltip_text: Some(i18n("Play").as_str()),
                                            set_valign: gtk::Align::Center,
                                            add_css_class: "flat",
                                            connect_clicked[sender] => move |_| {
                                                sender.input(PrefInput::PlaySound);
                                            },
                                        },
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeSound(row.selected()));
                                        },
                                    },

                                    adw::ActionRow {
                                        #[watch]
                                        set_visible: model.sound.sound == crate::config::CUSTOM_SOUND,
                                        #[watch]
                                        set_sensitive: model.notifications && model.sound.enabled,
                                        set_title: &i18n("Sound file"),
                                        #[watch]
                                        set_subtitle: &model
                                            .custom_sound
                                            .as_ref()
                                            .and_then(|p| p.file_name())
                                            .map_or_else(|| i18n("None"), |n| n.to_string_lossy().into_owned()),
                                        add_suffix = &gtk::Button {
                                            set_label: &i18n("Choose…"),
                                            set_valign: gtk::Align::Center,
                                            connect_clicked[sender] => move |_| {
                                                sender.input(PrefInput::ChooseSound);
                                            },
                                        },
                                    },

                                    // GNOME shows no notification for the app
                                    // in front, so by default there is no sound
                                    // either; this plays it anyway (#337).
                                    #[name = "sound_focused_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.notifications && model.sound.enabled,
                                        set_title: &i18n("Also play while Hylki is in front"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleSoundFocused(row.is_active()));
                                        },
                                    },

                                },

                                // The buttons a single-message notification
                                // carries (#244); a summary of several has none.
                                // Three at most: once three are on, the rest
                                // grey out until one is switched off.
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Notification Buttons"),
                                    set_description: Some(&i18n("Up to three buttons on a notification about one message.")),

                                    #[name = "notify_mark_read_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::MarkRead),
                                        set_title: &i18n("Mark as Read"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::MarkRead, row.is_active()));
                                        },
                                    },

                                    #[name = "notify_archive_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::Archive),
                                        set_title: &i18n("Archive"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::Archive, row.is_active()));
                                        },
                                    },

                                    #[name = "notify_delete_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::Delete),
                                        set_title: &i18n("Delete"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::Delete, row.is_active()));
                                        },
                                    },

                                    #[name = "notify_reply_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::Reply),
                                        set_title: &i18n("Reply"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::Reply, row.is_active()));
                                        },
                                    },

                                    #[name = "notify_forward_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::Forward),
                                        set_title: &i18n("Forward"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::Forward, row.is_active()));
                                        },
                                    },

                                    #[name = "notify_spam_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.button_row_sensitive(crate::config::NotificationButton::Spam),
                                        set_title: &i18n("Mark as Spam"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleNotificationButton(crate::config::NotificationButton::Spam, row.is_active()));
                                        },
                                    },
                                },
                            },

                            #[name = "appearance_page"]
                            add_named[Some("appearance")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    // Rendered as Pango markup — a bare "&" breaks it.
                                    set_title: &i18n("Appearance"),

                                    #[name = "app_theme_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Style"),
                                        set_subtitle: &i18n("Messages have their own setting under Reading."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeAppTheme(row.selected()));
                                        },
                                    },

                                    #[name = "text_scale_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Text size"),
                                        set_subtitle: &i18n("Adjusts the interface and message text size."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeTextScale(row.selected()));
                                        },
                                    },

                                    // The theme gallery; its content is built
                                    // in init (a grid the view! macro can't
                                    // declare), like the app-icon strip below.
                                    #[name = "theme_row"]
                                    adw::PreferencesRow {
                                        set_title: &i18n("Theme"),
                                        set_activatable: false,
                                        set_focusable: false,
                                    },

                                    // The app-icon gallery; its content is built in init
                                    // (a grid of textures the view! macro can't declare).
                                    #[name = "app_icon_row"]
                                    adw::PreferencesRow {
                                        set_title: &i18n("App icon"),
                                        set_activatable: false,
                                        set_focusable: false,
                                    },

                                    #[name = "desktop_icons_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Use your desktop environment's icon set"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetDesktopIcons(row.is_active()));
                                        },
                                    },

                                    #[name = "settings_open_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("This window opens to"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeSettingsOpen(row.selected()));
                                        },
                                    },

                                },

                                // The reader toolbar's buttons: three drop zones
                                // (left group, right group, not shown) of
                                // draggable chips, filled in init from the saved
                                // layout. Every drop is applied and saved at once.
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Toolbar"),
                                    set_description: Some(
                                        &i18n("Drag buttons between the groups. The right group folds into ⋯ when the pane is narrow."),
                                    ),
                                    #[name = "toolbar_editor_box"]
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 12,

                                        gtk::Button {
                                            set_label: &i18n("Restore Defaults"),
                                            set_halign: gtk::Align::End,
                                            set_valign: gtk::Align::Center,
                                            connect_clicked => PrefInput::ToolbarRestore,
                                        },
                                    },
                                },

                                // Focus Mode: the master switch and its parts.
                                // Every row applies at once, animated.
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Focus Mode"),
                                    set_description: Some(
                                        &i18n("The parts switched on below hide while Focus Mode is on. Activated by Ctrl+Shift+F or the main menu."),
                                    ),

                                    #[name = "focus_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.enabled,
                                        set_title: &i18n("Focus Mode"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::Enabled, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_start_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.start_focused,
                                        set_title: &i18n("Start in Focus Mode"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::StartFocused, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_toolbar_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.reader_toolbar,
                                        set_title: &i18n("Fold the reading pane's toolbar"),
                                        set_subtitle: &i18n("Every button goes into its ⋯ menu."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::ReaderToolbar, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_list_header_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.list_header,
                                        set_title: &i18n("Fold the message list's header"),
                                        set_subtitle: &i18n("Everything but the sidebar toggle goes into ⋯."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::ListHeader, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_accounts_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.hide_accounts,
                                        set_title: &i18n("Hide the accounts in the sidebar"),
                                        set_subtitle: &i18n("Only the unified section stays."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HideAccounts, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_unified_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.fold_unified,
                                        set_title: &i18n("Fold up the unified rows"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::FoldUnified, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_avatars_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.hide_avatars,
                                        set_title: &i18n("Hide sender avatars"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HideAvatars, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_hide_preview_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.hide_preview,
                                        set_title: &i18n("Hide the preview text"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HidePreview, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_preview_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.one_preview_line,
                                        // Nothing left to cap once the preview
                                        // text is hidden altogether.
                                        #[watch]
                                        set_sensitive: !model.focus.hide_preview,
                                        set_title: &i18n("One line of preview text"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::OnePreviewLine, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_subject_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.hide_subject,
                                        set_title: &i18n("Hide the subject"),
                                        set_subtitle: &i18n("Tag chips hide with it."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::HideSubject, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_rail_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.rail_sidebar,
                                        set_title: &i18n("Fold the sidebar to the icon rail"),
                                        set_subtitle: &i18n("Hovering it slides the full sidebar out."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::RailSidebar, row.is_active()));
                                        },
                                    },
                                    #[name = "focus_reader_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.focus.reader_view,
                                        set_title: &i18n("Reader View"),
                                        set_subtitle: &i18n("Open messages in Reader View."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleFocus(crate::config::FocusPart::ReaderView, row.is_active()));
                                        },
                                    },
                                },
                            },

                            add_named[Some("sidebar")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Sidebar"),

                                    #[name = "start_view_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Open at startup"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeStartView(row.selected()));
                                        },
                                    },

                                    #[name = "show_accounts_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_active: model.show_accounts,
                                        set_title: &i18n("Accounts in the sidebar"),
                                        set_subtitle: &i18n("Off leaves only the unified section. Toggle with Ctrl+Shift+A or the main menu."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleShowAccounts(row.is_active()));
                                        },
                                    },

                                    #[name = "folder_sort_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Folder order"),
                                        set_subtitle: &i18n("Dragging a folder switches to Custom Order."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFolderSort(row.selected()));
                                        },
                                    },

                                    #[name = "chevron_side_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Chevron placement"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeChevronSide(row.selected()));
                                        },
                                    },

                                    #[name = "account_click_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Fold accounts with one click"),
                                        set_subtitle: &i18n("When off, only its chevron or a double click activates the fold."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetAccountClickFolds(row.is_active()));
                                        },
                                    },

                                    #[name = "show_attachments_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Show Attachments Browser"),
                                        set_subtitle: &i18n("Browse every account's attachments in one place."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetAttachmentsRow(row.is_active()));
                                        },
                                    },

                                    #[name = "sidebar_hover_expand_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Expand the sidebar on hover"),
                                        set_subtitle: &i18n("Hovering the icon rail floats the full sidebar out."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetSidebarHoverExpand(row.is_active()));
                                        },
                                    },

                                    #[name = "remember_sidebar_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Remember the sidebar layout"),
                                        set_subtitle: &i18n("Off starts every launch folded up."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetRememberSidebar(row.is_active()));
                                        },
                                    },

                                    #[name = "remember_rail_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Remember icon rail state"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetRememberRail(row.is_active()));
                                        },
                                    },

                                    #[name = "seen_counts_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Highlight only new unread mail"),
                                        set_subtitle: &i18n("A count turns grey once you have looked at the folder."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetSeenCounts(row.is_active()));
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Unified"),
                                    set_description: Some(
                                        &i18n("Shown only with more than one account."),
                                    ),

                                    #[name = "show_unified_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Inboxes"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleShowUnified(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_starred_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Starred"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedStarred(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_sent_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Sent"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedSent(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_drafts_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Drafts"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedDrafts(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_archive_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Archive"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedArchive(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_junk_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Junk"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedJunk(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_trash_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Trash"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleUnifiedTrash(row.is_active()));
                                        },
                                    },

                                    #[name = "unified_chips_row"]
                                    adw::ExpanderRow {
                                        set_title: &i18n("Unread counts"),
                                        set_subtitle: &i18n("Shown on the unified rows while they are folded up."),
                                        set_expanded: false,

                                        #[name = "unified_chip_all_inboxes_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Inboxes"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipAllInboxes(row.is_active()));
                                            },
                                        },
                                        #[name = "unified_chip_starred_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Starred"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipStarred(row.is_active()));
                                            },
                                        },
                                        #[name = "unified_chip_drafts_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Drafts"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipDrafts(row.is_active()));
                                            },
                                        },
                                        #[name = "unified_chip_archive_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Archive"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipArchive(row.is_active()));
                                            },
                                        },
                                        #[name = "unified_chip_junk_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Junk"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipJunk(row.is_active()));
                                            },
                                        },
                                        #[name = "unified_chip_trash_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Trash"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleUnifiedChipTrash(row.is_active()));
                                            },
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Sections"),

                                    #[name = "section_style_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Section style"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeSectionStyle(row.selected()));
                                        },
                                    },

                                    #[name = "filtered_placement_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Filters placement"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFilteredPlacement(row.selected()));
                                        },
                                    },

                                    #[name = "tags_placement_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Tags placement"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeTagsPlacement(row.selected()));
                                        },
                                    },

                                    #[name = "favorites_placement_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Favorites placement"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFavoritesPlacement(row.selected()));
                                        },
                                    },

                                    #[name = "contacts_placement_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Contacts placement"),
                                        set_subtitle: &i18n("'In the footer' disables expand/collapse."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeContactsPlacement(row.selected()));
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Icon rail"),

                                    #[name = "rail_dots_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Unread dots instead of counts"),
                                        set_subtitle: &i18n("The count stays in the tooltip."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetRailDots(row.is_active()));
                                        },
                                    },

                                    #[name = "rail_fold_row"]
                                    adw::ExpanderRow {
                                        set_title: &i18n("Fold up expanded items"),
                                        set_subtitle: &i18n("The items below start folded up in the rail. Long-press to open one."),
                                        set_show_enable_switch: true,
                                        set_expanded: false,
                                        connect_enable_expansion_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleRailFoldEnabled(row.enables_expansion()));
                                        },

                                        #[name = "rail_fold_accounts_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("All accounts"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldAccounts(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_all_inboxes_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Inboxes"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldAllInboxes(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_starred_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Starred"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldStarred(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_sent_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Sent"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldSent(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_drafts_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Drafts"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldDrafts(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_archive_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Archive"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldArchive(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_junk_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Junk"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldJunk(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_trash_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Trash"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldTrash(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_filtered_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Filters"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldFiltered(row.is_active()));
                                            },
                                        },
                                        #[name = "rail_fold_tags_row"]
                                        add_row = &adw::SwitchRow {
                                            set_title: &i18n("Tags"),
                                            connect_active_notify[sender] => move |row| {
                                                sender.input(PrefInput::ToggleRailFoldTags(row.is_active()));
                                            },
                                        },
                                    },
                                },
                            },

                            add_named[Some("list")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Message List"),

                                    #[name = "avatars_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Show sender avatars"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleAvatars(row.is_active()));
                                        },
                                    },

                                    // Your own mail (#189): its mailbox's
                                    // face, or the circle any sender gets.
                                    #[name = "own_mailbox_face_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.avatars,
                                        set_title: &i18n("Sent mail uses your account circle"),
                                        set_subtitle: &i18n("The account's picture or emoji, as in the sidebar."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetOwnMailboxFace(row.is_active()));
                                        },
                                    },

                                    #[name = "list_layout_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Layout"),
                                        set_subtitle: &i18n("Automatic uses single lines in wide windows."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeListLayout(row.selected()));
                                        },
                                    },

                                    #[name = "preview_lines_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Preview lines"),
                                        set_subtitle: &i18n("Off also stops downloading previews."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangePreviewLines(row.selected()));
                                        },
                                    },

                                    #[name = "group_by_date_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Group by date"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetGroupByDate(row.is_active()));
                                        },
                                    },

                                    #[name = "pills_below_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Tags and labels under the text"),
                                        set_subtitle: &i18n("Gives the subject its full width."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetPillsBelow(row.is_active()));
                                        },
                                    },

                                    #[name = "list_search_open_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Keep the search field open"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetListSearchOpen(row.is_active()));
                                        },
                                    },

                                    #[name = "list_search_place_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Search field position"),
                                        connect_selected_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetListSearchBottom(row.selected() == 1));
                                        },
                                    },

                                    #[name = "list_bulk_place_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Selection bar position"),
                                        connect_selected_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetListBulkBarBottom(row.selected() == 1));
                                        },
                                    },

                                    #[name = "tag_row_tint_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Tint tagged messages"),
                                        set_subtitle: &i18n("In the color of its first tag."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetTagRowTint(row.is_active()));
                                        },
                                    },

                                    #[name = "list_palette_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Actions palette in the message list"),
                                        set_subtitle: &i18n("The ⋯ row under each message."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleListPalette(row.is_active()));
                                        },
                                    },

                                    #[name = "list_palette_hover_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.list_palette,
                                        set_title: &i18n("Open the actions palette on hover"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetListPaletteHover(row.is_active()));
                                        },
                                    },

                                    #[name = "swipe_enabled_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Swipe actions"),
                                        set_subtitle: &i18n("Left deletes, right archives. Drag with the mouse or swipe with two fingers."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleSwipeEnabled(row.is_active()));
                                        },
                                    },

                                    #[name = "swipe_reversed_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.swipe_enabled,
                                        set_title: &i18n("Reverse swipe directions"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetSwipeReversed(row.is_active()));
                                        },
                                    },

                                    #[name = "swipe_sensitivity_row"]
                                    adw::SpinRow {
                                        #[watch]
                                        set_sensitive: model.swipe_enabled,
                                        set_title: &i18n("Trackpad swipe sensitivity"),
                                        set_subtitle: &i18n("Raise it if swipes stop short. Mouse drags are unaffected."),
                                        connect_value_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetSwipeSensitivity(row.value()));
                                        },
                                    },

                                    #[name = "palette_collapse_row"]
                                    adw::SpinRow {
                                        set_title: &i18n("Actions palette timeout"),
                                        set_subtitle: &i18n("Seconds it stays open after the pointer leaves."),
                                        connect_value_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetPaletteCollapse(row.value() as u64));
                                        },
                                    },
                                },

                                // The single-line list's columns (#334): two
                                // drop zones of draggable chips, filled in init.
                                #[name = "columns_group"]
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Columns"),
                                    set_description: Some(
                                        &i18n("For the single-line layout. Drag columns to reorder them or out to remove them. Due Date shows only for Microsoft 365 mail."),
                                    ),
                                    #[watch]
                                    set_sensitive: model.list_single_possible,

                                    #[name = "list_headings_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Show column headings"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetListHeadings(row.is_active()));
                                        },
                                    },

                                    #[name = "list_fit_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Fit columns to the list"),
                                        set_subtitle: &i18n("Off, a narrow list scrolls sideways."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetListColumnScroll(!row.is_active()));
                                        },
                                    },

                                    #[name = "column_editor_box"]
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 12,
                                        // Clear of the switch's card above.
                                        set_margin_top: 12,

                                        gtk::Button {
                                            set_label: &i18n("Restore Defaults"),
                                            set_halign: gtk::Align::End,
                                            set_valign: gtk::Align::Center,
                                            connect_clicked => PrefInput::ColumnRestore,
                                        },
                                    },
                                },
                            },

                            add_named[Some("conversations")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Conversations"),

                                    #[name = "threading_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Group messages by conversation"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleThreading(row.is_active()));
                                        },
                                    },

                                    #[name = "thread_expansion_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.threading,
                                        set_title: &i18n("Expandable conversations"),
                                        set_subtitle: &i18n("Off, a conversation's messages show only in the reading pane."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::ToggleThreadExpansion(row.is_active()));
                                        },
                                    },

                                    #[name = "threads_expanded_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.threading && model.thread_expansion,
                                        set_title: &i18n("Expand conversations by default"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetThreadsExpanded(row.is_active()));
                                        },
                                    },

                                    #[name = "thread_newest_first_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Newest message first"),
                                        set_subtitle: &i18n("In the reading pane."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetThreadNewestFirst(row.is_active()));
                                        },
                                    },

                                    #[name = "fold_messages_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Fold earlier messages"),
                                        set_subtitle: &i18n("The newest always opens."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFoldMessages(row.selected()));
                                        },
                                    },

                                    #[name = "thread_row_newest_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.threading,
                                        set_title: &i18n("Show your own replies in the message list"),
                                        set_subtitle: &i18n("Off shows the last message that arrived."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetThreadRowNewest(row.is_active()));
                                        },
                                    },

                                    #[name = "reply_position_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Reply editor position"),
                                        set_subtitle: &i18n("Following the reading order puts it where the conversation ends."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeReplyPosition(row.selected()));
                                        },
                                    },

                                    #[name = "card_attachments_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Attachments on each message"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetCardAttachments(row.is_active()));
                                        },
                                    },

                                    #[name = "attachment_drawer_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Attachment drawer"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetAttachmentDrawer(row.is_active()));
                                        },
                                    },

                                    #[name = "confirm_thread_delete_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Confirm conversation deletion"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetConfirmThreadDelete(row.is_active()));
                                        },
                                    },
                                },
                            },

                            add_named[Some("reading")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Reading"),

                                    #[name = "read_mark_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Mark as read"),
                                        set_subtitle: &i18n("Conversations mark each message as it comes into view."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeReadMark(row.selected()));
                                        },
                                    },

                                    #[name = "message_theme_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Message appearance"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeMessageTheme(row.selected()));
                                        },
                                    },

                                    #[name = "theme_switch_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Light / Dark Mode switch"),
                                        set_subtitle: &i18n("Displays in the message header for each message."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetThemeSwitch(row.is_active()));
                                        },
                                    },

                                    #[name = "reader_zoom_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Default zoom"),
                                        set_subtitle: &i18n("Ctrl+0 returns to the default."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeReaderZoom(row.selected()));
                                        },
                                    },

                                    #[name = "override_fonts_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Use my own font"),
                                        set_subtitle: &i18n("The font below replaces the sender's."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetOverrideFonts(row.is_active()));
                                        },
                                    },

                                    #[name = "reader_font_row"]
                                    adw::ActionRow {
                                        set_title: &i18n("Message font"),
                                        set_subtitle: &i18n("The interface font unless another is chosen."),
                                        #[name = "reader_font_button"]
                                        add_suffix = &gtk::FontDialogButton {
                                            set_valign: gtk::Align::Center,
                                            set_dialog: &gtk::FontDialog::new(),
                                            set_level: gtk::FontLevel::Font,
                                            set_use_font: true,
                                            connect_font_desc_notify[sender] => move |button| {
                                                let font = button
                                                    .font_desc()
                                                    .map(|d| d.to_string())
                                                    .unwrap_or_default();
                                                let _ = sender.output(PrefOutput::SetReaderFont(font));
                                            },
                                        },
                                    },

                                    #[name = "override_colors_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Use my own colors"),
                                        set_subtitle: &i18n("Ignores the sender's colors. Pictures stay; links use the accent color."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetOverrideColors(row.is_active()));
                                        },
                                    },

                                    #[name = "plain_mono_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Plain-text messages in monospace"),
                                        set_subtitle: &i18n("Formatted messages are not affected."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetPlainMonospace(row.is_active()));
                                        },
                                    },

                                    #[name = "plain_font_row"]
                                    adw::ActionRow {
                                        set_title: &i18n("Monospace font"),
                                        set_subtitle: &i18n("The system's monospace font unless another is chosen."),
                                        #[name = "plain_font_button"]
                                        add_suffix = &gtk::FontDialogButton {
                                            set_valign: gtk::Align::Center,
                                            set_dialog: &gtk::FontDialog::new(),
                                            set_level: gtk::FontLevel::Font,
                                            set_use_font: true,
                                            connect_font_desc_notify[sender] => move |button| {
                                                let font = button
                                                    .font_desc()
                                                    .map(|d| d.to_string())
                                                    .unwrap_or_default();
                                                let _ = sender.output(PrefOutput::SetPlainFont(font));
                                            },
                                        },
                                    },

                                    #[name = "card_actions_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Message card actions palette"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeCardActionsMode(row.selected()));
                                        },
                                    },

                                    #[name = "card_palette_collapse_row"]
                                    adw::SpinRow {
                                        set_title: &i18n("Message card actions palette timeout"),
                                        set_subtitle: &i18n("Seconds it stays open after the pointer leaves."),
                                        connect_value_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetCardPaletteCollapse(row.value() as u64));
                                        },
                                    },

                                    // Only the hidden-behind-a-toggle mode has
                                    // a ⋯ to press, so the choice is only live
                                    // there.
                                    #[name = "card_palette_menu_row"]
                                    adw::SwitchRow {
                                        #[watch]
                                        set_sensitive: model.card_actions_hover,
                                        set_title: &i18n("Message card actions palette as a menu"),
                                        set_subtitle: &i18n("The ⋯ opens the right-click menu instead."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetCardPaletteMenu(row.is_active()));
                                        },
                                    },

                                    #[name = "single_message_card_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Single messages as cards"),
                                        set_subtitle: &i18n("Off fills the pane edge to edge."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetSingleMessageCard(row.is_active()));
                                        },
                                    },

                                    #[name = "always_show_recipients_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Always show recipients"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetAlwaysShowRecipients(row.is_active()));
                                        },
                                    },

                                    #[name = "pgp_labels_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Name the OpenPGP result"),
                                        set_subtitle: &i18n("Off shows the icons alone."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetPgpLabels(row.is_active()));
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Reader View"),
                                    set_description: Some(&i18n("A message's text alone, without the sender's layout, colors and fonts.")),

                                    #[name = "reader_switch_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Show Reader View switch in message header"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetReaderSwitch(row.is_active()));
                                        },
                                    },

                                    #[name = "reader_default_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("When a message is opened"),
                                        set_subtitle: &i18n("The header's switch still changes the open message."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeReaderDefault(row.selected()));
                                        },
                                    },
                                },

                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Printing"),

                                    #[name = "print_page_numbers_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Page numbers"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::TogglePrintPageNumbers(row.is_active()));
                                        },
                                    },

                                    #[name = "print_date_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Date printed"),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::TogglePrintDate(row.is_active()));
                                        },
                                    },

                                    #[name = "print_punch_margin_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Wider left margin"),
                                        set_subtitle: &i18n("25 mm, so punched holes miss the text."),
                                        connect_active_notify[sender] => move |row| {
                                            sender.input(PrefInput::TogglePrintPunchMargin(row.is_active()));
                                        },
                                    },
                                },
                            },

                            add_named[Some("composing")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Composing"),

                                    #[name = "compose_inline_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Compose in the main window"),
                                        set_subtitle: &i18n("Off opens new messages in their own window."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetComposeInline(row.is_active()));
                                        },
                                    },

                                    #[name = "reply_inline_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Reply and forward in the main window"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetReplyInline(row.is_active()));
                                        },
                                    },

                                    #[name = "reply_fields_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Show From, To and Subject in the reply panel"),
                                        set_subtitle: &i18n("Off folds them behind a button."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetReplyFields(row.is_active()));
                                        },
                                    },

                                    #[name = "default_from_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Send new messages from"),
                                        set_subtitle: &i18n("Replies answer from the address they were sent to."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeComposeDefaultFrom(row.selected()));
                                        },
                                    },

                                    #[name = "paste_plain_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Paste as plain text"),
                                        set_subtitle: &i18n("Right-clicking the editor offers both either way."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetPastePlain(row.is_active()));
                                        },
                                    },

                                    #[name = "return_paragraph_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Return starts a new paragraph"),
                                        set_subtitle: &i18n("Shift+Return does the other."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetReturnParagraph(row.is_active()));
                                        },
                                    },

                                    #[name = "toolbar_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Formatting toolbar"),
                                        set_subtitle: &i18n("For new messages. The toolbar's chevron toggles it."),
                                        set_model: Some(&gtk::StringList::new(&[
                                            &i18n("Always collapsed"),
                                            &i18n("Always expanded"),
                                        ])),
                                        connect_selected_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetToolbarExpanded(row.selected() == 1));
                                        },
                                    },

                                    #[name = "compose_format_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Start new messages in"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeComposeFormat(row.selected()));
                                        },
                                    },

                                    // Filled and connected in init: the
                                    // list grows with the fonts picked
                                    // through Other Font….
                                    #[name = "compose_font_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Font"),
                                        set_subtitle: &i18n("For rich text. Default leaves it to the recipient's own font."),
                                    },

                                    #[name = "compose_size_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Font size"),
                                        set_subtitle: &i18n("For rich text, in points."),
                                    },

                                    #[name = "follow_plain_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Reply in the original's format"),
                                        set_subtitle: &i18n("Plain-text mail is answered in plain text."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetFollowPlain(row.is_active()));
                                        },
                                    },

                                    #[name = "signature_position_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Signature in replies"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeSignaturePosition(row.selected()));
                                        },
                                    },

                                    #[name = "signature_dashes_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Separator line above the signature"),
                                        set_subtitle: &i18n("The standard “-- ” line, which other mail apps recognize."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetSignatureDashes(row.is_active()));
                                        },
                                    },

                                    #[name = "undo_send_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Undo Send"),
                                        set_subtitle: &i18n("How long Undo is offered after sending."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeUndoSend(row.selected()));
                                        },
                                    },
                                },

                                #[name = "spelling_group"]
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Spelling"),
                                    // The description is filled in at init with the
                                    // dictionaries the app can actually see — checking a
                                    // language without one silently checks nothing, so
                                    // honesty about what is installed beats silence.

                                    #[name = "spellcheck_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Check spelling as you type"),
                                        set_subtitle: &i18n("Right-click a word for corrections."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetSpellcheck(row.is_active()));
                                        },
                                    },

                                    // One switch per installed dictionary, filled
                                    // in at init (#365); the subtitle names the
                                    // languages in use.
                                    #[name = "spell_lang_row"]
                                    adw::ExpanderRow {
                                        set_title: &i18n("Languages"),
                                    },

                                    // Words the user taught the checker ("Learn Spelling"
                                    // in the composer, or added right here). Populated
                                    // and maintained imperatively in init.
                                    #[name = "spell_words_row"]
                                    adw::ExpanderRow {
                                        set_title: &i18n("Added words"),
                                        set_subtitle: &i18n("Changes apply after a restart."),
                                    },
                                },
                            },

                            add_named[Some("privacy")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Privacy"),
                                    set_description: Some(
                                        i18n("Hylki collects no telemetry and sends no analytics.").as_str()
                                    ),

                                    #[name = "auto_remote_content_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Always load remote content"),
                                        set_subtitle: &i18n("Remote content can tell a sender when and where you read."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetAutoRemoteContent(row.is_active()));
                                        },
                                    },

                                    #[name = "show_remote_banner_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Warn when remote content is blocked"),
                                        set_subtitle: &i18n("Off hides the banner. Remote content stays blocked."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetShowRemoteBanner(row.is_active()));
                                        },
                                    },

                                    #[name = "show_spoof_banner_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Warn when the addressing doesn't match"),
                                        set_subtitle: &i18n("The badge beside the sender stays. A possible forgery always shows the banner."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetShowSpoofBanner(row.is_active()));
                                        },
                                    },

                                    #[name = "gravatar_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Use Gravatar when a contact has no photo"),
                                        set_subtitle: &i18n("Sends a hash of the sender's address to Gravatar."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetGravatar(row.is_active()));
                                        },
                                    },

                                    #[name = "sender_logos_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Show sender logos"),
                                        set_subtitle: &i18n("Most are fetched from the sender's domain, which learns your IP address."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetSenderLogos(row.is_active()));
                                        },
                                    },

                                },
                            },

                            add_named[Some("datetime")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Date and Time"),
                                    set_description: Some(
                                        i18n("Dates follow the system unless a format is chosen here.").as_str()
                                    ),

                                    #[name = "date_style_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Date format"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeDateStyle(row.selected()));
                                        },
                                    },

                                    #[name = "clock_style_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Clock"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeClockStyle(row.selected()));
                                        },
                                    },
                                },
                            },

                            add_named[Some("system")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("System"),

                                    #[name = "language_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Language"),
                                        set_subtitle: &i18n("Applies after a restart."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeLanguage(row.selected()));
                                        },
                                    },

                                    #[name = "renderer_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Renderer"),
                                        set_subtitle: &i18n("Try OpenGL or Software on older graphics. Applies after a restart."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeRenderer(row.selected()));
                                        },
                                    },

                                    #[name = "background_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Keep running in the background"),
                                        set_subtitle: &i18n("Closing the window hides it. Quit from Background Apps."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetRunInBackground(row.is_active()));
                                        },
                                    },

                                    #[name = "autostart_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Start at login"),
                                        set_subtitle: &i18n("Starts hidden."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetAutostart(row.is_active()));
                                        },
                                    },

                                    #[name = "tray_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Show a tray icon"),
                                        set_subtitle: &i18n("To use on GNOME, install an AppIndicator extension."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetTray(row.is_active()));
                                        },
                                    },

                                    #[name = "tray_icon_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Tray icon"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeTrayIcon(row.selected()));
                                        },
                                    },

                                    #[name = "tray_mail_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Unread mail in the tray menu"),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetTrayMail(row.is_active()));
                                        },
                                    },

                                    #[name = "launcher_badge_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Unread count on the app icon"),
                                        set_subtitle: &i18n("To use on GNOME, install a dock extension such as Dash to Dock."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeLauncherBadge(row.selected()));
                                        },
                                    },

                                    #[name = "single_key_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Single-key shortcuts"),
                                        set_subtitle: &i18n("Press Ctrl+? to list all shortcuts."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetSingleKey(row.is_active()));
                                        },
                                    },

                                    #[name = "gmail_keys_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Gmail keys"),
                                        set_subtitle: &i18n("Swaps the single-key shortcuts for Gmail equivalents."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetGmailKeys(row.is_active()));
                                        },
                                    },

                                    #[name = "status_bar_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Status bar"),
                                        connect_selected_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetStatusBarTop(row.selected() == 1));
                                        },
                                    },

                                    #[name = "console_mode_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Console mode"),
                                        set_subtitle: &i18n("Shows a console in the status bar."),
                                        connect_active_notify[sender] => move |row| {
                                            let _ = sender.output(PrefOutput::SetConsoleMode(row.is_active()));
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: &i18n("Export log"),
                                        set_subtitle: &i18n("For bug reports. Addresses are cut to their domain for privacy."),
                                        set_activatable: true,
                                        connect_activated => PrefInput::ExportLog,
                                        add_suffix = &gtk::Image {
                                            set_icon_name: Some("go-next-symbolic"),
                                        },
                                    },
                                },

                                // What downloaded mail takes up, and how long it
                                // stays (#394).
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Storage"),

                                    adw::ActionRow {
                                        set_title: &i18n("Downloaded mail"),
                                        #[watch]
                                        set_subtitle: &storage_text(model.storage),
                                        add_suffix = &gtk::Button {
                                            set_label: &i18n("Remove…"),
                                            set_valign: gtk::Align::Center,
                                            add_css_class: "destructive-action",
                                            #[watch]
                                            set_sensitive: model.storage.is_some_and(|(b, f, _)| b + f > 0),
                                            connect_clicked => PrefInput::RemoveDownloads,
                                        },
                                    },

                                    #[name = "keep_downloads_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Keep downloaded mail"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeKeepDownloads(row.selected()));
                                        },
                                    },
                                },

                                // The Files right-click extension (#188).
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("GNOME Files Integration"),
                                    set_description: Some(
                                        i18n("Adds “Send with Hylki” to the right-click menu in Files. Needs the nautilus-python package and a restart of Files.").as_str()
                                    ),

                                    adw::ActionRow {
                                        set_title: &i18n("Right-click menu entry"),
                                        #[watch]
                                        set_subtitle: &nautilus_status_text(&model.nautilus),
                                        add_suffix = &gtk::Button {
                                            set_label: &i18n("Remove"),
                                            set_valign: gtk::Align::Center,
                                            #[watch]
                                            set_visible: model.nautilus.status != crate::nautilus_ext::Status::NotInstalled,
                                            connect_clicked => PrefInput::NautilusRemove,
                                        },
                                        add_suffix = &gtk::Button {
                                            #[watch]
                                            set_label: &if model.nautilus.status == crate::nautilus_ext::Status::Outdated {
                                                i18n("Update")
                                            } else {
                                                i18n("Install")
                                            },
                                            set_valign: gtk::Align::Center,
                                            add_css_class: "suggested-action",
                                            #[watch]
                                            set_visible: model.nautilus.status != crate::nautilus_ext::Status::Installed,
                                            connect_clicked => PrefInput::NautilusInstall,
                                        },
                                    },

                                    // The loader's install command, ready to paste, while
                                    // there is no sign Files can load the extension.
                                    #[name = "nautilus_cmd_row"]
                                    adw::EntryRow {
                                        set_title: &i18n("Install nautilus-python first"),
                                        set_text: model.nautilus_cmd.unwrap_or_default(),
                                        set_editable: false,
                                        add_css_class: "monospace",
                                        #[watch]
                                        set_visible: model.nautilus_cmd.is_some()
                                            && !model.nautilus.loaded
                                            && model.nautilus.loader != Some(true),
                                        add_suffix = &gtk::Button {
                                            set_icon_name: "edit-copy-symbolic",
                                            set_valign: gtk::Align::Center,
                                            set_tooltip_text: Some(i18n("Copy").as_str()),
                                            add_css_class: "flat",
                                            connect_clicked[cmd = model.nautilus_cmd.unwrap_or_default().to_string()] => move |b| {
                                                b.clipboard().set_text(&cmd);
                                                b.set_icon_name("verified-checkmark-symbolic");
                                                let b = b.clone();
                                                gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
                                                    b.set_icon_name("edit-copy-symbolic");
                                                });
                                            },
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: &i18n("Restart Files"),
                                        set_subtitle: &i18n("Closes every Files window, like “nautilus -q”."),
                                        add_suffix = &gtk::Button {
                                            set_label: &i18n("Restart"),
                                            set_valign: gtk::Align::Center,
                                            connect_clicked => PrefInput::NautilusRestartFiles,
                                        },
                                    },

                                    // What the handed-in files open into, and
                                    // what happens when they are big.
                                    #[name = "files_action_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Default Send with Hylki behavior"),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFilesAction(row.selected()));
                                        },
                                    },

                                    #[name = "files_large_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Default large attachments behavior"),
                                        set_subtitle: &i18n("Without a cloud storage account, files are attached."),
                                        connect_selected_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFilesLarge(row.selected()));
                                        },
                                    },

                                    #[name = "files_limit_row"]
                                    adw::SpinRow {
                                        set_title: &i18n("Size limit"),
                                        set_subtitle: &i18n("In MB, for all the files together."),
                                        set_adjustment: Some(&gtk::Adjustment::new(20.0, 1.0, 5000.0, 1.0, 10.0, 0.0)),
                                        set_numeric: true,
                                        connect_value_notify[sender] => move |row| {
                                            sender.input(PrefInput::ChangeFilesLimit(row.value().round() as u32));
                                        },
                                    },
                                },

                                // Where a link in a message goes (#232).
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Links"),
                                    set_description: Some(link_group_description(&model.browsers).as_str()),

                                    // Its handler is connected after the list
                                    // is filled below, not here: giving a
                                    // ComboRow its model moves the selection
                                    // to the first item, and that notification
                                    // would read as the user choosing
                                    // "System default".
                                    #[name = "link_browser_row"]
                                    adw::ComboRow {
                                        set_title: &i18n("Open links in"),
                                    },
                                },
                            },

                            add_named[Some("backup")] = &adw::PreferencesPage {
                                add = &adw::PreferencesGroup {
                                    set_title: &i18n("Backup"),
                                    set_description: Some(
                                        i18n("Accounts, preferences, filters, tags and layout, in one file. Passwords and OpenPGP keys are not included.").as_str()
                                    ),

                                    adw::ActionRow {
                                        set_title: &i18n("Export settings"),
                                        set_activatable: true,
                                        connect_activated => PrefInput::ExportSettings,
                                        add_suffix = &gtk::Image {
                                            set_icon_name: Some("go-next-symbolic"),
                                        },
                                    },

                                    adw::ActionRow {
                                        set_title: &i18n("Import settings"),
                                        set_subtitle: &i18n("Replaces everything in place. Don't remove accounts first: that deletes their passwords."),
                                        set_activatable: true,
                                        connect_activated => PrefInput::ImportSettings,
                                        add_suffix = &gtk::Image {
                                            set_icon_name: Some("go-next-symbolic"),
                                        },
                                    },
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
        let mut model = Preferences {
            storage: None,
            nautilus: crate::nautilus_ext::State::read(),
            nautilus_cmd: crate::platform::nautilus_python_install_command(),
            files: init.files,
            files_rows: None,
            spell_langs: None,
            browsers: crate::ui::launch::browsers(),
            notifications: init.notifications,
            notification_buttons: init.notification_buttons,
            sound: crate::config::load_new_mail_sound(),
            custom_sound: crate::config::custom_sound(),
            toolbar: init.reader_toolbar.clone(),
            focus: init.focus,
            toolbar_editor: None,
            list_columns: init.list_columns.clone(),
            column_editor: None,
            columns_group: None,
            list_single_possible: init.list_layout != crate::config::ListLayout::Cards,
            show_unified: init.show_unified,
            unified_kinds: init.unified_kinds,
            unified_chips: init.unified_chips,
            print_options: init.print_options,
            show_accounts: init.show_accounts,
            rail_fold: init.rail_fold,
            swipe_enabled: init.swipe_enabled,
            threading: init.threading,
            thread_expansion: init.thread_expansion,
            list_palette: init.list_palette,
            card_actions_hover: init.card_actions_hover,
            avatars: init.avatars,
            panels_stack: None,
            accounts_slot: None,
            deferred_pages: std::cell::RefCell::new(Vec::new()),
            side_list: None,
            search: None,
            content_page: None,
            split: None,
            accounts_sender: init.accounts_sender.clone(),
            host_header: None,
            contacts_placement_row: None,
            pgp_keys: None,
            identities: init.identities.clone(),
            cloud: None,
            contacts: None,
            contacts_open: false,
            editor_open: false,
            editor_page: "accounts",
        };

        // The view's handlers report through `ViewSender`, silent until the
        // rows below hold their saved values.
        let component_sender = sender;
        let sender = ViewSender {
            inner: component_sender.clone(),
            ready: Rc::new(std::cell::Cell::new(false)),
        };
        let widgets = view_output!();
        let ready = sender.ready.clone();
        let sender = component_sender;
        tracing::debug!(
            "settings window: preferences view built in {:?}",
            t_init.elapsed()
        );

        model.make_combos_whole(&widgets);
        model.init_general_rows(&widgets, &init);
        model.init_notification_rows(&widgets, &init);
        model.init_sidebar_rows(&widgets, &init);
        model.init_list_rows(&widgets, &init);
        model.init_background_and_galleries(&widgets, &init, &sender);
        tracing::debug!(
            "settings window: preferences icon strip done at {:?}",
            t_init.elapsed()
        );

        model.init_tray_rows(&widgets, &init);
        tracing::debug!(
            "settings window: prefs tail A (tray) at {:?}",
            t_init.elapsed()
        );
        model.init_keyboard_and_system_rows(&widgets, &init);
        model.init_conversation_and_reading_rows(&widgets, &init);
        model.init_files_and_links_rows(&widgets, &init, &sender);
        model.init_composing_rows(&widgets, &init, &sender);
        tracing::debug!(
            "settings window: prefs tail B (words) at {:?}",
            t_init.elapsed()
        );

        model.init_date_and_theme_rows(&widgets, &init);
        tracing::debug!(
            "settings window: prefs tail C (themes) at {:?}",
            t_init.elapsed()
        );

        model.init_reader_fonts_and_gestures(&widgets, &init);
        model.mount_hosted_pages(&widgets, &init, &sender);
        tracing::debug!(
            "settings window: prefs tail D (before sidebar rows) at {:?}",
            t_init.elapsed()
        );
        model.init_side_list_and_editors(&widgets, &sender, &root);
        tracing::debug!(
            "settings window: prefs tail E (sidebar rows built) at {:?}",
            t_init.elapsed()
        );
        model.init_search_and_first_page(&widgets, &init, &sender, &root);

        tracing::debug!("settings window: preferences init {:?}", t_init.elapsed());
        // From here on a row's signal is the user's doing.
        ready.set(true);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            PrefInput::ChangeDateStyle(i) => self.on_change_date_style(i, sender),
            PrefInput::ChangeClockStyle(i) => self.on_change_clock_style(i, sender),
            PrefInput::ChangeRenderer(i) => self.on_change_renderer(i, sender),
            PrefInput::ChangeLanguage(i) => self.on_change_language(i, sender),
            PrefInput::NautilusInstall => self.on_nautilus_install(root),
            PrefInput::MeasureStorage => self.on_measure_storage(sender),
            PrefInput::StorageMeasured {
                bodies,
                files,
                total,
            } => self.on_storage_measured(bodies, files, total),
            PrefInput::RemoveDownloads => self.on_remove_downloads(sender, root),
            PrefInput::RemoveDownloadsNow => self.on_remove_downloads_now(sender),
            PrefInput::ChangeKeepDownloads(idx) => self.on_change_keep_downloads(idx, sender),
            PrefInput::NautilusRemove => self.on_nautilus_remove(root),
            PrefInput::ChangeFilesAction(idx) => self.on_change_files_action(idx, sender),
            PrefInput::ChangeFilesLarge(idx) => self.on_change_files_large(idx, sender),
            PrefInput::ChangeLinkBrowser(idx) => self.on_change_link_browser(idx, sender),
            PrefInput::ChangeFilesLimit(mb) => self.on_change_files_limit(mb, sender),
            PrefInput::SetFilesPrefs(p) => self.on_set_files_prefs(p),
            PrefInput::SetSpellcheckLangs(langs) => self.on_set_spellcheck_langs(langs),
            PrefInput::NautilusRestartFiles => self.on_nautilus_restart_files(root),
            PrefInput::NautilusRefresh => {
                self.nautilus = crate::nautilus_ext::State::read();
            }
            PrefInput::ToggleAvatars(on) => self.on_toggle_avatars(on, sender),
            PrefInput::ToggleThreading(on) => self.on_toggle_threading(on, sender),
            PrefInput::ToggleThreadExpansion(on) => self.on_toggle_thread_expansion(on, sender),
            PrefInput::ChangeReaderDefault(idx) => self.on_change_reader_default(idx, sender),
            PrefInput::ChangeReaderZoom(idx) => self.on_change_reader_zoom(idx, sender),
            PrefInput::ChangeCardActionsMode(index) => {
                self.on_change_card_actions_mode(index, sender)
            }
            PrefInput::ToggleListPalette(on) => self.on_toggle_list_palette(on, sender),
            PrefInput::ToggleSwipeEnabled(on) => self.on_toggle_swipe_enabled(on, sender),
            PrefInput::ChangeComposeDefaultFrom(index) => {
                self.on_change_compose_default_from(index, sender)
            }
            PrefInput::ChangeFetchInterval(index) => self.on_change_fetch_interval(index, sender),
            PrefInput::ToggleNotifications(on) => self.on_toggle_notifications(on, sender),
            PrefInput::ToggleNotificationButton(button, on) => {
                self.on_toggle_notification_button(button, on, sender)
            }
            PrefInput::ChooseSound => self.on_choose_sound(sender, root),
            PrefInput::SoundChosen(path) => self.on_sound_chosen(path),
            PrefInput::PlaySound => self.on_play_sound(),
            PrefInput::ToggleSound(on) => self.on_toggle_sound(on),
            PrefInput::ToggleSoundFocused(on) => self.on_toggle_sound_focused(on),
            PrefInput::ChangeSound(index) => self.on_change_sound(index, sender),
            PrefInput::ToggleShowUnified(on) => self.on_toggle_show_unified(on, sender),
            PrefInput::TogglePrintPageNumbers(on) => self.on_toggle_print_page_numbers(on, sender),
            PrefInput::TogglePrintDate(on) => self.on_toggle_print_date(on, sender),
            PrefInput::TogglePrintPunchMargin(on) => self.on_toggle_print_punch_margin(on, sender),
            PrefInput::ToggleUnifiedChipAllInboxes(on) => {
                self.on_toggle_unified_chip_all_inboxes(on, sender)
            }
            PrefInput::ToggleUnifiedChipStarred(on) => {
                self.on_toggle_unified_chip_starred(on, sender)
            }
            PrefInput::ToggleUnifiedChipDrafts(on) => {
                self.on_toggle_unified_chip_drafts(on, sender)
            }
            PrefInput::ToggleUnifiedChipArchive(on) => {
                self.on_toggle_unified_chip_archive(on, sender)
            }
            PrefInput::ToggleUnifiedChipJunk(on) => self.on_toggle_unified_chip_junk(on, sender),
            PrefInput::ToggleUnifiedChipTrash(on) => self.on_toggle_unified_chip_trash(on, sender),
            PrefInput::ToggleUnifiedStarred(on) => self.on_toggle_unified_starred(on, sender),
            PrefInput::ToggleUnifiedSent(on) => self.on_toggle_unified_sent(on, sender),
            PrefInput::ToggleUnifiedDrafts(on) => self.on_toggle_unified_drafts(on, sender),
            PrefInput::ToggleUnifiedArchive(on) => self.on_toggle_unified_archive(on, sender),
            PrefInput::ToggleUnifiedJunk(on) => self.on_toggle_unified_junk(on, sender),
            PrefInput::ToggleUnifiedTrash(on) => self.on_toggle_unified_trash(on, sender),
            PrefInput::MountPages => self.mount_pages(),
            PrefInput::Search(text) => self.on_search(text),
            PrefInput::SearchPick(i) => self.on_search_pick(i, sender),
            PrefInput::ToggleSearch => self.on_toggle_search(),
            PrefInput::SetAccountsPanel {
                panel,
                sender: accounts,
            } => self.on_set_accounts_panel(panel, accounts),
            PrefInput::ToggleShowAccounts(on) => self.on_toggle_show_accounts(on, sender),
            PrefInput::ToggleFocus(part, on) => self.on_toggle_focus(part, on, sender),
            PrefInput::SetFocusMode(focus) => {
                self.focus = focus;
            }
            PrefInput::SetShowAccounts(on) => {
                self.show_accounts = on;
            }
            PrefInput::ChangeSectionStyle(idx) => self.on_change_section_style(idx, sender),
            PrefInput::ChangeFilteredPlacement(idx) => {
                let _ = sender.output(PrefOutput::SetFilteredPlacement(placement_from_index(idx)));
            }
            PrefInput::ChangeTagsPlacement(idx) => {
                let _ = sender.output(PrefOutput::SetTagsPlacement(placement_from_index(idx)));
            }
            PrefInput::ChangeFavoritesPlacement(idx) => {
                let _ = sender.output(PrefOutput::SetFavoritesPlacement(placement_from_index(idx)));
            }
            PrefInput::ChangeContactsPlacement(idx) => {
                self.on_change_contacts_placement(idx, sender)
            }
            PrefInput::ChangeStartView(idx) => self.on_change_start_view(idx, sender),
            PrefInput::ChangeFolderSort(idx) => self.on_change_folder_sort(idx, sender),
            PrefInput::ChangeChevronSide(idx) => {
                let _ = sender.output(PrefOutput::SetChevronsLeft(idx == 0));
            }
            PrefInput::ToolbarDrop { key, side, index } => {
                self.on_toolbar_drop(key, side, index, sender)
            }
            PrefInput::ToolbarGapPreview { zone, index } => {
                self.on_toolbar_gap_preview(zone, index)
            }
            PrefInput::ColumnDrop { key, shown, index } => {
                self.on_column_drop(key, shown, index, sender)
            }
            PrefInput::ColumnRestore => self.on_column_restore(sender),
            PrefInput::ToolbarRestore => self.on_toolbar_restore(sender),
            PrefInput::ToggleRailFoldEnabled(on) => self.on_toggle_rail_fold_enabled(on, sender),
            PrefInput::ToggleRailFoldAccounts(on) => self.on_toggle_rail_fold_accounts(on, sender),
            PrefInput::ToggleRailFoldAllInboxes(on) => {
                self.on_toggle_rail_fold_all_inboxes(on, sender)
            }
            PrefInput::ToggleRailFoldStarred(on) => self.on_toggle_rail_fold_starred(on, sender),
            PrefInput::ToggleRailFoldSent(on) => self.on_toggle_rail_fold_sent(on, sender),
            PrefInput::ToggleRailFoldDrafts(on) => self.on_toggle_rail_fold_drafts(on, sender),
            PrefInput::ToggleRailFoldArchive(on) => self.on_toggle_rail_fold_archive(on, sender),
            PrefInput::ToggleRailFoldJunk(on) => self.on_toggle_rail_fold_junk(on, sender),
            PrefInput::ToggleRailFoldTrash(on) => self.on_toggle_rail_fold_trash(on, sender),
            PrefInput::ToggleRailFoldFiltered(on) => self.on_toggle_rail_fold_filtered(on, sender),
            PrefInput::ToggleRailFoldTags(on) => self.on_toggle_rail_fold_tags(on, sender),
            PrefInput::ChangeReadMark(idx) => self.on_change_read_mark(idx, sender),
            PrefInput::ExportLog => {
                let _ = sender.output(PrefOutput::ExportLog);
            }
            PrefInput::SetContactBooks(books) => self.on_set_contact_books(books),
            PrefInput::SetLocalContactCount(n) => self.on_set_local_contact_count(n),
            PrefInput::ExportSettings => {
                let _ = sender.output(PrefOutput::ExportSettings);
            }
            PrefInput::ImportSettings => {
                let _ = sender.output(PrefOutput::ImportSettings);
            }
            PrefInput::ChangeTrayIcon(index) => self.on_change_tray_icon(index, sender),
            PrefInput::ChangeLauncherBadge(index) => self.on_change_launcher_badge(index, sender),
            PrefInput::ChangeListLayout(index) => self.on_change_list_layout(index, sender),
            PrefInput::ChangePreviewLines(index) => self.on_change_preview_lines(index, sender),
            PrefInput::ChangeAppTheme(index) => self.on_change_app_theme(index, sender),
            PrefInput::ChangeTextScale(index) => self.on_change_text_scale(index, sender),
            PrefInput::ChangeSettingsOpen(index) => {
                let _ = sender.output(PrefOutput::SetSettingsOpenAccounts(index == 1));
            }
            PrefInput::ShowAccounts(accounts) => {
                self.select_row(if accounts { "accounts" } else { "general" });
            }
            PrefInput::SelectPage(id) => self.on_select_page(id, sender),
            PrefInput::ShowPageById(id) => self.select_row(&id),
            PrefInput::RevealColumns => self.on_reveal_columns(),
            PrefInput::LeaveEditorPrompt(id) => self.ask_to_leave_editor(&id, &sender),
            PrefInput::LeaveEditorTo(id) => self.on_leave_editor_to(id, sender),
            PrefInput::EditorOpen(page) => self.on_editor_open(page),
            PrefInput::CloudEditorOpen(open) => self.on_cloud_editor_open(open),
            PrefInput::Contacts(out) => self.on_contacts(out, sender),
            PrefInput::ChangeMessageTheme(index) => self.on_change_message_theme(index, sender),
            PrefInput::ChangeComposeFormat(idx) => self.on_change_compose_format(idx, sender),
            PrefInput::ChangeFoldMessages(idx) => self.on_change_fold_messages(idx, sender),
            PrefInput::ChangeReplyPosition(idx) => self.on_change_reply_position(idx, sender),
            PrefInput::ChangeUndoSend(idx) => self.on_change_undo_send(idx, sender),
            PrefInput::ChangeSignaturePosition(idx) => {
                self.on_change_signature_position(idx, sender)
            }
        }
    }
}

thread_local! {
    /// Whether Settings → Sidebar's Unread counts and Fold up expanded
    /// items rows are open, for this run of the app only.
    static SESSION_OPEN: std::cell::Cell<[bool; 2]> = const { std::cell::Cell::new([false; 2]) };
}
