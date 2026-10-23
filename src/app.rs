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
mod window;
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
use diagnostics::start_heap_trim;
use filter_run::{FilterRun, FolderTally};
use flags::{read_label, sender_label, star_label};
use folders::{apply_folder_roles, kind_label, merged_unread, same_folder_list};
use hand_off::{FileHandOff, HandOffFiles, HandOffTarget};
use keymap::{ctrl_shortcut_for, focus_in_compose, gmail_sequence, GoTarget, WindowCompose};
pub(crate) use keymap::{
    focus_is_text, focus_takes_keys, gmail_shortcut_for, reader_keeps, shortcut_for,
    without_caps_lock, Shortcut,
};
use list_header::{ListHeaderWidgets, LIST_WIDE_ON};
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
use reader_toolbar::ReaderToolbarWidgets;
use release_notes::release_notes_page;
use search::default_scope;
use send_morph::{MorphPending, Provisional};
pub(crate) use showcase::showcase_capture;
use showcase::showcase_pgp;
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
use uri_queue::{eml_file_name, parse_mailto, FILE_FOLDER};
pub(crate) use uri_queue::{
    is_message_file, queue_attach_files, queue_mailto, queue_message_file, queue_mid,
};
use window::install_window_actions;

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
        model.init_reader_header_menu(&widgets, &sender);
        model.init_reader_toolbar_fold(&widgets, &sender);
        model.init_compose_overlay(&widgets);
        model.init_gallery_page(&widgets, &sender);
        model.install_notification_actions(&sender);
        model.init_window_size(&root, &widgets, &sender);
        model.init_hover_peek(&widgets, &sender);
        model.sidebar_split = Some(widgets.sidebar_split.clone());
        model.peek_split = Some(widgets.peek_split.clone());
        model.app_title = Some(widgets.app_title.clone());
        model.sidebar_header = Some(widgets.sidebar_header.clone());
        model.sidebar_menu = Some(widgets.sidebar_menu.clone());
        model.init_refresh_buttons(&widgets, &sender);

        model.install_app_actions(&root, &sender);

        let (undo_action, redo_action) = install_window_actions(&root, &sender);

        root.add_controller(palette_key(&sender));

        model.install_key_controller(&root, &sender);

        model.show_startup_notices(&sender);

        start_heap_trim();
        model.install_showcase_probes(&sender);
        model.stage_showcase(&root, &widgets, &sender);

        model.init_list_sort_menu(&widgets, &sender);

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
        model.install_showcase_overlays(&sender);
        model.drain_hand_off_queues(&sender);

        model.install_drop_targets(&root, &widgets, &sender);

        model.start_outbox_clocks(&sender);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        self.route(msg, sender)
    }
}
