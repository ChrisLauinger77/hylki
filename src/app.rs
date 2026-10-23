//! Root component: the application window, three-pane adaptive layout, and the
//! routing between the sidebar, list, reader, and the per-account mail workers.

use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};
use relm4::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

// The message list's opening width now comes from config (the remembered pane
// width, #28); its floor lives with the pane in message_list.rs
// (LIST_MIN_WIDTH).

use crate::ui::FOCUS_ANIM_MS;

relm4::new_action_group!(WindowActionGroup, "win");
relm4::new_stateless_action!(AccountsAction, WindowActionGroup, "accounts");
relm4::new_stateless_action!(PreferencesAction, WindowActionGroup, "preferences");
relm4::new_stateless_action!(AboutAction, WindowActionGroup, "about");
relm4::new_stateless_action!(ShortcutsAction, WindowActionGroup, "shortcuts");
relm4::new_stateless_action!(PrintAction, WindowActionGroup, "print");
relm4::new_stateless_action!(PrintPreviewAction, WindowActionGroup, "print-preview");
/// Store a setting the user changed and save the settings, if it really
/// changed; says whether it did, so the caller can apply it.
macro_rules! pref {
    ($self:ident . $($field:ident).+ = $value:expr) => {{
        let value = $value;
        let changed = $self.$($field).+ != value;
        if changed {
            $self.$($field).+ = value;
            $self.save_settings();
        }
        changed
    }};
}

mod about;
mod accounts;
mod actions;
mod attachments;
mod card_menu;
mod compose_host;
mod compose_init;
mod compose_open;
mod consts;
mod contacts;
mod conversation;
mod counts;
mod demo;
mod diagnostics;
mod drafts;
mod events_bodies;
mod events_folders;
mod events_messages;
mod filter_run;
mod filters;
mod flags;
mod focus;
mod folder_menu;
mod folders;
mod gallery;
mod hand_off;
mod identity;
mod invites;
mod keymap;
mod list_header;
mod model;
mod move_tag;
mod msg;
mod navigation;
mod notifications;
mod outbox;
mod palette;
mod pending;
mod picker;
mod popout;
mod prefill;
mod prefs_compose;
mod prefs_data;
mod prefs_general;
mod prefs_list;
mod prefs_reader;
mod prefs_sidebar;
mod quote;
mod reader_select;
mod reader_toolbar;
mod release_notes;
mod removal;
mod router;
mod search;
mod selection;
mod send;
mod send_morph;
mod settings_store;
mod settings_window;
mod shortcuts;
mod showcase;
mod sidebar;
mod sidebar_contents;
mod sidebar_peek;
mod smart;
mod split_reply;
mod tags;
#[cfg(test)]
mod test_support;
mod theme;
mod transfer;
mod tray;
mod types;
mod undo;
mod unified;
mod unified_starred;
mod unsubscribe;
mod uri_queue;
mod workers;

pub(crate) use accounts::finish_keyring_writes;
use accounts::{forget_account_secrets, reconcile_goa, GoaReconcile};
use attachments::{prune_downloads, AttachmentTarget};
use compose_host::{ComposeHost, ReaderCompose};
use consts::{ATTACHMENT_CACHE_BUDGET, BODY_CACHE_BUDGET, READER_MIN_WIDTH};
use contacts::{reload_contacts, sync_contact_books};
use conversation::{
    batch_bodies_by_folder, body_key, is_event_target, next_after_vanish, BodyKey, MissingBody,
    CARRIED_BODY_LIMIT, THREAD_BODY_WAIT,
};
use demo::{demo_account_configs_saved, demo_filters, demo_mode, demo_tags};
use filter_run::{FilterRun, FolderTally};
use flags::{read_label, sender_label, star_label};
use folders::{apply_folder_roles, kind_label, merged_unread, same_folder_list};
use hand_off::{window_drop_target, FileHandOff, HandOffFiles, HandOffTarget};
use keymap::{ctrl_shortcut_for, focus_in_compose, gmail_sequence, GoTarget, WindowCompose};
pub(crate) use keymap::{
    focus_is_text, focus_takes_keys, gmail_shortcut_for, reader_keeps, shortcut_for,
    without_caps_lock, Shortcut,
};
use list_header::{ListHeaderWidgets, SortFilterPopover, LIST_WIDE_ON};
pub(crate) use model::AppModel;
pub(crate) use msg::AppMsg;
use palette::{keyed_item, palette_key};
use pending::{PENDING_MOVE_MAX, PENDING_SEEN_MAX, PENDING_TAG_MAX, PENDING_TAG_SETTLED};
use picker::show_message_picker;
use popout::PopOut;
use prefill::{
    forward_prefill, reply_all_prefill, reply_prefill, reply_subject, stage_attachments,
    unsent_prefill,
};
use quote::editable_copy_html;
pub(crate) use quote::{body_is_pre_wrap, message_text};
use reader_toolbar::{ReaderToolbarWidgets, READER_ACTIONS_BREAKPOINT};
use release_notes::release_notes_page;
use search::default_scope;
use send_morph::{MorphPending, Provisional};
pub(crate) use showcase::showcase_capture;
use showcase::{scroll_all_to, showcase_at, showcase_log_focus, showcase_pgp};
use sidebar::{set_sidebar_header_compact, sidebar_output_msg};
use sidebar_peek::SIDEBAR_RAIL_WIDTH;
use smart::SMART_VIEW_LIMIT;
use split_reply::{set_split_shrink, split_reply_height};
use tags::TagScan;
pub(crate) use theme::desktop_icon_theme;
use theme::{
    apply_app_theme, apply_icon_theme, follow_motion_setting, install_scheme_css, motion_wanted,
    refresh_scheme_css, register_icons, WIZARD_HOLDS_LIGHT,
};
use transfer::{Transfer, TransferTally};
use types::{FolderMessages, SelectedFolder, UnifiedView};
use undo::{FlagChange, FlagKind, FlagRef, UndoEntry, UndoStep};
use unified::LIST_NEWEST;
use uri_queue::{
    eml_file_name, parse_mailto, ATTACH_PENDING, FILE_FOLDER, FILE_PENDING, MAILTO_PENDING,
    MAILTO_SENDER, MID_PENDING,
};
pub(crate) use uri_queue::{
    is_message_file, queue_attach_files, queue_mailto, queue_message_file, queue_mid,
};

relm4::new_stateless_action!(StatusBarAction, WindowActionGroup, "status-bar");
relm4::new_stateless_action!(ConsoleAction, WindowActionGroup, "console");
relm4::new_stateless_action!(FindAction, WindowActionGroup, "find");
relm4::new_stateless_action!(UndoAction, WindowActionGroup, "undo");
relm4::new_stateless_action!(RedoAction, WindowActionGroup, "redo");

use crate::config::{self, AccountConfig};
use crate::models::{
    thread_ids, Account, Attachment, Folder, FolderKind, KeywordFinding, Message, ThreadSummary,
};
use crate::ui::accounts::{AccountsOutput, AccountsWindow};
use crate::ui::attachments_gallery::{
    AttachmentsGallery, GalleryAccount, GalleryFolder, GalleryInput, GalleryOutput, GalleryRequest,
};
use crate::ui::compose::{Compose, ComposeInit, ComposeInput, ComposeOutput, ComposePrefill};
use crate::ui::message_list::{
    BulkAction, MessageList, MessageListInput, MessageListOutput, QuickFilter, QuickFilters,
    RowAction, SearchScope,
};

use crate::i18n::{i18n, i18n_f, i18n_noop, ni18n_f};
use crate::ui::attachment_drawer::{AttachmentDrawer, AttachmentDrawerInput};
use crate::ui::contacts_page::{ContactsPage, ContactsPageInput, ContactsPageOutput};
use crate::ui::message_view::{MessageView, MessageViewInput, MessageViewOutput, UnsubState};
use crate::ui::message_window::{
    MessageWindow, MessageWindowInit, MessageWindowInput, MessageWindowOutput,
};
use crate::ui::notifications::{NotificationCenter, NotifyInput, NotifyOutput};
use crate::ui::preferences::{PrefInit, PrefInput, PrefOutput, Preferences};
use crate::ui::sidebar::{
    CtxAction, SectionData, Sidebar, SidebarInit, SidebarInput, SidebarOutput, UnifiedFolderRef,
};
use crate::worker::{self, MailRequest, OutgoingMessage, WorkerEvent};

#[relm4::component(pub)]
impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some(crate::APP_NAME),
            set_icon_name: Some(crate::APP_ID),
            add_css_class: "hylki",

            // Persist the window size + maximized state on close. (Position and
            // which monitor can't be restored on Wayland — the compositor owns
            // placement — so only the geometry is saved.)
            connect_close_request[background = model.run_in_background.clone(), outgoing = model.outgoing.clone()] => move |w| {
                let maximized = w.is_maximized();
                let (width, height) = if maximized {
                    let (sw, sh, _) = crate::config::load_window_state();
                    (sw, sh)
                } else {
                    (w.width(), w.height())
                };
                crate::config::save_window_state(width, height, maximized);
                // Running in the background: hide the window and stay alive, so
                // mail keeps arriving and GNOME lists Hylki under Background Apps
                // (issue #3). The window is kept rather than rebuilt, so reopening
                // is instant and nothing is torn down — which is also what makes
                // this safe, given the exit below.
                if background.get() {
                    w.set_visible(false);
                    return gtk::glib::Propagation::Stop;
                }
                // Exit cleanly the moment window state is saved. Letting GTK,
                // WebKit and the per-account worker threads tear down the normal
                // way can abort — a Rust panic fired from a GObject dispose
                // callback becomes SIGABRT, which the Flatpak surfaces as a crash
                // notification. Nothing else needs persisting on quit: accounts
                // and settings are written as they change, the keyring's share
                // once its thread is done.
                crate::outgoing::park_held(&outgoing);
                finish_keyring_writes();
                std::process::exit(0)
            },

            // Send's toasts (#434) float over everything, the status bar
            // included, and never move the layout.
            #[wrap(Some)]
            #[name = "toasts"]
            set_content = &adw::ToastOverlay {
            #[wrap(Some)]
            #[name = "window_overlay"]
            set_child = &gtk::Overlay {

                #[wrap(Some)]
                set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                // The peek panel's split view: permanently collapsed, so
                // showing its sidebar slides the expanded panel in from the
                // window's left edge over the rail and the panes — with
                // libadwaita's scrim, shadow and swipe — while the docked
                // rail underneath stays exactly as it is (see
                // set_sidebar_peek). The account split view is its content.
                #[name = "peek_split"]
                adw::OverlaySplitView {
                    set_vexpand: true,
                    set_collapsed: true,
                    set_show_sidebar: false,
                    set_min_sidebar_width: 280.0,
                    set_max_sidebar_width: 280.0,
                    // No edge-swipe to open: it would fight the message
                    // list's own swipe actions.
                    set_enable_show_gesture: false,

                    #[wrap(Some)]
                    set_sidebar = &adw::ToolbarView {
                        // Laid out like the expanded sidebar's header: Refresh
                        // top-left, the title centred, the menu top-right.
                        add_top_bar = &adw::HeaderBar {
                            add_css_class: "flat",
                            set_show_start_title_buttons: false,
                            set_show_end_title_buttons: false,
                            #[wrap(Some)]
                            set_title_widget = &gtk::Label {
                                set_label: crate::APP_NAME,
                                add_css_class: "app-title",
                            },
                            pack_start = &gtk::Button {
                                set_tooltip_text: Some(i18n("Refresh or long-press for Status Bar").as_str()),
                                add_css_class: "flat",
                                set_valign: gtk::Align::Center,
                                set_child: Some(&model.peek_refresh_stack),
                                connect_clicked[sender] => move |_| sender.input(AppMsg::Refresh),
                                add_controller = gtk::GestureLongPress {
                                    connect_pressed[sender] => move |gesture, _, _| {
                                        gesture.set_state(gtk::EventSequenceState::Claimed);
                                        sender.input(AppMsg::ToggleNotifications);
                                    },
                                },
                            },
                            pack_end = &gtk::MenuButton {
                                // Hylki's own copy of the three bars: an icon of
                                // the GNOME name from a theme or another program
                                // drew a gear on some systems.
                                set_icon_name: "hylki-open-menu-symbolic",
                                set_tooltip_text: Some(i18n("Main Menu").as_str()),
                                add_css_class: "flat",
                                set_menu_model: Some(&model.menu),
                            },
                        },
                        #[wrap(Some)]
                        set_content = model.peek_sidebar.widget(),
                    },

                    #[wrap(Some)]
                    #[name = "sidebar_split"]
                    set_content = &adw::OverlaySplitView {
                    set_vexpand: true,
                    set_max_sidebar_width: 280.0,
                    // Low enough (with the panes' own minimums) that the whole
                    // window fits half of a 1920px screen — see READER_MIN_WIDTH.
                    set_min_sidebar_width: 180.0,
                    set_sidebar_width_fraction: 0.2,

                    #[wrap(Some)]
                    set_sidebar = &adw::ToolbarView {
                        #[name = "sidebar_header"]
                        add_top_bar = &adw::HeaderBar {
                            add_css_class: "flat",
                            #[wrap(Some)]
                            #[name = "app_title"]
                            set_title_widget = &gtk::Label {
                                set_label: crate::APP_NAME,
                                add_css_class: "app-title",
                            },
                            pack_start: &model.sidebar_refresh,
                            #[name = "sidebar_menu"]
                            pack_end = &gtk::MenuButton {
                                set_icon_name: "hylki-open-menu-symbolic",
                                set_tooltip_text: Some(i18n("Main Menu").as_str()),
                                add_css_class: "flat",
                                set_menu_model: Some(&model.menu),
                            },
                        },
                        #[wrap(Some)]
                        set_content = model.sidebar.widget(),
                    },

                    #[wrap(Some)]
                    set_content = &gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,

                    #[name = "content_stack"]
                    gtk::Stack {
                        set_hexpand: true,
                        // Swap the mail panes for the attachments gallery or
                        // the contacts view, at once: no transition.
                        #[watch]
                        set_visible_child_name: if model.showing_gallery {
                            "gallery"
                        } else if model.showing_contacts {
                            "contacts"
                        } else {
                            "mail"
                        },

                    add_named[Some("mail")] = &gtk::Paned {
                        set_orientation: gtk::Orientation::Horizontal,
                        // Thin handle so the panes sit flush (just a 1px divider),
                        // no wide-handle gap between them. A theme paints that
                        // divider through this class (see theme::css); without
                        // one it keeps the stock separator.
                        add_css_class: "mail-split",
                        set_wide_handle: false,
                        // Launch wide enough for a row's actions palette. That is
                        // also the list's minimum while the avatars are on,
                        // so `shrink_start_child: false` clamps to the same figure
                        // either way. With the circles off the minimum drops to what
                        // the sender and subject need (#29) — a fine width to be
                        // able to drag down to, but a poor one to open at.
                        set_position: crate::config::load_list_pane_width(),
                        // Remember the width the user drags to (#28). Debounced:
                        // position-notify fires per pixel of a drag (and when the
                        // window squeezes the pane), one write once it settles.
                        connect_position_notify[
                            sender,
                            pending = std::rc::Rc::new(std::cell::RefCell::new(
                                None::<gtk::glib::SourceId>,
                            ))
                        ] => move |p| {
                            let pos = p.position();
                            // Automatic layout follows the width as it is
                            // dragged, not once the drag has settled (#334).
                            sender.input(AppMsg::ListPaneWidth(pos));
                            if let Some(id) = pending.borrow_mut().take() {
                                id.remove();
                            }
                            let armed = pending.clone();
                            *pending.borrow_mut() = Some(gtk::glib::timeout_add_local_once(
                                std::time::Duration::from_millis(600),
                                move || {
                                    *armed.borrow_mut() = None;
                                    crate::config::save_list_pane_width(pos);
                                },
                            ));
                        },
                        // The list keeps its width as the window resizes (the
                        // reader absorbs the change), and can't be dragged narrower
                        // than its own minimum. The reader has a floor of its own:
                        // its header carries a full row of actions, and squeezing it
                        // pushed them off the right-hand edge of the window.
                        set_resize_start_child: false,
                        set_shrink_start_child: false,
                        set_resize_end_child: true,
                        set_shrink_end_child: false,

                        #[wrap(Some)]
                        set_start_child = &adw::ToolbarView {
                            add_top_bar = &adw::HeaderBar {
                                add_css_class: "flat",
                                // Middle pane: no window controls (the reader pane's
                                // header carries the window's close button).
                                set_show_start_title_buttons: false,
                                set_show_end_title_buttons: false,
                                // No folder title here — the sidebar's selection
                                // already names it. An empty label keeps the
                                // window's "Hylki" title from appearing instead.
                                #[wrap(Some)]
                                set_title_widget = &gtk::Label {
                                    set_label: "",
                                },
                                // Leftmost, mirroring the pane it acts on: the
                                // sidebar expand/collapse toggle (moved here from
                                // the sidebar's own footer).
                                pack_start = &gtk::Button {
                                    set_icon_name: "sidebar-show-symbolic",
                                    #[watch]
                                    set_tooltip_text: Some(if model.rail_active { i18n("Expand sidebar") } else { i18n("Collapse sidebar") }.as_str()),
                                    add_css_class: "flat",
                                    connect_clicked[sender] => move |_| sender.input(AppMsg::ToggleSidebar),
                                },
                                // Search lives behind this button (#102);
                                // Ctrl+F and / open it too.
                                #[name = "lh_search"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "system-search-symbolic",
                                        set_tooltip_text: Some(i18n("Search messages (Ctrl+F)").as_str()),
                                        add_css_class: "flat",
                                        connect_clicked[sender] => move |_| {
                                            sender.input(AppMsg::OpenListSearch);
                                        },
                                    },
                                },
                                // Across from the sidebar toggle: the visible
                                // message count and the sort menu (moved out of
                                // the list's own toolbar to reclaim a row).
                                // pack_end packs right-to-left: sort rightmost.
                                // Quick filters (#97, #441): session state,
                                // like Mail.app's filter bar.
                                // Focus Mode: the ⋯ that stands in for the
                                // search, filters, count and sort while they
                                // are folded away. Rightmost, so packed first.
                                #[name = "lh_overflow"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    set_reveal_child: false,
                                    gtk::Button {
                                        set_icon_name: "view-more-horizontal-symbolic",
                                        set_tooltip_text: Some(i18n("Search, filters and sort").as_str()),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ListOverflowMenu),
                                    },
                                },
                                // The sort orders and every quick filter share
                                // one popover (#441): a button each would crowd
                                // the header.
                                #[name = "lh_filter"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    #[name = "list_filter_btn"]
                                    gtk::MenuButton {
                                        set_icon_name: "filter-symbolic",
                                        set_tooltip_text: Some(i18n("Sort and filter messages").as_str()),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                    },
                                },

                                #[name = "lh_count"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Label {
                                        #[watch]
                                        set_label: &model.list_count,
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "list-count",
                                    },
                                },
                            },
                            #[wrap(Some)]
                            set_content = model.message_list.widget(),
                        },

                        #[wrap(Some)]
                        #[name = "reader_bin"]
                        set_end_child = &adw::BreakpointBin {
                            // The narrowest the reader may become. The header's
                            // actions collapse into the overflow menu before this
                            // matters (see the breakpoint added in init). The
                            // height floor exists because AdwBreakpointBin insists
                            // on an explicit one; the window's own minimum is what
                            // really binds vertically.
                            set_size_request: (READER_MIN_WIDTH, 200),
                            #[wrap(Some)]
                            set_child = &adw::ToolbarView {
                            #[name = "reader_header"]
                            add_top_bar = &adw::HeaderBar {
                                // Rightmost of the packed items, beside the window
                                // controls: the overflow ⋯ that stands in for the
                                // action buttons while collapsed.
                                pack_end: &model.reader_overflow_btn,
                                add_css_class: "flat",
                                // Tighter icon spacing than stock so the full
                                // action row fits a narrower pane (see
                                // READER_ACTIONS_BREAKPOINT and styles.css).
                                add_css_class: "reader-toolbar",
                                // Empty title so the window's "Hylki" title isn't
                                // shown here; the app title lives above the sidebar.
                                #[wrap(Some)]
                                set_title_widget = &gtk::Label {
                                    set_label: "",
                                },
                                // Outbox actions, in place of Reply/Forward/Flag —
                                // a message that hasn't been sent can't be replied
                                // to, and the questions worth asking about it are
                                // whether to edit, send or bin it.
                                #[name = "tb_outbox_edit"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "document-edit-symbolic",
                                        set_tooltip_text: Some(i18n("Edit this message").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.showing_outbox
                                            && model.reader_compose.is_none(),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::EditCurrentOutbox),
                                    },
                                },
                                #[name = "tb_outbox_send"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "mail-send-symbolic",
                                        set_tooltip_text: Some(i18n("Try to send this message now").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.showing_outbox
                                            && model.reader_compose.is_none(),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::SendCurrentOutbox),
                                    },
                                },
                                #[name = "tb_outbox_send_all"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_label: &i18n("Send all"),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.showing_outbox
                                            && model.reader_compose.is_none(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::RetryAllOutbox),
                                    },
                                },
                                // ---- The action buttons. Declared here in the
                                // default order, then re-packed in init (and on
                                // every change) in the user's saved order — see
                                // relayout_reader_toolbar. The left group stays
                                // at every width; the right group folds into
                                // the ⋯ overflow. Default left group: Reply,
                                // Reply All, Forward, Star, Archive, Delete.
                                #[name = "tb_reply"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "mail-reply-sender-symbolic",
                                        set_tooltip_text: Some(i18n("Reply").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Reply),
                                        // In a conversation these act on the one
                                        // highlighted card; with none (or several)
                                        // highlighted they grey out — no way to say
                                        // which message they'd mean.
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Reply),
                                    },
                                },
                                #[name = "tb_reply_all"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "mail-reply-all-symbolic",
                                        set_tooltip_text: Some(i18n("Reply All").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::ReplyAll),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ReplyAll),
                                    },
                                },
                                #[name = "tb_forward"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "mail-forward-symbolic",
                                        set_tooltip_text: Some(i18n("Forward").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Forward),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Forward),
                                    },
                                },
                                #[name = "tb_star"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        #[watch]
                                        set_tooltip_text: Some(if model.toolbar_star_lit() { i18n("Remove Star") } else { i18n("Star") }.as_str()),
                                        // One glyph in both states, like every other
                                        // icon; the starred state carries color only.
                                        set_icon_name: "hylki-non-starred-symbolic",
                                        #[watch]
                                        set_css_classes: if model.toolbar_star_lit() {
                                            &["flat", "star-active"]
                                        } else {
                                            &["flat"]
                                        },
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Star),
                                        #[watch]
                                        set_sensitive: model.acts_on_selection(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ToggleStar),
                                    },
                                },
                                #[name = "tb_archive"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "mail-archive-symbolic",
                                        set_tooltip_text: Some(i18n("Archive").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Archive),
                                        #[watch]
                                        set_sensitive: model.acts_on_selection(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Archive),
                                    },
                                },
                                #[name = "tb_delete"]
                                pack_start = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideRight,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "user-trash-symbolic",
                                        #[watch]
                                        set_tooltip_text: Some(&model.delete_tooltip()),
                                        add_css_class: "flat",
                                        // Shown for the Outbox too (a queued message
                                        // can still be binned) — see toolbar_visible.
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Delete),
                                        #[watch]
                                        set_sensitive: model.reply_target().is_some()
                                            || model.many_selected(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::Delete),
                                    },
                                },
                                // ---- Default right group, left to right: Tags,
                                // Read/Unread, Spam, Move To, Find, Print.
                                // (The sender-check seal lives in the
                                // message header now — #88; no Add-to-Contacts
                                // button either: right-click any address in a
                                // message header.)
                                #[name = "tb_print"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "printer-symbolic",
                                        set_tooltip_text: Some(i18n("Print Preview (Ctrl+Shift+P)").as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Print),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        // The preview, not the print dialog: the button
                                        // shows what will come out and prints from
                                        // there, so nobody spends paper to find out.
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::PrintPreview),
                                    },
                                },
                                // In-message find (#103).
                                #[name = "tb_find"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        set_icon_name: "loupe-with-arrow-symbolic",
                                        set_tooltip_text: Some(i18n("Find in message (Ctrl+F)").as_str()),
                                        add_css_class: "flat",
                                        // Greyed out, not hidden, with no message
                                        // open: the toolbar must not shift.
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Find),
                                        #[watch]
                                        set_sensitive: model.current.is_some(),
                                        connect_clicked[sender] => move |_| {
                                            sender.input(AppMsg::OpenReaderFind);
                                        },
                                    },
                                },
                                // Move To… (#164): a folder picker for the
                                // target, or the whole list selection.
                                #[name = "tb_move"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Box {
                                        #[local_ref]
                                        reader_move_btn -> gtk::Button {
                                            #[watch]
                                            set_visible: model.toolbar_visible(config::ToolbarItem::MoveTo),
                                            #[watch]
                                            set_sensitive: model.reply_target().is_some()
                                                || model.many_selected(),
                                        },
                                    },
                                },
                                #[name = "tb_spam"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        #[watch]
                                        set_icon_name: if model.target_in_junk() {
                                            "mail-mark-notjunk-symbolic"
                                        } else {
                                            "mail-mark-junk-symbolic"
                                        },
                                        #[watch]
                                        set_tooltip_text: Some(if model.target_in_junk() { i18n("Not Spam") } else { i18n("Mark as Spam") }.as_str()),
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: model.toolbar_visible(config::ToolbarItem::Spam),
                                        #[watch]
                                        set_sensitive: model.acts_on_selection(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::MarkSpam),
                                    },
                                },
                                #[name = "tb_read"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Button {
                                        add_css_class: "flat",
                                        #[watch]
                                        set_visible: !model.showing_outbox && !model.reader_actions_collapsed
                                            && model.reader_compose.is_none(),
                                        // The icon shows the ACTION (read envelope =
                                        // "mark as read"), matching the menus.
                                        #[watch]
                                        set_icon_name: if model.reply_target().is_some_and(|m| m.unread) {
                                            "hylki-mail-read-symbolic"
                                        } else {
                                            "mail-unread-symbolic"
                                        },
                                        #[watch]
                                        set_tooltip_text: Some(if model.reply_target().is_some_and(|m| m.unread) { i18n("Mark as Read") } else { i18n("Mark as Unread") }.as_str()),
                                        #[watch]
                                        set_sensitive: model.acts_on_selection(),
                                        connect_clicked[sender] => move |_| sender.input(AppMsg::ToggleReadCurrent),
                                    },
                                },
                                // Tags (#71): a menu of the tags, ticked where
                                // the target carries them. Only once a tag exists.
                                #[name = "tb_tags"]
                                pack_end = &gtk::Revealer {
                                    set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                                    set_transition_duration: FOCUS_ANIM_MS,
                                    add_css_class: "focus-fade",
                                    gtk::Box {
                                        #[local_ref]
                                        reader_tag_btn -> gtk::Button {
                                            #[watch]
                                            set_visible: model.toolbar_visible(config::ToolbarItem::Tags) && !model.tags.is_empty(),
                                            #[watch]
                                            set_sensitive: model.acts_on_selection(),
                                        },
                                    },
                                },
                                #[name = "tb_spinner"]
                                pack_end = &gtk::Spinner {
                                    set_valign: gtk::Align::Center,
                                    set_tooltip_text: Some(i18n("Downloading attachments…").as_str()),
                                    #[watch]
                                    set_spinning: model.attachments_loading,
                                    #[watch]
                                    set_visible: model.attachments_loading
                                        && model.reader_compose.is_none(),
                                },
                            },
                            // Reader content: the inline reply/forward pane drops
                            // down (SlideDown revealer) above the message body,
                            // pushing it down to make room. The revealer is
                            // prepended in `init`. The drawer's widget is a Paned
                            // that holds the reader body + attachment footer.
                            #[wrap(Some)]
                            #[name = "reader_content_box"]
                            set_content = &gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                append: model.attachment_drawer.widget(),
                            },
                            },
                        },
                    },
                    },
                    },
                    },
                },

                // The status bar sits at the foot of the window (#433).
                append: model.notifications.widget(),
                },

                // ======== Full-window attachment lightbox ========
                // Covers all three panes; shown for images and PDFs coming
                // from the drawer. Same look as the gallery's own lightbox
                // (same CSS), no extra window.
                add_overlay: model.lightbox.widget(),
            },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Every setting in privacy.toml, read once.
        let prefs = config::load_privacy();
        // Before the workers start, so their first connection already knows
        // whether there is a network to make it on (#442).
        crate::power::read_network(prefs.metered_save_data);
        let launcher_badge = prefs.launcher_badge();
        relm4::set_global_css(include_str!("styles.css"));
        register_icons(prefs.desktop_icons);
        // Before install_scheme_css and before the reader exists: both read
        // the theme's colors back, and both listen for the light/dark flip
        // that swaps a theme's two palettes — GTK runs those handlers in
        // connection order, so the palette has to be in place first.
        crate::theme::install(&config::load_theme());
        install_scheme_css(&root);
        // Before any web view loads: a font fallback WebKit needed under
        // this runtime has to be in place for its first web process (#296).
        crate::web_fonts::startup(&root);

        let mut sidebar_state = config::load_sidebar_state();
        // Settings → Sidebar: "Remember the icon rail" off starts every
        // launch with the full sidebar; "Remember the sidebar layout" off
        // starts with every section folded up — and every account, folded as
        // it arrives (SetAccount) — keeping only the account order. The two
        // are independent.
        let remember_sidebar = prefs.remember_sidebar;
        let remember_rail = prefs.remember_rail;
        let icon_only = remember_rail && sidebar_state.icon_only;
        // Read here, ahead of the sidebar itself: Focus Mode's rail part
        // decides what the sidebar is built as. `icon_only` stays the user's
        // own choice — the one that is saved and the one that comes back
        // when Focus Mode ends.
        let mut focus = config::load_focus_mode();
        // "Start in Focus Mode" decides how the app opens, whatever the last
        // session left on. Written back at once so the file, the menu's check
        // item and the window all say the same thing from the first frame.
        if focus.enabled != focus.start_focused {
            focus.enabled = focus.start_focused;
            config::save_focus_mode(&focus);
        }
        let rail_now = icon_only || focus.active(config::FocusPart::RailSidebar);
        if !remember_sidebar {
            // The layout starts fresh; the order things were put in stays.
            sidebar_state = config::SidebarState {
                order: sidebar_state.order,
                folder_order: sidebar_state.folder_order,
                ..Default::default()
            };
        }
        let unified_expanded = sidebar_state.unified_expanded;
        let filtered_expanded = sidebar_state.filtered_expanded;
        let tags_expanded = sidebar_state.tags_expanded;
        let starred_expanded = sidebar_state.starred_expanded;
        let sent_expanded = sidebar_state.sent_expanded;
        let drafts_expanded = sidebar_state.drafts_expanded;
        let archive_expanded = sidebar_state.archive_expanded;
        let junk_expanded = sidebar_state.junk_expanded;
        let trash_expanded = sidebar_state.trash_expanded;
        let favorites_expanded = sidebar_state.favorites_expanded;
        let smart_expanded = sidebar_state.smart_expanded;
        let contacts_expanded = sidebar_state.contacts_expanded;
        let contacts_folded = sidebar_state.contacts_folded.clone();
        let favorites = sidebar_state.favorites.clone();

        // Load accounts, then reconcile against GNOME Online Accounts: drop any
        // imported account GOA no longer has, pause any whose Mail service is
        // switched off there. Reconciliation is skipped when GOA is unreachable,
        // so a momentary outage never wipes imported accounts. Live changes are
        // handled by the watcher below.
        let mut config = config::load().unwrap_or_default();
        // Snapshot before reconciling: a removed account's alias-password
        // keyring entries are keyed by addresses only its config knows.
        let config_before_goa = config.clone();
        let goa_outcome = match crate::goa::live_state() {
            Some(live) => reconcile_goa(&mut config, &live),
            None => GoaReconcile::default(),
        };
        let goa_removed = goa_outcome.removed;
        if !goa_removed.is_empty() {
            forget_account_secrets(&goa_removed, &config_before_goa);
            sidebar_state.order.retain(|e| !goa_removed.contains(e));
            sidebar_state.collapsed.retain(|e| !goa_removed.contains(e));
            sidebar_state
                .folders_expanded
                .retain(|e| !goa_removed.contains(e));
            sidebar_state
                .filtered_expanded_accounts
                .retain(|e| !goa_removed.contains(e));
            sidebar_state
                .tags_expanded_accounts
                .retain(|e| !goa_removed.contains(e));
            config::save_sidebar_state(&sidebar_state);
        }
        if !goa_removed.is_empty() || goa_outcome.paused_changed || goa_outcome.servers_changed {
            let _ = config::save(&config);
        }
        let order = sidebar_state.order;
        let collapsed = sidebar_state.collapsed;
        let folders_expanded = sidebar_state.folders_expanded;
        let filtered_expanded_accounts = sidebar_state.filtered_expanded_accounts;
        let tags_expanded_accounts = sidebar_state.tags_expanded_accounts;
        let tree_collapsed = sidebar_state.tree_collapsed;
        let folder_order = sidebar_state.folder_order;

        // Whether this run serves the built-in sample data (see spawn_workers):
        // decided after the GOA reconcile, which can add accounts.
        let demo_data = demo_mode() && config.is_empty();
        let show_attachments = prefs.show_attachments;
        let start_view = prefs.start_view;
        let start = {
            use crate::ui::sidebar::StartTarget;
            let (last, last_account) = config::load_last_view();
            match start_view {
                config::StartView::AllInboxes => None,
                config::StartView::AccountInbox => Some(last_account)
                    .filter(|e| !e.is_empty())
                    .map(StartTarget::Inbox),
                config::StartView::LastFolder => last
                    .filter(|v| !v.email.is_empty())
                    .map(|v| StartTarget::Folder(v.email, v.path)),
            }
        };
        let start_pending = start.is_some();
        let sidebar = Sidebar::builder()
            .launch(SidebarInit {
                collapsed: rail_now,
                mirror: false,
                unified_expanded,
                filtered_expanded,
                tags_expanded,
                starred_expanded,
                sent_expanded,
                drafts_expanded,
                archive_expanded,
                junk_expanded,
                trash_expanded,
                favorites_expanded,
                smart_expanded,
                contacts_expanded,
                contacts_folded: contacts_folded.clone(),
                show_attachments,
                start,
            })
            .forward(sender.input_sender(), sidebar_output_msg);
        // The sidebar waits for the launch view's account to list its
        // folders; one that is offline with nothing cached never does.
        if start_pending {
            let s = sidebar.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(5, move || {
                let _ = s.send(SidebarInput::DropStart);
            });
        }
        // The peek panel's rows: a second, always-expanded instance.
        let peek_sidebar = Sidebar::builder()
            .launch(SidebarInit {
                collapsed: false,
                mirror: true,
                unified_expanded,
                filtered_expanded,
                tags_expanded,
                starred_expanded,
                sent_expanded,
                drafts_expanded,
                archive_expanded,
                junk_expanded,
                trash_expanded,
                favorites_expanded,
                smart_expanded,
                contacts_expanded,
                contacts_folded: contacts_folded.clone(),
                show_attachments,
                start: None,
            })
            .forward(sender.input_sender(), sidebar_output_msg);

        let message_list =
            MessageList::builder()
                .launch(())
                .forward(sender.input_sender(), |out| match out {
                    MessageListOutput::Selected {
                        message,
                        thread,
                        solo,
                    } => AppMsg::MessageSelected {
                        message,
                        thread,
                        solo,
                    },
                    MessageListOutput::SelectionKeys(keys) => AppMsg::SelectionKeys(keys),
                    MessageListOutput::Checked(count) => AppMsg::ListChecked(count),
                    MessageListOutput::ViewMissed => AppMsg::EmitUnifiedNow,
                    MessageListOutput::MoveMessages {
                        account_id,
                        dest,
                        messages,
                    } => AppMsg::MoveMessagesTo {
                        account_id,
                        dest,
                        messages,
                    },
                    MessageListOutput::DeleteThread { messages } => AppMsg::DeleteThread(messages),
                    MessageListOutput::ThreadGrew { message, thread } => AppMsg::ThreadGrew {
                        message: Box::new(message),
                        thread,
                    },
                    MessageListOutput::ThreadsListed { groups } => AppMsg::ThreadsListed { groups },
                    MessageListOutput::CountChanged(text) => AppMsg::ListCount(text),
                    MessageListOutput::Activated { message, thread } => {
                        AppMsg::OpenMessageWindow { message, thread }
                    }
                    MessageListOutput::Action {
                        action,
                        message,
                        conversation,
                    } => AppMsg::RowAction {
                        action,
                        message,
                        conversation,
                    },
                    MessageListOutput::SetTag {
                        message,
                        keyword,
                        add,
                    } => AppMsg::SetTag {
                        message,
                        keyword,
                        add,
                    },
                    MessageListOutput::SetTagMany {
                        messages,
                        keyword,
                        add,
                    } => AppMsg::SetTagMany {
                        messages,
                        keyword,
                        add,
                    },
                    MessageListOutput::Bulk { action, messages } => {
                        AppMsg::Bulk { action, messages }
                    }
                    MessageListOutput::MoveTo {
                        messages,
                        offer_whole,
                        x,
                        y,
                    } => AppMsg::ListMoveTo {
                        messages,
                        offer_whole,
                        x,
                        y,
                    },
                    MessageListOutput::SelectionCleared => AppMsg::ClearReader,
                    MessageListOutput::OpenedByKey { already_open } => {
                        AppMsg::OpenedByKey { already_open }
                    }
                    MessageListOutput::SearchActive(active) => AppMsg::SearchActive(active),
                    MessageListOutput::SortChanged(key) => AppMsg::ListSortChanged(key),
                    MessageListOutput::ColumnWidths(widths) => AppMsg::ListColumnWidths(widths),
                    MessageListOutput::CustomizeColumns => AppMsg::CustomizeColumns,
                });

        let message_view =
            MessageView::builder()
                .launch(())
                .forward(sender.input_sender(), |out| match out {
                    MessageViewOutput::AllowSender(addr) => AppMsg::AllowSender(addr),
                    MessageViewOutput::SetRemote {
                        account_id,
                        id,
                        show,
                    } => AppMsg::SetRemoteContent {
                        account_id,
                        id,
                        show,
                    },
                    MessageViewOutput::OpenWindow(m) => AppMsg::OpenMessageWindow {
                        message: *m,
                        thread: Vec::new(),
                    },
                    MessageViewOutput::CardAction { action, message } => {
                        AppMsg::CardAction { action, message }
                    }
                    MessageViewOutput::ContactSender(m) => AppMsg::CardContact(m),
                    MessageViewOutput::AttachmentAction {
                        account_id,
                        id,
                        index,
                        save,
                    } => AppMsg::CardAttachment {
                        account_id,
                        id,
                        index,
                        save,
                    },
                    MessageViewOutput::CardMenu { message, x, y, hit } => AppMsg::CardMenu {
                        message,
                        x,
                        y,
                        hit,
                        popout: None,
                    },
                    MessageViewOutput::CardMoveTo { message, x, y } => AppMsg::ListMoveTo {
                        messages: vec![*message],
                        offer_whole: false,
                        x,
                        y,
                    },
                    MessageViewOutput::MarkSeen { account_id, id } => {
                        AppMsg::ThreadMessageSeen { account_id, id }
                    }
                    MessageViewOutput::SelectCards(keys) => AppMsg::SelectCards(keys),
                    MessageViewOutput::ComposeTo(addr) => AppMsg::ComposeTo(addr),
                    MessageViewOutput::AddContactAddr(addr) => AppMsg::AddContactAddr(addr),
                    MessageViewOutput::ReloadBody(m) => AppMsg::ReloadBody(m),
                    MessageViewOutput::Notice(text) => AppMsg::Notice(text),
                    MessageViewOutput::ReaderMode(on) => AppMsg::SetReaderMode(on),
                    MessageViewOutput::Zoom(step) => AppMsg::ZoomMessage(step),
                    MessageViewOutput::Unsubscribe { message, info } => {
                        AppMsg::Unsubscribe { message, info }
                    }
                    MessageViewOutput::InviteAction {
                        message,
                        invite,
                        action,
                    } => AppMsg::InviteAction {
                        message,
                        invite,
                        action,
                    },
                });

        // The drawer owns a Paned whose top pane is the reader body, so hand it
        // the message-view widget to dock beneath.
        let attachment_drawer = AttachmentDrawer::builder()
            .launch(crate::ui::attachment_drawer::DrawerInit {
                state: config::load_drawer_state(),
                reader: message_view.widget().clone().upcast(),
            })
            .forward(sender.input_sender(), |out| match out {
                crate::ui::attachment_drawer::DrawerOutput::ShowLightbox { items, start } => {
                    AppMsg::ShowLightbox { items, start }
                }
                crate::ui::attachment_drawer::DrawerOutput::ShowInMessage(att) => {
                    AppMsg::ShowAttachmentInMessage(att)
                }
                crate::ui::attachment_drawer::DrawerOutput::DeleteFromServer(att) => {
                    AppMsg::DeleteDrawerAttachment(att)
                }
            });

        let lightbox = crate::ui::lightbox::Lightbox::builder().launch(()).detach();

        let gallery = AttachmentsGallery::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                GalleryOutput::OpenMessage {
                    account_id,
                    folder_path,
                    uid,
                } => AppMsg::OpenAttachmentMessage {
                    account_id,
                    folder_path,
                    uid,
                },
                GalleryOutput::Load(req) => AppMsg::GalleryLoad(Box::new(req)),
                GalleryOutput::Fetch {
                    account_id,
                    folder_path,
                    uid,
                } => AppMsg::GalleryFetch {
                    account_id,
                    folder_path,
                    uid,
                },
                GalleryOutput::DeleteFromServer {
                    account_id,
                    folder_path,
                    uid,
                    name,
                    size,
                } => AppMsg::AskDeleteAttachment(AttachmentTarget {
                    account_id,
                    message_id: uid,
                    path: folder_path,
                    uid,
                    name,
                    size,
                }),
            },
        );

        // Built here (not in the model literal) because the contacts page
        // mounts it over its detail pane at launch.
        let contacts_compose_revealer = {
            let r = gtk::Revealer::new();
            r.set_transition_type(gtk::RevealerTransitionType::SlideDown);
            r.set_transition_duration(300);
            r.set_reveal_child(false);
            r.set_can_target(false);
            r
        };
        let contacts_page = ContactsPage::builder()
            .launch(contacts_compose_revealer.clone())
            .forward(sender.input_sender(), |out| match out {
                ContactsPageOutput::Compose(email) => AppMsg::ComposeTo(email),
                ContactsPageOutput::ComposeBcc(field) => AppMsg::ComposeBcc(field),
                ContactsPageOutput::ToggleSidebar => AppMsg::ToggleSidebar,
                ContactsPageOutput::SaveContact { book_uid, vcard } => {
                    AppMsg::SaveContact { book_uid, vcard }
                }
                ContactsPageOutput::CreateContact { book_uid, vcard } => {
                    AppMsg::CreateContact { book_uid, vcard }
                }
                ContactsPageOutput::SaveGroup {
                    book_uid,
                    format,
                    original,
                    edit,
                } => AppMsg::SaveGroup {
                    book_uid,
                    format,
                    original,
                    edit,
                },
                ContactsPageOutput::DeleteContacts(keys) => AppMsg::DeleteContacts(keys),
                ContactsPageOutput::MoveGroup {
                    from,
                    uid,
                    to,
                    format,
                    edit,
                } => AppMsg::MoveGroup {
                    from,
                    uid,
                    to,
                    format,
                    edit,
                },
                ContactsPageOutput::MoveContacts { to, cards } => {
                    AppMsg::MoveContacts { to, cards }
                }
                ContactsPageOutput::CopyContacts { to, cards } => {
                    AppMsg::CopyContacts { to, cards }
                }
                ContactsPageOutput::RenameBook { uid, name } => AppMsg::RenameBook { uid, name },
                ContactsPageOutput::Tree(tree) => AppMsg::ContactTree(tree),
                ContactsPageOutput::ImportContacts(paths) => AppMsg::ImportContacts(paths),
                ContactsPageOutput::WantSuggestions => AppMsg::ContactSuggestions,
                ContactsPageOutput::ShowPhoto { name, data } => {
                    // The lightbox routes by extension — a bare contact
                    // name sent the JPEG down the PDF path, where poppler
                    // hung the UI trying to parse it. Name it by content.
                    let name = format!("{name}.{}", crate::models::image_ext(&data));
                    AppMsg::ShowLightbox {
                        items: vec![crate::models::Attachment { name, data }],
                        start: 0,
                    }
                }
            });

        let notifications = NotificationCenter::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                NotifyOutput::CountChanged(n) => AppMsg::NotifyCount(n),
                NotifyOutput::ExportLog => AppMsg::Pref(PrefOutput::ExportLog),
                NotifyOutput::Shown(shown) => AppMsg::StatusBarShown(shown),
            },
        );

        // Sectioned: settings / printing / window & help / quit.
        let menu = gtk::gio::Menu::new();
        let help_menu = gtk::gio::Menu::new();
        let model_undo_menu = gtk::gio::Menu::new();
        {
            // #200: the only place undo and redo are visible. They name the
            // action they would reverse ("Undo Archive") and grey out when
            // there is nothing on the stack.
            menu.append_section(None, &model_undo_menu);

            let settings = gtk::gio::Menu::new();
            settings.append(Some(i18n("Settings").as_str()), Some("win.accounts"));
            menu.append_section(None, &settings);

            // The sidebar's account sections, for those who work from the
            // unified section alone (also Settings → Sidebar).
            let sidebar = gtk::gio::Menu::new();
            sidebar.append_item(&keyed_item(
                &i18n("Show Accounts"),
                "app.show-accounts",
                "<Ctrl><Shift>a",
            ));
            // Focus Mode: the distraction-free layout (also Ctrl+Shift+F
            // and Settings → Appearance, where its parts are chosen).
            sidebar.append(Some(i18n("Focus Mode").as_str()), Some("app.focus-mode"));
            menu.append_section(None, &sidebar);

            let printing = gtk::gio::Menu::new();
            printing.append(
                Some(i18n("Print Preview…").as_str()),
                Some("win.print-preview"),
            );
            printing.append(Some(i18n("Print Message…").as_str()), Some("win.print"));
            menu.append_section(None, &printing);

            menu.append_section(None, &help_menu);

            // Last, where a Quit item belongs.
            let quit = gtk::gio::Menu::new();
            quit.append(Some(i18n("Quit").as_str()), Some("app.quit"));
            menu.append_section(None, &quit);
        }

        let show_accounts = prefs.show_accounts;
        let show_accounts_action = gtk::gio::SimpleAction::new_stateful(
            "show-accounts",
            None,
            &show_accounts.to_variant(),
        );
        {
            let s = sender.clone();
            show_accounts_action.connect_change_state(move |action, value| {
                if let Some(on) = value.and_then(|v| v.get::<bool>()) {
                    action.set_state(&on.to_variant());
                    s.input(AppMsg::Pref(PrefOutput::SetShowAccounts(on)));
                }
            });
        }

        let focus_action =
            gtk::gio::SimpleAction::new_stateful("focus-mode", None, &focus.enabled.to_variant());
        {
            let s = sender.clone();
            focus_action.connect_change_state(move |action, value| {
                if let Some(on) = value.and_then(|v| v.get::<bool>()) {
                    action.set_state(&on.to_variant());
                    let mut focus = config::load_focus_mode();
                    focus.enabled = on;
                    s.input(AppMsg::Pref(PrefOutput::SetFocusMode(focus)));
                }
            });
        }

        let reader_override = config::load_reader_override();
        let plain_style = config::load_plain_style();
        let mut model = AppModel {
            workers: HashMap::new(),
            mid_searches: HashMap::new(),
            tag_scan: None,
            tag_scan_gen: 0,
            demo_config: if demo_mode() && config.is_empty() {
                demo_account_configs_saved()
            } else {
                Vec::new()
            },
            config,
            window: root.clone(),
            prefs: None,
            accounts_win: None,
            accounts_seed: None,
            composers: Vec::new(),
            reader_compose: None,
            send_morph: crate::ui::send_morph::SendMorph::new(),
            arriving: None,
            morph_pending: None,
            self_sender: sender.input_sender().clone(),
            provisionals: HashMap::new(),
            draining_composers: Vec::new(),
            reader_split_top: {
                let r = gtk::Revealer::new();
                r.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                r.set_transition_duration(300);
                r.set_reveal_child(false);
                r
            },
            reader_split_bottom: {
                let r = gtk::Revealer::new();
                r.set_transition_type(gtk::RevealerTransitionType::SlideUp);
                r.set_transition_duration(300);
                r.set_reveal_child(false);
                r
            },
            reader_split_lower: {
                let p = gtk::Paned::new(gtk::Orientation::Vertical);
                // Same invisible separator as the outer split: the bottom
                // panel's grab pill is its visible affordance too.
                p.add_css_class("reader-split");
                p
            },
            split_close_anim: std::rc::Rc::new(std::cell::RefCell::new(None)),
            reader_header: std::cell::OnceCell::new(),
            toasts: std::cell::OnceCell::new(),
            outgoing: Default::default(),
            send_ticket: 0,
            reader_split: {
                let p = gtk::Paned::new(gtk::Orientation::Vertical);
                // The separator itself is styled invisible — painted the
                // composer's own ground, so the panel reads as running to its
                // edge. The visible affordance is the floating grab pill the
                // split slot overlays on the composer (open_inline_reply).
                p.add_css_class("reader-split");
                p
            },
            window_drop_zones: None,
            reader_compose_revealer: {
                let r = gtk::Revealer::new();
                r.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                r.set_transition_duration(300);
                r.set_reveal_child(false);
                r
            },
            contacts_compose_revealer,
            next_compose_id: 1,
            menu,
            help_menu,
            status_bar_shown: false,
            undo_menu: model_undo_menu.clone(),
            accounts: Vec::new(),
            folders: HashMap::new(),
            account_order: order,
            collapsed,
            folders_expanded,
            tree_collapsed,
            folder_order,
            selected: None,
            attachments: Vec::new(),
            lightbox,
            attachments_loading: false,
            reader_actions_collapsed: false,
            reader_toolbar: config::load_reader_toolbar(),
            reader_toolbar_widgets: std::cell::OnceCell::new(),
            reader_overflow_btn: {
                let b = gtk::Button::from_icon_name("view-more-horizontal-symbolic");
                b.set_tooltip_text(Some(i18n("Actions").as_str()));
                b.add_css_class("flat");
                b.set_visible(false);
                b
            },
            reader_tag_btn: {
                let b = gtk::Button::from_icon_name("tag-outline-symbolic");
                b.set_tooltip_text(Some(i18n("Tags").as_str()));
                b.add_css_class("flat");
                b
            },
            reader_move_btn: {
                let b = gtk::Button::from_icon_name("folder-symbolic");
                b.set_tooltip_text(Some(i18n("Move To…").as_str()));
                b.add_css_class("flat");
                b
            },
            attachment_cache: crate::ram_cache::RamCache::new(ATTACHMENT_CACHE_BUDGET),
            unified: false,
            unified_view: UnifiedView::Kind(FolderKind::Inbox),
            unified_account: None,
            unified_slices: HashMap::new(),
            unified_boot_requested: HashSet::new(),
            sent_index_requested: HashSet::new(),
            message_cache: HashMap::new(),
            search_gen: 0,
            indexed_folders: HashSet::new(),
            body_cache: crate::ram_cache::RamCache::new(BODY_CACHE_BUDGET),
            sender_cache: HashMap::new(),
            pending_draft: None,
            pending_draft_token: None,
            pending_reply: None,
            pending_draft_pick: None,
            pending_template_pick: None,
            pending_edit_as_new: None,
            pending_forward: None,
            pending_notified_reply: None,
            files_prefs: config::load_files_prefs(),
            link_browser: {
                // The launcher reads its choice from here, not from disk, so
                // it is handed over before the first link can be clicked.
                let choice = config::load_privacy().link_browser;
                crate::ui::launch::set_browser(&choice);
                choice
            },
            popouts: HashMap::new(),
            current_thread: Vec::new(),
            listed_thread: Vec::new(),
            reader_focus_pending: false,
            list_selection: Vec::new(),
            list_slots: Vec::new(),
            checked: None,
            compose_waiting: false,
            compose_now: false,
            list_heard: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            compose_history: (None, None),
            carried_bodies: HashMap::new(),
            remote_override: HashMap::new(),
            carried_threads: HashMap::new(),
            undo_action: None,
            redo_action: None,
            bulk_pending: 0,
            related_id_seq: u32::MAX,
            related_ids: HashMap::new(),
            folder_unread: HashMap::new(),
            pending_seen: HashMap::new(),
            pending_tags: HashMap::new(),
            pending_answers: HashMap::new(),
            dropped_unread: Default::default(),
            pending_moves: std::cell::RefCell::new(HashMap::new()),
            transfers: HashMap::new(),
            next_transfer: 0,
            next_local_id: u32::MAX - 1,
            pending_saves: HashMap::new(),
            transfer_tally: TransferTally::default(),
            sidebar_split: None,
            app_title: None,
            sidebar_menu: None,
            sidebar_header: None,
            sidebar_refresh: {
                let b = gtk::Button::new();
                b.set_tooltip_text(Some(i18n("Refresh or long-press for Status Bar").as_str()));
                b.add_css_class("flat");
                b.set_valign(gtk::Align::Center);
                b
            },
            sidebar_refresh_stack: {
                let s = gtk::Stack::new();
                s.set_transition_type(gtk::StackTransitionType::Crossfade);
                s
            },
            sidebar_refresh_spinner: gtk::Spinner::new(),
            sidebar_collapsed: icon_only,
            sidebar_anim: None,
            auto_rail: false,
            rail_active: rail_now,
            sidebar_peek: false,
            peek_transition: std::rc::Rc::new(std::cell::Cell::new(false)),
            peek_split: None,
            peek_refresh_stack: {
                let s = gtk::Stack::new();
                s.set_transition_type(gtk::StackTransitionType::Crossfade);
                s
            },
            peek_refresh_spinner: gtk::Spinner::new(),
            sidebar_hover_expand: prefs.sidebar_hover_expand,
            remember_sidebar,
            remember_rail,
            unified_expanded,
            filtered_expanded,
            tags_expanded,
            starred_expanded,
            sent_expanded,
            drafts_expanded,
            archive_expanded,
            junk_expanded,
            trash_expanded,
            favorites,
            favorites_expanded,
            smart_expanded,
            contacts_expanded,
            contacts_folded,
            filtered_expanded_accounts,
            tags_expanded_accounts,
            rail_dots: prefs.rail_dots,
            seen_counts: prefs.seen_counts,
            unread_seen: std::cell::RefCell::new(config::load_unread_seen()),
            rail_fold: prefs.rail_fold,
            app_theme: prefs.app_theme,
            text_scale: config::load_text_scale(),
            theme: config::load_theme(),
            current: None,
            allowed_senders: prefs.allowed_senders,
            unsubscribed: config::load_unsubscribed(),
            invite_answers: config::load_invite_answers(),
            auto_remote_content: prefs.auto_remote_content,
            show_remote_banner: prefs.show_remote_banner,
            show_spoof_banner: prefs.show_spoof_banner,
            blacklist: prefs.blacklist,
            palette_collapse_secs: prefs.palette_collapse_secs,
            card_palette_collapse_secs: prefs.card_palette_collapse_secs,
            gravatar: prefs.gravatar,
            avatars: prefs.avatars,
            own_mailbox_face: prefs.own_mailbox_face,
            tag_row_tint: prefs.tag_row_tint,
            contact_book: prefs.contact_book.clone(),
            sender_logos: prefs.sender_logos,
            date_style: config::load_date_format().0,
            clock_style: config::load_date_format().1,
            fetch_interval_secs: prefs.fetch_interval_secs,
            push: prefs.push,
            metered_save_data: prefs.metered_save_data,
            notifications_enabled: prefs.notifications,
            notification_content: prefs.notification_content,
            notification_buttons: config::load_notification_buttons(),
            show_attachments,
            settings_open_accounts: prefs.settings_open_accounts,
            last_settings_page: None,
            list_count: String::new(),
            preview_lines: config::load_preview_lines(),
            list_layout: config::load_list_layout(),
            list_columns: config::load_list_columns(),
            list_headings: config::load_privacy().list_headings,
            list_column_scroll: config::load_privacy().list_column_scroll,
            list_group_by_date: config::load_privacy().list_group_by_date,
            list_pills_below: config::load_privacy().list_pills_below,
            list_search_open: config::load_privacy().list_search_open,
            contacts_search_open: config::load_privacy().contacts_search_open,
            list_search_bottom: config::load_privacy().list_search_bottom,
            list_bulk_bar_bottom: config::load_privacy().list_bulk_bar_bottom,
            list_column_widths: config::load_list_column_widths(),
            list_wide: config::load_list_pane_width() >= LIST_WIDE_ON,
            shortcuts_win: None,
            run_in_background: std::rc::Rc::new(std::cell::Cell::new(
                config::load_privacy().run_in_background,
            )),
            autostart: config::load_autostart(),
            tray_enabled: prefs.tray,
            tray_icon: prefs.tray_icon,
            tray_mail: prefs.tray_mail,
            launcher_badge,
            arrivals: Default::default(),
            app_icon: crate::app_icon::init_on_startup(),
            restart_pending: false,
            tray: None,
            tray_mail_key: std::cell::RefCell::new(None),
            single_key: std::rc::Rc::new(std::cell::Cell::new(
                config::load_privacy().single_key_shortcuts,
            )),
            gmail_keys: std::rc::Rc::new(std::cell::Cell::new(config::load_privacy().gmail_keys)),
            keep_downloads_days: config::load_privacy().keep_downloads_days,
            threading: prefs.threading,
            thread_render_queued: false,
            thread_opened_at: None,
            thread_related_pending: false,
            thread_painted: false,
            thread_cache: HashMap::new(),
            deleting_attachments: Vec::new(),
            showcase_confirm_delete: false,
            thread_cache_order: Vec::new(),
            thread_key: None,
            threads_expanded: prefs.threads_expanded,
            thread_newest_first: prefs.thread_newest_first,
            fold_messages: prefs.fold_messages,
            always_show_recipients: prefs.always_show_recipients,
            pgp_labels: prefs.pgp_labels,
            show_unified_pref: prefs.show_unified,
            unified_chips: config::load_unified_chips(),
            section_style: prefs.section_style,
            filtered_placement: prefs.filtered_placement,
            tags_placement: prefs.tags_placement,
            favorites_placement: prefs.favorites_placement,
            contacts_placement: prefs.contacts_placement,
            unified_kinds: prefs.unified_kinds,
            show_accounts,
            show_accounts_action,
            focus,
            focus_action,
            list_header_widgets: std::cell::OnceCell::new(),
            chevrons_left: prefs.chevrons_left,
            account_click_folds: prefs.account_click_folds,
            start_view,
            folder_sort: prefs.folder_sort,
            console_mode: prefs.console_mode,
            status_bar_top: prefs.status_bar_top,
            read_mark: prefs.read_mark,
            // The demo (no accounts of its own) ships with tags and filter
            // rules, so its sidebar shows the Tags and Filtered Folders
            // sections; a staged tags.toml / filters.toml still wins.
            saved_addresses: crate::contacts::SavedAddresses::default(),
            quick_filters: QuickFilters::default(),
            filters: {
                let filters = config::load_filters();
                if filters.is_empty() && demo_data {
                    demo_filters()
                } else {
                    filters
                }
            },
            tags: {
                let tags = config::load_tags();
                if tags.is_empty() && demo_data {
                    demo_tags()
                } else {
                    tags
                }
            },
            tag_view: None,
            tag_view_cache: HashMap::new(),
            tag_view_loading: false,
            tag_view_dirty: false,
            smart_folders: config::load_smart_folders(),
            smart_view: None,
            smart_view_cache: HashMap::new(),
            smart_view_loading: false,
            smart_view_dirty: false,
            unified_starred_loading: false,
            unified_starred_dirty: false,
            unified_starred_rows: HashMap::new(),
            unified_starred_due: false,
            preload_due: false,
            prebuild_queue: Vec::new(),
            prebuild_due: false,
            smart_counts_due: false,
            smart_counts_running: false,
            keyword_sync_at: HashMap::new(),
            tag_provider: crate::ui::DisplayCss::new(),
            // Demo mode gets a cache that never touches disk, seeded with the
            // sample mail: the gallery's paging, search and sort are database
            // queries, so the demo has to run them against a real database or
            // it would be exercising nothing. Not with real accounts: their
            // workers keep the real one, and the lists read it.
            cache: if demo_data {
                let c = crate::cache::Cache::in_memory().ok();
                if let Some(c) = c.as_ref() {
                    crate::backend::seed_demo_cache(c);
                }
                c
            } else {
                crate::cache::Cache::open().ok()
            },
            filter_run: None,
            filter_moved: Default::default(),
            body_hits: Default::default(),
            single_message_card: prefs.single_message_card,
            reader_mode: prefs.reader_mode,
            zoom: config::load_reader_zoom(),
            zoom_default: config::load_reader_zoom(),
            reader_switch: prefs.reader_switch,
            theme_switch: prefs.theme_switch,
            desktop_icons: prefs.desktop_icons,
            reader_default: prefs.reader_default,
            card_attachments: prefs.card_attachments,
            drawer_enabled: prefs.attachment_drawer,
            thread_expansion: prefs.thread_expansion,
            thread_row_newest: prefs.thread_row_newest,
            confirm_thread_delete: prefs.confirm_thread_delete,
            selection_from_cards: false,
            card_actions_hover: prefs.card_actions_hover,
            card_actions_auto: prefs.card_actions_auto,
            list_palette: prefs.list_palette,
            list_palette_hover: prefs.list_palette_hover,
            card_palette_menu: prefs.card_palette_menu,
            swipe_enabled: prefs.swipe_enabled,
            swipe_reversed: prefs.swipe_reversed,
            swipe_sensitivity: config::load_swipe_sensitivity(),
            compose_inline: prefs.compose_inline,
            reply_inline: prefs.reply_inline,
            reply_fields: prefs.reply_fields,
            compose_default_from: prefs.compose_default_from,
            paste_plain: prefs.paste_plain,
            return_paragraph: prefs.return_paragraph,
            toolbar_expanded: prefs.toolbar_expanded,
            compose_font: prefs.compose_font,
            compose_font_size: prefs.compose_font_size,
            print_options: prefs.print,
            compose_format: config::load_compose_format(),
            follow_plain: prefs.follow_plain,
            reply_position: prefs.reply_position,
            signature_position: prefs.signature_position,
            signature_dashes: prefs.signature_dashes,
            undo_send_secs: prefs.undo_send_secs,
            spellcheck: prefs.spellcheck,
            spellcheck_langs: prefs.spellcheck_langs,
            message_theme: prefs.message_theme,
            override_fonts: reader_override.0,
            reader_font: reader_override.1,
            override_colors: reader_override.2,
            plain_monospace: plain_style.0,
            plain_font: plain_style.1,
            auto_fetch_source: None,
            last_wake: None,
            notifications,
            welcome: None,
            notify_count: 0,
            busy: HashSet::new(),
            sidebar,
            peek_sidebar,
            message_list,
            message_view,
            attachment_drawer,
            gallery,
            showing_gallery: false,
            contacts_page,
            showing_contacts: false,
            showing_outbox: false,
            outbox_by_account: HashMap::new(),
            server_identities: HashMap::new(),
            unified_emit_queued: std::cell::Cell::new(false),
            unified_dirty: std::cell::Cell::new(false),
            thread_links_due: std::cell::Cell::new(false),
            unified_emitted_at: std::cell::Cell::new(std::time::Instant::now()),
            list_fp: std::cell::Cell::new(None),
            unified_unfilled: std::collections::HashSet::new(),
            gallery_total: 0,
            gallery_scan_left: HashMap::new(),
        };
        model.prime_from_cache();
        model.refresh_tag_css();
        if model.filters.iter().any(|r| r.needs_contacts()) {
            model.saved_addresses.refresh();
        }
        model
            .message_list
            .emit(MessageListInput::SetTags(model.tags.clone()));
        model
            .message_view
            .emit(MessageViewInput::SetTags(model.tags.clone()));
        // What each mailbox of the user's own shows (#189), before the first
        // cached message is painted: the sidebar refreshes this whenever the
        // accounts change, but nothing has rebuilt it yet at this point.
        model.refresh_own_faces();
        model.warm_own_gravatars(&sender);
        model.spawn_workers(&sender);
        // Downloaded mail past Settings → System → Keep downloaded mail goes
        // a little after start, once the first syncs are under way, and
        // every six hours after (#394).
        gtk::glib::timeout_add_seconds_local_once(120, || {
            prune_downloads(config::load_privacy().keep_downloads_days);
            gtk::glib::timeout_add_seconds_local(6 * 3600, || {
                prune_downloads(config::load_privacy().keep_downloads_days);
                gtk::glib::ControlFlow::Continue
            });
        });
        // GNOME keeps a notification after the app that posted it has quit,
        // and a new instance knows nothing of what it pointed at, so it
        // could only linger (#333). New mail since is notified afresh.
        for a in &model.accounts {
            crate::notify::withdraw_mail(a.id);
        }
        if model.tray_enabled {
            model.start_tray(&sender);
        }
        // Nothing keeps a CardDAV or Online Accounts address book in step
        // unless an app holds it open, and off GNOME there may be no other
        // (#408). A moment after the window, not in its way.
        if !demo_mode() {
            sync_contact_books(&sender, false, 5000);
        }
        // Contacts is read and listed in the background, so the first visit
        // shows it at once (#477); a visit re-reads it to catch up.
        reload_contacts(&sender, 4000);
        // Refresh visible avatars when the GNOME Contacts photo index
        // changes (first load finishing, or an EDS/CardDAV sync).
        crate::contacts::watch_photo_changes({
            let input = sender.input_sender().clone();
            move || {
                let _ = input.send(AppMsg::ContactPhotosChanged);
            }
        });
        // Watch GNOME Online Accounts so a change there (account removed, Mail
        // toggled) is reflected in Hylki live, no restart needed. The watcher
        // debounces signal bursts and snapshots GOA on its own thread;
        // reconciliation happens on GoaChanged.
        crate::goa::watch_changes({
            let s = sender.input_sender().clone();
            move |state| {
                let _ = s.send(AppMsg::GoaChanged(state));
            }
        });
        // Watch for resume-from-sleep: suspended IMAP sockets die silently, so
        // on wake we reconnect every worker and refresh, otherwise no new mail
        // arrives until the next auto-fetch (#322). logind is instant but out
        // of the Flatpak's reach; the clock check covers it there.
        crate::power::watch_resume({
            let s = sender.input_sender().clone();
            move || {
                let _ = s.send(AppMsg::SystemResumed);
            }
        });
        crate::power::watch_clock_gap({
            let s = sender.input_sender().clone();
            move || {
                let _ = s.send(AppMsg::SystemResumed);
            }
        });
        // A dock that has just (re)started holds no badge until we send it.
        crate::launcher_badge::watch_dock({
            let s = sender.input_sender().clone();
            move || {
                let _ = s.send(AppMsg::LauncherCountLost);
            }
        });
        crate::power::watch_network({
            let s = sender.input_sender().clone();
            move |change| {
                let _ = s.send(match change {
                    crate::power::NetworkChange::Back => AppMsg::NetworkBack,
                    crate::power::NetworkChange::Lost => AppMsg::NetworkLost,
                    crate::power::NetworkChange::Metered(_) => AppMsg::MeteredChanged,
                });
            }
        });
        // With no accounts, no worker events will populate the sidebar, so render
        // its empty state (the "Add first account" prompt) up front — and greet
        // a first run with the welcome wizard (src/ui/welcome.rs).
        // A restart the wizard's language pick asked for: the wizard comes
        // back once, whatever mode this is; the flag must not reach a
        // later restart (the icon change's), so it is cleared here.
        let wizard_again = std::env::var_os(crate::WIZARD_AGAIN_VAR).is_some();
        if wizard_again {
            std::env::remove_var(crate::WIZARD_AGAIN_VAR);
        }
        if model.config.is_empty() || std::env::var("HYLKI_WELCOME").is_ok() || wizard_again {
            model.rebuild_sidebar();
            // HYLKI_WELCOME=1 forces the wizard over an existing config, for
            // design review and screenshots. A wizard already completed once
            // (Start Reading pressed, even with no account added) doesn't
            // come back on its own — a restart right after it must not loop.
            if std::env::var("HYLKI_WELCOME").is_ok()
                || wizard_again
                || (!demo_mode() && !config::wizard_completed())
            {
                model.open_wizard(&sender);
            }
        }
        model
            .message_view
            .emit(MessageViewInput::SetReadMark(model.read_mark));
        model
            .message_list
            .emit(MessageListInput::SetGravatar(model.gravatar));
        model
            .message_list
            .emit(MessageListInput::SetAvatars(model.list_avatars()));
        // The formatter is a free function, so the preference has to be handed to
        // it before anything draws a date.
        model
            .message_list
            .emit(MessageListInput::SetSenderLogos(model.sender_logos));
        crate::datefmt::set_style(model.date_style, model.clock_style);
        crate::ui::print_preview::set_options(model.print_options);
        model.message_list.emit(MessageListInput::SetLook {
            avatars: model.list_avatars(),
            preview_lines: model.list_preview_lines(),
            subject: model.list_show_subject(),
            animate: false,
        });
        model.message_view.emit(MessageViewInput::SetFoldLook {
            preview_lines: model.list_preview_lines(),
        });
        model.push_single_line();
        model
            .message_list
            .emit(MessageListInput::SetColumns(model.list_columns.clone()));
        model
            .message_list
            .emit(MessageListInput::SetHeadings(model.list_headings));
        model
            .message_list
            .emit(MessageListInput::SetColumnScroll(model.list_column_scroll));
        model
            .message_list
            .emit(MessageListInput::SetSearchPinned(model.list_search_open));
        model.push_list_bars();
        model.contacts_page.emit(ContactsPageInput::SetSearchPinned(
            model.contacts_search_open,
        ));
        model.contacts_page.emit(ContactsPageInput::SetBookPicker(
            !model.contacts_placement.shown(),
        ));
        model.message_list.emit(MessageListInput::SetColumnWidths(
            model.list_column_widths.clone(),
        ));
        model.sidebars_emit(SidebarInput::SetFocus {
            hide_accounts: model.focus.active(config::FocusPart::HideAccounts),
            fold_unified: model.focus.active(config::FocusPart::FoldUnified),
            animate: false,
        });
        model
            .message_list
            .emit(MessageListInput::SetThreading(model.threading));
        model
            .message_list
            .emit(MessageListInput::SetThreadExpansion(model.thread_expansion));
        model
            .message_list
            .emit(MessageListInput::SetThreadRowNewest(
                model.thread_row_newest,
            ));
        model
            .message_list
            .emit(MessageListInput::SetListPalette(model.list_palette));
        model
            .message_list
            .emit(MessageListInput::SetThreadsExpanded(model.threads_expanded));
        model
            .message_list
            .emit(MessageListInput::SetPaletteCollapse(
                model.palette_collapse_secs,
            ));
        model.message_view.emit(MessageViewInput::SetContentTheme(
            model.message_theme.dark_override(),
        ));
        model
            .message_view
            .emit(MessageViewInput::SetReaderStyle(model.reader_style()));
        model
            .message_view
            .emit(MessageViewInput::SetAlwaysShowRecipients(
                model.always_show_recipients,
            ));
        model
            .message_view
            .emit(MessageViewInput::SetPgpLabels(model.pgp_labels));
        model
            .message_view
            .emit(MessageViewInput::SetSingleMessageCard(
                model.single_message_card,
            ));
        model
            .message_view
            .emit(MessageViewInput::SetFoldMessages(model.fold_messages));
        model.message_view.emit(MessageViewInput::SetReaderMode(
            model.effective_reader_mode(),
        ));
        model
            .message_view
            .emit(MessageViewInput::SetZoomDefault(model.zoom_default));
        model
            .message_view
            .emit(MessageViewInput::SetZoom(model.zoom));
        model
            .message_view
            .emit(MessageViewInput::SetReaderSwitchShown(model.reader_switch));
        model
            .message_view
            .emit(MessageViewInput::SetThemeSwitchShown(model.theme_switch));
        model.message_view.emit(MessageViewInput::SetReaderDefault(
            model.effective_reader_default(),
        ));
        model
            .message_view
            .emit(MessageViewInput::SetCardAttachmentsShown(
                model.card_attachments,
            ));
        model
            .message_view
            .emit(MessageViewInput::SetAttachmentDrawer(model.drawer_enabled));
        model
            .message_view
            .emit(MessageViewInput::SetUnsubscribed(model.unsubscribed_map()));
        model
            .message_view
            .emit(MessageViewInput::SetIdentities(model.identities_map()));
        model.message_view.emit(MessageViewInput::SetInviteAnswers(
            model.invite_answers_map(),
        ));
        model.arm_auto_fetch(&sender);

        // The app-wide theme choice must be in force before the first frame.
        apply_app_theme(model.app_theme);
        crate::text_scale::apply(model.text_scale);
        follow_motion_setting();
        let reader_tag_btn = model.reader_tag_btn.clone();
        let reader_move_btn = model.reader_move_btn.clone();
        let widgets = view_output!();
        let _ = model.reader_header.set(widgets.reader_header.clone());
        let _ = model.toasts.set(widgets.toasts.clone());
        // Right-click on the header's empty space (not a button) offers
        // the way to the toolbar editor.
        {
            let click = gtk::GestureClick::new();
            click.set_button(3);
            // Capture phase: the header's own window handle (a child, so
            // earlier in the bubble phase) would otherwise take the click
            // for the compositor's window menu and never let it through.
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let header: gtk::Widget = widgets.reader_header.clone().upcast();
            let s = sender.input_sender().clone();
            click.connect_pressed(move |g, _, x, y| {
                // Anything clickable under the pointer keeps its own
                // right-click (or lack of one).
                let mut w = header.pick(x, y, gtk::PickFlags::DEFAULT);
                while let Some(cur) = w {
                    if cur == header {
                        break;
                    }
                    if cur.is::<gtk::Button>() || cur.is::<gtk::WindowControls>() {
                        return;
                    }
                    w = cur.parent();
                }
                g.set_state(gtk::EventSequenceState::Claimed);
                let _ = s.send(AppMsg::ReaderToolbarMenu { x, y });
            });
            widgets.reader_header.add_controller(click);
        }
        // Collapse the reader header's actions into the overflow menu when the
        // pane can no longer fit the full row — squeezing it further must never
        // push the window controls off the right edge. The threshold is
        // measured, not assumed: the window-controls cost depends on the
        // user's decoration layout (GNOME ships one button, others run three),
        // so the row's cost is taken from the real headerbar now — while every
        // action is still visible, before the breakpoint could hide any — and
        // the controls' share is re-derived if the decoration layout changes.
        {
            // The controls' width comes from the headerbar's own live
            // GtkWindowControls children (a detached WindowControls never
            // populates its buttons and measures 0).
            fn measure_controls(w: &gtk::Widget, sum: &mut i32) {
                if let Some(c) = w.downcast_ref::<gtk::WindowControls>() {
                    *sum += c.measure(gtk::Orientation::Horizontal, -1).1;
                    return;
                }
                let mut child = w.first_child();
                while let Some(c) = child {
                    measure_controls(&c, sum);
                    child = c.next_sibling();
                }
            }
            let header: gtk::Widget = widgets.reader_header.clone().upcast();
            let full = header.measure(gtk::Orientation::Horizontal, -1).1;
            let mut controls = 0;
            measure_controls(&header, &mut controls);
            use config::ToolbarItem as T;
            let buttons: Vec<(T, gtk::Widget)> = vec![
                (T::Reply, widgets.tb_reply.clone().upcast()),
                (T::ReplyAll, widgets.tb_reply_all.clone().upcast()),
                (T::Forward, widgets.tb_forward.clone().upcast()),
                (T::Star, widgets.tb_star.clone().upcast()),
                (T::Archive, widgets.tb_archive.clone().upcast()),
                (T::Delete, widgets.tb_delete.clone().upcast()),
                (T::Spam, widgets.tb_spam.clone().upcast()),
                (T::ReadUnread, widgets.tb_read.clone().upcast()),
                (T::Tags, widgets.tb_tags.clone().upcast()),
                (T::MoveTo, widgets.tb_move.clone().upcast()),
                (T::Find, widgets.tb_find.clone().upcast()),
                (T::Print, widgets.tb_print.clone().upcast()),
            ];
            // One button's cost, from the buttons themselves (a hidden one —
            // Tags before any tag exists — measures 0 and is skipped); what
            // is left of the row after them and the controls is the base.
            //
            // Two different widths, and the difference is load-bearing. Each
            // button sits in a Focus Mode revealer, and a revealer that has
            // not been revealed yet measures 0 however wide its child is —
            // which is exactly the state this runs in, before
            // `sync_focus_chrome` opens them. So a button's own cost comes
            // from the button inside the revealer, while what is taken off
            // `full` is what the row counted for it *just now*: the revealer.
            // Subtracting the buttons from a row that never included them is
            // how the base went negative (and the fold stopped happening) the
            // moment Focus Mode wrapped the row in revealers.
            let mut packed_sum = 0;
            let mut button_w = 0;
            for (_, w) in &buttons {
                packed_sum += w.measure(gtk::Orientation::Horizontal, -1).1;
                let inner = w
                    .downcast_ref::<gtk::Revealer>()
                    .and_then(|r| r.child())
                    .unwrap_or_else(|| w.clone());
                let nat = inner.measure(gtk::Orientation::Horizontal, -1).1;
                if nat > 0 {
                    button_w = button_w.max(nat);
                }
            }
            let base = if full <= 0 {
                0
            } else {
                (full - controls - packed_sum).max(0)
            };
            let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MaxWidth,
                READER_ACTIONS_BREAKPOINT,
                adw::LengthUnit::Px,
            ));
            let s = sender.input_sender().clone();
            bp.connect_apply(move |_| {
                let _ = s.send(AppMsg::SetReaderActionsCollapsed(true));
            });
            let s = sender.input_sender().clone();
            bp.connect_unapply(move |_| {
                let _ = s.send(AppMsg::SetReaderActionsCollapsed(false));
            });
            widgets.reader_bin.add_breakpoint(bp.clone());
            let _ = model.reader_toolbar_widgets.set(ReaderToolbarWidgets {
                buttons,
                outbox: vec![
                    widgets.tb_outbox_edit.clone(),
                    widgets.tb_outbox_send.clone(),
                    widgets.tb_outbox_send_all.clone(),
                ],
                spinner: widgets.tb_spinner.clone().upcast(),
                bin: widgets.reader_bin.clone(),
                breakpoint: bp,
                base,
                button_w,
                controls: std::cell::Cell::new(controls),
            });
            // Packs the saved order and sets the real threshold.
            model.relayout_reader_toolbar();
            if let Some(settings) = gtk::Settings::default() {
                let s = sender.input_sender().clone();
                settings.connect_notify_local(Some("gtk-decoration-layout"), move |_, _| {
                    // Re-measure in an idle: the headerbar's own controls
                    // rebuild on this same notify, in unspecified order.
                    let header = header.clone();
                    let s = s.clone();
                    gtk::glib::idle_add_local_once(move || {
                        let mut controls = 0;
                        measure_controls(&header, &mut controls);
                        let _ = s.send(AppMsg::ReaderControlsChanged(controls));
                    });
                });
            }
            let s = sender.input_sender().clone();
            model.reader_overflow_btn.connect_clicked(move |_| {
                let _ = s.send(AppMsg::ReaderOverflowMenu);
            });
            let s = sender.input_sender().clone();
            model.reader_tag_btn.connect_clicked(move |_| {
                let _ = s.send(AppMsg::ReaderTagMenu);
            });
            let s = sender.input_sender().clone();
            model.reader_move_btn.connect_clicked(move |_| {
                let _ = s.send(AppMsg::MoveToMenu);
            });
        }
        // The inline compose/reply pane is an overlay over the WHOLE reader
        // pane — its header bar included: revealed, it slides down from the
        // top and covers everything (the composer's own header carries the
        // window decorations meanwhile). While closed it must not eat the
        // reader's clicks. Wrapping happens here because the view macro's
        // deep nesting makes an in-place Overlay unwieldy.
        model.reader_compose_revealer.set_can_target(false);
        {
            let pane = widgets.reader_bin.child().expect("reader pane");
            widgets.reader_bin.set_child(None::<&gtk::Widget>);
            // Split-reply slot (#86) above the reader; the full-cover
            // revealer stays an overlay above the whole assembly. A Paned
            // rather than a Box so the composer's height is the divider
            // position — dragged by the user, immune to the editor's natural
            // height — instead of whatever the composer asks for.
            pane.set_vexpand(true);
            // The reader over the bottom slot (#212), the pair beneath the
            // top slot. Each slot is hidden while no split reply is open in
            // it, which hides its divider too.
            let lower = &model.reader_split_lower;
            lower.set_start_child(Some(&pane));
            lower.set_end_child(Some(&model.reader_split_bottom));
            lower.set_resize_start_child(true);
            lower.set_shrink_start_child(true);
            lower.set_resize_end_child(false);
            lower.set_shrink_end_child(false);
            lower.set_vexpand(true);
            model.reader_split_bottom.set_visible(false);
            let split = &model.reader_split;
            split.set_start_child(Some(&model.reader_split_top));
            split.set_end_child(Some(lower));
            // Window resizes go to the reader; the composer keeps its set
            // height and never shrinks below its minimum. The reader may
            // shrink — the divider clamp at open time is what keeps a slice
            // of it on screen.
            split.set_resize_start_child(false);
            split.set_shrink_start_child(false);
            split.set_resize_end_child(true);
            split.set_shrink_end_child(true);
            model.reader_split_top.set_visible(false);
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(split));
            overlay.add_overlay(&model.reader_compose_revealer);
            // Over everything: the send morph's stage covers the whole pane.
            overlay.add_overlay(&model.send_morph);
            widgets.reader_bin.set_child(Some(&overlay));
        }
        // The attachments gallery is the content stack's second page. Wrap it in a
        // ToolbarView + HeaderBar so it keeps the window controls (close/minimize)
        // that otherwise live only on the reader pane's header.
        {
            use gtk::prelude::*;
            let gallery_tv = adw::ToolbarView::new();
            let gallery_hb = adw::HeaderBar::new();
            gallery_hb.add_css_class("flat");
            let title = gtk::Label::new(Some(i18n("Attachments").as_str()));
            title.add_css_class("pane-title");
            gallery_hb.set_title_widget(Some(&title));
            // Leftmost, same spot as the message list header's: the sidebar
            // collapse/expand toggle.
            let sidebar_btn = gtk::Button::from_icon_name("sidebar-show-symbolic");
            sidebar_btn.set_tooltip_text(Some(i18n("Toggle sidebar").as_str()));
            sidebar_btn.add_css_class("flat");
            let s = sender.input_sender().clone();
            sidebar_btn.connect_clicked(move |_| {
                let _ = s.send(AppMsg::ToggleSidebar);
            });
            gallery_hb.pack_start(&sidebar_btn);
            gallery_tv.add_top_bar(&gallery_hb);
            gallery_tv.set_content(Some(model.gallery.widget()));
            widgets
                .content_stack
                .add_named(&gallery_tv, Some("gallery"));

            // The contacts view brings its own per-pane headers (so its
            // divider runs to the very top, like the mail panes').
            widgets
                .content_stack
                .add_named(model.contacts_page.widget(), Some("contacts"));
        }
        // Desktop-notification click actions: raise the window (error alerts) and
        // raise + open a specific message (new-mail alerts). Registered here rather
        // than in `notify` because opening a message needs the app's channel.
        {
            use gtk::prelude::*;
            let app = relm4::main_application();
            let present = gtk::gio::SimpleAction::new(crate::notify::PRESENT_ACTION, None);
            let win = model.window.clone();
            present.connect_activate(move |_, _| {
                crate::desktop::present_window(&win);
            });
            app.add_action(&present);

            // "Message ready" alerts (a hand-off whose window stayed behind):
            // the click raises the composer too, and the alert goes away by
            // itself once any window of ours has the focus.
            let present_compose =
                gtk::gio::SimpleAction::new(crate::notify::PRESENT_COMPOSE_ACTION, None);
            let psender = sender.clone();
            present_compose.connect_activate(move |_, _| {
                psender.input(AppMsg::PresentComposers);
            });
            app.add_action(&present_compose);
            let fsender = sender.clone();
            model.window.connect_is_active_notify(move |w| {
                if w.is_active() {
                    crate::notify::withdraw_compose_ready();
                    // Focus comes back after an unlock, when the dock has
                    // just been rebuilt without the badge; and the mail
                    // that arrived meanwhile has now been seen (#431).
                    fsender.input(AppMsg::WindowInFront);
                }
            });

            let ty = gtk::glib::VariantTy::new("(uuu)").unwrap();
            let open = gtk::gio::SimpleAction::new(crate::notify::OPEN_MESSAGE_ACTION, Some(ty));
            let win = model.window.clone();
            let osender = sender.clone();
            open.connect_activate(move |_, param| {
                crate::desktop::present_window(&win);
                if let Some((account_id, folder_id, message_id)) =
                    param.and_then(|v| v.get::<(u32, u32, u32)>())
                {
                    osender.input(AppMsg::OpenMessageFromNotification {
                        account_id,
                        folder_id,
                        message_id,
                    });
                }
            });
            app.add_action(&open);

            // Notification buttons (#38): act on the message where it lies,
            // without presenting the window — that's their point.
            for (name, mk) in [
                (
                    crate::notify::MARK_READ_ACTION,
                    (&|account_id, folder_id, message_id| AppMsg::NotificationMarkRead {
                        account_id,
                        folder_id,
                        message_id,
                    }) as &dyn Fn(u32, u32, u32) -> AppMsg,
                ),
                (
                    crate::notify::ARCHIVE_ACTION,
                    &|account_id, folder_id, message_id| AppMsg::NotificationArchive {
                        account_id,
                        folder_id,
                        message_id,
                    },
                ),
                (
                    crate::notify::DELETE_ACTION,
                    &|account_id, folder_id, message_id| AppMsg::NotificationDelete {
                        account_id,
                        folder_id,
                        message_id,
                    },
                ),
                (
                    crate::notify::SPAM_ACTION,
                    &|account_id, folder_id, message_id| AppMsg::NotificationSpam {
                        account_id,
                        folder_id,
                        message_id,
                    },
                ),
            ] {
                let act = gtk::gio::SimpleAction::new(name, Some(ty));
                let asender = sender.clone();
                act.connect_activate(move |_, param| {
                    // No window will spend the shell's launch token, so
                    // end the launch here, or the busy pointer stays up.
                    crate::startup::complete();
                    if let Some((account_id, folder_id, message_id)) =
                        param.and_then(|v| v.get::<(u32, u32, u32)>())
                    {
                        asender.input(mk(account_id, folder_id, message_id));
                    }
                });
                app.add_action(&act);
            }
            // Reply and Forward (#244) need the window: the message opens
            // as a click on the notification would open it, and the
            // composer follows once its body is in.
            for (name, mk) in [
                (
                    crate::notify::REPLY_ACTION,
                    (&|account_id, folder_id, message_id| AppMsg::NotificationReply {
                        account_id,
                        folder_id,
                        message_id,
                    }) as &dyn Fn(u32, u32, u32) -> AppMsg,
                ),
                (
                    crate::notify::FORWARD_ACTION,
                    &|account_id, folder_id, message_id| AppMsg::NotificationForward {
                        account_id,
                        folder_id,
                        message_id,
                    },
                ),
            ] {
                let act = gtk::gio::SimpleAction::new(name, Some(ty));
                let win = model.window.clone();
                let asender = sender.clone();
                act.connect_activate(move |_, param| {
                    crate::desktop::present_window(&win);
                    if let Some((account_id, folder_id, message_id)) =
                        param.and_then(|v| v.get::<(u32, u32, u32)>())
                    {
                        asender.input(AppMsg::OpenMessageFromNotification {
                            account_id,
                            folder_id,
                            message_id,
                        });
                        asender.input(mk(account_id, folder_id, message_id));
                    }
                });
                app.add_action(&act);
            }
        }
        // Restore the last window size + maximized state (Wayland can't restore
        // position/monitor).
        let (win_w, win_h, win_max) = config::load_window_state();
        root.set_default_size(win_w, win_h);
        if win_max {
            root.maximize();
        }
        // Below this width the expanded sidebar and a full-width Actions
        // Palette can't both fit (280 sidebar + 324 list + 492 reader), so the
        // sidebar drops to its icon rail automatically — this is what keeps the
        // palette whole when the window is tiled to half of a 1920px screen.
        // With a breakpoint present the window no longer derives its minimum
        // size from its content, so pin an explicit floor: the sidebar rail
        // (80) + the list's palette floor (324) + the reader header (~492).
        root.set_size_request(904, 360);
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            1094.0,
            adw::LengthUnit::Px,
        ));
        {
            let s = sender.clone();
            narrow.connect_apply(move |_| s.input(AppMsg::AutoRail(true)));
            let s = sender.clone();
            narrow.connect_unapply(move |_| s.input(AppMsg::AutoRail(false)));
        }
        root.add_breakpoint(narrow);
        {
            let s = sender.clone();
            let guard = model.peek_transition.clone();
            widgets
                .peek_split
                .connect_show_sidebar_notify(move |split| {
                    tracing::info!(
                        "peek: show-sidebar notify shown={} guarded={}",
                        split.shows_sidebar(),
                        guard.get()
                    );
                    // Only a *user* dismissal (scrim click / swipe) counts — our
                    // own open/close transitions notify too.
                    if !guard.get() && !split.shows_sidebar() {
                        s.input(AppMsg::SidebarPeekDismissed);
                    }
                });
        }
        // Pointer tracking drives the hover peek: with the hover-expand
        // preference on, moving the pointer over the docked rail floats the
        // panel out. The peek never folds back on its own — the pointer
        // leaving used to arm a one-second dismissal, but the panel's menu
        // popover is its own surface, so merely opening it counted as
        // leaving and the panel slid away under the menu. Now only a click
        // outside the panel (the scrim), a swipe, or a navigation closes it.
        // The handler fires in every mode — the guards in the AppMsg handler
        // keep it meaningless outside a rail.
        if let Some(rail) = widgets.sidebar_split.sidebar() {
            let motion = gtk::EventControllerMotion::new();
            let armed = std::rc::Rc::new(std::cell::Cell::new(false));
            {
                let armed = armed.clone();
                motion.connect_enter(move |_, _, _| armed.set(true));
            }
            // Hover-open waits for the pointer to actually move over the
            // rail. GTK also synthesises an "enter" when the rail reappears
            // under a resting pointer as the panel slides away — opening on
            // that would fold and float forever.
            let s = sender.input_sender().clone();
            motion.connect_motion(move |_, _, _| {
                if armed.replace(false) {
                    let _ = s.send(AppMsg::SidebarHoverEnter);
                }
            });
            rail.add_controller(motion);
        }
        model.sidebar_split = Some(widgets.sidebar_split.clone());
        model.peek_split = Some(widgets.peek_split.clone());
        model.app_title = Some(widgets.app_title.clone());
        model.sidebar_header = Some(widgets.sidebar_header.clone());
        model.sidebar_menu = Some(widgets.sidebar_menu.clone());
        // The header Refresh's icon/spinner faces, and its click.
        {
            let icon = gtk::Image::from_icon_name("view-refresh-symbolic");
            model.sidebar_refresh_stack.add_named(&icon, Some("icon"));
            model
                .sidebar_refresh_stack
                .add_named(&model.sidebar_refresh_spinner, Some("spinner"));
            model.sidebar_refresh_stack.set_visible_child_name("icon");
            model
                .sidebar_refresh
                .set_child(Some(&model.sidebar_refresh_stack));
            let s = sender.input_sender().clone();
            model.sidebar_refresh.connect_clicked(move |_| {
                let _ = s.send(AppMsg::Refresh);
            });
            // Long-press slides the status bar down — the place to see what
            // the spinner is spinning about. Claiming the sequence keeps the
            // release from also firing the button's click (a refresh).
            let long = gtk::GestureLongPress::new();
            let s = sender.input_sender().clone();
            long.connect_pressed(move |gesture, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let _ = s.send(AppMsg::ToggleNotifications);
            });
            model.sidebar_refresh.add_controller(long);
        }
        // The peek panel's Refresh: the same icon/spinner faces.
        {
            let icon = gtk::Image::from_icon_name("view-refresh-symbolic");
            model.peek_refresh_stack.add_named(&icon, Some("icon"));
            model
                .peek_refresh_stack
                .add_named(&model.peek_refresh_spinner, Some("spinner"));
            model.peek_refresh_stack.set_visible_child_name("icon");
        }
        // Started with no network: orange from the first frame (#442).
        if !crate::power::online() {
            model.show_network_state();
        }
        if model.rail_active {
            widgets
                .sidebar_split
                .set_min_sidebar_width(SIDEBAR_RAIL_WIDTH);
            widgets
                .sidebar_split
                .set_max_sidebar_width(SIDEBAR_RAIL_WIDTH);
            set_sidebar_header_compact(
                &widgets.sidebar_header,
                &widgets.app_title,
                &widgets.sidebar_menu,
                &model.sidebar_refresh,
                true,
            );
        }

        // GNOME's Background Apps menu quits an app by activating its `quit`
        // action over D-Bus, falling back to `flatpak kill` if nothing answers
        // within five seconds. Without this the ✕ there would be a hard kill.
        {
            let app = relm4::main_application();
            app.add_action(&model.show_accounts_action);
            app.add_action(&model.focus_action);
            let window = root.clone();
            let outgoing = model.outgoing.clone();
            let quit = gtk::gio::SimpleAction::new("quit", None);
            quit.connect_activate(move |_, _| {
                // Same teardown as closing the last window: save the geometry,
                // then exit rather than unwinding through GTK and WebKit.
                let maximized = window.is_maximized();
                let (width, height) = if maximized {
                    let (sw, sh, _) = crate::config::load_window_state();
                    (sw, sh)
                } else {
                    (window.width(), window.height())
                };
                crate::config::save_window_state(width, height, maximized);
                crate::background::set_status("");
                crate::outgoing::park_held(&outgoing);
                finish_keyring_writes();
                std::process::exit(0)
            });
            app.add_action(&quit);
            gtk::prelude::GtkApplicationExt::set_accels_for_action(&app, "app.quit", &["<Ctrl>q"]);
            // Ctrl+Shift+A shows or hides the sidebar's account sections and
            // Ctrl+Shift+C opens the console, but in a composer they are
            // Thunderbird's Attach and Cc (#490, #491). An application key
            // reaches the main window before the composer in it, so the two
            // belong to actions of their own that ask where the focus is
            // (AppMsg::ComposerKey). The menus keep the real actions, with
            // the keys shown beside them, so a click there never reaches a
            // composer that had the focus before.
            for (name, keyval, accel) in [
                ("accounts-key", gtk::gdk::Key::a, "<Ctrl><Shift>a"),
                ("console-key", gtk::gdk::Key::c, "<Ctrl><Shift>c"),
            ] {
                let action = gtk::gio::SimpleAction::new(name, None);
                let s = sender.clone();
                action.connect_activate(move |_, _| s.input(AppMsg::ComposerKey(keyval)));
                app.add_action(&action);
                gtk::prelude::GtkApplicationExt::set_accels_for_action(
                    &app,
                    &format!("app.{name}"),
                    &[accel],
                );
            }
            // Ctrl+Shift+F switches Focus Mode on and off.
            gtk::prelude::GtkApplicationExt::set_accels_for_action(
                &app,
                "app.focus-mode",
                &["<Ctrl><Shift>f"],
            );
            // Ctrl+ / Ctrl- / Ctrl+0: message zoom, the bodies alone. The
            // = key stands in for + on layouts where + needs Shift, and the
            // keypad's keys count too, as in every browser.
            for (name, step, keys) in [
                (
                    "zoom-in",
                    1i8,
                    &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"][..],
                ),
                ("zoom-out", -1, &["<Ctrl>minus", "<Ctrl>KP_Subtract"][..]),
                ("zoom-reset", 0, &["<Ctrl>0", "<Ctrl>KP_0"][..]),
            ] {
                let action = gtk::gio::SimpleAction::new(name, None);
                let s = sender.clone();
                action.connect_activate(move |_, _| s.input(AppMsg::ZoomKey(step)));
                app.add_action(&action);
                gtk::prelude::GtkApplicationExt::set_accels_for_action(
                    &app,
                    &format!("app.{name}"),
                    keys,
                );
            }
            // Ctrl+W closes the window only (issue #64): with "run in the
            // background" on, mail keeps arriving — unlike Ctrl+Q, which
            // quits outright. GTK's built-in window.close action does
            // exactly the same as the titlebar's close button.
            gtk::prelude::GtkApplicationExt::set_accels_for_action(
                &app,
                "window.close",
                &["<Ctrl>w"],
            );

            // Activating the app again — its icon, a notification, or the
            // autostart entry — brings the hidden window back rather than doing
            // nothing.
            let window = root.clone();
            // Started hidden? Then the first activation is this very launch, and
            // presenting here would undo it. Every activation after that is a
            // person asking for the window.
            let pending_hidden_start = std::cell::Cell::new(
                crate::HIDDEN_START.load(std::sync::atomic::Ordering::Relaxed),
            );
            app.connect_activate(move |_| {
                if pending_hidden_start.replace(false) {
                    return;
                }
                crate::desktop::present_window(&window);
            });
        }

        let mut group = RelmActionGroup::<WindowActionGroup>::new();
        let accounts_sender = sender.clone();
        group.add_action(RelmAction::<AccountsAction>::new_stateless(move |_| {
            accounts_sender.input(AppMsg::OpenSettings);
        }));
        let prefs_sender = sender.clone();
        group.add_action(RelmAction::<PreferencesAction>::new_stateless(move |_| {
            prefs_sender.input(AppMsg::OpenPreferences);
        }));
        let about_sender = sender.clone();
        group.add_action(RelmAction::<AboutAction>::new_stateless(move |_| {
            about_sender.input(AppMsg::OpenAbout);
        }));
        let shortcuts_sender = sender.clone();
        group.add_action(RelmAction::<ShortcutsAction>::new_stateless(move |_| {
            shortcuts_sender.input(AppMsg::ShowShortcuts);
        }));
        let print_sender = sender.clone();
        group.add_action(RelmAction::<PrintAction>::new_stateless(move |_| {
            print_sender.input(AppMsg::PrintMessage);
        }));
        let preview_sender = sender.clone();
        group.add_action(RelmAction::<PrintPreviewAction>::new_stateless(move |_| {
            preview_sender.input(AppMsg::PrintPreview);
        }));
        // The status bar reveals itself for errors; this is the manual path
        // (the dedicated button is gone from the sidebar).
        let status_sender = sender.clone();
        {
            let s = sender.clone();
            group.add_action(RelmAction::<ConsoleAction>::new_stateless(move |_| {
                s.input(AppMsg::OpenConsole);
            }));
        }
        {
            let s = sender.clone();
            group.add_action(RelmAction::<FindAction>::new_stateless(move |_| {
                s.input(AppMsg::OpenListSearch);
            }));
        }
        let undo_action = {
            let s = sender.clone();
            RelmAction::<UndoAction>::new_stateless(move |_| s.input(AppMsg::Undo))
        };
        let redo_action = {
            let s = sender.clone();
            RelmAction::<RedoAction>::new_stateless(move |_| s.input(AppMsg::Redo))
        };
        group.add_action(undo_action.clone());
        group.add_action(redo_action.clone());
        group.add_action(RelmAction::<StatusBarAction>::new_stateless(move |_| {
            status_sender.input(AppMsg::ToggleNotifications);
        }));
        group.register_for_widget(&root);

        // A real accelerator rather than a key handler: GTK matches these before
        // the keystroke reaches whatever has focus, so Ctrl+? works while reading
        // a message (the web view would otherwise swallow it). Both spellings are
        // bound because layouts disagree about whether Ctrl+Shift+/ arrives as
        // `question` or as `slash`, and F1 is the GNOME convention.
        relm4::main_application().set_accelerators_for_action::<PrintAction>(&["<Ctrl>p"]);
        relm4::main_application()
            .set_accelerators_for_action::<PrintPreviewAction>(&["<Ctrl><Shift>p"]);
        relm4::main_application()
            .set_accelerators_for_action::<StatusBarAction>(&["<Ctrl><Shift>s"]);
        relm4::main_application().set_accelerators_for_action::<FindAction>(&["<Ctrl>f"]);
        // On the menu's action, so the menu shows the keys beside Settings
        // (#513).
        relm4::main_application().set_accelerators_for_action::<AccountsAction>(&["<Ctrl>comma"]);
        relm4::main_application().set_accelerators_for_action::<ShortcutsAction>(&[
            "<Ctrl>question",
            "<Ctrl><Shift>question",
            "<Ctrl>slash",
            "<Ctrl><Shift>slash",
            "F1",
        ]);

        root.add_controller(palette_key(&sender));

        // Single-key shortcuts. The controller is on the window in the bubble
        // phase, so the focused widget always gets first refusal: a search entry
        // or the composer consumes the letter itself and nothing here fires.
        // `focus_takes_keys` covers the rest — chiefly the reader's web view,
        // which handles keys without consuming them.
        {
            let keys = gtk::EventControllerKey::new();
            let s = sender.clone();
            let window = root.clone();
            let enabled = model.single_key.clone();
            let gmail = model.gmail_keys.clone();
            let probe = model.message_list.model().selection_probe();
            // A tag key carries the rows selected as it is pressed (#416).
            let shortcut = move |action: Shortcut| match action {
                Shortcut::Tag(n) => AppMsg::TagKey {
                    n,
                    slots: probe.slots(),
                },
                action => AppMsg::Shortcut(action),
            };
            // The first key of a Gmail sequence (g or *) and when it came.
            let pending: std::rc::Rc<std::cell::Cell<Option<(char, std::time::Instant)>>> =
                Default::default();
            keys.connect_key_pressed(move |_, keyval, _, state| {
                let keyval = without_caps_lock(keyval, state);
                let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                // Ctrl+? opens the reference whether or not the shortcuts
                // themselves are switched on.
                if ctrl && matches!(keyval, gtk::gdk::Key::question | gtk::gdk::Key::slash) {
                    s.input(AppMsg::ShowShortcuts);
                    return gtk::glib::Propagation::Stop;
                }
                // Ctrl+Z undoes the last action, Ctrl+Shift+Z and Ctrl+Y put
                // it back (#200). Text fields and the composer keep all three
                // for their own text history.
                let undo_key = ctrl && !shift && keyval == gtk::gdk::Key::z;
                let redo_key = ctrl
                    && ((shift && matches!(keyval, gtk::gdk::Key::z | gtk::gdk::Key::Z))
                        || keyval == gtk::gdk::Key::y);
                if undo_key || redo_key {
                    if focus_is_text(&window) || focus_in_compose(&window) {
                        return gtk::glib::Propagation::Proceed;
                    }
                    s.input(if redo_key { AppMsg::Redo } else { AppMsg::Undo });
                    return gtk::glib::Propagation::Stop;
                }
                // Ctrl+C with the keyboard on the list or the window itself:
                // a selection made in the reader still wants copying (the
                // list takes focus back after a click over a message body).
                // The reader's own document handles the key when it has focus;
                // a text field or composer keeps its native copy.
                if ctrl
                    && !shift
                    && keyval == gtk::gdk::Key::c
                    && !focus_takes_keys(&window)
                    && !focus_in_compose(&window)
                {
                    s.input(AppMsg::CopyReaderSelection);
                    return gtk::glib::Propagation::Proceed;
                }
                // The keys other mail apps use (#328), with or without
                // single-key shortcuts. A composer keeps them: Ctrl+U
                // underlines there, and a reply should not start from the
                // middle of another.
                if ctrl
                    && !state.contains(gtk::gdk::ModifierType::ALT_MASK)
                    && !focus_in_compose(&window)
                {
                    if let Some(msg) = ctrl_shortcut_for(keyval, shift) {
                        s.input(msg);
                        return gtk::glib::Propagation::Stop;
                    }
                }
                if ctrl || state.contains(gtk::gdk::ModifierType::ALT_MASK) {
                    return gtk::glib::Propagation::Proceed;
                }
                // Escape backs out to the message list whatever else is going on,
                // and without needing single-key shortcuts switched on. A text
                // field keeps it, though — there it clears the search.
                if keyval == gtk::gdk::Key::Escape {
                    if focus_is_text(&window) {
                        return gtk::glib::Propagation::Proceed;
                    }
                    s.input(AppMsg::Shortcut(Shortcut::BackToList));
                    return gtk::glib::Propagation::Stop;
                }
                // Delete moves the selection to Trash and Shift+Delete erases
                // it, Thunderbird's key (#471), also without single-key
                // shortcuts. The list catches both first when it has focus;
                // this is the reader and the rest (#486).
                if matches!(keyval, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete)
                    && !focus_is_text(&window)
                    && !focus_in_compose(&window)
                {
                    s.input(shortcut(if shift {
                        Shortcut::DeleteForever
                    } else {
                        Shortcut::Delete
                    }));
                    return gtk::glib::Propagation::Stop;
                }
                if !enabled.get() || focus_is_text(&window) || focus_in_compose(&window) {
                    return gtk::glib::Propagation::Proceed;
                }
                if focus_takes_keys(&window) && reader_keeps(keyval) {
                    return gtk::glib::Propagation::Proceed;
                }
                if gmail.get() {
                    // Shift alone starts nothing and ends nothing.
                    if matches!(keyval, gtk::gdk::Key::Shift_L | gtk::gdk::Key::Shift_R) {
                        return gtk::glib::Propagation::Proceed;
                    }
                    let first = pending
                        .take()
                        .filter(|(_, at)| at.elapsed() < std::time::Duration::from_millis(1500));
                    if let Some((first, _)) = first {
                        if let Some(action) = gmail_sequence(first, keyval) {
                            s.input(shortcut(action));
                        }
                        return gtk::glib::Propagation::Stop;
                    }
                    let start = match keyval {
                        gtk::gdk::Key::g => Some('g'),
                        gtk::gdk::Key::asterisk => Some('*'),
                        _ => None,
                    };
                    if let Some(c) = start {
                        pending.set(Some((c, std::time::Instant::now())));
                        return gtk::glib::Propagation::Stop;
                    }
                    return match gmail_shortcut_for(keyval, shift) {
                        Some(action) => {
                            s.input(shortcut(action));
                            gtk::glib::Propagation::Stop
                        }
                        None => gtk::glib::Propagation::Proceed,
                    };
                }
                match shortcut_for(keyval, shift) {
                    Some(action) => {
                        s.input(shortcut(action));
                        gtk::glib::Propagation::Stop
                    }
                    None => gtk::glib::Propagation::Proceed,
                }
            });
            root.add_controller(keys);
        }

        // One-time, dismissible keyring setup tip for Linux Mint / Cinnamon, where
        // the Secret Service often needs configuring so passwords persist and the
        // keyring auto-unlocks at login. Only shown once the user actually has an
        // account (so it isn't the very first thing a new user sees), and never
        // again after "Don't show again".
        if !model.config.is_empty()
            && crate::platform::is_mint_cinnamon()
            && !config::mint_keyring_help_dismissed()
        {
            sender.input(AppMsg::ShowKeyringHelp { problem: false });
        }

        // An earlier install's data came across at this start (see
        // `legacy`): say so once, and take its start-at-login over — the
        // portal's autostart entry is per app ID, so it has to be asked
        // for again under this one.
        if let Some(from) = crate::legacy::migrated_from() {
            if model.run_in_background.get() && model.autostart {
                crate::background::request(true);
            }
            sender.input(AppMsg::ShowCarryOverNotice(from));
        } else if std::env::var_os("HYLKI_SHOWCASE_CARRY_OVER").is_some() {
            // Opens the notice as if Vireo's data had just come across, for
            // checking it by hand.
            sender.input(AppMsg::ShowCarryOverNotice(&crate::legacy::PREDECESSORS[0]));
        }

        // Screenshot showcase (HYLKI_DEMO + HYLKI_SHOWCASE=/path.png): stage
        // the demo the way the marketing shots want it — first row (the demo
        // conversation, expanded via the threads_expanded preference) selected,
        // one mid-thread card highlighted — then render the window to a PNG.
        // Return freed heap to the system now and then. What the gallery,
        // the composer or a big conversation allocated and dropped otherwise
        // stays held by the allocator, and a system monitor counts it against
        // the app: a user's log showed 1.73 GB held with 270 MB live.
        gtk::glib::timeout_add_seconds_local(30, || {
            if let Some(held) = crate::memory_report::trim_if_worthwhile() {
                tracing::debug!(target: "hylki::memory", "trimmed the heap: {} was freed but held", crate::memory_report::human_bytes(held as u64));
            }
            gtk::glib::ControlFlow::Continue
        });
        // HYLKI_SHOWCASE_MEMORY=/path.txt writes the exported log (memory
        // section included) there after HYLKI_SHOWCASE_MEMORY_AT seconds
        // (default 20), on a real mailbox as much as the demo: a memory
        // reading is only worth having once the index has loaded. With
        // HYLKI_SHOWCASE_MEMORY_EVERY=N it is rewritten every N seconds
        // after that, for watching a session grow.
        if let Some(path) = std::env::var_os("HYLKI_SHOWCASE_MEMORY") {
            let secs = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u32>().ok());
            let at = secs("HYLKI_SHOWCASE_MEMORY_AT").unwrap_or(20);
            let every = secs("HYLKI_SHOWCASE_MEMORY_EVERY").filter(|n| *n > 0);
            let s = sender.clone();
            let path = std::path::PathBuf::from(path);
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                s.input(AppMsg::ExportLogTo(path.clone()));
                if let Some(every) = every {
                    gtk::glib::timeout_add_seconds_local(every, move || {
                        s.input(AppMsg::ExportLogTo(path.clone()));
                        gtk::glib::ControlFlow::Continue
                    });
                }
            });
        }
        // HYLKI_SHOWCASE_SELECT=<account>:<id> opens that message at 5 s
        // (HYLKI_SHOWCASE_SELECT_AT overrides the seconds) through its row,
        // or the head of its conversation, real accounts included: a way to
        // open a conversation the reader assembles from the cache without a
        // pointer, so its log can be read (#236).
        // HYLKI_SHOWCASE_ZOOM=N presses Ctrl+ N times (Ctrl- for a negative
        // N) at 8 s, so the live zoom path can be captured.
        if let Some(n) = std::env::var("HYLKI_SHOWCASE_ZOOM")
            .ok()
            .and_then(|v| v.parse::<i8>().ok())
        {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(8, move || {
                for _ in 0..n.unsigned_abs() {
                    s.input(AppMsg::ZoomMessage(n.signum()));
                }
            });
        }
        // HYLKI_SHOWCASE_DELETE_ATTACHMENT=<name>[:yes] chooses Delete from
        // Server for that file of the message on screen at 10 s
        // (HYLKI_SHOWCASE_DELETE_AT overrides the seconds); `:yes` answers
        // the question as well (#289).
        // `gallery:<name>` opens the attachments gallery 4 s before and
        // deletes the file from there.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_DELETE_ATTACHMENT") {
            let (name, confirmed) = match v.strip_suffix(":yes") {
                Some(name) => (name.to_string(), true),
                None => (v, false),
            };
            let at: u32 = std::env::var("HYLKI_SHOWCASE_DELETE_AT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10);
            if name.starts_with("gallery:") {
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(at.saturating_sub(4), move || {
                    s.input(AppMsg::ShowAttachments)
                });
            }
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                s.input(AppMsg::ShowcaseDeleteAttachment { name, confirmed });
            });
        }
        // Several may be given, comma-separated, each with its own
        // @<seconds>, to open one message and then another.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_SELECT") {
            let default_at: u32 = std::env::var("HYLKI_SHOWCASE_SELECT_AT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5);
            for item in v.split(',') {
                let (item, at) = showcase_at(item, default_at);
                let Some((a, id)) = item
                    .split_once(':')
                    .and_then(|(a, id)| Some((a.parse::<u32>().ok()?, id.parse::<u32>().ok()?)))
                else {
                    continue;
                };
                let list = model.message_list.sender().clone();
                gtk::glib::timeout_add_seconds_local_once(at, move || {
                    let _ = list.send(MessageListInput::SelectAndLoad((a, id)));
                });
            }
        }
        // HYLKI_SHOWCASE_TRANSLATE=<account>:<id>[@<seconds>] translates
        // that message (open by then) at 8 s, as its A文 button would, so a
        // translation can be captured (#327).
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_TRANSLATE") {
            let (item, at) = showcase_at(&v, 8);
            if let Some((a, id)) = item
                .split_once(':')
                .and_then(|(a, id)| Some((a.parse::<u32>().ok()?, id.parse::<u32>().ok()?)))
            {
                let view = model.message_view.sender().clone();
                gtk::glib::timeout_add_seconds_local_once(at, move || {
                    let _ = view.send(MessageViewInput::Translate { account_id: a, id });
                });
            }
        }
        // HYLKI_SHOWCASE_CARD_DELETE=<account>:<id>[@<seconds>] deletes that
        // card of the open conversation at 8 s, as its Delete would. Several
        // may be given, comma-separated.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_CARD_DELETE") {
            for item in v.split(',') {
                let (item, at) = showcase_at(item, 8);
                let Some((a, id)) = item
                    .split_once(':')
                    .and_then(|(a, id)| Some((a.parse::<u32>().ok()?, id.parse::<u32>().ok()?)))
                else {
                    continue;
                };
                let view = model.message_view.sender().clone();
                gtk::glib::timeout_add_seconds_local_once(at, move || {
                    let _ = view.send(MessageViewInput::CardAction {
                        action: crate::ui::message_list::RowAction::Delete,
                        account_id: a,
                        id,
                    });
                });
            }
        }
        // HYLKI_SHOWCASE_INBOX=<account>[@<seconds>] switches to that
        // account's Inbox at 3 s or the time given (the first account's when
        // it is not a number), real accounts included, for the same probe
        // from a folder view.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_INBOX") {
            let (v, at) = showcase_at(&v, 3);
            let account = v.parse::<u32>().ok().filter(|a| *a > 0);
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                s.input(AppMsg::ShowcaseFolder {
                    kind: FolderKind::Inbox,
                    account,
                });
            });
        }
        // HYLKI_SHOWCASE_LIST_SEARCH=<text>[:<scope>][@<seconds>] searches
        // the list at 4 s or the time given, the scope by its place in the
        // dropdown, real accounts included (#472).
        if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_LIST_SEARCH") {
            let (spec, at) = showcase_at(&spec, 4);
            let (text, scope) = match spec.rsplit_once(':') {
                Some((t, s)) => (t.to_string(), s.parse().unwrap_or(0)),
                None => (spec.to_string(), 0),
            };
            let list = model.message_list.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                let _ = list.send(MessageListInput::ShowcaseSearch(text, scope));
            });
        }
        // HYLKI_SHOWCASE_NOTIFY[=<seconds>[,<seconds>...]] posts the new-mail
        // notification at 5 s or at each time given (the same second twice
        // exercises the sound's hold-off), real accounts included.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_NOTIFY") {
            let mut times: Vec<u32> = v.split(',').filter_map(|t| t.trim().parse().ok()).collect();
            if times.is_empty() {
                times.push(5);
            }
            for at in times {
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(at, move || {
                    s.input(AppMsg::ShowcaseNotify)
                });
            }
        }
        // Timers leave room for the WebViews to load and settle between steps.
        if demo_mode() {
            if let Some(shot) = std::env::var("HYLKI_SHOWCASE").ok() {
                // HYLKI_SHOWCASE_STAGE=0 skips the staging (capture-only, for
                // testing arbitrary states); HYLKI_SHOWCASE_DELAY overrides the
                // capture time (seconds, default 9).
                let stage = std::env::var("HYLKI_SHOWCASE_STAGE").as_deref() != Ok("0");
                let delay: u32 = std::env::var("HYLKI_SHOWCASE_DELAY")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(9);
                // HYLKI_SHOWCASE_HEIGHTS=1 logs the allocated height of the
                // sidebar's section headings and their rows just before the
                // capture, for checking pill geometry without a pointer.
                if std::env::var("HYLKI_SHOWCASE_HEIGHTS").is_ok() {
                    let sidebar = model.sidebar.widget().clone().upcast::<gtk::Widget>();
                    gtk::glib::timeout_add_seconds_local_once(
                        delay.saturating_sub(1).max(1),
                        move || {
                            fn walk(w: &gtk::Widget, depth: usize, root: &gtk::Widget) {
                                let classes = w.css_classes();
                                let heading = classes.iter().any(|c| c == "unified-folders-toggle");
                                let child_row = w.is::<gtk::ListBoxRow>()
                                    && w.parent().is_some_and(|p| {
                                        p.css_classes().iter().any(|c| c == "section-child-list")
                                    });
                                if heading || child_row {
                                    let y =
                                        w.compute_bounds(root).map(|b| b.y() as i32).unwrap_or(-1);
                                    tracing::info!(
                                    "showcase heights: {} y={y} h={} margins={}/{} classes={:?}",
                                    if heading { "heading" } else { "row" },
                                    w.height(),
                                    w.margin_top(),
                                    w.margin_bottom(),
                                    classes,
                                );
                                }
                                let mut c = w.first_child();
                                while let Some(ch) = c {
                                    walk(&ch, depth + 1, root);
                                    c = ch.next_sibling();
                                }
                            }
                            walk(&sidebar, 0, &sidebar);
                        },
                    );
                }
                if stage {
                    let list = model.message_list.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = list.send(MessageListInput::MoveSelection(1));
                    });
                    // HYLKI_SHOWCASE_PALETTE=N opens row N's actions palette
                    // (so a capture can verify the floating palette's look).
                    if let Some(Ok(idx)) = std::env::var("HYLKI_SHOWCASE_PALETTE")
                        .ok()
                        .map(|v| v.parse())
                    {
                        let list = model.message_list.sender().clone();
                        let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
                        let win = root.clone();
                        {
                            // An occluded window's frame clock is suspended
                            // and animations cannot progress — raise it well
                            // ahead so the slide actually runs for the burst.
                            let win = win.clone();
                            gtk::glib::timeout_add_seconds_local_once(5, move || {
                                win.present();
                            });
                        }
                        gtk::glib::timeout_add_seconds_local_once(7, move || {
                            let _ = list.send(MessageListInput::DebugOpenPalette(idx));
                            // TEMP: frame burst right after the open, to see
                            // the slide animation (or its absence) in stills.
                            if let Some(base) = burst.clone() {
                                for (i, ms) in [40u64, 90, 140, 240].iter().enumerate() {
                                    let win = win.clone();
                                    let path = format!("{base}.{i}.png");
                                    gtk::glib::timeout_add_local_once(
                                        std::time::Duration::from_millis(*ms),
                                        move || {
                                            showcase_capture(
                                                win.upcast_ref::<gtk::Widget>(),
                                                &path,
                                            );
                                        },
                                    );
                                }
                            }
                        });
                    }
                    let view = model.message_view.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = view.send(crate::ui::message_view::MessageViewInput::CardClicked {
                            account_id: 1,
                            id: 2,
                            mode: crate::ui::message_view::SelectMode::Plain,
                        });
                    });
                }
                // HYLKI_SHOWCASE_SWIPE=N[:left|right] swipes row N fully and
                // releases it at 7s, with HYLKI_SHOWCASE_BURST=<path> writing
                // stills through the commit exit — the only way to see the
                // gesture's animation here (no input injection).
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_SWIPE") {
                    let (idx, side) = spec.split_once(':').unwrap_or((spec.as_str(), "left"));
                    let index: usize = idx.trim().parse().unwrap_or(0);
                    let left = side != "right";
                    let list = model.message_list.sender().clone();
                    let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
                    let win = root.clone();
                    {
                        // An occluded window's frame clock is suspended, so
                        // raise it well before the animation runs.
                        let win = win.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || win.present());
                    }
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        let _ = list.send(MessageListInput::DebugSwipe { index, left });
                        if let Some(base) = burst.clone() {
                            for (i, ms) in [40u64, 120, 200, 300, 500].iter().enumerate() {
                                let win = win.clone();
                                let path = format!("{base}.{i}.png");
                                gtk::glib::timeout_add_local_once(
                                    std::time::Duration::from_millis(*ms),
                                    move || {
                                        showcase_capture(win.upcast_ref::<gtk::Widget>(), &path);
                                    },
                                );
                            }
                        }
                    });
                }
                // HYLKI_SHOWCASE_ROW=N selects the list's row N at 4s (with
                // HYLKI_SHOWCASE_STAGE=0), for capturing or probing one message.
                if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_ROW")
                    .ok()
                    .map(|v| v.parse::<u32>())
                {
                    let list = model.message_list.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        for _ in 0..=n {
                            let _ = list.send(MessageListInput::MoveSelection(1));
                        }
                    });
                }
                // HYLKI_SHOWCASE_UNSUB=ask|go presses the open card's
                // Unsubscribe button at 7s (after HYLKI_SHOWCASE_ROW opened
                // it): `ask` stops at the confirmation, `go` skips it.
                if std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok() {
                    let view = model.message_view.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        let _ = view.send(MessageViewInput::Unsubscribe {
                            account_id: 1,
                            id: 10,
                        });
                    });
                }
                // HYLKI_SHOWCASE_INVITE=accept|maybe|decline|calendar presses
                // that button on the open card's invitation banner at 7s
                // (after HYLKI_SHOWCASE_ROW=8 opened the meeting request).
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_INVITE") {
                    use crate::models::Rsvp;
                    use crate::ui::message_view::InviteAction;
                    let action = match which.as_str() {
                        "maybe" => InviteAction::Answer(Rsvp::Tentative),
                        "decline" => InviteAction::Answer(Rsvp::Declined),
                        "calendar" => InviteAction::AddToCalendar,
                        _ => InviteAction::Answer(Rsvp::Accepted),
                    };
                    let view = model.message_view.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        let _ = view.send(MessageViewInput::InviteAction {
                            account_id: 1,
                            id: 13,
                            action,
                        });
                    });
                }
                // HYLKI_SHOWCASE_UNIFIED=sent|starred|drafts|archive|junk|trash|smart opens that unified
                // row at 3s, All Inboxes at 6s and the row again at 9s, so the
                // timing logs show a cold and a warm open. starred@N picks the
                // Nth account's row under Starred instead.
                // HYLKI_SHOWCASE_UNIFIED_AT=<s> starts that at s seconds rather
                // than 3, once a real profile's accounts have loaded.
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_UNIFIED") {
                    let at: u32 = std::env::var("HYLKI_SHOWCASE_UNIFIED_AT")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(3);
                    let sub = which
                        .strip_prefix("starred@")
                        .and_then(|n| n.parse::<i32>().ok());
                    let pick = move || match which.as_str() {
                        _ if let Some(index) = sub => SidebarInput::KindSubRowSelected {
                            kind: FolderKind::Starred,
                            index,
                        },
                        "starred" => SidebarInput::UnifiedKindRowSelected(FolderKind::Starred),
                        "drafts" => SidebarInput::UnifiedKindRowSelected(FolderKind::Drafts),
                        "archive" => SidebarInput::UnifiedKindRowSelected(FolderKind::Archive),
                        "junk" => SidebarInput::UnifiedKindRowSelected(FolderKind::Junk),
                        "trash" => SidebarInput::UnifiedKindRowSelected(FolderKind::Trash),
                        "filtered" => SidebarInput::UnifiedFilteredSelected,
                        "favorites" => SidebarInput::UnifiedFavoritesSelected,
                        "tags" => SidebarInput::UnifiedTagsSelected,
                        "smart" => SidebarInput::SmartRowSelected(0),
                        _ => SidebarInput::UnifiedKindRowSelected(FolderKind::Sent),
                    };
                    let sb = model.sidebar.sender().clone();
                    let p = pick.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || {
                        let _ = sb.send(p());
                    });
                    let sb = model.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(at + 3, move || {
                        let _ = sb.send(SidebarInput::SelectUnifiedRow);
                    });
                    let sb = model.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(at + 6, move || {
                        let _ = sb.send(pick());
                    });
                }
                // HYLKI_SHOWCASE_ROW_MENU=1 opens the first row's context
                // menu at 5s (pair with HYLKI_SHOWCASE_MENU to capture it).
                if std::env::var("HYLKI_SHOWCASE_ROW_MENU").is_ok() {
                    let ml = model.message_list.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = ml.send(MessageListInput::DebugRowMenu);
                    });
                }
                // HYLKI_SHOWCASE_APPLY_FILTERS=inboxes|<account>:<folder>
                // starts a manual filter run at 5s (#198), the way Apply Now
                // and the folder menu's Apply Filters do, and the run's
                // report lands in the status bar a beat later.
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_APPLY_FILTERS") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let targets = spec
                            .split_once(':')
                            .and_then(|(a, f)| Some(vec![(a.parse().ok()?, f.parse().ok()?)]))
                            .unwrap_or_default();
                        s.input(AppMsg::ApplyFilters(targets));
                    });
                }
                // HYLKI_SHOWCASE_TOOLBAR_GAP=<zone>:<index> opens a drop gap
                // in the Settings toolbar editor at 6s (pair with
                // HYLKI_SHOWCASE_SETTINGS=appearance), as a hovering drag
                // would.
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_TOOLBAR_GAP") {
                    if let Some((z, i)) = spec.split_once(':') {
                        if let (Ok(zone), Ok(index)) = (z.parse::<usize>(), i.parse::<usize>()) {
                            let s = sender.input_sender().clone();
                            gtk::glib::timeout_add_seconds_local_once(6, move || {
                                let _ = s.send(AppMsg::ShowcaseToolbarGap { zone, index });
                            });
                        }
                    }
                }
                // HYLKI_SHOWCASE_TOOLBAR_MENU=1 opens the header's right-click
                // menu (Customize Toolbar…) at 5s, as a click on its empty
                // middle would.
                if std::env::var("HYLKI_SHOWCASE_TOOLBAR_MENU").is_ok() {
                    let s = sender.input_sender().clone();
                    let header: gtk::Widget = widgets.reader_header.clone().upcast();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = s.send(AppMsg::ReaderToolbarMenu {
                            x: header.width() as f64 / 2.0,
                            y: header.height() as f64 / 2.0,
                        });
                    });
                }
                // HYLKI_SHOWCASE_READER_MENU=1 opens the reader header's ⋯
                // overflow menu at 5s (pair with HYLKI_SHOWCASE_MENU to
                // capture it; the window must be narrow enough to collapse).
                if std::env::var("HYLKI_SHOWCASE_READER_MENU").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = s.send(AppMsg::ReaderOverflowMenu);
                    });
                }
                // HYLKI_SHOWCASE_PRINT=1 opens the print preview of the open
                // message at 6s (#359); HYLKI_SHOWCASE_TOP captures it.
                if std::env::var("HYLKI_SHOWCASE_PRINT").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = s.send(AppMsg::PrintPreview);
                    });
                }
                // HYLKI_SHOWCASE_POPOUT=1 opens the open message in a window
                // of its own at 6s; HYLKI_SHOWCASE_TOP captures it.
                if std::env::var("HYLKI_SHOWCASE_POPOUT").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = s.send(AppMsg::ShowcasePopout);
                    });
                }
                // HYLKI_SHOWCASE_EDIT_AS_NEW=1 copies the open message into
                // the composer at 6s (#232), as its menu entry does.
                if std::env::var("HYLKI_SHOWCASE_EDIT_AS_NEW").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = s.send(AppMsg::EditAsNewCurrent);
                    });
                }
                // HYLKI_SHOWCASE_MOVE=1 opens the Move To… picker at 5s
                // (it captures itself a second later).
                if std::env::var("HYLKI_SHOWCASE_MOVE").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = s.send(AppMsg::MoveToMenu);
                    });
                }
                // HYLKI_SHOWCASE_GALLERY=1 opens the attachments gallery at
                // 3s; add HYLKI_SHOWCASE_GALLERY_FOLDERS=1 to drop its folder
                // popover open a second later, or HYLKI_SHOWCASE_GALLERY_ACCOUNT=<row>
                // to pick that row of its account filter (0 = all accounts).
                if std::env::var("HYLKI_SHOWCASE_GALLERY").is_ok() {
                    let s = sender.input_sender().clone();
                    let g = model.gallery.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = s.send(AppMsg::ShowAttachments);
                        let row: Option<u32> = std::env::var("HYLKI_SHOWCASE_GALLERY_ACCOUNT")
                            .ok()
                            .and_then(|v| v.parse().ok());
                        let folders = std::env::var("HYLKI_SHOWCASE_GALLERY_FOLDERS").is_ok();
                        // HYLKI_SHOWCASE_GALLERY_SEARCH=<text> types into the
                        // search box and reports whether it keeps the focus.
                        let search = std::env::var("HYLKI_SHOWCASE_GALLERY_SEARCH").ok();
                        // HYLKI_SHOWCASE_GALLERY_MORE=<n> pages down n times,
                        // as scrolling to the end would.
                        let more: u32 = std::env::var("HYLKI_SHOWCASE_GALLERY_MORE")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        gtk::glib::timeout_add_seconds_local_once(1, move || {
                            if let Some(row) = row {
                                let _ = g.send(GalleryInput::SetAccountFilter(row));
                            }
                            for i in 0..more {
                                let g = g.clone();
                                gtk::glib::timeout_add_seconds_local_once(1 + i, move || {
                                    let _ = g.send(GalleryInput::LoadMore);
                                });
                            }
                            if let Some(text) = search {
                                let _ = g.send(GalleryInput::ShowcaseSearch(text));
                            }
                            if folders {
                                let _ = g.send(GalleryInput::ShowcaseFolders);
                            }
                        });
                    });
                }
                // HYLKI_SHOWCASE_RAIL=1 collapses the sidebar to the rail
                // at 3s (the user's own toggle, fold-ups and all).
                if std::env::var("HYLKI_SHOWCASE_RAIL").is_ok() {
                    let sb = model.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = sb.send(SidebarInput::ToggleCollapsed);
                    });
                }
                // HYLKI_SHOWCASE_PEEK=<seconds>[,<seconds>…] works the header's
                // sidebar toggle at each of those moments (1 or anything
                // unparseable means 8 s), with the window raised first so the
                // slide really animates; HYLKI_SHOWCASE_BURST writes stills
                // through the first one. In a narrow window (a window.toml
                // under the 1094px breakpoint) that is the floating peek; at
                // full width, or under Focus Mode's rail, it is the rail
                // expanding and folding again.
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_PEEK") {
                    let mut at: Vec<f64> = spec
                        .split(',')
                        .filter_map(|p| p.trim().parse::<f64>().ok())
                        .filter(|s| *s > 1.5)
                        .collect();
                    if at.is_empty() {
                        at.push(8.0);
                    }
                    let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
                    let win = root.clone();
                    {
                        let win = win.clone();
                        let first = at[0];
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(((first - 1.5) * 1000.0) as u64),
                            move || win.present(),
                        );
                    }
                    for (n, secs) in at.into_iter().enumerate() {
                        let s = sender.clone();
                        let win = win.clone();
                        let burst = (n == 0).then(|| burst.clone()).flatten();
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis((secs * 1000.0) as u64),
                            move || {
                                s.input(AppMsg::ToggleSidebar);
                                if let Some(base) = burst {
                                    for (i, ms) in [60u64, 120, 200, 400].iter().enumerate() {
                                        let win = win.clone();
                                        let path = format!("{base}.{i}.png");
                                        gtk::glib::timeout_add_local_once(
                                            std::time::Duration::from_millis(*ms),
                                            move || {
                                                showcase_capture(
                                                    win.upcast_ref::<gtk::Widget>(),
                                                    &path,
                                                );
                                            },
                                        );
                                    }
                                }
                            },
                        );
                    }
                }
                // HYLKI_SHOWCASE_TOGGLE=starred|sent|drafts toggles that
                // unified row's list at 5s (what a long-press does).
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_TOGGLE") {
                    let kind = match which.as_str() {
                        "sent" => FolderKind::Sent,
                        "drafts" => FolderKind::Drafts,
                        "archive" => FolderKind::Archive,
                        _ => FolderKind::Starred,
                    };
                    let sb = model.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = sb.send(SidebarInput::ToggleKindExpand(kind));
                    });
                }
                // HYLKI_SHOWCASE_FOLD_FILTERED folds All Inboxes' Filtered
                // Folders section, to check its folded header.
                if std::env::var("HYLKI_SHOWCASE_FOLD_FILTERED").is_ok() {
                    let sb = model.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = sb.send(SidebarInput::ToggleFilteredExpand(
                            crate::ui::sidebar::Slot::Unified,
                        ));
                    });
                }
                // HYLKI_SHOWCASE_REPLY opens the inline reply composer on the
                // selected message, to check the composer's grounds (#148).
                // HYLKI_SHOWCASE_FLIP=dark|light then switches the app theme
                // at 6 s, to check a live flip re-resolves those grounds.
                // HYLKI_SHOWCASE_FOLDER=drafts|templates|sent|archive|junk|trash|outbox[@<s>]
                // switches to that folder at 2 s, before the staging's 3 s
                // selection moves onto its first row, or at the time given.
                // HYLKI_SHOWCASE_FOCUS=<seconds> switches Focus Mode on at
                // that moment (its parts as saved in focus.toml), so a
                // capture a little later catches the slide, and one later
                // still the settled layout. HYLKI_SHOWCASE_FOCUS_OFF=<seconds>
                // switches it off again.
                for (var, on) in [
                    ("HYLKI_SHOWCASE_FOCUS", true),
                    ("HYLKI_SHOWCASE_FOCUS_OFF", false),
                ] {
                    if let Some(at) = std::env::var(var).ok().and_then(|v| v.parse::<f64>().ok()) {
                        let s = sender.clone();
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis((at * 1000.0) as u64),
                            move || {
                                let mut focus = config::load_focus_mode();
                                focus.enabled = on;
                                s.input(AppMsg::Pref(PrefOutput::SetFocusMode(focus)));
                            },
                        );
                    }
                }
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_FOLDER") {
                    let (kind, at) = showcase_at(&v, 2);
                    if kind == "outbox" {
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(at, move || {
                            s.input(AppMsg::ShowOutbox)
                        });
                    }
                    let kind = match kind {
                        "drafts" => Some(FolderKind::Drafts),
                        "templates" => Some(FolderKind::Templates),
                        "sent" => Some(FolderKind::Sent),
                        "archive" => Some(FolderKind::Archive),
                        "junk" => Some(FolderKind::Junk),
                        "trash" => Some(FolderKind::Trash),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(at, move || {
                            s.input(AppMsg::ShowcaseFolder {
                                kind,
                                account: None,
                            });
                        });
                    }
                }
                // HYLKI_SHOWCASE_SEND=<seconds>[:<text>|:@<file>] writes in the
                // reply's editor and sends it at that moment, for the send
                // morph.
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_SEND") {
                    let (at, text) = v
                        .split_once(':')
                        .unwrap_or((v.as_str(), "Sounds good, ship it.\nJason"));
                    let at = at.parse::<f64>().unwrap_or(9.0);
                    // `@<path>` reads the text from a file.
                    let text = match text.strip_prefix('@') {
                        Some(path) => std::fs::read_to_string(path).unwrap_or_default(),
                        None => text.replace("\\n", "\n"),
                    };
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis((at * 1000.0) as u64),
                        move || {
                            s.input(AppMsg::ShowcaseSend(text));
                        },
                    );
                }
                // HYLKI_SHOWCASE_UNDO=<seconds> presses Undo on the first
                // message sent, to check it comes back where it was (#434).
                if let Some(at) = std::env::var("HYLKI_SHOWCASE_UNDO")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis((at * 1000.0) as u64),
                        move || {
                            s.input(AppMsg::UndoSend(1));
                        },
                    );
                }
                // HYLKI_SHOWCASE_COPY=<seconds> has the sent reply's copy
                // read back then, so its stand-in card can be seen to give
                // way to it.
                if let Some(at) = std::env::var("HYLKI_SHOWCASE_COPY")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis((at * 1000.0) as u64),
                        move || {
                            s.input(AppMsg::ShowcaseCopy);
                        },
                    );
                }
                // Two seconds on, it logs where focus landed (#266).
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_REPLY") {
                    // A number is the moment, in seconds (4 otherwise).
                    let at = v.parse::<u32>().unwrap_or(4);
                    let s = sender.clone();
                    let window = root.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || {
                        s.input(AppMsg::Reply);
                        // After the split's slide, which focus waits for.
                        gtk::glib::timeout_add_seconds_local_once(2, move || {
                            showcase_log_focus(&window)
                        });
                    });
                }
                // HYLKI_SHOWCASE_NEW=<seconds>[:<address>] opens a new message
                // at that moment (4 s unless it parses), addressed when an
                // address follows, and, a second later, logs
                // where keyboard focus landed: the field row's title, or the
                // focused widget's type (#266).
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_NEW") {
                    let (at, to) = v.split_once(':').unwrap_or((v.as_str(), ""));
                    let at = at.parse::<u32>().unwrap_or(4);
                    let to = to.to_string();
                    let s = sender.clone();
                    let window = root.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || {
                        s.input(if to.is_empty() {
                            AppMsg::Compose
                        } else {
                            AppMsg::ComposeTo(to)
                        });
                        gtk::glib::timeout_add_seconds_local_once(1, move || {
                            showcase_log_focus(&window)
                        });
                    });
                }
                // HYLKI_SHOWCASE_FORWARD=<seconds> does the same with
                // Forward at that moment (4 s unless it parses), to check
                // the original's attachments come along (#240). As
                // <seconds>:<account>:<id> it forwards that message of the
                // open conversation, the way its card's button would, so a
                // message that is not the conversation's newest can be
                // picked.
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_FORWARD") {
                    let mut parts = v.split(':');
                    let at = parts
                        .next()
                        .and_then(|p| p.parse::<u32>().ok())
                        .unwrap_or(4);
                    let target = parts.next().zip(parts.next()).and_then(|(a, id)| {
                        Some((a.parse::<u32>().ok()?, id.parse::<u32>().ok()?))
                    });
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || match target {
                        Some((account_id, id)) => {
                            s.input(AppMsg::ShowcaseForward { account_id, id })
                        }
                        None => s.input(AppMsg::Forward),
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_PREVIEW=1 turns the inline
                // composer's preview on a beat after it opens, so a
                // capture can show the rendered message rather than the
                // source it was written in.
                // HYLKI_SHOWCASE_COMPOSE_MENU=format opens the inline
                // composer's format chooser; any other value opens its
                // overflow menu. Pair either with HYLKI_SHOWCASE_MENU=main.
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_COMPOSE_MENU") {
                    let s = sender.clone();
                    let format = which == "format";
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::ShowcaseComposeMenu { format });
                    });
                }
                if std::env::var("HYLKI_SHOWCASE_COMPOSE_PREVIEW").is_ok() {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::ShowcaseComposePreview);
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_FROM=N picks the inline composer's
                // From entry N at 7 s, to check where the new account's
                // signature lands (#237).
                if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_COMPOSE_FROM")
                    .ok()
                    .map(|v| v.parse::<u32>())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        s.input(AppMsg::ShowcaseComposeFrom(n));
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_CLOSE=N cancels the inline composer
                // N seconds in (pair with HYLKI_SHOWCASE_REPLY), to check
                // that a closed composer's web view, and so its web process,
                // actually goes away (#221).
                if let Some(at) = std::env::var("HYLKI_SHOWCASE_COMPOSE_CLOSE")
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || {
                        s.input(AppMsg::ShowcaseComposeClose);
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_UNDO=1 runs the composer's history
                // (#200) through a scripted round of edits and undos on the
                // `hylki::compose::undo` log target: the one way to exercise
                // it without a keyboard, since the keys cannot be injected
                // on this desktop. Pair with HYLKI_SHOWCASE_REPLY.
                // Set it to markdown, html or plain to walk a source-mode
                // body (a textarea) instead of the rich document, to
                // `backspace` to trim text rather than type it, or to
                // `pause` to leave a long gap between two runs of typing.
                // HYLKI_SHOWCASE_COMPOSE_FORMAT=plain@6,rich@10 switches the
                // inline composer's format at those moments (#424). Pair
                // with HYLKI_SHOWCASE_REPLY.
                if let Ok(steps) = std::env::var("HYLKI_SHOWCASE_COMPOSE_FORMAT") {
                    for step in steps.split(',') {
                        let (name, at) = showcase_at(step, 6);
                        let format = match name {
                            "rich" => config::ComposeFormat::Rich,
                            "markdown" => config::ComposeFormat::Markdown,
                            "html" => config::ComposeFormat::Html,
                            "plain" => config::ComposeFormat::Plain,
                            _ => continue,
                        };
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(at, move || {
                            s.input(AppMsg::ShowcaseComposeFormat(format));
                        });
                    }
                }
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_COMPOSE_UNDO") {
                    let format = match which.as_str() {
                        "markdown" => Some(config::ComposeFormat::Markdown),
                        "html" => Some(config::ComposeFormat::Html),
                        "plain" => Some(config::ComposeFormat::Plain),
                        _ => None,
                    };
                    if let Some(format) = format {
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            s.input(AppMsg::ShowcaseComposeFormat(format));
                        });
                    }
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        s.input(AppMsg::ShowcaseComposeUndo);
                    });
                    // …and drop the burger menu open over it near the end,
                    // which is the state the entries are actually read in.
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(16, move || {
                        s.input(AppMsg::ShowcaseBurger);
                    });
                }
                // HYLKI_SHOWCASE_FILES=/a:/b hands those files in at 4 s,
                // as GNOME Files' "Send with Hylki" would; with
                // HYLKI_SHOWCASE_TOP=1 the capture takes the newest window
                // (the dialog asking about them) instead of the main one.
                if let Ok(list) = std::env::var("HYLKI_SHOWCASE_FILES") {
                    let s = sender.clone();
                    let paths: Vec<std::path::PathBuf> = list
                        .split(':')
                        .filter(|p| !p.is_empty())
                        .map(Into::into)
                        .collect();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        s.input(AppMsg::OpenWithFiles(paths));
                    });
                }
                if let Ok(flip) = std::env::var("HYLKI_SHOWCASE_FLIP") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::Pref(PrefOutput::SetAppTheme(if flip == "dark" {
                            config::AppTheme::Dark
                        } else {
                            config::AppTheme::Light
                        })));
                    });
                }
                // HYLKI_SHOWCASE_THEME=<id> picks that appearance theme at
                // 6 s, exactly as the Settings gallery does, so the live
                // repaint (chrome, reader and composer) can be captured.
                if let Ok(id) = std::env::var("HYLKI_SHOWCASE_THEME") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::Pref(PrefOutput::SetTheme(id.clone())));
                    });
                }
                // HYLKI_SHOWCASE_SHORTCUTS=<height> opens the shortcut list at
                // 4 s, that tall, so all of it fits a capture (with
                // HYLKI_SHOWCASE_TOP).
                if let Some(h) = std::env::var("HYLKI_SHOWCASE_SHORTCUTS")
                    .ok()
                    .and_then(|v| v.parse::<i32>().ok())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        s.input(AppMsg::ShowShortcuts);
                    });
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let tops = gtk::Window::toplevels();
                        if let Some(w) = (0..tops.n_items())
                            .filter_map(|i| tops.item(i))
                            .filter_map(|o| o.downcast::<adw::Window>().ok())
                            .find(|w| {
                                w.is_visible()
                                    && w.title().is_some_and(|t| t == i18n("Keyboard Shortcuts"))
                            })
                        {
                            w.set_default_size(w.default_width(), h);
                        }
                    });
                }
                // HYLKI_SHOWCASE_ACCOUNT=N opens account N's editor a beat
                // after the Settings window (with HYLKI_SHOWCASE_SETTINGS),
                // so the editor itself can be captured.
                // `new[:<provider label>]` opens a blank editor instead.
                if let Some(rest) = std::env::var("HYLKI_SHOWCASE_ACCOUNT")
                    .ok()
                    .and_then(|v| v.strip_prefix("new").map(str::to_string))
                {
                    let provider = rest.strip_prefix(':').map(str::to_string);
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        s.input(AppMsg::ShowcaseNewAccount(provider.clone()));
                    });
                }
                if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_ACCOUNT")
                    .ok()
                    .map(|v| v.parse::<u32>())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        s.input(AppMsg::SidebarContext(CtxAction::OpenAccountSettings(
                            n + 1,
                        )));
                    });
                }
                // HYLKI_SHOWCASE_EDITOR_DIRTY=1 types into the open account
                // editor's Label field at 7s, so leaving an edited editor can
                // be exercised without a keyboard.
                if std::env::var("HYLKI_SHOWCASE_EDITOR_DIRTY").is_ok() {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        s.input(AppMsg::ShowcaseDirtyEditor);
                    });
                }
                // HYLKI_SHOWCASE_SETTINGS_GO=<category> picks that sidebar
                // category at 8s, exactly as a click on its row does.
                if let Ok(page) = std::env::var("HYLKI_SHOWCASE_SETTINGS_GO") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(8, move || {
                        s.input(AppMsg::ShowSettingsPage(page.clone()));
                    });
                }
                // HYLKI_SHOWCASE_ADD_CONTACT=<address> opens Add to Contacts
                // for it at 6s and captures the dialog at 10s, beside the
                // HYLKI_SHOWCASE file, so the books it offers can be checked.
                if let Ok(addr) = std::env::var("HYLKI_SHOWCASE_ADD_CONTACT") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::AddContactAddr(addr));
                    });
                    gtk::glib::timeout_add_seconds_local_once(10, move || {
                        let tops = gtk::Window::toplevels();
                        let dialog = (0..tops.n_items())
                            .filter_map(|i| tops.item(i))
                            .filter_map(|o| o.downcast::<adw::MessageDialog>().ok())
                            .find(|d| d.is_visible());
                        match (dialog, std::env::var("HYLKI_SHOWCASE")) {
                            (Some(d), Ok(path)) => {
                                showcase_capture(d.upcast_ref(), &format!("{path}.contact.png"))
                            }
                            _ => tracing::warn!("showcase: no Add to Contacts dialog"),
                        }
                    });
                }
                // HYLKI_SHOWCASE_DIALOG=save|discard|cancel answers whatever
                // message dialog is on screen at 10s, captured first beside
                // the HYLKI_SHOWCASE file.
                if let Ok(answer) = std::env::var("HYLKI_SHOWCASE_DIALOG") {
                    gtk::glib::timeout_add_seconds_local_once(10, move || {
                        let tops = gtk::Window::toplevels();
                        let dialog = (0..tops.n_items())
                            .filter_map(|i| tops.item(i))
                            .filter_map(|o| o.downcast::<adw::MessageDialog>().ok())
                            .find(|d| d.is_visible());
                        match dialog {
                            Some(d) => {
                                if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
                                    showcase_capture(d.upcast_ref(), &format!("{path}.dialog.png"));
                                }
                                // Answering is what a button click emits; the
                                // click also takes the dialog off screen, so
                                // hide it (closing would answer a second time).
                                d.response(&answer);
                                d.set_visible(false);
                            }
                            None => tracing::warn!("showcase: no dialog to answer"),
                        }
                    });
                }
                // HYLKI_SHOWCASE_CONTACTS=list|new|group|search:<text>[@<n>]|filter:<n>|multi:<n>
                // opens Contacts at 3s, and with "new" its editor for a new
                // contact at 5s ("group": for a new group), or with "search:"
                // types the text into its search
                // then, so the directories are asked too (#307), and with
                // "@<n>" shows only book n of the filter at 6s; "filter:"
                // turns on its nth "Show only" filter at 5s; "multi:"
                // selects the first n entries at 5s.
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_CONTACTS") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        s.input(AppMsg::OpenContacts)
                    });
                    let page = model.contacts_page.sender().clone();
                    if which == "new" || which == "group" {
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            let _ = page.send(if which == "group" {
                                ContactsPageInput::NewGroup
                            } else {
                                ContactsPageInput::NewContact
                            });
                        });
                    } else if let Some(filter) = which
                        .strip_prefix("filter:")
                        .and_then(|n| n.parse::<usize>().ok())
                        .and_then(|n| crate::ui::contacts_page::ContactFilter::ALL.get(n).copied())
                    {
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            let _ = page.send(ContactsPageInput::SetFilter(filter, true));
                        });
                    } else if let Some(n) = which
                        .strip_prefix("multi:")
                        .and_then(|n| n.parse::<u32>().ok())
                    {
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            let _ = page.send(ContactsPageInput::SelectFirst(n));
                        });
                    } else if let Some(spec) = which.strip_prefix("search:") {
                        let (text, book) = match spec.rsplit_once('@') {
                            Some((t, n)) if n.parse::<u32>().is_ok() => {
                                (t.to_string(), n.parse::<u32>().ok())
                            }
                            _ => (spec.to_string(), None),
                        };
                        let p = page.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            let _ = p.send(ContactsPageInput::SetSearch(text));
                        });
                        if let Some(n) = book {
                            gtk::glib::timeout_add_seconds_local_once(6, move || {
                                let _ = page.send(ContactsPageInput::PickBook(n));
                            });
                        }
                    }
                }
                // HYLKI_SHOWCASE_SETTINGS=accounts|prefs opens the Settings
                // window on that panel (about: the About window) and
                // captures it instead of the main window, so its pages can
                // be checked in stills too.
                let settings = std::env::var("HYLKI_SHOWCASE_SETTINGS").ok();
                if let Some(panel) = settings.clone() {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        s.input(if panel == "accounts" {
                            AppMsg::OpenAccounts
                        } else if panel == "about" {
                            AppMsg::OpenAbout
                        } else {
                            AppMsg::OpenPreferences
                        });
                        // Any other value names a sidebar category (#141).
                        if panel != "accounts" && panel != "prefs" && panel != "about" {
                            let s = s.clone();
                            gtk::glib::timeout_add_seconds_local_once(1, move || {
                                s.input(AppMsg::ShowSettingsPage(panel));
                            });
                        }
                    });
                    // HYLKI_SHOWCASE_SETTINGS_REOPEN=1 closes the window at
                    // 5s and opens it again at 7s, to time a reopen.
                    if std::env::var("HYLKI_SHOWCASE_SETTINGS_REOPEN").is_ok() {
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            s.input(AppMsg::DebugCloseSettings);
                        });
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(7, move || {
                            s.input(AppMsg::OpenAccounts);
                        });
                    }
                }
                let main: gtk::Window = root.clone().upcast();
                let settings_window = move || {
                    let tops = gtk::Window::toplevels();
                    (0..tops.n_items())
                        .filter_map(|i| tops.item(i))
                        .filter_map(|o| o.downcast::<gtk::Window>().ok())
                        .filter(|w| *w != main && w.is_visible())
                        // The newest: a dialog over Settings, when one is up.
                        .last()
                };
                // With HYLKI_SHOWCASE_SCROLL set, the Settings window is
                // made tall a beat after opening and every scroller in it
                // is run to its end just before the capture, so the lower
                // groups of a panel can be checked.
                // The value is the fraction of the way down (default 1).
                let scroll: Option<f64> = std::env::var("HYLKI_SHOWCASE_SCROLL")
                    .ok()
                    .map(|v| v.parse().unwrap_or(1.0));
                if let (Some(_), Some(frac)) = (&settings, scroll) {
                    // HYLKI_SHOWCASE_SCROLL_WIDTH overrides the 720px width,
                    // for checking a panel at the window's real width.
                    let width: i32 = std::env::var("HYLKI_SHOWCASE_SCROLL_WIDTH")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(720);
                    let find = settings_window.clone();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        if let Some(w) = find() {
                            w.set_default_size(width, 1300);
                        }
                    });
                    let find = settings_window.clone();
                    gtk::glib::timeout_add_seconds_local_once(delay.saturating_sub(1), move || {
                        if let Some(w) = find() {
                            scroll_all_to(w.upcast_ref::<gtk::Widget>(), frac);
                        }
                    });
                }
                let win = root.clone();
                let top = std::env::var_os("HYLKI_SHOWCASE_TOP").is_some();
                // HYLKI_SHOWCASE_DELAY_MS names the moment to the
                // millisecond instead (a frame inside an animation).
                let delay_ms: u64 = std::env::var("HYLKI_SHOWCASE_DELAY_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(u64::from(delay) * 1000);
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(delay_ms),
                    move || {
                        let target = settings
                            .as_ref()
                            .map(|_| ())
                            .or(top.then_some(()))
                            .and_then(|_| settings_window())
                            .map(|w| w.upcast::<gtk::Widget>())
                            .unwrap_or_else(|| win.clone().upcast::<gtk::Widget>());
                        showcase_capture(&target, &shot);
                    },
                );
            }
        }

        // The list header's sort menu (moved out of the message list's own
        // toolbar): a stateful radio action feeding the list's SetSort.
        {
            use crate::ui::message_list::SortOrder;
            let sort_group = gtk::gio::SimpleActionGroup::new();
            let sort_action = gtk::gio::SimpleAction::new_stateful(
                "order",
                Some(gtk::glib::VariantTy::STRING),
                &"date_newest".to_variant(),
            );
            let list = model.message_list.sender().clone();
            sort_action.connect_activate(move |action, param| {
                if let Some(key) = param.and_then(|v| v.str()) {
                    action.set_state(&key.to_variant());
                    let _ = list.send(MessageListInput::SetSort(SortOrder::from_key(key)));
                }
            });
            sort_group.add_action(&sort_action);
            widgets
                .list_filter_btn
                .insert_action_group("sortmenu", Some(&sort_group));

            let _ = model.list_header_widgets.set(ListHeaderWidgets {
                revealers: vec![
                    widgets.lh_search.clone(),
                    widgets.lh_filter.clone(),
                    widgets.lh_count.clone(),
                ],
                overflow: widgets.lh_overflow.clone(),
                filter: SortFilterPopover::new(
                    widgets.list_filter_btn.clone(),
                    &sort_action,
                    sender.input_sender(),
                ),
                sort: sort_action,
            });
            // Focus Mode's chrome as saved: nothing is mapped yet, so the
            // revealers jump to their state without animating.
            model.sync_focus_chrome();
            model
                .reader_overflow_btn
                .set_visible(model.reader_overflow_wanted());
        }

        // mailto: URIs can arrive (via GApplication `open`) before this init
        // ran — install the live sender and drain anything that queued early.
        model.rebuild_help_menu();
        model.undo_action = Some(undo_action);
        model.redo_action = Some(redo_action);
        // Both start empty, so both start greyed out.
        model.refresh_undo_menu();
        model
            .notifications
            .emit(NotifyInput::SetConsoleEnabled(model.console_mode));
        model
            .notifications
            .emit(NotifyInput::SetAtTop(model.status_bar_top));
        // Screenshot/dev hook: open the status bar console shortly after
        // launch (pref permitting) so captures can show it.
        // HYLKI_SHOWCASE_ABOUT=1 opens the About window a beat after
        // launch; pair it with HYLKI_SHOWCASE_TOP=1 to capture that window
        // instead of the main one.
        if std::env::var("HYLKI_SHOWCASE_ABOUT").is_ok() {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                s.input(AppMsg::OpenAbout);
            });
        }
        if std::env::var("HYLKI_SHOWCASE_CONSOLE").is_ok() {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                s.input(AppMsg::OpenConsole);
            });
        }
        let _ = MAILTO_SENDER.set(sender.input_sender().clone());
        for uri in MAILTO_PENDING.lock().unwrap().drain(..) {
            sender.input(AppMsg::OpenMailto(uri));
        }
        for uri in MID_PENDING.lock().unwrap().drain(..) {
            sender.input(AppMsg::OpenMid(uri));
        }
        for path in FILE_PENDING.lock().unwrap().drain(..) {
            sender.input(AppMsg::OpenMessageFile(path));
        }
        for paths in ATTACH_PENDING.lock().unwrap().drain(..) {
            sender.input(AppMsg::OpenWithFiles(paths));
        }

        // Files dragged onto the window with no composer open in it bring
        // up the cards for starting a new message with them (#293).
        {
            use crate::ui::drop_zones::{DropContext, DropZones};
            let overlay = widgets.window_overlay.clone();
            let s = sender.input_sender().clone();
            let zones = DropZones::install(
                DropContext::NewMessage,
                &root,
                &overlay,
                move |choice, paths| {
                    s.emit(AppMsg::DropFilesAs(choice, paths));
                },
            );
            let slots = [
                model.reader_compose_revealer.clone(),
                model.contacts_compose_revealer.clone(),
                model.reader_split_top.clone(),
                model.reader_split_bottom.clone(),
            ];
            zones.set_gate(move || {
                !slots
                    .iter()
                    .any(|slot| slot.reveals_child() && slot.is_visible())
            });
            zones.set_refresh(|zones| {
                zones
                    .set_allow_inline(config::load_compose_format() == config::ComposeFormat::Rich);
                zones.set_cloud_names(
                    crate::cloud::load_enabled_accounts()
                        .into_iter()
                        .map(|a| a.name)
                        .collect(),
                );
            });
            // HYLKI_SHOWCASE_WINDOW_DROP=<attach|inline|cloud|none>:<file>[:<file>…]
            // raises the window's cards at 3 s, for a capture.
            if let Ok(v) = std::env::var("HYLKI_SHOWCASE_WINDOW_DROP") {
                let mut parts = std::env::split_paths(&v);
                let hover = match parts.next().as_deref().and_then(|p| p.to_str()) {
                    Some("attach") => Some(crate::ui::drop_zones::DropChoice::Attach),
                    Some("inline") => Some(crate::ui::drop_zones::DropChoice::Inline),
                    Some("cloud") => Some(crate::ui::drop_zones::DropChoice::Cloud),
                    _ => None,
                };
                let paths: Vec<std::path::PathBuf> = parts.collect();
                let (zones, host) = (zones.clone(), root.clone().upcast::<gtk::Widget>());
                gtk::glib::timeout_add_seconds_local_once(3, move || {
                    zones.set_allow_inline(
                        config::load_compose_format() == config::ComposeFormat::Rich,
                    );
                    zones.set_cloud_names(
                        crate::cloud::load_enabled_accounts()
                            .into_iter()
                            .map(|a| a.name)
                            .collect(),
                    );
                    zones.showcase(&paths, &host, hover);
                });
            }
            model.window_drop_zones = Some(zones);
        }
        // With a composer open in the window, files dropped anywhere else in
        // it go into that composer. The reader's web views would take a file
        // themselves and never pass it up, so the reader gets a target that
        // goes before them.
        root.add_controller(window_drop_target(&sender, gtk::PropagationPhase::Bubble));
        model
            .message_view
            .widget()
            .add_controller(window_drop_target(&sender, gtk::PropagationPhase::Capture));
        if let Ok(list) = std::env::var("HYLKI_SHOWCASE_DROP") {
            let paths: Vec<std::path::PathBuf> = std::env::split_paths(&list).collect();
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || s.input(AppMsg::DropFiles(paths)));
        }

        // Send Later (#145): the app's clock. Every half minute, any queued
        // message whose time has come is flushed by id, which sends it even
        // though the ordinary flush leaves scheduled mail alone. One try per
        // scheduling: a failure shows in the Outbox with Send Now to retry,
        // rather than a loud attempt every tick.
        {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local(30, move || {
                s.input(AppMsg::SendDueScheduled);
                gtk::glib::ControlFlow::Continue
            });
        }
        {
            // Settings, built hidden a moment after startup (see PrewarmSettings).
            let s = sender.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                s.input(AppMsg::PrewarmSettings);
            });
        }
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        self.route(msg, sender)
    }
}
