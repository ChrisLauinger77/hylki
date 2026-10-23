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

        // Settings never truncates. AdwComboRow's DEFAULT item factory builds
        // the selected-value display with an ellipsizing label — and rebuilds
        // it on every selection change, so fixing the widget after the fact
        // doesn't stick ("Follow system" → "Follow s…"). Give every combo a
        // plain factory whose labels never ellipsize; the short row titles
        // yield the space instead.
        fn no_truncate(row: &adw::ComboRow) {
            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(|_, item| {
                if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                    let label = gtk::Label::new(None);
                    label.set_xalign(0.0);
                    item.set_child(Some(&label));
                }
            });
            factory.connect_bind(|_, item| {
                let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                if let (Some(label), Some(s)) = (
                    item.child().and_downcast::<gtk::Label>(),
                    item.item().and_downcast::<gtk::StringObject>(),
                ) {
                    label.set_label(&s.string());
                }
            });
            row.set_factory(Some(&factory));
        }
        for row in [
            &widgets.fetch_row,
            &widgets.preview_lines_row,
            &widgets.message_theme_row,
            &widgets.card_actions_row,
            &widgets.app_theme_row,
            &widgets.text_scale_row,
            &widgets.settings_open_row,
            &widgets.tray_icon_row,
            &widgets.launcher_badge_row,
            &widgets.date_style_row,
            &widgets.clock_style_row,
            &widgets.language_row,
            &widgets.renderer_row,
            &widgets.chevron_side_row,
            &widgets.section_style_row,
            &widgets.filtered_placement_row,
            &widgets.tags_placement_row,
            &widgets.favorites_placement_row,
            &widgets.contacts_placement_row,
            &widgets.start_view_row,
            &widgets.folder_sort_row,
        ] {
            no_truncate(row);
        }

        // The install-command field is read-only: the row's pencil, which
        // says "type here", would be a lie.
        hide_edit_icon(widgets.nautilus_cmd_row.upcast_ref());

        // Language combo: the system's, then every catalogue shipped.
        let choices = language_choices();
        let lang_labels: Vec<&str> = choices.iter().map(|(l, _)| l.as_str()).collect();
        widgets
            .language_row
            .set_model(Some(&gtk::StringList::new(&lang_labels)));
        widgets.language_row.set_selected(
            choices
                .iter()
                .position(|(_, c)| *c == init.language)
                .unwrap_or(0) as u32,
        );

        widgets.renderer_row.set_model(Some(&gtk::StringList::new(&[
            &i18n("Automatic"),
            "Vulkan",
            "OpenGL",
            &i18n("Software"),
        ])));
        let renderer = crate::config::load_renderer();
        widgets
            .renderer_row
            .set_selected(RENDERERS.iter().position(|r| *r == renderer).unwrap_or(0) as u32);

        widgets
            .auto_remote_content_row
            .set_active(init.auto_remote_content);
        widgets
            .show_remote_banner_row
            .set_active(init.show_remote_banner);
        widgets
            .show_spoof_banner_row
            .set_active(init.show_spoof_banner);
        widgets.gravatar_row.set_active(init.gravatar);
        widgets.avatars_row.set_active(init.avatars);
        widgets
            .own_mailbox_face_row
            .set_active(init.own_mailbox_face);
        widgets.tag_row_tint_row.set_active(init.tag_row_tint);
        widgets.sender_logos_row.set_active(init.sender_logos);

        // Mail-check interval combo.
        let labels_owned: Vec<String> = FETCH_INTERVALS.iter().map(|(l, _)| i18n(l)).collect();
        let labels: Vec<&str> = labels_owned.iter().map(String::as_str).collect();
        widgets
            .fetch_row
            .set_model(Some(&gtk::StringList::new(&labels)));
        let selected = FETCH_INTERVALS
            .iter()
            .position(|(_, secs)| *secs == init.fetch_interval_secs)
            .unwrap_or(0);
        widgets.fetch_row.set_selected(selected as u32);
        widgets.push_row.set_active(init.push);
        widgets.metered_row.set_active(init.metered_save_data);
        widgets.notifications_row.set_active(init.notifications);
        widgets
            .notification_content_row
            .set_active(init.notification_content);
        // Set before the handlers can act on it: the model already holds
        // these values, so the echo is ignored rather than played.
        widgets.sound_row.set_active(model.sound.enabled);
        let labels_owned: Vec<String> = sound_labels();
        let labels: Vec<&str> = labels_owned.iter().map(String::as_str).collect();
        widgets
            .sound_choice_row
            .set_model(Some(&gtk::StringList::new(&labels)));
        widgets
            .sound_choice_row
            .set_selected(sound_index(&model.sound.sound));
        widgets
            .sound_focused_row
            .set_active(model.sound.when_focused);
        widgets
            .notify_mark_read_row
            .set_active(init.notification_buttons.mark_read);
        widgets
            .notify_archive_row
            .set_active(init.notification_buttons.archive);
        widgets
            .notify_delete_row
            .set_active(init.notification_buttons.delete);
        widgets
            .notify_reply_row
            .set_active(init.notification_buttons.reply);
        widgets
            .notify_forward_row
            .set_active(init.notification_buttons.forward);
        widgets
            .notify_spam_row
            .set_active(init.notification_buttons.spam);
        widgets
            .show_attachments_row
            .set_active(init.show_attachments);
        widgets.show_unified_row.set_active(init.show_unified);
        widgets
            .unified_chip_all_inboxes_row
            .set_active(init.unified_chips.all_inboxes);
        widgets
            .unified_chip_starred_row
            .set_active(init.unified_chips.starred);
        widgets
            .unified_chip_drafts_row
            .set_active(init.unified_chips.drafts);
        widgets
            .unified_chip_archive_row
            .set_active(init.unified_chips.archive);
        widgets
            .unified_chip_junk_row
            .set_active(init.unified_chips.junk);
        widgets
            .unified_chip_trash_row
            .set_active(init.unified_chips.trash);
        widgets
            .unified_starred_row
            .set_active(init.unified_kinds.starred);
        widgets.unified_sent_row.set_active(init.unified_kinds.sent);
        widgets
            .unified_drafts_row
            .set_active(init.unified_kinds.drafts);
        widgets
            .unified_archive_row
            .set_active(init.unified_kinds.archive);
        widgets.unified_junk_row.set_active(init.unified_kinds.junk);
        widgets
            .unified_trash_row
            .set_active(init.unified_kinds.trash);
        widgets
            .section_style_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Unified style").as_str(),
                i18n("Tree style").as_str(),
            ])));
        widgets.section_style_row.set_selected(u32::from(
            init.section_style == crate::config::SectionStyle::Tree,
        ));
        for (row, placement) in [
            (&widgets.filtered_placement_row, init.filtered_placement),
            (&widgets.tags_placement_row, init.tags_placement),
            (&widgets.favorites_placement_row, init.favorites_placement),
        ] {
            row.set_model(Some(&gtk::StringList::new(&[
                i18n("Above the accounts").as_str(),
                i18n("Below the accounts").as_str(),
                i18n("Off").as_str(),
            ])));
            row.set_selected(placement_index(placement));
        }
        widgets
            .contacts_placement_row
            .set_model(Some(&contacts_placement_model()));
        widgets
            .contacts_placement_row
            .set_selected(contacts_placement_index(init.contacts_placement));
        widgets
            .start_view_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("All Inboxes").as_str(),
                i18n("The last account's inbox").as_str(),
                i18n("The last folder").as_str(),
            ])));
        widgets.start_view_row.set_selected(match init.start_view {
            crate::config::StartView::AllInboxes => 0,
            crate::config::StartView::AccountInbox => 1,
            crate::config::StartView::LastFolder => 2,
        });
        let sort_labels: Vec<String> = crate::config::FolderSort::ALL
            .iter()
            .map(|s| s.label())
            .collect();
        let sort_labels: Vec<&str> = sort_labels.iter().map(String::as_str).collect();
        widgets
            .folder_sort_row
            .set_model(Some(&gtk::StringList::new(&sort_labels)));
        widgets
            .folder_sort_row
            .set_selected(init.folder_sort.index());
        widgets
            .chevron_side_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Left").as_str(),
                i18n("Right").as_str(),
            ])));
        widgets
            .chevron_side_row
            .set_selected(if init.chevrons_left { 0 } else { 1 });
        widgets
            .account_click_row
            .set_active(init.account_click_folds);
        widgets
            .sidebar_hover_expand_row
            .set_active(init.sidebar_hover_expand);
        widgets
            .remember_sidebar_row
            .set_active(init.remember_sidebar);
        widgets.remember_rail_row.set_active(init.remember_rail);
        widgets.rail_dots_row.set_active(init.rail_dots);
        widgets.seen_counts_row.set_active(init.seen_counts);
        widgets
            .rail_fold_row
            .set_enable_expansion(init.rail_fold.enabled);
        // Unread counts and Fold up expanded items start folded each
        // launch and stay as they are left for the rest of the session,
        // a rebuilt Settings window included. Switching expansion on opens
        // a row, so its state is set after that.
        for (row, i) in [(&widgets.unified_chips_row, 0), (&widgets.rail_fold_row, 1)] {
            row.set_expanded(SESSION_OPEN.with(|o| o.get()[i]));
            row.connect_expanded_notify(move |row| {
                SESSION_OPEN.with(|o| {
                    let mut open = o.get();
                    open[i] = row.is_expanded();
                    o.set(open);
                });
            });
        }
        widgets
            .rail_fold_accounts_row
            .set_active(init.rail_fold.accounts);
        widgets
            .rail_fold_all_inboxes_row
            .set_active(init.rail_fold.all_inboxes);
        widgets
            .rail_fold_starred_row
            .set_active(init.rail_fold.starred);
        widgets.rail_fold_sent_row.set_active(init.rail_fold.sent);
        widgets
            .rail_fold_drafts_row
            .set_active(init.rail_fold.drafts);
        widgets
            .rail_fold_archive_row
            .set_active(init.rail_fold.archive);
        widgets.rail_fold_junk_row.set_active(init.rail_fold.junk);
        widgets.rail_fold_trash_row.set_active(init.rail_fold.trash);
        widgets
            .rail_fold_filtered_row
            .set_active(init.rail_fold.filtered);
        widgets.rail_fold_tags_row.set_active(init.rail_fold.tags);
        let preview_labels_owned = [
            i18n("Off"),
            i18n("1 line"),
            i18n("2 lines"),
            i18n("3 lines"),
        ];
        let preview_labels: Vec<&str> = preview_labels_owned.iter().map(String::as_str).collect();
        widgets
            .preview_lines_row
            .set_model(Some(&gtk::StringList::new(&preview_labels)));
        widgets
            .preview_lines_row
            .set_selected(init.preview_lines.min(3));
        widgets
            .list_layout_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Cards"),
                &i18n("Single line"),
                &i18n("Automatic"),
            ])));
        no_truncate(&widgets.list_layout_row);
        widgets.list_headings_row.set_active(init.list_headings);
        widgets.list_fit_row.set_active(!init.list_column_scroll);
        widgets
            .group_by_date_row
            .set_active(init.list_group_by_date);
        widgets.pills_below_row.set_active(init.list_pills_below);
        widgets
            .list_search_open_row
            .set_active(init.list_search_open);
        for (row, bottom) in [
            (&widgets.list_search_place_row, init.list_search_bottom),
            (&widgets.list_bulk_place_row, init.list_bulk_bar_bottom),
        ] {
            row.set_model(Some(&gtk::StringList::new(&[
                &i18n("Top of the list"),
                &i18n("Bottom of the list"),
            ])));
            row.set_selected(u32::from(bottom));
            no_truncate(row);
        }
        widgets
            .list_layout_row
            .set_selected(match init.list_layout {
                crate::config::ListLayout::Cards => 0,
                crate::config::ListLayout::SingleLine => 1,
                crate::config::ListLayout::Automatic => 2,
            });

        widgets.background_row.set_active(init.run_in_background);
        widgets.autostart_row.set_active(init.autostart);
        // Starting at login only means anything if Hylki stays running.
        widgets.autostart_row.set_sensitive(init.run_in_background);
        {
            let autostart_row = widgets.autostart_row.clone();
            widgets.background_row.connect_active_notify(move |row| {
                autostart_row.set_sensitive(row.is_active());
            });
        }
        // Theme: title in the row's own voice, the gallery beneath — the
        // same shape as the app-icon row under it. A pick goes straight
        // out and is painted at once.
        {
            let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
            body.set_margin_top(12);
            body.set_margin_bottom(12);
            body.set_margin_start(12);
            body.set_margin_end(12);
            let title = gtk::Label::new(Some(i18n("Theme").as_str()));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            body.append(&title);
            let s = sender.clone();
            let gallery = crate::ui::theme_picker::gallery(
                &init.theme,
                std::rc::Rc::new(move |id: &str| {
                    let _ = s.output(PrefOutput::SetTheme(id.to_string()));
                }),
            );
            gallery.set_margin_top(10);
            body.append(&gallery);
            widgets.theme_row.set_child(Some(&body));
        }

        // App icon: title in the row's own voice, the gallery beneath.
        // Picks go straight out; the app applies and offers the restart.
        {
            let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
            body.set_margin_top(12);
            body.set_margin_bottom(8);
            body.set_margin_start(12);
            body.set_margin_end(12);
            let title = gtk::Label::new(Some(i18n("App icon").as_str()));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            body.append(&title);
            // An icon set outside Hylki stays until a pick here (#252): say
            // so, and ring no tile, since none of them is what shows.
            let custom = crate::app_icon::custom_icon().is_some();
            let note = gtk::Label::new(Some(
                i18n("The launcher shows an icon set elsewhere. Picking one here replaces it.")
                    .as_str(),
            ));
            note.add_css_class("dim-label");
            note.add_css_class("caption");
            note.set_halign(gtk::Align::Start);
            note.set_xalign(0.0);
            note.set_wrap(true);
            note.set_visible(custom);
            body.append(&note);
            widgets.app_icon_row.set_child(Some(&body));
            // The icon gallery decodes the whole catalogue; it fills in a
            // moment after the window is up rather than holding it back.
            let s = sender.clone();
            let app_icon = if custom {
                String::new()
            } else {
                init.app_icon.clone()
            };
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
                let s = s.clone();
                let note = note.clone();
                let strip = crate::ui::icon_picker::strip(
                    &app_icon,
                    56,
                    std::rc::Rc::new(move |id: &str| {
                        note.set_visible(false);
                        let _ = s.output(PrefOutput::SetAppIcon(id.to_string()));
                    }),
                );
                strip.set_margin_top(6);
                body.append(&strip);
                gtk::glib::ControlFlow::Break
            });
        }
        tracing::debug!(
            "settings window: preferences icon strip done at {:?}",
            t_init.elapsed()
        );

        widgets.tray_row.set_active(init.tray);
        let tray_icon_labels_owned: Vec<String> = TRAY_ICONS.iter().map(|(l, _)| i18n(l)).collect();
        let tray_icon_labels: Vec<&str> =
            tray_icon_labels_owned.iter().map(String::as_str).collect();
        widgets
            .tray_icon_row
            .set_model(Some(&gtk::StringList::new(&tray_icon_labels)));
        let tray_icon_sel = TRAY_ICONS
            .iter()
            .position(|(_, t)| *t == init.tray_icon)
            .unwrap_or(0);
        widgets.tray_icon_row.set_selected(tray_icon_sel as u32);
        widgets.tray_mail_row.set_active(init.tray_mail);
        let badge_labels: Vec<String> = LAUNCHER_BADGES.iter().map(|(l, _)| i18n(l)).collect();
        let badge_labels: Vec<&str> = badge_labels.iter().map(String::as_str).collect();
        widgets
            .launcher_badge_row
            .set_model(Some(&gtk::StringList::new(&badge_labels)));
        let badge_sel = LAUNCHER_BADGES
            .iter()
            .position(|(_, b)| *b == init.launcher_badge)
            .unwrap_or(1);
        widgets.launcher_badge_row.set_selected(badge_sel as u32);
        // The icon choice and the menu's mail list only mean anything with
        // a tray icon shown.
        widgets.tray_icon_row.set_sensitive(init.tray);
        widgets.tray_mail_row.set_sensitive(init.tray);
        {
            let tray_icon_row = widgets.tray_icon_row.clone();
            let tray_mail_row = widgets.tray_mail_row.clone();
            widgets.tray_row.connect_active_notify(move |row| {
                tray_icon_row.set_sensitive(row.is_active());
                tray_mail_row.set_sensitive(row.is_active());
            });
        }
        tracing::debug!(
            "settings window: prefs tail A (tray) at {:?}",
            t_init.elapsed()
        );
        widgets.single_key_row.set_active(init.single_key_shortcuts);
        widgets.gmail_keys_row.set_active(init.gmail_keys);
        // Gmail's keys are a kind of single-key shortcut, so they mean
        // nothing with those off (#392): the switch keeps its state but
        // greys out, as the tray's own options do.
        widgets
            .single_key_row
            .bind_property("active", &widgets.gmail_keys_row, "sensitive")
            .sync_create()
            .build();
        widgets
            .keep_downloads_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Always"),
                &i18n("A year"),
                &i18n("90 days"),
                &i18n("30 days"),
            ])));
        widgets.keep_downloads_row.set_selected(
            KEEP_DOWNLOADS_DAYS
                .iter()
                .position(|d| *d == init.keep_downloads_days)
                .unwrap_or(0) as u32,
        );
        widgets.console_mode_row.set_active(init.console_mode);
        widgets
            .status_bar_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Bottom of the window"),
                &i18n("Top of the window"),
            ])));
        widgets
            .status_bar_row
            .set_selected(u32::from(init.status_bar_top));
        widgets
            .read_mark_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("When displayed").as_str(),
                i18n("After two seconds").as_str(),
                i18n("Manually").as_str(),
            ])));
        no_truncate(&widgets.read_mark_row);
        widgets.read_mark_row.set_selected(match init.read_mark {
            crate::config::ReadMark::Shown => 0,
            crate::config::ReadMark::Delay => 1,
            crate::config::ReadMark::Manual => 2,
        });
        widgets.threading_row.set_active(init.threading);
        widgets
            .threads_expanded_row
            .set_active(init.threads_expanded);
        widgets
            .thread_newest_first_row
            .set_active(init.thread_newest_first);
        widgets
            .fold_messages_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Never"),
                &i18n("Messages already read"),
                &i18n("All but the newest"),
            ])));
        no_truncate(&widgets.fold_messages_row);
        widgets
            .fold_messages_row
            .set_selected(match init.fold_messages {
                crate::config::FoldMessages::Never => 0,
                crate::config::FoldMessages::Read => 1,
                crate::config::FoldMessages::AllButNewest => 2,
            });
        widgets
            .always_show_recipients_row
            .set_active(init.always_show_recipients);
        widgets.pgp_labels_row.set_active(init.pgp_labels);
        widgets
            .print_page_numbers_row
            .set_active(init.print_options.page_numbers);
        widgets.print_date_row.set_active(init.print_options.date);
        widgets
            .print_punch_margin_row
            .set_active(init.print_options.punch_margin);
        widgets
            .single_message_card_row
            .set_active(init.single_message_card);
        widgets.reader_switch_row.set_active(init.reader_switch);
        widgets.theme_switch_row.set_active(init.theme_switch);
        widgets.desktop_icons_row.set_active(init.desktop_icons);
        widgets
            .reader_default_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Remember the last choice"),
                &i18n("Reader View on"),
                &i18n("Reader View off"),
            ])));
        no_truncate(&widgets.reader_default_row);
        widgets
            .reader_default_row
            .set_selected(match init.reader_default {
                crate::config::ReaderDefault::Remember => 0,
                crate::config::ReaderDefault::On => 1,
                crate::config::ReaderDefault::Off => 2,
            });
        let zoom_labels: Vec<String> = crate::config::READER_ZOOM_STEPS
            .iter()
            .map(|z| format!("{z}%"))
            .collect();
        let zoom_labels: Vec<&str> = zoom_labels.iter().map(String::as_str).collect();
        widgets
            .reader_zoom_row
            .set_model(Some(&gtk::StringList::new(&zoom_labels)));
        no_truncate(&widgets.reader_zoom_row);
        widgets.reader_zoom_row.set_selected(
            crate::config::READER_ZOOM_STEPS
                .iter()
                .position(|&z| z == init.reader_zoom)
                .unwrap_or(5) as u32,
        );
        widgets
            .card_attachments_row
            .set_active(init.card_attachments);
        widgets
            .attachment_drawer_row
            .set_active(init.attachment_drawer);
        widgets
            .thread_expansion_row
            .set_active(init.thread_expansion);
        widgets
            .thread_row_newest_row
            .set_active(init.thread_row_newest);
        widgets
            .confirm_thread_delete_row
            .set_active(init.confirm_thread_delete);
        widgets
            .card_actions_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Hidden behind a toggle").as_str(),
                i18n("Shown while hovering").as_str(),
                i18n("Always visible").as_str(),
            ])));
        widgets
            .card_actions_row
            .set_selected(if init.card_actions_hover {
                0
            } else if init.card_actions_auto {
                1
            } else {
                2
            });
        widgets.list_palette_row.set_active(init.list_palette);
        widgets
            .list_palette_hover_row
            .set_active(init.list_palette_hover);
        widgets
            .card_palette_menu_row
            .set_active(init.card_palette_menu);
        widgets.swipe_enabled_row.set_active(init.swipe_enabled);
        widgets.swipe_reversed_row.set_active(init.swipe_reversed);
        widgets.compose_inline_row.set_active(init.compose_inline);
        widgets.reply_inline_row.set_active(init.reply_inline);
        widgets.reply_fields_row.set_active(init.reply_fields);
        widgets
            .files_action_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Ask each time").as_str(),
                i18n("A new message").as_str(),
                i18n("A draft you pick").as_str(),
                i18n("A reply to a message you pick").as_str(),
            ])));
        no_truncate(&widgets.files_action_row);
        widgets
            .files_action_row
            .set_selected(files_action_index(init.files.action));
        widgets
            .files_large_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Ask each time").as_str(),
                i18n("Attach them anyway").as_str(),
                i18n("Upload to cloud storage and link them").as_str(),
            ])));
        no_truncate(&widgets.files_large_row);
        widgets
            .files_large_row
            .set_selected(files_large_index(init.files.large));
        widgets
            .files_limit_row
            .set_value(init.files.limit_mb as f64);
        model.files_rows = Some((
            widgets.files_action_row.clone(),
            widgets.files_large_row.clone(),
            widgets.files_limit_row.clone(),
        ));
        // The links combo: the desktop's own default, the chooser, then every
        // browser this process can launch.
        {
            let mut labels: Vec<String> = vec![i18n("System default"), i18n("Ask each time")];
            labels.extend(model.browsers.iter().map(|b| b.name.clone()));
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            widgets
                .link_browser_row
                .set_model(Some(&gtk::StringList::new(&refs)));
            no_truncate(&widgets.link_browser_row);
            widgets
                .link_browser_row
                .set_selected(link_browser_index(&model.browsers, &init.link_browser));
            widen_combo_value(&widgets.link_browser_row, 50);
            let s = sender.clone();
            widgets
                .link_browser_row
                .connect_selected_notify(move |row| {
                    s.input(PrefInput::ChangeLinkBrowser(row.selected()));
                });
        }
        // "Send new messages from": the open folder's account, then every
        // enabled account and alias, labelled as the composer's From row
        // labels them. Only meaningful with more than one identity.
        {
            let mut labels: Vec<String> = vec![i18n("Account of the current folder")];
            labels.extend(model.identities.iter().map(|(name, addr)| {
                if name.trim().is_empty() {
                    addr.clone()
                } else {
                    format!("{name} <{addr}>")
                }
            }));
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            widgets
                .default_from_row
                .set_model(Some(&gtk::StringList::new(&refs)));
            let want = init.compose_default_from.trim();
            let sel = model
                .identities
                .iter()
                .position(|(_, addr)| addr.eq_ignore_ascii_case(want))
                .map(|i| i + 1)
                .unwrap_or(0);
            widgets.default_from_row.set_selected(sel as u32);
            widgets
                .default_from_row
                .set_visible(model.identities.len() > 1);
            middle_ellipsize(&widgets.default_from_row);
        }
        widgets.paste_plain_row.set_active(init.paste_plain);
        widgets
            .return_paragraph_row
            .set_active(init.return_paragraph);
        // "Always collapsed" in full, not ellipsized beside the description.
        widgets
            .toolbar_row
            .set_factory(Some(&crate::ui::accounts::non_ellipsizing_factory()));
        widgets
            .toolbar_row
            .set_selected(u32::from(init.toolbar_expanded));
        widgets
            .compose_format_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Rich text"),
                &i18n("Markdown"),
                &i18n("HTML source"),
                &i18n("Plain text"),
            ])));
        no_truncate(&widgets.compose_format_row);
        widgets
            .compose_format_row
            .set_selected(match init.compose_format {
                crate::config::ComposeFormat::Rich => 0,
                crate::config::ComposeFormat::Markdown => 1,
                crate::config::ComposeFormat::Html => 2,
                crate::config::ComposeFormat::Plain => 3,
            });
        compose_font_setup(
            &widgets.compose_font_row,
            &init.compose_font,
            sender.clone(),
        );
        {
            let row = &widgets.compose_size_row;
            let sizes = crate::ui::rich_editor::SIZES;
            let names: Vec<String> = std::iter::once(i18n("Default"))
                .chain(sizes.iter().map(|pt| pt.to_string()))
                .collect();
            row.set_model(Some(&gtk::StringList::new(
                &names.iter().map(String::as_str).collect::<Vec<_>>(),
            )));
            let at = sizes
                .iter()
                .position(|pt| *pt == init.compose_font_size)
                .map_or(0, |i| i + 1);
            row.set_selected(at as u32);
            let sender = sender.clone();
            row.connect_selected_notify(move |row| {
                let pt = row
                    .selected()
                    .checked_sub(1)
                    .and_then(|i| sizes.get(i as usize))
                    .copied()
                    .unwrap_or(0);
                let _ = sender.output(PrefOutput::SetComposeFontSize(pt));
            });
        }
        widgets.follow_plain_row.set_active(init.follow_plain);
        widgets
            .signature_position_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Above the quoted message"),
                &i18n("Below the quoted message"),
            ])));
        no_truncate(&widgets.signature_position_row);
        widgets
            .signature_position_row
            .set_selected(match init.signature_position {
                crate::config::SignaturePosition::AboveQuote => 0,
                crate::config::SignaturePosition::BelowQuote => 1,
            });
        widgets
            .signature_dashes_row
            .set_active(init.signature_dashes);
        let undo_labels: Vec<String> = crate::config::UNDO_SEND_CHOICES
            .iter()
            .map(|&secs| match secs {
                0 => i18n("Off"),
                n => ni18n_f("{n} second", "{n} seconds", n, &[("n", &n.to_string())]),
            })
            .collect();
        let undo_labels: Vec<&str> = undo_labels.iter().map(String::as_str).collect();
        widgets
            .undo_send_row
            .set_model(Some(&gtk::StringList::new(&undo_labels)));
        no_truncate(&widgets.undo_send_row);
        // A delay set by hand in the file shows as the nearest one offered.
        let nearest = crate::config::UNDO_SEND_CHOICES
            .iter()
            .enumerate()
            .min_by_key(|(_, &secs)| secs.abs_diff(init.undo_send_secs))
            .map_or(0, |(i, _)| i as u32);
        widgets.undo_send_row.set_selected(nearest);
        widgets
            .reply_position_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Above the messages"),
                &i18n("Below the messages"),
                &i18n("Follows the reading order"),
            ])));
        no_truncate(&widgets.reply_position_row);
        widgets
            .reply_position_row
            .set_selected(match init.reply_position {
                crate::config::ReplyPosition::Top => 0,
                crate::config::ReplyPosition::Bottom => 1,
                crate::config::ReplyPosition::Follow => 2,
            });
        widgets.spellcheck_row.set_active(init.spellcheck);
        // The language switches offer exactly what checking can use: the
        // installed dictionaries, behind a "System language" default. Typed
        // codes are gone — a language nobody has a dictionary for silently
        // checks nothing, so only real options are offered (#114). Any
        // number of them can be on at once (#365).
        {
            let rows = SpellLangRows::new(
                widgets.spell_lang_row.clone(),
                init.spellcheck_langs.clone(),
            );
            model.spell_langs = Some(rows.clone());
            // Listing the installed dictionaries reads several directories;
            // done a moment after the window is up, not before it.
            let spelling_group = widgets.spelling_group.clone();
            let sender = sender.clone();
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
                let dicts = crate::ui::rich_editor::installed_dictionaries();
                let system = adw::SwitchRow::builder()
                    .title(i18n("System language"))
                    .subtitle(gtk::glib::markup_escape_text(
                        &crate::ui::rich_editor::spell_language_names(
                            &crate::ui::rich_editor::system_spell_languages(),
                        ),
                    ))
                    .build();
                rows.expander.add_row(&system);
                *rows.system.borrow_mut() = Some(system.clone());
                for d in &dicts {
                    let row = adw::SwitchRow::builder()
                        .title(gtk::glib::markup_escape_text(
                            &crate::spell::language_display_name(d),
                        ))
                        .build();
                    rows.expander.add_row(&row);
                    rows.dicts.borrow_mut().push((d.clone(), row));
                }
                let current = rows.setting.borrow().clone();
                rows.show(&current);
                if dicts.is_empty() {
                    rows.expander.set_sensitive(false);
                    spelling_group.set_description(Some(&i18n(
                        "No dictionaries found. On Flatpak, add a language with: flatpak config --set extra-languages <code>",
                    )));
                }
                // Connected after the switches show the saved choice, and
                // guarded besides, so showing it never saves it again.
                let switches = std::iter::once((String::new(), system))
                    .chain(rows.dicts.borrow().iter().cloned())
                    .collect::<Vec<_>>();
                for (code, row) in switches {
                    let (rows, s) = (rows.clone(), sender.clone());
                    row.connect_active_notify(move |row| {
                        if let Some(next) = rows.toggled(&code, row.is_active()) {
                            let _ = s.output(PrefOutput::SetSpellcheckLangs(next));
                        }
                    });
                }
                gtk::glib::ControlFlow::Break
            });
        }
        {
            // The learned-words list manages itself entirely in widget-land:
            // the rows call the spell module directly and rebuild in place.
            let exp = widgets.spell_words_row.clone();
            let add = adw::EntryRow::builder().title(&i18n("Add a word")).build();
            add.set_show_apply_button(true);
            {
                let exp = exp.clone();
                add.connect_apply(move |row| {
                    crate::spell::add_personal_word(&row.text());
                    row.set_text("");
                    rebuild_personal_words(&exp);
                });
            }
            exp.add_row(&add);
            rebuild_personal_words(&exp);
        }
        tracing::debug!(
            "settings window: prefs tail B (words) at {:?}",
            t_init.elapsed()
        );

        // Date and clock combos.
        let date_labels_owned: Vec<String> = DATE_STYLES.iter().map(|(l, _)| i18n(l)).collect();
        let date_labels: Vec<&str> = date_labels_owned.iter().map(String::as_str).collect();
        widgets
            .date_style_row
            .set_model(Some(&gtk::StringList::new(&date_labels)));
        widgets.date_style_row.set_selected(
            DATE_STYLES
                .iter()
                .position(|(_, s)| *s == init.date_style)
                .unwrap_or(0) as u32,
        );
        let clock_labels_owned: Vec<String> = CLOCK_STYLES.iter().map(|(l, _)| i18n(l)).collect();
        let clock_labels: Vec<&str> = clock_labels_owned.iter().map(String::as_str).collect();
        widgets
            .clock_style_row
            .set_model(Some(&gtk::StringList::new(&clock_labels)));
        widgets.clock_style_row.set_selected(
            CLOCK_STYLES
                .iter()
                .position(|(_, s)| *s == init.clock_style)
                .unwrap_or(0) as u32,
        );

        // Message-content appearance combo.
        let app_theme_labels_owned: Vec<String> = APP_THEMES.iter().map(|(l, _)| i18n(l)).collect();
        let app_theme_labels: Vec<&str> =
            app_theme_labels_owned.iter().map(String::as_str).collect();
        widgets
            .app_theme_row
            .set_model(Some(&gtk::StringList::new(&app_theme_labels)));
        let app_theme_sel = APP_THEMES
            .iter()
            .position(|(_, t)| *t == init.app_theme)
            .unwrap_or(0);
        widgets.app_theme_row.set_selected(app_theme_sel as u32);
        let text_scale_labels_owned: Vec<String> = crate::text_scale::STEPS
            .iter()
            .map(|p| match p {
                100 => i18n("Default"),
                p => i18n_f("{percent}%", &[("percent", &p.to_string())]),
            })
            .collect();
        let text_scale_labels: Vec<&str> =
            text_scale_labels_owned.iter().map(String::as_str).collect();
        widgets
            .text_scale_row
            .set_model(Some(&gtk::StringList::new(&text_scale_labels)));
        let text_scale_sel = crate::text_scale::STEPS
            .iter()
            .position(|p| *p == init.text_scale)
            .unwrap_or(1);
        widgets.text_scale_row.set_selected(text_scale_sel as u32);

        let theme_labels_owned: Vec<String> = MESSAGE_THEMES.iter().map(|(l, _)| i18n(l)).collect();
        let theme_labels: Vec<&str> = theme_labels_owned.iter().map(String::as_str).collect();
        widgets
            .message_theme_row
            .set_model(Some(&gtk::StringList::new(&theme_labels)));
        let theme_sel = MESSAGE_THEMES
            .iter()
            .position(|(_, t)| *t == init.message_theme)
            .unwrap_or(0);
        widgets.message_theme_row.set_selected(theme_sel as u32);
        tracing::debug!(
            "settings window: prefs tail C (themes) at {:?}",
            t_init.elapsed()
        );

        // The reader's font override (#56): the button shows the chosen font,
        // or the interface font when none was chosen yet (so what it shows is
        // what the reader will use); the row only responds while the switch
        // is on.
        {
            let desc = if init.reader_font.trim().is_empty() {
                gtk::Settings::default()
                    .and_then(|s| s.gtk_font_name())
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "Cantarell 11".to_string())
            } else {
                init.reader_font.clone()
            };
            widgets
                .reader_font_button
                .set_font_desc(&gtk::pango::FontDescription::from_string(&desc));
            widgets.reader_font_row.set_sensitive(init.override_fonts);
            let font_row = widgets.reader_font_row.clone();
            widgets
                .override_fonts_row
                .connect_active_notify(move |row| {
                    font_row.set_sensitive(row.is_active());
                });
        }
        widgets.override_fonts_row.set_active(init.override_fonts);
        widgets.override_colors_row.set_active(init.override_colors);
        // Plain-text messages in monospace (#181): the font button shows
        // the desktop's monospace font until another is chosen.
        {
            let desc = if init.plain_font.trim().is_empty() {
                crate::desktop::monospace_font()
            } else {
                init.plain_font.clone()
            };
            widgets
                .plain_font_button
                .set_font_desc(&gtk::pango::FontDescription::from_string(&desc));
            widgets.plain_font_row.set_sensitive(init.plain_monospace);
            let font_row = widgets.plain_font_row.clone();
            widgets.plain_mono_row.connect_active_notify(move |row| {
                font_row.set_sensitive(row.is_active());
            });
        }
        widgets.plain_mono_row.set_active(init.plain_monospace);

        // Trackpad swipe sensitivity: 1 (libadwaita's own, far too long a
        // swipe for most trackpads) to 10, in half steps.
        let adj = gtk::Adjustment::new(
            init.swipe_sensitivity,
            crate::config::SWIPE_SENSITIVITY_MIN,
            crate::config::SWIPE_SENSITIVITY_MAX,
            0.5,
            1.0,
            0.0,
        );
        widgets.swipe_sensitivity_row.set_digits(1);
        widgets.swipe_sensitivity_row.set_adjustment(Some(&adj));

        // Hover-palette delay spinner (0–3000ms, step 50).
        // Actions palette timeout: 1–30 seconds.
        let adj = gtk::Adjustment::new(init.palette_collapse_secs as f64, 1.0, 30.0, 1.0, 5.0, 0.0);
        widgets.palette_collapse_row.set_adjustment(Some(&adj));
        let adj = gtk::Adjustment::new(
            init.card_palette_collapse_secs as f64,
            1.0,
            30.0,
            1.0,
            5.0,
            0.0,
        );
        widgets.card_palette_collapse_row.set_adjustment(Some(&adj));

        widgets
            .settings_open_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Settings").as_str(),
                i18n("Accounts").as_str(),
            ])));
        widgets
            .settings_open_row
            .set_selected(if init.settings_open_accounts { 1 } else { 0 });

        widgets.accounts_slot.set_child(Some(&init.accounts_panel));
        let pgp = crate::ui::pgp_keys::PgpKeys::builder()
            .launch(crate::ui::pgp_keys::PgpKeysInit {
                identities: init.identities.clone(),
            })
            .detach();
        widgets.pgp_slot.set_child(Some(pgp.widget()));
        model.pgp_keys = Some(pgp);
        let cloud = crate::ui::cloud_accounts::CloudAccounts::builder()
            .launch(())
            .forward(sender.input_sender(), |o| match o {
                crate::ui::cloud_accounts::CloudAccountsOutput::EditorOpen(open) => {
                    PrefInput::CloudEditorOpen(open)
                }
            });
        widgets.cloud_slot.set_child(Some(cloud.widget()));
        widgets
            .translation_slot
            .set_child(Some(&crate::ui::translation_page::build()));
        model.cloud = Some(cloud);
        let contacts = crate::ui::contact_sources::ContactSources::builder()
            .launch(crate::ui::contact_sources::ContactSourcesInit {
                search_open: init.contacts_search_open,
                placement: init.contacts_placement,
            })
            .forward(sender.input_sender(), PrefInput::Contacts);
        widgets.contacts_slot.set_child(Some(contacts.widget()));
        model.contacts = Some(contacts);
        model.contacts_placement_row = Some(widgets.contacts_placement_row.clone());
        // The sidebar (#141): a heading per section, a row per category.
        tracing::debug!(
            "settings window: prefs tail D (before sidebar rows) at {:?}",
            t_init.elapsed()
        );
        for (section, pages) in SIDE_PAGES {
            let heading = gtk::ListBoxRow::new();
            heading.set_selectable(false);
            heading.set_activatable(false);
            heading.add_css_class("settings-side-heading");
            let label = gtk::Label::new(Some(&i18n(section)));
            label.set_xalign(0.0);
            label.add_css_class("caption-heading");
            label.add_css_class("dim-label");
            heading.set_child(Some(&label));
            widgets.side_list.append(&heading);
            for page in pages.iter() {
                let row = gtk::ListBoxRow::new();
                row.set_widget_name(&format!("page:{}", page.id));
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                line.append(&gtk::Image::from_icon_name(page.icon));
                let label = gtk::Label::new(Some(&i18n(page.title)));
                label.set_xalign(0.0);
                line.append(&label);
                row.set_child(Some(&line));
                widgets.side_list.append(&row);
            }
        }
        // One pane under 640sp: the sidebar first, a category on top of it.
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            640.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&widgets.split, "collapsed", Some(&true.to_value()));
        root.add_breakpoint(narrow);
        model.panels_stack = Some(widgets.panels_stack.clone());
        model.accounts_slot = Some(widgets.accounts_slot.clone());
        model.toolbar_editor = Some(ToolbarEditor::build(&widgets.toolbar_editor_box, &sender));
        // The toolbar editor's zones want a row of six chips: the page's
        // column is 40px wider than the stock preferences clamp allows.
        widen_page(widgets.appearance_page.upcast_ref(), 640);
        model.rebuild_toolbar_chips();
        model.column_editor = Some(ColumnEditor::build(&widgets.column_editor_box, &sender));
        model.columns_group = Some(widgets.columns_group.clone().upcast());
        model.rebuild_column_chips();
        tracing::debug!(
            "settings window: prefs tail E (sidebar rows built) at {:?}",
            t_init.elapsed()
        );
        model.side_list = Some(widgets.side_list.clone());
        model.search = Some(SettingsSearch::build(
            &widgets.side_toolbar,
            &widgets.side_header,
            &widgets.side_scroller,
            root.upcast_ref(),
            &sender,
        ));
        root.add_controller(crate::ui::close_key());
        // HYLKI_SHOWCASE_SETTINGS_SEARCH=<text>[:<n>] types <text> into the
        // search 2 s after the window is built and, given <n>, picks the
        // n-th result at 4 s (#260).
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_SETTINGS_SEARCH") {
            let (text, pick) = match v.rsplit_once(':') {
                Some((t, n)) if n.parse::<usize>().is_ok() => {
                    (t.to_string(), n.parse::<usize>().ok())
                }
                _ => (v.clone(), None),
            };
            let bar = model
                .search
                .as_ref()
                .map(|s| (s.bar.clone(), s.entry.clone()));
            if let Some((bar, entry)) = bar {
                gtk::glib::timeout_add_seconds_local_once(2, move || {
                    bar.set_search_mode(true);
                    entry.set_text(&text);
                });
            }
            if let Some(n) = pick {
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(4, move || {
                    s.input(PrefInput::SearchPick(n))
                });
            }
        }
        model.content_page = Some(widgets.content_page.clone());
        model.split = Some(widgets.split.clone());
        let first = init
            .start_page
            .as_deref()
            .filter(|id| side_page(id).is_some())
            .unwrap_or(if init.start_on_accounts {
                "accounts"
            } else {
                "general"
            });
        model.select_row(first);
        // Every page but the one shown leaves the stack until after the
        // first paint: laid out and styled together, the pages' hundreds of
        // rows were most of the wait for the window to appear.
        {
            let stack = &widgets.panels_stack;
            let shown = stack.visible_child_name().map(|n| n.to_string());
            let pages = stack.pages();
            let mut deferred = Vec::new();
            for i in 0..pages.n_items() {
                let Some(page) = pages.item(i).and_downcast::<gtk::StackPage>() else {
                    continue;
                };
                let name = page.name().map(|n| n.to_string()).unwrap_or_default();
                if Some(&name) != shown.as_ref() {
                    deferred.push((name, page.child()));
                }
            }
            for (_, child) in &deferred {
                stack.remove(child);
            }
            *model.deferred_pages.borrow_mut() = deferred;
            let s = sender.clone();
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
                s.input(PrefInput::MountPages);
                gtk::glib::ControlFlow::Break
            });
        }
        model.host_header = Some(widgets.host_header.clone());

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
