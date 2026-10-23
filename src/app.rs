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
mod msg;
mod navigation;
mod notifications;
mod outbox;
mod palette;
mod pending;
mod picker;
mod popout;
mod prefill;
mod prefs_general;
mod prefs_reader;
mod quote;
mod reader_toolbar;
mod release_notes;
mod removal;
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
use accounts::{
    forget_account_secrets, look_only_change, reconcile_goa, secrets_kept, GoaReconcile,
};
use attachments::{prune_downloads, save_attachment, AttachmentTarget};
use compose_host::{ComposeHost, ReaderCompose};
use consts::{ATTACHMENT_CACHE_BUDGET, BODY_CACHE_BUDGET, READER_MIN_WIDTH};
use contacts::{import_summary, reload_contacts, sync_contact_books};
use conversation::{
    batch_bodies_by_folder, body_key, is_event_target, next_after_vanish, BodyKey, MissingBody,
    CARRIED_BODY_LIMIT, THREAD_BODY_WAIT,
};
use demo::{demo_account_configs_saved, demo_filters, demo_mode, demo_tags};
use filter_run::{FilterRun, FolderTally};
use flags::{read_label, sender_label, star_label};
use folders::{apply_folder_roles, kind_label, merged_unread, same_folder_list};
use gallery::GALLERY_DATA_CAP;
use hand_off::{window_drop_target, FileHandOff, HandOffFiles, HandOffTarget};
use keymap::{ctrl_shortcut_for, focus_in_compose, gmail_sequence, GoTarget, WindowCompose};
pub(crate) use keymap::{
    focus_is_text, focus_takes_keys, gmail_shortcut_for, reader_keeps, shortcut_for,
    without_caps_lock, Shortcut,
};
use list_header::{ListHeaderWidgets, SortFilterPopover, LIST_WIDE_OFF, LIST_WIDE_ON};
pub(crate) use model::AppModel;
pub(crate) use msg::AppMsg;
use notifications::notified_message;
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
    eml_file_name, parse_mailto, parse_mid, ATTACH_PENDING, FILE_FOLDER, FILE_PENDING,
    MAILTO_PENDING, MAILTO_SENDER, MID_PENDING,
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
        match msg {
            AppMsg::ShowOutbox => {
                self.close_sidebar_peek();
                self.mirror_selection(crate::ui::sidebar::Sel::Outbox);
                // Treated as a folder: same list, same reader, nothing swapped
                // out from under the user. `selected` stays None so no sync or
                // server request is ever aimed at it.
                self.showing_outbox = true;
                self.leave_gallery();
                self.showing_contacts = false;
                self.unified = false;
                self.tag_view = None;
                self.smart_view = None;
                self.selected = None;
                self.current = None;
                self.current_thread.clear();
                self.attachments.clear();
                self.attachments_loading = false;
                self.sync_attachment_drawer();
                self.message_list.emit(MessageListInput::SetSelected(None));
                self.message_list.emit(MessageListInput::ResetPaging);
                self.message_list
                    .emit(MessageListInput::SetScope(SearchScope::ThisFolder));
                self.message_list
                    .emit(MessageListInput::SetColorize(self.accounts.len() > 1));
                self.show_message(None, false);
                self.push_outbox();
            }

            AppMsg::OutboxItems { account_id, items } => {
                self.outbox_by_account.insert(account_id, items);
                self.push_outbox();
            }

            AppMsg::EditCurrentOutbox => {
                if let Some(m) = self.current.clone() {
                    match self.outgoing_ticket(m.id) {
                        Some(ticket) => self.undo_send(ticket, &sender),
                        None => self.compose_from_outbox(m.account_id, m.id, &sender),
                    }
                }
            }

            AppMsg::SendCurrentOutbox => {
                if let Some(m) = self.current.clone() {
                    let in_flight = self
                        .outgoing
                        .borrow()
                        .iter()
                        .any(|o| crate::outgoing::row_id(o.ticket) == m.id);
                    match self.outgoing_ticket(m.id) {
                        Some(ticket) => self.dispatch_send(ticket),
                        None if in_flight => {}
                        None => {
                            self.send_to(m.account_id, MailRequest::FlushOutbox { id: Some(m.id) })
                        }
                    }
                }
            }

            AppMsg::SendDueScheduled => {
                // Its one try would be spent on no network; the time is
                // still past when the network is back (#442).
                if !crate::power::reaches_internet() {
                    return;
                }
                let now = crate::datefmt::now();
                let due: Vec<(u32, u32)> = self
                    .outbox_by_account
                    .iter()
                    .flat_map(|(account_id, items)| {
                        items
                            .iter()
                            .filter(|i| i.send_at.is_some_and(|t| t <= now) && i.attempts <= 1)
                            .map(move |i| (*account_id, i.id))
                    })
                    .collect();
                for (account_id, id) in due {
                    self.send_to(account_id, MailRequest::FlushOutbox { id: Some(id) });
                }
            }

            AppMsg::RetryAllOutbox => {
                let held: Vec<u64> = self
                    .outgoing
                    .borrow()
                    .iter()
                    .filter(|o| o.held)
                    .map(|o| o.ticket)
                    .collect();
                for ticket in held {
                    self.dispatch_send(ticket);
                }
                let accounts: Vec<u32> = self
                    .outbox_by_account
                    .iter()
                    .filter(|(_, items)| !items.is_empty())
                    .map(|(id, _)| *id)
                    .collect();
                for account_id in accounts {
                    self.send_to(account_id, MailRequest::FlushOutbox { id: None });
                }
            }

            AppMsg::Notice(text) => {
                self.notifications.emit(NotifyInput::Push {
                    text,
                    error: false,
                    connectivity: false,
                });
            }

            AppMsg::ShowAttachments => {
                self.close_sidebar_peek();
                self.mirror_selection(crate::ui::sidebar::Sel::Attachments);
                self.showing_outbox = false;
                self.showing_gallery = true;
                self.showing_contacts = false;
                self.gallery.emit(GalleryInput::SetLoading(true));
                // Handing over the folder list is what starts the load: the
                // gallery asks for its first page once it knows the scope, so
                // the query is never run against an empty one.
                self.gallery
                    .emit(GalleryInput::SetAccounts(self.gallery_scope()));
                // Keep the scan working through whatever is still unindexed, so
                // the archive fills in behind the user as they browse.
                self.start_attachment_scan();
            }

            AppMsg::GalleryFetch {
                account_id,
                folder_path,
                uid,
            } => {
                // The gallery knows this file exists because the scan described
                // it, but its bytes were never downloaded. Reuse the reader's
                // own fetch: it stores what comes back, so the next visit finds
                // it cached and the thumbnail appears.
                self.send_to(
                    account_id,
                    MailRequest::LoadAttachments {
                        message_id: uid,
                        path: folder_path,
                        uid,
                        download: true,
                    },
                );
                self.gallery.emit(GalleryInput::SetFetching(true));
            }

            AppMsg::AttachmentsScanned {
                account_id,
                folder_path,
                added,
                remaining,
            } => {
                let folder_path_key = folder_path.clone();
                // Keep going while this folder has more to describe. The reply
                // to each chunk is what asks for the next, so the scan runs at
                // whatever pace the server answers and never piles up requests.
                if remaining > 0 && self.showing_gallery {
                    self.send_to(account_id, MailRequest::ScanAttachments { folder_path });
                }
                // Only refresh when the scan actually found something, and only
                // while the gallery is on screen — a chunk of mail with no
                // attachments after all should not reshuffle what's on show.
                if added > 0 && self.showing_gallery {
                    self.gallery.emit(GalleryInput::ScanProgress);
                }
                self.gallery_scan_left
                    .insert((account_id, folder_path_key), remaining);
                let left: u32 = self.gallery_scan_left.values().sum();
                self.gallery.emit(GalleryInput::ScanStatus(left));
            }

            AppMsg::GalleryLoad(req) => {
                let Some(cache) = self.cache.as_ref() else {
                    self.gallery.emit(GalleryInput::Page {
                        items: Vec::new(),
                        total: 0,
                        offset: req.offset,
                    });
                    return;
                };
                let q = crate::cache::GalleryQuery {
                    folders: &req.folders,
                    account_id: req.account_id,
                    tokens: &req.tokens,
                    bucket: req.bucket,
                    sort: req.sort,
                    limit: req.limit,
                    offset: req.offset,
                    data_cap: GALLERY_DATA_CAP,
                };
                let items = cache.gallery_page(&q);
                // Only worth counting for the first page: the total cannot
                // change while the user scrolls one set of results.
                let total = if req.offset == 0 {
                    cache.gallery_total(&q)
                } else {
                    self.gallery_total
                };
                self.gallery_total = total;
                self.gallery.emit(GalleryInput::Page {
                    items,
                    total,
                    offset: req.offset,
                });
            }

            AppMsg::OpenAttachmentMessage {
                account_id,
                folder_path,
                uid,
            } => {
                self.leave_gallery();
                self.showing_contacts = false;
                self.showing_outbox = false;
                if let Some(folder) = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.path == folder_path))
                    .cloned()
                {
                    self.select_folder(
                        account_id,
                        folder.id,
                        folder.name.clone(),
                        folder.path.clone(),
                    );
                    // Messages use their UID as id, so select by (account, uid).
                    self.message_list
                        .emit(MessageListInput::SelectAndLoad((account_id, uid)));
                }
            }

            AppMsg::UnifiedSelected(view) => {
                self.open_unified(view, &sender);
            }

            AppMsg::AccountStarredSelected(account_id) => {
                self.open_unified_in(
                    UnifiedView::Kind(FolderKind::Starred),
                    Some(account_id),
                    &sender,
                );
            }

            AppMsg::FolderSelected {
                account_id,
                folder_id,
                name,
                path,
            } => {
                self.close_sidebar_peek();
                self.select_folder(account_id, folder_id, name, path);
            }

            AppMsg::OpenMessageFromNotification {
                account_id,
                folder_id,
                message_id,
            } => {
                // The user clicked a new-mail notification: they've engaged with
                // that account's mail, so clear its toast, then navigate to the
                // message's folder and open it in the reader.
                crate::notify::withdraw_mail(account_id);
                tracing::info!(
                    "notification: open account {account_id} folder {folder_id} message {message_id} (unified row: {})",
                    self.unified_inboxes_shown()
                );
                if let Some((name, path, kind)) = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
                    .map(|f| (f.name.clone(), f.path.clone(), f.kind))
                {
                    // Ask for the body before anything else goes to the
                    // account's worker. Opening Inboxes below asks every
                    // account for its inbox list, and the worker serves one
                    // request at a time: the body request the selection
                    // sends afterwards would wait behind that whole fetch
                    // (and the IDLE hand-off before it), so the message the
                    // user clicked sat on a spinner while the list loaded.
                    // Sent first, it comes back into the body cache, which
                    // the selection reads before fetching. A body already
                    // there (prefetched on arrival) needs nothing.
                    let uid = self
                        .message_cache
                        .get(&(account_id, folder_id))
                        .and_then(|msgs| msgs.iter().find(|m| m.id == message_id))
                        .map(|m| m.uid);
                    if let Some(uid) = uid {
                        if !self.body_cache.contains_key(&(account_id, folder_id, uid)) {
                            self.send_to(
                                account_id,
                                MailRequest::LoadBody {
                                    message_id,
                                    path: path.clone(),
                                    uid,
                                },
                            );
                        }
                    }
                    // Mail that landed in an inbox opens in the unified
                    // Inboxes when the sidebar has that row (the view the
                    // app opens with; the account's own Inbox row may sit
                    // inside a folded section, where a highlight has nowhere
                    // to show). Without the row — one account, or the
                    // preference off — or when a filter filed the message
                    // elsewhere, its folder opens directly and the sidebar
                    // unfolds the account to show where that is. Both emit
                    // the (cached) list synchronously, so the SelectAndLoad
                    // that follows finds the row and opens it.
                    if kind == FolderKind::Inbox
                        && self.unified_inboxes_shown()
                        && self.in_unified(account_id)
                    {
                        self.open_unified(UnifiedView::Kind(FolderKind::Inbox), &sender);
                    } else {
                        self.select_folder(account_id, folder_id, name, path);
                    }
                    self.message_list
                        .emit(MessageListInput::SelectAndLoad((account_id, message_id)));
                }
            }

            AppMsg::NotificationMarkRead {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    // set_read clears the account's notification itself.
                    self.user_set_flags(read_label(true), &[m], FlagKind::Read, true);
                }
            }

            AppMsg::NotificationArchive {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.move_to(m, FolderKind::Archive);
                    crate::notify::withdraw_mail(account_id);
                }
            }

            AppMsg::NotificationDelete {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    // The same path as the Delete key: to Trash, with the
                    // usual undo; a message already there asks first.
                    self.delete_messages(vec![m], &sender);
                    crate::notify::withdraw_mail(account_id);
                }
            }

            AppMsg::NotificationSpam {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.mark_spam_msg(m);
                    crate::notify::withdraw_mail(account_id);
                }
            }

            // The message is opening in the reader (the action sent
            // OpenMessageFromNotification first); the composer waits for
            // the body it quotes when that is still on its way.
            AppMsg::NotificationReply {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    let m = self.with_cached_body(m);
                    if m.body.is_empty() {
                        self.pending_notified_reply = Some(m);
                    } else {
                        self.open_reply(
                            m.account_id,
                            self.reply_context(&m, reply_prefill(&m)),
                            (m.account_id, m.id),
                            &sender,
                        );
                    }
                }
            }

            AppMsg::NotificationForward {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.forward(m, true, &sender);
                }
            }

            AppMsg::AccountFolded { id, folded } => {
                // The sidebar already animated it; just record the state (a
                // rebuild here would interrupt the animation).
                if let Some(email) = self.email_of(id) {
                    let was = self.collapsed.contains(&email);
                    if was != folded {
                        if folded {
                            self.collapsed.push(email);
                        } else {
                            self.collapsed.retain(|e| *e != email);
                        }
                        self.save_sidebar_state();
                    }
                }
            }

            AppMsg::ToggleCustomFolders(account_id) => {
                // The sidebar animated the toggle locally; record the new state
                // (the "folders_expanded" list holds accounts whose custom
                // folders are revealed; absence means hidden, the default).
                if let Some(email) = self.email_of(account_id) {
                    if let Some(pos) = self.folders_expanded.iter().position(|e| *e == email) {
                        self.folders_expanded.remove(pos);
                    } else {
                        self.folders_expanded.push(email);
                    }
                    self.save_sidebar_state();
                }
            }

            AppMsg::ReorderFolders { account_id, paths } => {
                let custom = self.folders.get(&account_id).is_some_and(|fs| {
                    fs.iter()
                        .any(|f| f.kind == FolderKind::Custom && paths.contains(&f.path))
                });
                if custom {
                    self.adopt_custom_order(account_id);
                }
                self.keep_folder_order(account_id, &paths);
            }

            AppMsg::PlaceFolder {
                account_id,
                path,
                dest,
                target,
                after,
            } => {
                self.adopt_custom_order(account_id);
                // Moved on the server first (the same move as a drop on a
                // folder, with its checks and its undo); only once that has
                // gone ahead does it take its place beside `target`.
                let Some(new_path) = self.move_folder(account_id, path, dest) else {
                    return;
                };
                let shown = self.shown_folders(account_id);
                let hierarchy = crate::ui::sidebar::Hierarchy {
                    delimiter: self.folder_delimiter(account_id),
                    namespace: self.folder_namespace(account_id),
                };
                if let Some(paths) =
                    crate::ui::sidebar::placed_run(&shown, &hierarchy, &new_path, &target, after)
                {
                    self.keep_folder_order(account_id, &paths);
                }
            }

            AppMsg::SetAccountFolderSort { account_id, sort } => {
                self.set_account_folder_sort(account_id, sort);
                self.rebuild_sidebar();
            }

            AppMsg::Pref(PrefOutput::SetFolderSort(sort)) => {
                if pref!(self.folder_sort = sort) {
                    self.rebuild_sidebar();
                }
            }

            AppMsg::FolderNodeCollapsed {
                account_id,
                path,
                collapsed,
            } => {
                // The sidebar already reshaped its rows; just remember it.
                if let Some(email) = self
                    .accounts
                    .iter()
                    .find(|a| a.id == account_id)
                    .map(|a| a.email.clone())
                {
                    let key = format!("{email}\t{path}");
                    if collapsed {
                        if !self.tree_collapsed.contains(&key) {
                            self.tree_collapsed.push(key);
                        }
                    } else {
                        self.tree_collapsed.retain(|k| *k != key);
                    }
                    self.save_sidebar_state();
                }
            }

            AppMsg::SidebarCollapsed(collapsed) => {
                tracing::info!(
                    "peek: sidebar reports collapsed={collapsed} (peek={} auto_rail={} rail_active={})",
                    self.sidebar_peek, self.auto_rail, self.rail_active
                );
                // The sidebar component has already switched its own rows; this
                // is the app-side reaction (split widths, header, persistence).
                // At a width that can host the full sidebar, the arrow inside a
                // floating peek PINS it: the sidebar becomes the normal
                // side-by-side pane (persisted) and hover-expand goes dormant
                // until it is collapsed again. In the narrow window the toggle
                // only opens/closes the overlay — there is no room to pin.
                if self.sidebar_peek && !self.auto_rail && collapsed {
                    self.pin_sidebar_from_peek();
                } else if self.auto_rail {
                    // Narrow window: the rail never expands in place — put
                    // its rows straight back and float the peek panel out
                    // (or fold it) instead. Only a showcase toggle gets here;
                    // the header button goes through AppMsg::ToggleSidebar.
                    self.sidebar.emit(SidebarInput::SetCollapsed(true));
                    self.rail_active = true;
                    self.set_sidebar_peek(!collapsed, true);
                } else {
                    // Focus Mode holding the rail borrows the sidebar's
                    // layout: expanding it for a moment is a look, not a
                    // choice, so only what is on screen moves.
                    if !self.sidebar_layout_borrowed() {
                        self.sidebar_collapsed = collapsed;
                    }
                    self.rail_active = collapsed;
                    self.animate_sidebar(collapsed);
                    self.compact_sidebar_header(collapsed);
                    if !self.sidebar_layout_borrowed() {
                        self.save_sidebar_state();
                    }
                }
            }

            AppMsg::AutoRail(on) => {
                tracing::info!("peek: auto-rail {on} (peek={})", self.sidebar_peek);
                self.auto_rail = on;
                if !on && self.sidebar_peek {
                    // Widened with the panel open: fold it back before the
                    // split view returns to side-by-side.
                    self.set_sidebar_peek(false, false);
                    self.rail_active = true;
                }
                // The rail wins while the window is narrow; the user's own
                // choice comes back the moment there is room again. Nothing is
                // persisted here — this is the window's shape, not a preference.
                let want = self.rail_wanted();
                if want != self.rail_active {
                    self.rail_active = want;
                    self.sidebar.emit(SidebarInput::SetCollapsed(want));
                    self.animate_sidebar(want);
                    self.compact_sidebar_header(want);
                }
            }

            AppMsg::ToggleSidebar => {
                tracing::info!(
                    "peek: toggle button (peek={} auto_rail={} rail_active={})",
                    self.sidebar_peek,
                    self.auto_rail,
                    self.rail_active
                );
                // Self-healing: with no peek open, the split should always be
                // un-collapsed and showing (rail or side-by-side). If it ended
                // hidden — a lost race in the peek machinery left users with no
                // sidebar and a dead button until relaunch (seen on 1.17.1) —
                // the button's job is to bring the sidebar back, not to feed
                // a toggle into state that already lost the plot: repair to
                // the rail first, then toggle normally from there.
                if self.sidebar_peek {
                    if self.auto_rail {
                        // Narrow window: the button folds the floating peek
                        // back to the rail.
                        self.rail_active = true;
                        self.set_sidebar_peek(false, true);
                    } else {
                        // A hover peek at a width with room for the full
                        // sidebar: the button pins it side by side.
                        self.pin_sidebar_from_peek();
                    }
                } else {
                    if let Some(split) = self.sidebar_split.clone() {
                        if split.is_collapsed() || !split.shows_sidebar() {
                            tracing::info!(
                                "peek: toggle found split hidden (collapsed={} shown={}) — repairing",
                                split.is_collapsed(),
                                split.shows_sidebar()
                            );
                            self.rail_active = true;
                            self.sidebar.emit(SidebarInput::SetCollapsed(true));
                            self.compact_sidebar_header(true);
                            self.peek_transition.set(true);
                            split.set_min_sidebar_width(SIDEBAR_RAIL_WIDTH);
                            split.set_max_sidebar_width(SIDEBAR_RAIL_WIDTH);
                            split.set_collapsed(false);
                            split.set_show_sidebar(true);
                            self.peek_transition.set(false);
                        }
                    }
                    if self.auto_rail {
                        // Narrow window: the toggle floats the peek panel
                        // out beside the rail; the rail itself never expands.
                        self.rail_active = false;
                        self.set_sidebar_peek(true, true);
                    } else {
                        self.sidebar.emit(SidebarInput::ToggleCollapsed);
                    }
                }
            }

            AppMsg::SidebarPeekDismissed => {
                tracing::info!("peek: dismissed (peek={})", self.sidebar_peek);
                if self.sidebar_peek {
                    self.rail_active = true;
                    self.set_sidebar_peek(false, true);
                }
            }

            AppMsg::SidebarHoverEnter => {
                // Hover-expand (preference): the icon rail floats the full
                // sidebar out without a click — whether the rail comes from
                // the narrow-window breakpoint or the user's own collapse.
                // The same peek the expand button opens, dismissed the same
                // ways (navigation, or a click outside the panel).
                let rail_up = self.rail_wanted();
                if self.sidebar_hover_expand && rail_up && !self.sidebar_peek {
                    self.rail_active = false;
                    self.set_sidebar_peek(true, true);
                }
            }

            AppMsg::Pref(PrefOutput::SetSidebarHoverExpand(on)) => {
                pref!(self.sidebar_hover_expand = on);
            }

            AppMsg::Pref(PrefOutput::SetRememberSidebar(on)) => {
                pref!(self.remember_sidebar = on);
            }

            AppMsg::Pref(PrefOutput::SetRememberRail(on)) => {
                pref!(self.remember_rail = on);
            }

            AppMsg::SidebarSectionsOpen {
                all_inboxes,
                filtered,
                tags,
                starred,
                sent,
                drafts,
                archive,
                junk,
                trash,
                favorites,
                smart,
                contacts,
            } => {
                let now = (
                    all_inboxes,
                    filtered,
                    tags,
                    starred,
                    sent,
                    drafts,
                    archive,
                    junk,
                    trash,
                    favorites,
                    smart,
                    contacts,
                );
                let was = (
                    self.unified_expanded,
                    self.filtered_expanded,
                    self.tags_expanded,
                    self.starred_expanded,
                    self.sent_expanded,
                    self.drafts_expanded,
                    self.archive_expanded,
                    self.junk_expanded,
                    self.trash_expanded,
                    self.favorites_expanded,
                    self.smart_expanded,
                    self.contacts_expanded,
                );
                if now != was {
                    self.unified_expanded = all_inboxes;
                    self.filtered_expanded = filtered;
                    self.tags_expanded = tags;
                    self.starred_expanded = starred;
                    self.sent_expanded = sent;
                    self.drafts_expanded = drafts;
                    self.archive_expanded = archive;
                    self.junk_expanded = junk;
                    self.trash_expanded = trash;
                    self.favorites_expanded = favorites;
                    self.smart_expanded = smart;
                    self.contacts_expanded = contacts;
                    self.save_sidebar_state();
                }
            }

            AppMsg::Pref(PrefOutput::SetSeenCounts(on)) => {
                if pref!(self.seen_counts = on) {
                    // What is unread now counts as looked at: only mail
                    // from here on is new.
                    self.unread_seen.borrow_mut().clear();
                    self.push_unread_counts();
                }
            }

            AppMsg::Pref(PrefOutput::SetRailDots(on)) => {
                if pref!(self.rail_dots = on) {
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetRailFold(fold)) => {
                if pref!(self.rail_fold = fold) {
                    // The sidebar folds (or reopens) the sections concerned
                    // itself if the rail is up.
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetAppTheme(theme)) => {
                if self.app_theme != theme {
                    self.app_theme = theme;
                    apply_app_theme(theme);
                    self.save_settings();
                }
            }

            AppMsg::Pref(PrefOutput::SetTextScale(percent)) => {
                if self.text_scale != percent {
                    self.text_scale = percent;
                    crate::text_scale::apply(percent);
                    self.save_settings();
                }
            }

            AppMsg::Pref(PrefOutput::SetTheme(id)) => {
                if self.theme != id {
                    self.theme = id.clone();
                    // Repaints the chrome and tells the reader and any open
                    // composer to re-ground their documents. The colors the
                    // scheme-dependent CSS reads back are taken a main-loop
                    // pass later, as they are on a light/dark flip, so the
                    // lookups answer for the palette that just landed.
                    crate::theme::set(&id);
                    gtk::glib::idle_add_local_once(refresh_scheme_css);
                    self.save_settings();
                }
            }

            AppMsg::SidebarContext(action) => match action {
                CtxAction::MarkFolderRead {
                    account_id,
                    folder_id,
                } => {
                    self.mark_folder_read(account_id, folder_id);
                }
                CtxAction::RefreshFolder {
                    account_id,
                    folder_id,
                } => {
                    if let Some(path) = self
                        .folders
                        .get(&account_id)
                        .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
                        .map(|f| f.path.clone())
                    {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                }
                CtxAction::MarkAllInboxesRead => {
                    // The inboxes the row merges, as its chip counts them.
                    let inboxes: Vec<(u32, u32)> = self
                        .accounts
                        .iter()
                        .filter(|a| self.in_unified(a.id))
                        .filter_map(|a| self.inbox_of(a.id).map(|f| (a.id, f.id)))
                        .collect();
                    for (account_id, folder_id) in inboxes {
                        self.mark_folder_read(account_id, folder_id);
                    }
                }
                CtxAction::RefreshAllInboxes => {
                    let reqs: Vec<(u32, u32, String)> = self
                        .accounts
                        .iter()
                        .filter(|a| self.in_unified(a.id))
                        .filter_map(|a| self.inbox_of(a.id).map(|f| (a.id, f.id, f.path.clone())))
                        .collect();
                    for (account_id, folder_id, path) in reqs {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                }
                CtxAction::EmptyAll(kind) => {
                    self.confirm_empty_all(kind, &sender);
                }
                CtxAction::ApplyFilters {
                    account_id,
                    folder_id,
                } => {
                    self.start_filter_run(vec![(account_id, folder_id)], &sender);
                }
                CtxAction::EditFilter { account_id, path } => {
                    // The first rule filing into this folder, in the
                    // Filters page's editor.
                    let email = self.email_of(account_id);
                    let index = email.and_then(|email| {
                        self.filters.iter().position(|r| {
                            r.account_email.eq_ignore_ascii_case(&email) && r.dest_path == path
                        })
                    });
                    if let Some(i) = index {
                        self.open_settings_window(&sender, true, false);
                        if let Some(p) = &self.prefs {
                            p.emit(PrefInput::ShowPageById("filters".to_string()));
                        }
                        if let Some(acc) = &self.accounts_win {
                            acc.emit(crate::ui::accounts::AccountsInput::EditFilter(i));
                        }
                    }
                }
                CtxAction::NewSmartFolder => {
                    self.open_settings_window(&sender, true, false);
                    if let Some(p) = &self.prefs {
                        p.emit(PrefInput::ShowPageById("smart".to_string()));
                    }
                    if let Some(acc) = &self.accounts_win {
                        acc.emit(crate::ui::accounts::AccountsInput::AddSmartFolder);
                    }
                }
                CtxAction::EditSmartFolder(id) => {
                    if let Some(i) = self.smart_folders.iter().position(|f| f.id == id) {
                        self.open_settings_window(&sender, true, false);
                        if let Some(p) = &self.prefs {
                            p.emit(PrefInput::ShowPageById("smart".to_string()));
                        }
                        if let Some(acc) = &self.accounts_win {
                            acc.emit(crate::ui::accounts::AccountsInput::EditSmartFolder(i));
                        }
                    }
                }
                CtxAction::DeleteSmartFolder(id) => {
                    self.confirm_delete_smart_folder(id, &sender);
                }
                CtxAction::EditTag(keyword) => {
                    if let Some(i) = self.tags.iter().position(|t| t.keyword == keyword) {
                        self.open_settings_window(&sender, true, false);
                        if let Some(p) = &self.prefs {
                            p.emit(PrefInput::ShowPageById("tags".to_string()));
                        }
                        if let Some(acc) = &self.accounts_win {
                            acc.emit(crate::ui::accounts::AccountsInput::EditTag(i));
                        }
                    }
                }
                CtxAction::OpenAccountSettings(account_id) => {
                    // Straight to this account's editor, not the accounts list.
                    let email = self
                        .accounts
                        .iter()
                        .find(|a| a.id == account_id)
                        .map(|a| a.email.clone());
                    self.open_settings_window(&sender, true, false);
                    if let (Some(email), Some(acc)) = (email, &self.accounts_win) {
                        acc.emit(crate::ui::accounts::AccountsInput::EditAccountByEmail(
                            email,
                        ));
                    }
                }
                CtxAction::RemoveAccount(account_id) => {
                    self.confirm_remove_account(account_id, &sender);
                }
                CtxAction::NewFolder(account_id) => {
                    self.prompt_new_folder(account_id, &sender);
                }
                CtxAction::DeleteFolder {
                    account_id,
                    name,
                    path,
                } => {
                    self.confirm_delete_folder(account_id, name, path, &sender);
                }
                CtxAction::RenameFolder {
                    account_id,
                    name,
                    path,
                } => {
                    self.prompt_rename_folder(account_id, name, path, &sender);
                }
                CtxAction::EmptyFolder {
                    account_id,
                    folder_id,
                    name,
                    path,
                } => {
                    self.confirm_empty_folder(account_id, folder_id, name, path, &sender);
                }
                CtxAction::HideFolder { account_id, path } => {
                    self.hide_folders(account_id, vec![path]);
                }
                CtxAction::AddFavorite { account_id, path } => {
                    if let Some(email) = self.email_of(account_id) {
                        let key = format!("{email}\t{path}");
                        if !self.favorites.contains(&key) {
                            // The first favorite shows its list open.
                            if self.favorites.is_empty() {
                                self.favorites_expanded = true;
                            }
                            self.favorites.push(key);
                            self.save_sidebar_state();
                            self.rebuild_sidebar();
                        }
                    }
                }
                CtxAction::RemoveFavorite { account_id, path } => {
                    if let Some(email) = self.email_of(account_id) {
                        let key = format!("{email}\t{path}");
                        self.favorites.retain(|k| *k != key);
                        self.save_sidebar_state();
                        self.rebuild_sidebar();
                    }
                }
                CtxAction::ResetFolderOrder(account_id) => {
                    if let Some(email) = self.email_of(account_id) {
                        let prefix = format!("{email}\t");
                        self.folder_order.retain(|k| !k.starts_with(&prefix));
                        self.save_sidebar_state();
                        self.rebuild_sidebar();
                    }
                }
            },

            AppMsg::DropMoveMessages {
                dest_account,
                dest,
                items,
            } => {
                self.drop_move(dest_account, dest, items);
            }

            AppMsg::CreateFolder { account_id, name } => {
                let name = name.trim();
                if !name.is_empty() {
                    // The server names mailboxes in modified UTF-7, so a name
                    // with any non-ASCII character has to be encoded before it
                    // becomes a path (issue #1, in the other direction).
                    let path = format!(
                        "{}{}",
                        self.folder_namespace(account_id),
                        crate::mutf7::encode(name)
                    );
                    self.create_folder(account_id, path.clone());
                    self.push_undo(
                        account_id,
                        i18n("Create Folder"),
                        UndoStep::DeleteFolder { path },
                    );
                }
            }

            AppMsg::MoveFolder {
                account_id,
                path,
                dest,
            } => {
                let _ = self.move_folder(account_id, path, dest);
            }

            AppMsg::RenameFolderTo {
                account_id,
                path,
                new_name,
            } => {
                self.rename_folder_to(account_id, path, new_name);
            }

            AppMsg::EmptyFolder {
                account_id,
                folder_id,
                path,
            } => {
                // Gone locally at once; the worker's empty message list and
                // zero count confirm it, or an error says why not.
                self.message_cache.remove(&(account_id, folder_id));
                self.folder_unread.insert((account_id, folder_id), 0);
                if self
                    .selected
                    .as_ref()
                    .is_some_and(|s| s.account_id == account_id && s.folder_id == folder_id)
                {
                    self.current = None;
                    self.current_thread.clear();
                    self.show_message(None, false);
                    self.message_list.emit(MessageListInput::SetLoading);
                }
                self.push_unread_counts();
                self.send_to(account_id, MailRequest::EmptyFolder { folder_id, path });
            }

            // Not recorded: deleting a folder cannot be taken back (#200).
            // The confirmation says so before it happens.
            AppMsg::DeleteFolder { account_id, path } => {
                self.delete_folder(account_id, path);
            }

            AppMsg::AccountsReordered(emails) => {
                // Display order only (by email) — no reconnect needed.
                if !emails.is_empty() {
                    self.account_order = emails;
                    self.save_sidebar_state();
                    self.rebuild_sidebar();
                }
            }

            AppMsg::SearchPoolLoaded { gen, lists } => {
                if gen != self.search_gen {
                    return;
                }
                let lists = lists
                    .into_iter()
                    .map(|((account_id, folder_id), messages)| {
                        let messages = self.merge_local_tags(account_id, messages);
                        let messages = self.drop_pending_moves(account_id, folder_id, messages);
                        let messages = self.apply_pending_seen(account_id, folder_id, messages);
                        let messages = self.apply_pending_answers(account_id, folder_id, messages);
                        self.apply_pending_tags(account_id, folder_id, messages)
                    })
                    .collect();
                self.build_search_pool(gen, lists, &sender);
            }
            AppMsg::SearchPoolBuilt { gen, pool, text } => {
                if gen == self.search_gen {
                    self.message_list.emit(MessageListInput::SetSearchPool(
                        pool,
                        self.pool_folders(),
                        text,
                    ));
                }
            }
            AppMsg::OpenedByKey { already_open } => {
                if already_open {
                    self.message_view.emit(MessageViewInput::FocusBody);
                } else {
                    self.reader_focus_pending = true;
                }
            }
            AppMsg::ClearReader => {
                self.current = None;
                self.current_thread.clear();
                self.listed_thread.clear();
                self.attachments.clear();
                self.attachments_loading = false;
                self.sync_attachment_drawer();
                self.show_message(None, false);
            }
            AppMsg::SearchActive(active) => {
                self.search_gen += 1;
                if active {
                    // Every folder's indexed messages, so the search can span
                    // the whole mailbox: the folders opened this run and the
                    // rest from disk, put together off the main loop (#515).
                    self.load_search_pool(&sender);
                    // Results span accounts; tint rows by account (as in the unified
                    // inbox) so their origin is legible.
                    if self.accounts.len() > 1 {
                        self.message_list.emit(MessageListInput::SetColorize(true));
                    }
                } else {
                    self.message_list.emit(MessageListInput::SetSearchPool(
                        Vec::new(),
                        HashMap::new(),
                        Vec::new(),
                    ));
                    // Restore the tint state the underlying view wants.
                    self.message_list
                        .emit(MessageListInput::SetColorize(self.unified));
                }
            }
            AppMsg::MessageSelected {
                message: m,
                thread,
                solo,
            } => {
                // What the reader is showing right now. A message that has just
                // been moved back arrives under a new UID, so the list hands it
                // over again as if it were a different message; rendering it a
                // second time only makes the reader blink (#200).
                // Captured before anything clears it: a conversation on screen
                // is a different document from the same message shown alone,
                // even though both answer to the one Message-ID.
                let was_thread = self.current_thread.len() > 1;
                let showing = self.current.as_ref().map(|c| {
                    (
                        c.account_id,
                        c.message_id.clone(),
                        c.body.clone(),
                        c.unread,
                        c.starred,
                        c.keywords.clone(),
                    )
                });
                // Navigating away releases any inline reply (save-if-dirty, or keep
                // it as an independent window if it was popped out).
                self.release_reader_compose();
                // A sent reply still on its way into the conversation left
                // behind: the reader shows the new message at once.
                let elsewhere = self
                    .current
                    .as_ref()
                    .is_some_and(|c| (c.account_id, c.id) != (m.account_id, m.id));
                if elsewhere && self.send_morph.running() {
                    self.send_morph.finish();
                }
                // A queued message reads like any other, straight from the bytes
                // already on disk — no folder, no UID, nothing to ask the server.
                if let Some(item) = self.outbox_item(m.account_id, m.id) {
                    self.show_outbox_message(&item);
                    return;
                }
                // Selecting a draft opens it in the compose editor, in the
                // reading pane where the message would otherwise show.
                if self.is_drafts_folder(m.account_id, m.folder_id) {
                    self.reader_focus_pending = false;
                    self.open_draft(m, true, HandOffFiles::default(), &sender);
                    return;
                }
                // Opened with Gmail's o or Enter: the keyboard follows it into
                // the reader, as Gmail's conversation view takes it, so j and
                // k go on to the next conversation and u or Esc come back
                // (#392). Once the reader has been handed the message.
                if std::mem::take(&mut self.reader_focus_pending) {
                    let view = self.message_view.sender().clone();
                    gtk::glib::idle_add_local_once(move || {
                        view.emit(MessageViewInput::FocusBody);
                    });
                }
                self.attachments.clear();
                self.attachments_loading = false;
                self.sync_attachment_drawer();
                let account_id = m.account_id;
                let folder_path = self.resolve_folder_path(&m);
                // Use an already-fetched body if we have one (on the message or in
                // our cache) so reopening renders instantly without a spinner.
                let cached_body = if !m.body.is_empty() {
                    Some(m.body.clone())
                } else {
                    self.body_cache.get(&body_key(&m)).cloned()
                };
                let needs_body = cached_body.is_none();

                // A conversation's row is not one message read: its cards are
                // marked as the reader shows them, the first one included.
                // Marking that one outright left it read, and folded, every
                // time an unread conversation was opened again (#392).
                let conversation = thread.len() > 1;
                if m.unread && !conversation {
                    match self.read_mark {
                        config::ReadMark::Shown => self.mark_opened_read(&m),
                        config::ReadMark::Delay => {
                            // Mark after a beat in view — navigating away
                            // before then leaves it unread (#100).
                            let s = sender.clone();
                            let msg = m.clone();
                            gtk::glib::timeout_add_seconds_local_once(2, move || {
                                s.input(AppMsg::DeferredMarkRead {
                                    message: Box::new(msg.clone()),
                                });
                            });
                        }
                        config::ReadMark::Manual => {}
                    }
                }

                let mut current = m.clone();
                if !conversation {
                    current.unread = false;
                }
                if let Some(body) = cached_body {
                    current.body = body;
                }
                self.current = Some(current.clone());
                self.listed_thread = if thread.is_empty() {
                    vec![(m.account_id, m.id)]
                } else {
                    thread.iter().map(|t| (t.account_id, t.id)).collect()
                };
                // A new selection: whatever the last one was waiting for no
                // longer holds this one back.
                self.thread_related_pending = false;

                // The folder being read holds only its half of the conversation:
                // your own replies are filed in Sent, and older parts may have
                // been archived. Ask for the rest from the cache (#21).
                // …unless the user picked this reply out of a conversation already
                // on screen. They asked for one message; assembling its thread
                // again would swap the conversation back in under them.
                // Asked for after the body, below: the cache lane answers both,
                // one after the other, and a cached body is what the reader is
                // waiting to show (#259).
                let mut related = None;
                if self.threading && !solo {
                    let only = [m.clone()];
                    let ids = thread_ids(if thread.is_empty() {
                        &only[..]
                    } else {
                        &thread
                    });
                    if !ids.is_empty() {
                        related = Some(MailRequest::LoadRelated {
                            message_id: m.id,
                            ids,
                        });
                        self.thread_related_pending = true;
                    }
                }

                if thread.len() > 1 {
                    self.thread_key = Some(body_key(&m));
                    // Already assembled: paint it now. No lookup, no body
                    // gathering, no spinner — returning to a thread shouldn't
                    // cost what opening it did.
                    tracing::debug!(
                        target: "hylki::undo",
                        "select {}: thread of {}, remembered={}, needs_body={needs_body}",
                        m.id,
                        thread.len(),
                        self.thread_cache.contains_key(&body_key(&m)),
                    );
                    if let Some(cached) = self.thread_cache.get(&body_key(&m)).cloned() {
                        self.current_thread = cached;
                        // The row's message as the list has it now, whatever
                        // the stored copy said when it was put away.
                        for tm in self.current_thread.iter_mut() {
                            if tm.id == m.id && tm.account_id == account_id {
                                tm.unread = current.unread;
                                tm.body = current.body.clone();
                            }
                        }
                        // A member whose body came after the conversation was
                        // put away (one from another folder, say) was stored
                        // without it, and would say "Loading…" for good.
                        self.load_missing_thread_bodies(Some((account_id, m.id)));
                        self.thread_painted = true;
                        self.thread_related_pending = false;
                        self.show_thread();
                        self.load_thread_attachments();
                        if let Some(req) = related.take() {
                            self.send_to(account_id, req);
                        }
                    } else {
                        // Conversation: assemble the thread with any cached bodies,
                        // request the rest, and render it as a scrollable conversation.
                        let mut conv: Vec<Message> = Vec::with_capacity(thread.len());
                        for tm in &thread {
                            let mut tm = tm.clone();
                            // Each keeps its real unread state, the row's own
                            // message too: the reader marks them as it shows them.
                            if tm.id == m.id && tm.account_id == account_id {
                                tm.unread = current.unread;
                                tm.body = current.body.clone();
                            } else if tm.body.is_empty() {
                                if let Some(b) = self.body_cache.get(&body_key(&tm)) {
                                    tm.body = b.clone();
                                }
                            }
                            conv.push(tm);
                        }
                        self.current_thread = conv;
                        self.thread_opened_at = Some(std::time::Instant::now());
                        self.thread_painted = false;
                        // The paint happens once the conversation has settled: the
                        // lookup answered and the bodies in. Until then the reader
                        // holds its spinner.
                        self.queue_thread_render(&sender);
                        // Fetch the bodies we don't have yet (primary first via order).
                        let to_load: Vec<MissingBody> = self
                            .current_thread
                            .iter()
                            .filter(|tm| tm.body.is_empty())
                            .filter_map(|tm| {
                                self.resolve_folder_path(tm)
                                    .map(|p| (tm.account_id, tm.id, tm.uid, p))
                            })
                            .collect();
                        for ((aid, path), items) in batch_bodies_by_folder(to_load) {
                            self.send_to(aid, MailRequest::LoadBodies { items, path });
                        }
                        if let Some(req) = related.take() {
                            self.send_to(account_id, req);
                        }
                        self.show_thread();
                        self.load_thread_attachments();
                    }
                } else {
                    self.current_thread.clear();
                    self.thread_key = None;
                    let display = current;
                    // Already on screen, pixel for pixel: leave it alone.
                    let unchanged = !needs_body
                        && !was_thread
                        && showing.is_some_and(|(aid, mid, body, unread, starred, keywords)| {
                            (aid, &mid, &body, unread, starred, &keywords)
                                == (
                                    display.account_id,
                                    &display.message_id,
                                    &display.body,
                                    display.unread,
                                    display.starred,
                                    &display.keywords,
                                )
                                && !mid.is_empty()
                        });
                    tracing::debug!(
                        target: "hylki::undo",
                        "select {}: single message, needs_body={needs_body}, unchanged={unchanged}",
                        m.id,
                    );
                    if unchanged {
                        if let Some(req) = related.take() {
                            self.send_to(account_id, req);
                        }
                        return;
                    }
                    // Request the body FIRST so it renders before attachments — the
                    // worker processes requests in order, so the body must come first.
                    if needs_body {
                        if let Some(path) = folder_path.clone() {
                            self.send_to(
                                account_id,
                                MailRequest::LoadBody {
                                    message_id: m.id,
                                    path,
                                    uid: m.uid,
                                },
                            );
                        }
                    }
                    if let Some(req) = related.take() {
                        self.send_to(account_id, req);
                    }
                    self.show_message(Some(display), needs_body);
                }

                // Attachments: use the in-memory cache if present; otherwise
                // fetch them (disk cache first, then the server). Opening the
                // message is the request — the paperclip appears when they
                // land, with no "load attachments" click in between.
                if m.has_attachment {
                    if let Some(cached) = self.attachment_cache.get(&body_key(&m)).cloned() {
                        self.attachments = cached;
                        self.sync_attachment_drawer();
                    } else if let Some(path) = folder_path {
                        self.attachments_loading = true;
                        self.send_to(
                            account_id,
                            MailRequest::LoadAttachments {
                                message_id: m.id,
                                path,
                                uid: m.uid,
                                download: true,
                            },
                        );
                    }
                }
            }

            AppMsg::ThreadGrew { message: m, thread } => {
                // Only for the conversation on screen: the reader's primary
                // is that head. A reply of the user's own filed in Sent is
                // not in the folder's list and stays as the related lookup
                // left it — kept below.
                let key = (m.account_id, m.id);
                if self.current.as_ref().map(|c| (c.account_id, c.id)) != Some(key) {
                    return;
                }
                let existing = std::mem::take(&mut self.current_thread);
                let mut conv: Vec<Message> = existing.clone();
                for tm in &thread {
                    let k = (tm.account_id, tm.id);
                    if !self.listed_thread.contains(&k) {
                        self.listed_thread.push(k);
                    }
                    if conv.iter().any(|e| (e.account_id, e.id) == k) {
                        continue;
                    }
                    let mut tm = tm.clone();
                    if k == key {
                        tm.unread = false;
                        if let Some(c) = self.current.as_ref().filter(|c| !c.body.is_empty()) {
                            tm.body = c.body.clone();
                        }
                    } else if tm.body.is_empty() {
                        if let Some(b) = self.body_cache.get(&body_key(&tm)) {
                            tm.body = b.clone();
                        }
                    }
                    conv.push(tm);
                }
                // Chronological, as a conversation is stored (display order
                // is show_thread's business).
                conv.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.uid.cmp(&b.uid)));
                tracing::info!(
                    "conversation on screen grew: {} -> {} messages",
                    existing.len(),
                    conv.len()
                );
                self.current_thread = conv;
                self.thread_key = Some(body_key(&m));
                // The conversation is already painted: the new card joins it
                // in place. Its body, when not prefetched, is asked for and
                // the render follows its arrival (the Body handler repaints a
                // conversation as bodies land) rather than showing an empty
                // card first.
                self.thread_painted = true;
                self.thread_related_pending = false;
                self.remember_thread();
                // What is on screen stays where it is: the render keeps the
                // reader's place through its saved anchor, and when none is
                // recorded yet (no scroll since the conversation opened) the
                // card that was at the top of the pane is pinned there — so
                // with newest first, the new card slots in above it unseen,
                // and is marked read only once the user scrolls up to it.
                let top = if self.thread_newest_first {
                    existing.iter().max_by_key(|tm| tm.timestamp)
                } else {
                    existing.first()
                }
                .map(|tm| (tm.account_id, tm.id))
                .or_else(|| self.current.as_ref().map(|c| (c.account_id, c.id)));
                if let Some((account_id, id)) = top {
                    self.message_view
                        .emit(MessageViewInput::HoldPlace { account_id, id });
                }
                let to_load: Vec<MissingBody> = self
                    .current_thread
                    .iter()
                    .filter(|tm| tm.body.is_empty())
                    .filter_map(|tm| {
                        self.resolve_folder_path(tm)
                            .map(|p| (tm.account_id, tm.id, tm.uid, p))
                    })
                    .collect();
                if to_load.is_empty() {
                    self.show_thread();
                } else {
                    for ((aid, path), items) in batch_bodies_by_folder(to_load) {
                        self.send_to(aid, MailRequest::LoadBodies { items, path });
                    }
                }
                self.load_thread_attachments();
            }

            AppMsg::OpenMessageWindow { message: m, thread } => {
                // Drafts open in the editor rather than a read-only window,
                // and a template opens a new message made from it (#360).
                if self.is_drafts_folder(m.account_id, m.folder_id) {
                    self.open_draft(m, false, HandOffFiles::default(), &sender);
                } else if self.folder_kind(m.account_id, m.folder_id) == Some(FolderKind::Templates)
                {
                    self.edit_as_new(m, &sender);
                } else {
                    // Popouts follow the reading pane's display order (#70).
                    let mut thread = thread;
                    if self.thread_newest_first {
                        thread.reverse();
                    }
                    self.open_message_window(m, thread, &sender);
                }
            }

            AppMsg::PopoutClosed(key) => {
                self.popouts.remove(&key);
            }

            AppMsg::AddContactFrom { name, email } => {
                self.show_add_contact_dialog(&name, &email, &sender);
            }

            AppMsg::LoadAttachmentsFor(m) => {
                let m = *m;
                if let Some(path) = self.resolve_folder_path(&m) {
                    self.send_to(
                        m.account_id,
                        MailRequest::LoadAttachments {
                            message_id: m.id,
                            path,
                            uid: m.uid,
                            download: true,
                        },
                    );
                }
            }

            AppMsg::ShowcasePopout => {
                if let Some(m) = self.current.clone() {
                    let thread = self.current_thread.clone();
                    sender.input(AppMsg::OpenMessageWindow { message: m, thread });
                }
            }

            AppMsg::PopoutMoveTo { key, message, x, y } => {
                let Some(window) = self.popouts.get(&key).map(|p| p.window.clone()) else {
                    return;
                };
                let accounts =
                    self.picker_accounts(message.account_id, self.resolve_folder_path(&message));
                if accounts.is_empty() {
                    return;
                }
                let s = sender.input_sender().clone();
                let picked = (*message).clone();
                let closing = window.clone();
                crate::ui::folder_picker::show_folder_picker(
                    &window,
                    x,
                    y,
                    accounts,
                    None,
                    move |account_id, dest, _| {
                        let _ = s.send(AppMsg::MoveMessagesTo {
                            account_id,
                            dest,
                            messages: vec![picked.clone()],
                        });
                        // As Archive and Delete do: the message has left.
                        closing.close();
                    },
                );
            }

            AppMsg::PopoutShortcut {
                action,
                message,
                thread,
            } => {
                self.run_popout_shortcut(action, *message, thread, &sender);
            }

            AppMsg::DeletePopoutAttachment {
                key,
                message,
                attachment,
            } => {
                let Some(window) = self.popouts.get(&key).map(|p| p.window.clone()) else {
                    return;
                };
                let Some(path) = self.resolve_folder_path(&message) else {
                    return;
                };
                let target = AttachmentTarget {
                    account_id: message.account_id,
                    message_id: message.id,
                    path,
                    uid: message.uid,
                    name: attachment.name.clone(),
                    size: attachment.data.len() as u64,
                };
                self.confirm_delete_attachment(target, window.upcast_ref(), &sender);
            }

            AppMsg::ToggleStar => {
                // Several selected: star them all, or clear them all once
                // every one is starred (#313).
                let many = self.multi_selection();
                if !many.is_empty() {
                    let star = !many.iter().all(|m| m.starred);
                    self.user_set_flags(star_label(star), &many, FlagKind::Star, star);
                    return;
                }
                if let Some(m) = self.reply_target() {
                    if self.thread_star_target(&m) {
                        // The conversation is the target: any member starred
                        // reads as a starred thread, and the toggle clears or
                        // sets the whole set (same semantics as the list row).
                        let any = self.current_thread.iter().any(|t| t.starred);
                        let members = self.current_thread.clone();
                        self.user_set_flags(star_label(!any), &members, FlagKind::Star, !any);
                    } else {
                        let starred = !m.starred;
                        self.user_set_flags(star_label(starred), &[m], FlagKind::Star, starred);
                    }
                }
            }

            AppMsg::Archive => {
                if self.many_selected() {
                    self.message_list
                        .emit(MessageListInput::Bulk(BulkAction::Archive));
                    return;
                }
                // An open conversation's row stands for all of it, as it does
                // for Delete; a picked card is that message alone (#392).
                if self.acts_on_conversation(false) {
                    let conversation = self.conversation_to_archive();
                    if conversation.len() > 1 {
                        self.apply_bulk_move(BulkAction::Archive, conversation);
                        return;
                    }
                }
                if let Some(m) = self.reply_target() {
                    self.move_to(m, FolderKind::Archive);
                }
            }
            AppMsg::Delete => {
                // More than one message selected: the trash button deletes the
                // whole selection, exactly as the bulk bar's Delete would. One
                // row checked with Gmail's x is a lone row, whose conversation
                // asks first as an open one does (#392).
                if self.many_selected() {
                    self.message_list.emit(if self.list_selection.len() == 1 {
                        MessageListInput::ResolveDelete
                    } else {
                        MessageListInput::Bulk(BulkAction::Delete)
                    });
                    return;
                }
                // A lone list-row selection over an open conversation may be
                // its thread head, which stands for the whole thread — the
                // list resolves that (DeleteThread) or falls back to a plain
                // bulk delete. A picked card is always just that message.
                if !self.selection_from_cards
                    && self.list_selection.len() == 1
                    && self.current_thread.len() > 1
                {
                    self.message_list.emit(MessageListInput::ResolveDelete);
                    return;
                }
                if let Some(m) = self.reply_target() {
                    // In the Outbox, deleting means giving up on sending it —
                    // there is no server-side copy to move to Trash.
                    if self.outbox_item(m.account_id, m.id).is_some() {
                        self.drop_outbox_row(m.account_id, m.id, &sender);
                        self.current = None;
                        self.show_message(None, false);
                    } else {
                        self.delete_messages(vec![m], &sender);
                    }
                }
            }
            AppMsg::DeleteForever => {
                // The list resolves its rows as it does for Delete, a
                // conversation's row bringing all of it, and hands them back
                // as a bulk action that asks first.
                if self.many_selected()
                    || (!self.selection_from_cards
                        && self.list_selection.len() == 1
                        && self.current_thread.len() > 1)
                {
                    self.message_list
                        .emit(MessageListInput::Bulk(BulkAction::DeleteForever));
                    return;
                }
                if let Some(m) = self.reply_target() {
                    if self.outbox_item(m.account_id, m.id).is_some() {
                        self.drop_outbox_row(m.account_id, m.id, &sender);
                        self.current = None;
                        self.show_message(None, false);
                    } else {
                        self.confirm_purge(vec![m], &sender);
                    }
                }
            }

            AppMsg::Related {
                account_id,
                message_id,
                messages,
            } => {
                if self
                    .current
                    .as_ref()
                    .is_some_and(|c| c.account_id == account_id && c.id == message_id)
                {
                    self.thread_related_pending = false;
                }
                let found = messages.len();
                self.merge_related(account_id, message_id, messages);
                tracing::debug!(
                    target: "hylki::undo",
                    "related for {account_id}:{message_id}: {found} in the cache, conversation now {}",
                    self.current_thread.len(),
                );
                // One render for the settled conversation, rather than one here
                // and another for whatever this brought with it.
                if self.current_thread.len() > 1 {
                    self.queue_thread_render(&sender);
                    // What was pulled in (a reply from Sent, an archived part)
                    // may carry attachments of its own.
                    self.load_thread_attachments();
                    // The same cache may know more than the row's badge was
                    // told when it asked (#351).
                    self.message_list.emit(MessageListInput::ConversationSize {
                        account_id,
                        id: message_id,
                        size: self.current_thread.len(),
                    });
                }
            }

            AppMsg::ThreadsListed { groups } => {
                self.request_thread_summaries(groups);
            }

            AppMsg::ThreadSummaries {
                account_id,
                summaries,
            } => {
                let summaries: Vec<((u32, String), ThreadSummary)> = summaries
                    .into_iter()
                    .map(|(root, mut summary)| {
                        for m in &mut summary.members {
                            m.id = self.related_id(m);
                        }
                        ((account_id, root), summary)
                    })
                    .collect();
                if !summaries.is_empty() {
                    self.message_list
                        .emit(MessageListInput::SetThreadSummaries(summaries));
                }
            }

            AppMsg::PurgeMessages(messages) => {
                self.purge_messages(messages);
            }

            AppMsg::CardAction { action, message } => {
                // A reply started from a card belongs in the main window's
                // inline composer, exactly like the toolbar's buttons — not in
                // a separate compose window.
                let m = self.with_cached_body(*message);
                tracing::debug!(
                    target: "hylki::reply",
                    "card {:?}: {}:{} from={}",
                    action, m.account_id, m.id, m.from_addr,
                );
                match action {
                    RowAction::Reply => {
                        self.open_reply(
                            m.account_id,
                            self.reply_context(&m, reply_prefill(&m)),
                            (m.account_id, m.id),
                            &sender,
                        );
                    }
                    RowAction::ReplyAll => {
                        let own = self.own_identities(m.account_id);
                        self.open_reply(
                            m.account_id,
                            self.reply_context(&m, reply_all_prefill(&m, &own)),
                            (m.account_id, m.id),
                            &sender,
                        );
                    }
                    RowAction::Forward => {
                        self.forward(m, true, &sender);
                    }
                    // A template's banner (#360): edited where a new message
                    // is written.
                    RowAction::EditTemplate => {
                        let inline = self.compose_inline;
                        self.open_draft(m, inline, HandOffFiles::default(), &sender);
                    }
                    // Cards only carry the three above; anything else falls
                    // through to the ordinary row behaviour.
                    other => sender.input(AppMsg::RowAction {
                        action: other,
                        message: Box::new(m),
                        // A card is always its own message.
                        conversation: Vec::new(),
                    }),
                }
            }

            AppMsg::ToggleReadCurrent => {
                // Rows selected in the list: the list marks every message
                // of the conversations they stand for (#392).
                if self.rows_selected() {
                    self.message_list.emit(MessageListInput::BulkToggleRead);
                    return;
                }
                // Several selected: any unread one makes them all read,
                // otherwise they all go back to unread (#313).
                let many = self.multi_selection();
                if !many.is_empty() {
                    let read = many.iter().any(|m| m.unread);
                    self.user_set_flags(read_label(read), &many, FlagKind::Read, read);
                } else if let Some(m) = self.reply_target() {
                    let read = m.unread;
                    self.user_set_flags(read_label(read), &[m], FlagKind::Read, read);
                }
            }

            AppMsg::CardContact(message) => {
                self.show_add_contact_dialog(&message.from_name, &message.from_addr, &sender);
            }

            AppMsg::RowAction {
                action,
                message,
                conversation,
            } => {
                let m = *message;
                if matches!(
                    action,
                    RowAction::Reply | RowAction::ReplyAll | RowAction::Forward
                ) {
                    tracing::debug!(
                        target: "hylki::reply",
                        "row {:?}: {}:{} from={} conversation={:?}",
                        action, m.account_id, m.id, m.from_addr,
                        conversation.iter().map(|c| c.id).collect::<Vec<_>>(),
                    );
                }
                if self.outbox_item(m.account_id, m.id).is_some() {
                    // Nothing else in the palette applies to an unsent message,
                    // and every other action would aim an IMAP command at a UID
                    // that doesn't exist on any server.
                    if matches!(action, RowAction::Delete) {
                        self.drop_outbox_row(m.account_id, m.id, &sender);
                    }
                    return;
                }
                match action {
                    // A row that stands for a whole conversation answers the
                    // conversation's newest message, exactly as the reader's
                    // own reply does (#210) — the row is filed under the
                    // thread's head, which is its oldest message.
                    RowAction::Reply => {
                        let m = self.newest_to_answer(&conversation, m);
                        let m = self.with_cached_body(m);
                        self.open_compose(
                            m.account_id,
                            self.reply_context(&m, reply_prefill(&m)),
                            &sender,
                        );
                    }
                    RowAction::ReplyAll => {
                        let m = self.newest_to_answer(&conversation, m);
                        let m = self.with_cached_body(m);
                        let own = self.own_identities(m.account_id);
                        self.open_compose(
                            m.account_id,
                            self.reply_context(&m, reply_all_prefill(&m, &own)),
                            &sender,
                        );
                    }
                    RowAction::Forward => {
                        let m = self.newest_to_answer(&conversation, m);
                        self.forward(m, false, &sender);
                    }
                    // A conversation row stands for its newest message here
                    // too: that is the one the row is showing.
                    RowAction::EditAsNew => {
                        let m = self.newest_to_answer(&conversation, m);
                        self.edit_as_new(m, &sender);
                    }
                    // Edited where a new message is written (#360), from a
                    // row's menu as from the banner.
                    RowAction::EditTemplate => {
                        let inline = self.compose_inline;
                        self.open_draft(m, inline, HandOffFiles::default(), &sender);
                    }
                    RowAction::ToggleStar => {
                        let starred = !m.starred;
                        self.user_set_flags(star_label(starred), &[m], FlagKind::Star, starred);
                    }
                    RowAction::ToggleRead => {
                        let read = m.unread;
                        self.user_set_flags(read_label(read), &[m], FlagKind::Read, read);
                    }
                    RowAction::Spam => self.mark_spam_msg(m),
                    RowAction::NotSpam => self.mark_ham_msg(m),
                    RowAction::Archive => self.move_to(m, FolderKind::Archive),
                    RowAction::MoveToInbox => self.move_to(m, FolderKind::Inbox),
                    RowAction::Delete => self.delete_messages(vec![m], &sender),
                    RowAction::ViewSource => {
                        if let Some(path) = self.resolve_folder_path(&m) {
                            self.send_to(
                                m.account_id,
                                MailRequest::LoadSource {
                                    message_id: m.id,
                                    path,
                                    uid: m.uid,
                                },
                            );
                        }
                    }
                    RowAction::SaveAsFile => {
                        // The whole message as the server keeps it, asked
                        // for first: the save dialog opens once it is here.
                        if let Some(path) = self.resolve_folder_path(&m) {
                            let token = self.next_transfer;
                            self.next_transfer += 1;
                            self.pending_saves.insert(token, eml_file_name(&m.subject));
                            self.send_to(
                                m.account_id,
                                MailRequest::ExportRaw {
                                    token,
                                    path,
                                    uid: m.uid,
                                    for_reader: true,
                                },
                            );
                        }
                    }
                    RowAction::AddContact => {
                        self.show_add_contact_dialog(&m.from_name, &m.from_addr, &sender);
                    }
                    RowAction::CreateFilter => {
                        let Some(email) = self.email_of(m.account_id) else {
                            return;
                        };
                        let rule = config::FilterRule::draft_from_message(
                            &email,
                            &m.from_addr,
                            &m.subject,
                        );
                        self.open_settings_window(&sender, true, false);
                        if let Some(p) = &self.prefs {
                            p.emit(PrefInput::ShowPageById("filters".to_string()));
                        }
                        if let Some(acc) = &self.accounts_win {
                            acc.emit(crate::ui::accounts::AccountsInput::AddFilterFrom(rule));
                        }
                    }
                }
            }

            AppMsg::Bulk { action, messages } => {
                if self.showing_outbox {
                    if matches!(action, BulkAction::Delete | BulkAction::DeleteForever) {
                        for m in &messages {
                            self.drop_outbox_row(m.account_id, m.id, &sender);
                        }
                    }
                    self.message_list.emit(MessageListInput::ClearSelection);
                    return;
                }
                match action {
                    // Flag/read changes update rows in place (no removal).
                    BulkAction::MarkRead => {
                        self.user_set_flags(read_label(true), &messages, FlagKind::Read, true)
                    }
                    BulkAction::MarkUnread => {
                        self.user_set_flags(read_label(false), &messages, FlagKind::Read, false)
                    }
                    BulkAction::Flag => {
                        self.user_set_flags(star_label(true), &messages, FlagKind::Star, true)
                    }
                    BulkAction::Unflag => {
                        self.user_set_flags(star_label(false), &messages, FlagKind::Star, false)
                    }
                    BulkAction::DeleteForever => self.confirm_purge(messages, &sender),
                    // Archive/Delete/Spam remove every selected row. Doing that one
                    // at a time blocks the UI thread (a render cycle per message) and
                    // trips GTK's "app is not responding" dialog for large selections.
                    // Batch it; for big selections show a spinner and defer the apply
                    // one tick so the spinner paints before the blocking work runs.
                    BulkAction::Archive
                    | BulkAction::Spam
                    | BulkAction::NotSpam
                    | BulkAction::Delete
                    | BulkAction::MoveToInbox => {
                        // Deleting in Trash means erasing, not moving — split the
                        // selection so each half takes the right path (and the
                        // erasures get their confirmation).
                        if matches!(action, BulkAction::Delete)
                            && messages.iter().any(|m| self.in_trash(m))
                        {
                            self.delete_messages(messages, &sender);
                            return;
                        }
                        // The apply is one batched widget update, so no overlay
                        // needed at any size: rows vanish at once, the server
                        // work runs invisibly in the workers, and the refresh
                        // spinner + status bar carry the progress story.
                        self.apply_bulk_move(action, messages);
                    }
                }
            }

            AppMsg::BulkComplete => {
                self.bulk_pending = self.bulk_pending.saturating_sub(1);
                self.update_busy_indicator();
                if self.bulk_pending == 0 && self.busy.is_empty() {
                    self.notifications
                        .emit(NotifyInput::SetStatus(String::new()));
                }
            }

            AppMsg::Refresh => {
                self.resend_launcher_count();
                // The monitor can be wrong about the network (one with no
                // default route): a Refresh asked for while it says offline
                // tries the servers anyway (#442).
                if !crate::power::online() {
                    crate::power::try_anyway();
                }
                if self.unified {
                    let reqs = self.unified_targets();
                    for (account_id, folder_id, path) in reqs {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                    self.refresh_unified_starred(&sender);
                } else {
                    match self.selected.clone() {
                        Some(sel) => self.send_to(
                            sel.account_id,
                            MailRequest::LoadMessages {
                                folder_id: sel.folder_id,
                                path: sel.path,
                            },
                        ),
                        None => {
                            for w in self.workers.values() {
                                let _ = w.send(MailRequest::Reconnect);
                            }
                        }
                    }
                }
                // Only the visible folder (and the IDLE-watched inbox) re-synced
                // above; every other folder's unread chip would drift until it
                // was selected. Have each account re-check them all.
                for w in self.workers.values() {
                    let _ = w.send(MailRequest::RefreshUnread);
                }
                // A tag view lists what the index holds, and the index only
                // learns a folder's flags when that folder syncs: have the
                // accounts in scope re-read their keywords (#166).
                self.sync_tag_keywords();
            }

            AppMsg::ShowcaseFolder { kind, account } => {
                let account = account.unwrap_or_else(|| self.active_account());
                let found = self
                    .folders
                    .get(&account)
                    .and_then(|fs| fs.iter().find(|f| f.kind == kind))
                    .map(|f| (f.id, f.name.clone(), f.path.clone()));
                if let Some((id, name, path)) = found {
                    self.select_folder(account, id, name, path);
                }
            }

            AppMsg::ShowcaseNotify => {
                let account = self.active_account();
                let inbox = self
                    .folders
                    .get(&account)
                    .and_then(|fs| fs.iter().find(|f| f.kind == FolderKind::Inbox))
                    .map(|f| f.id);
                let newest = inbox.and_then(|f| {
                    self.message_cache
                        .get(&(account, f))
                        .and_then(|ms| ms.iter().max_by_key(|m| m.timestamp))
                        .map(|m| (f, m.id, m.from_name.clone(), m.subject.clone()))
                });
                match newest {
                    Some((folder, id, from, subject)) => {
                        self.announce_new_mail(account, folder, id, &from, &subject, 0, true);
                    }
                    None => tracing::warn!(
                        "showcase notify: no Inbox message cached for account {account}"
                    ),
                }
            }

            // Building a composer holds the window a moment, longest the
            // first time in a session: the pointer turns busy, and that is
            // drawn before the build starts, so the key or click is seen to
            // have landed (#392). Another press meanwhile is dropped.
            asked @ (AppMsg::Compose
            | AppMsg::Reply
            | AppMsg::ReplyAll
            | AppMsg::Forward
            | AppMsg::ComposeInWindow(_))
                if !self.compose_now =>
            {
                if !std::mem::replace(&mut self.compose_waiting, true) {
                    self.window.set_cursor_from_name(Some("progress"));
                    let s = sender.clone();
                    self.after_paint(move || s.input(AppMsg::ComposeNow(Box::new(asked))));
                }
            }
            AppMsg::ComposeNow(asked) => {
                self.compose_waiting = false;
                self.compose_now = true;
                <Self as SimpleComponent>::update(self, *asked, sender.clone());
                self.compose_now = false;
                // Back once the composer's first frame, the slow one, is drawn.
                let window = self.window.clone();
                self.after_paint(move || window.set_cursor(None::<&gtk::gdk::Cursor>));
            }

            AppMsg::Compose => {
                let (account, prefill) =
                    self.new_message_from(self.active_account(), ComposePrefill::default());
                if self.compose_inline {
                    // The new-message pane slides down over the reader,
                    // exactly like an inline reply — same composer, same
                    // pop-out-to-window toggle in its header.
                    self.open_inline_reply(account, prefill, None, &sender);
                } else {
                    self.open_compose(account, prefill, &sender);
                }
            }

            AppMsg::Reply => {
                if let Some(m) = self.compose_target() {
                    self.open_reply(
                        m.account_id,
                        self.reply_context(&m, reply_prefill(&m)),
                        (m.account_id, m.id),
                        &sender,
                    );
                }
            }

            AppMsg::ReplyAll => {
                if let Some(m) = self.compose_target() {
                    let own = self.own_identities(m.account_id);
                    self.open_reply(
                        m.account_id,
                        self.reply_context(&m, reply_all_prefill(&m, &own)),
                        (m.account_id, m.id),
                        &sender,
                    );
                }
            }

            AppMsg::Forward => {
                if let Some(m) = self.compose_target() {
                    self.forward(m, true, &sender);
                }
            }

            AppMsg::ComposeInWindow(what) => match what {
                WindowCompose::New => {
                    let (account, prefill) =
                        self.new_message_from(self.active_account(), ComposePrefill::default());
                    self.open_compose(account, prefill, &sender);
                }
                WindowCompose::Forward => {
                    if let Some(m) = self.compose_target() {
                        self.forward(m, false, &sender);
                    }
                }
                WindowCompose::Reply | WindowCompose::ReplyAll => {
                    if let Some(m) = self.compose_target() {
                        let prefill = if what == WindowCompose::Reply {
                            reply_prefill(&m)
                        } else {
                            reply_all_prefill(&m, &self.own_identities(m.account_id))
                        };
                        self.open_compose(m.account_id, self.reply_context(&m, prefill), &sender);
                    }
                }
            },

            AppMsg::AddContactAddr(addr) => {
                // From an address's right-click menu: only the address is
                // known; the dialog's name field starts blank for the user.
                self.show_add_contact_dialog("", &addr, &sender);
            }

            AppMsg::AddContactBooks { name, email, books } => {
                self.add_contact_dialog(&name, &email, books, &sender);
            }
            AppMsg::ContactAdded(result) => {
                use crate::contacts::AddOutcome;
                let (text, error) = match result {
                    Ok(AddOutcome::Created) => (i18n("Added to Contacts"), false),
                    Ok(AddOutcome::Merged(name)) => {
                        (i18n_f("Added email to {name}", &[("name", &name)]), false)
                    }
                    Ok(AddOutcome::AlreadyPresent(name)) => (
                        i18n_f("Already in Contacts ({name})", &[("name", &name)]),
                        false,
                    ),
                    Err(e) => (
                        i18n_f("Could not add contact: {e}", &[("e", &e.to_string())]),
                        true,
                    ),
                };
                self.notifications.emit(NotifyInput::Push {
                    text,
                    error,
                    connectivity: false,
                });
            }

            AppMsg::ViewSource => {
                if let Some(m) = self.current.clone() {
                    if let Some(path) = self.resolve_folder_path(&m) {
                        self.send_to(
                            m.account_id,
                            MailRequest::LoadSource {
                                message_id: m.id,
                                path,
                                uid: m.uid,
                            },
                        );
                    }
                }
            }

            AppMsg::OpenAbout => {
                self.open_about(&sender);
            }

            AppMsg::AllowSender(addr) | AppMsg::AddSender(addr) => {
                let addr = addr.trim().to_lowercase();
                if !addr.is_empty() && !self.allowed_senders.contains(&addr) {
                    self.allowed_senders.push(addr);
                    self.save_settings();
                }
            }

            AppMsg::Unsubscribe { message, info } => {
                if std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok_and(|v| v == "go" || v == "fail") {
                    self.unsubscribe_go(*message, *info, &sender);
                } else {
                    self.confirm_unsubscribe(*message, *info, &sender);
                }
            }
            AppMsg::UnsubscribeGo { message, info } => {
                self.unsubscribe_go(*message, *info, &sender);
            }
            AppMsg::UnsubscribeByMail { message, info } => {
                self.unsubscribe_by_mail(*message, &info, &sender);
            }
            AppMsg::InviteAction {
                message,
                invite,
                action,
            } => {
                self.invite_action(*message, *invite, action);
            }
            AppMsg::UnsubscribeDone { message, result } => {
                let key = (message.account_id, message.id);
                match result {
                    Ok(left) => {
                        if left {
                            self.record_unsubscribed(&message);
                            self.notifications.emit(NotifyInput::Push {
                                text: i18n_f(
                                    "Unsubscribed from {name}.",
                                    &[("name", &sender_label(&message))],
                                ),
                                error: false,
                                connectivity: false,
                            });
                        } else {
                            self.notifications.emit(NotifyInput::Push {
                                text: i18n("The list's unsubscribe page is open in your browser."),
                                error: false,
                                connectivity: false,
                            });
                        }
                        self.push_unsubscribe_state(key, None);
                    }
                    Err(why) => {
                        tracing::warn!("unsubscribe failed for {}: {why}", message.from_addr);
                        self.push_unsubscribe_state(key, Some(UnsubState::Failed(why)));
                    }
                }
            }

            AppMsg::RemoveSender(addr) => {
                let addr = addr.to_lowercase();
                self.allowed_senders.retain(|s| *s != addr);
                self.save_settings();
            }

            AppMsg::AddBlacklist(addr) => {
                let addr = addr.trim().trim_start_matches('@').to_lowercase();
                if !addr.is_empty() && !self.blacklist.contains(&addr) {
                    self.blacklist.push(addr);
                    self.save_settings();
                    // Sweep mail already in view from the newly-blocked sender.
                    self.sweep_blacklisted();
                }
            }

            AppMsg::RemoveBlacklist(addr) => {
                let addr = addr.to_lowercase();
                self.blacklist.retain(|s| *s != addr);
                self.save_settings();
            }

            AppMsg::MarkSpam => {
                // In Junk the same button, shortcut and menu entry mean the
                // reverse (#168); for a selection the list makes that swap.
                if self.many_selected() {
                    self.message_list
                        .emit(MessageListInput::Bulk(BulkAction::Spam));
                    return;
                }
                if let Some(m) = self.reply_target() {
                    if self.in_junk(&m) {
                        self.mark_ham_msg(m);
                    } else {
                        self.mark_spam_msg(m);
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetAvatars(on)) => {
                if pref!(self.avatars = on) {
                    self.push_list_look(false);
                }
            }

            AppMsg::Pref(PrefOutput::SetFocusMode(focus)) => self.set_focus_mode(focus),

            AppMsg::ListOverflowMenu => self.show_list_overflow_menu(&sender),

            AppMsg::Pref(PrefOutput::SetContactBook(uid)) => {
                if pref!(self.contact_book = uid) && self.showing_contacts {
                    reload_contacts(&sender, 0);
                }
            }
            AppMsg::Pref(PrefOutput::LoadContactBooks) => {
                if demo_mode() {
                    sender.input(AppMsg::ContactBooksLoaded {
                        books: crate::contacts::demo_books(),
                        local_count: 1,
                    });
                    return;
                }
                let s = sender.clone();
                std::thread::spawn(move || {
                    s.input(AppMsg::ContactBooksLoaded {
                        books: crate::contacts::writable_books(),
                        local_count: crate::local_contacts::count(),
                    });
                });
            }
            AppMsg::Pref(PrefOutput::ImportLocalContacts) => {
                let s = sender.clone();
                crate::ui::contacts_page::choose_vcf_files(
                    self.dialog_parent().as_ref(),
                    move |paths| {
                        s.input(AppMsg::ImportContacts(paths));
                    },
                );
            }
            AppMsg::Pref(PrefOutput::ExportLocalContacts) => {
                let dialog = gtk::FileDialog::builder()
                    .title(&i18n("Export Contacts"))
                    .initial_name("hylki-contacts.vcf")
                    .build();
                let notif = self.notifications.sender().clone();
                dialog.save(
                    self.dialog_parent().as_ref(),
                    gtk::gio::Cancellable::NONE,
                    move |res| {
                        let Ok(file) = res else { return };
                        let Some(path) = file.path() else { return };
                        let notif = notif.clone();
                        std::thread::spawn(move || {
                            let _ = notif.send(match crate::local_contacts::export_to(&path) {
                                Ok(n) => NotifyInput::SetStatus(ni18n_f(
                                    "{n} contact exported to {path}",
                                    "{n} contacts exported to {path}",
                                    n as u32,
                                    &[("n", &n.to_string()), ("path", &path.display().to_string())],
                                )),
                                Err(e) => NotifyInput::Push {
                                    text: i18n_f("Could not export contacts: {e}", &[("e", &e)]),
                                    error: true,
                                    connectivity: false,
                                },
                            });
                        });
                    },
                );
            }
            AppMsg::Pref(PrefOutput::DeleteAllLocalContacts) => {
                let n = crate::local_contacts::count();
                let dialog = adw::MessageDialog::new(
                    self.dialog_parent().as_ref(),
                    Some(i18n("Delete All Contacts?").as_str()),
                    Some(&ni18n_f(
                        "The {n} contact in the Hylki address book is deleted. This cannot be undone; export the book first to keep a copy.",
                        "The {n} contacts in the Hylki address book are deleted. This cannot be undone; export the book first to keep a copy.",
                        n as u32,
                        &[("n", &n.to_string())],
                    )),
                );
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("delete", &i18n("Delete All"));
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let s = sender.clone();
                dialog.connect_response(None, move |_, resp| {
                    if resp != "delete" {
                        return;
                    }
                    let s = s.clone();
                    std::thread::spawn(move || {
                        let err = crate::local_contacts::delete_all().err();
                        crate::contacts::local_changed();
                        s.input(AppMsg::ContactWriteDone(err));
                    });
                });
                dialog.present();
            }
            AppMsg::Pref(PrefOutput::SetTagRowTint(on)) => {
                if pref!(self.tag_row_tint = on) {
                    self.refresh_tag_css();
                }
            }
            AppMsg::Pref(PrefOutput::SetOwnMailboxFace(on)) => {
                if pref!(self.own_mailbox_face = on) {
                    self.refresh_own_faces();
                    // Switched on, the accounts that want a Gravatar may never
                    // have been asked about (the switch was off at startup).
                    if on {
                        self.warm_own_gravatars(&sender);
                    }
                    self.refresh_faces();
                }
            }

            AppMsg::OwnGravatarFetched { email, outcome } => {
                // Nothing to redraw unless the address turned out to have one.
                if crate::avatar::cache_own_gravatar(&email, outcome) {
                    self.rebuild_sidebar();
                    self.refresh_faces();
                }
            }

            AppMsg::Pref(PrefOutput::SetSenderLogos(on)) => {
                if pref!(self.sender_logos = on) {
                    self.message_list.emit(MessageListInput::SetSenderLogos(on));
                }
            }

            AppMsg::SetReaderActionsCollapsed(on) => {
                self.reader_actions_collapsed = on;
                self.reader_overflow_btn
                    .set_visible(self.reader_overflow_wanted());
            }

            AppMsg::Pref(PrefOutput::SetReaderToolbar(layout)) => {
                if self.reader_toolbar != layout {
                    self.reader_toolbar = layout;
                    config::save_reader_toolbar(&self.reader_toolbar);
                    self.relayout_reader_toolbar();
                    for p in self.popouts.values() {
                        p.controller
                            .emit(MessageWindowInput::SetToolbar(self.reader_toolbar.clone()));
                    }
                    // Not while the inline composer covers the header: that
                    // path hides the ⋯ by hand and restores it on close.
                    if !self
                        .reader_compose
                        .as_ref()
                        .is_some_and(|r| r.window.is_none())
                    {
                        self.reader_overflow_btn
                            .set_visible(self.reader_overflow_wanted());
                    }
                }
            }

            AppMsg::ReaderControlsChanged(controls) => {
                if let Some(tb) = self.reader_toolbar_widgets.get() {
                    tb.controls.set(controls);
                }
                self.refresh_reader_breakpoint();
            }

            AppMsg::ReaderOverflowMenu => self.show_reader_overflow_menu(&sender),

            AppMsg::Pref(PrefOutput::SetShowRemoteBanner(on)) => {
                if pref!(self.show_remote_banner = on) {
                    self.message_view.emit(MessageViewInput::SetBannerShown(on));
                }
            }

            AppMsg::Pref(PrefOutput::SetShowSpoofBanner(on)) => {
                if pref!(self.show_spoof_banner = on) {
                    self.message_view
                        .emit(MessageViewInput::SetSpoofBannerShown(on));
                    for p in self.popouts.values() {
                        p.controller
                            .emit(MessageWindowInput::SetSpoofBannerShown(on));
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetAutoRemoteContent(on)) => {
                if pref!(self.auto_remote_content = on) {
                    // Re-render what is open so the change takes effect there too:
                    // on, the blocked content loads; off, it is stripped again.
                    if self.current_thread.len() > 1 {
                        self.show_thread();
                    } else {
                        let current = self.current.clone();
                        self.show_message(current, false);
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetDateStyle(style)) => {
                if pref!(self.date_style = style) {
                    self.apply_date_style();
                }
            }

            AppMsg::Pref(PrefOutput::SetRenderer(renderer)) => {
                // The combo notifies as its model is set too: only a real
                // change is saved and announced.
                if config::load_renderer() != renderer {
                    config::save_renderer(&renderer);
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("The renderer applies the next time Hylki starts."),
                        error: false,
                        connectivity: false,
                    });
                }
            }

            AppMsg::Pref(PrefOutput::SetLanguage(code)) => {
                // The combo also notifies as its model is set; only a real
                // change is saved and announced.
                if config::load_language() != code {
                    config::save_language(&code);
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("The language applies the next time Hylki starts."),
                        error: false,
                        connectivity: false,
                    });
                }
            }

            AppMsg::WizardLanguage(code) => {
                // The drop-down also notifies as it is set up; only a real
                // change counts. Saved, then Hylki restarts through the same
                // helper the icon change uses, so every window — the wizard
                // first, since it is not completed yet — comes up in the
                // chosen language. Should the helper fail, the wizard alone
                // is rebuilt in it (gettext reads LANGUAGE on every lookup).
                if config::load_language() == code {
                    return;
                }
                config::save_language(&code);
                tracing::info!("wizard: language {code:?} saved, restarting");
                // The instance that comes back must open the wizard again,
                // review mode or not: the helper drops the review switch on
                // purpose, so a one-shot flag of its own goes along (init
                // takes it and clears it, so it never outlives that start).
                std::env::set_var(crate::WIZARD_AGAIN_VAR, "1");
                match crate::app_icon::launch_restart_helper() {
                    Ok(()) => {
                        // The quit action's own teardown, inline: nothing to
                        // look up, nothing to wait for.
                        tracing::info!("wizard: restart helper up, exiting");
                        let (w, h, maximized) = crate::config::load_window_state();
                        crate::config::save_window_state(w, h, maximized);
                        crate::outgoing::park_held(&self.outgoing);
                        finish_keyring_writes();
                        std::process::exit(0);
                    }
                    Err(e) => {
                        std::env::remove_var(crate::WIZARD_AGAIN_VAR);
                        tracing::warn!("restart for the wizard's language failed: {e}");
                        crate::i18n::apply_language(&code);
                        if let Some(old) = self.welcome.take() {
                            old.widget().set_visible(false);
                        }
                        self.open_wizard(&sender);
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetClockStyle(style)) => {
                if pref!(self.clock_style = style) {
                    self.apply_date_style();
                }
            }

            AppMsg::Pref(PrefOutput::SetGravatar(on)) => {
                if pref!(self.gravatar = on) {
                    self.message_list.emit(MessageListInput::SetGravatar(on));
                    // Refresh the reader's avatar for the open message.
                    let current = self.current.clone();
                    self.show_message(current, false);
                }
            }

            AppMsg::ShowLightbox { items, start } => {
                self.lightbox
                    .emit(crate::ui::lightbox::LightboxInput::Show { items, start });
            }
            AppMsg::ContactPhotosChanged => {
                // The list skips the work when avatars are off; the
                // reader's cards draw initials only, so it has nothing to do.
                self.message_list
                    .emit(MessageListInput::ContactPhotosChanged);
                // The books' files changed: a sync landed, which EDS writes
                // after its refresh call has already returned (#408).
                if self.showing_contacts && !demo_mode() {
                    reload_contacts(&sender, 0);
                }
            }

            AppMsg::Pref(PrefOutput::SetFetchInterval(secs)) => {
                if pref!(self.fetch_interval_secs = secs) {
                    self.arm_auto_fetch(&sender);
                }
            }

            AppMsg::Pref(PrefOutput::SetMeteredSaveData(on)) => {
                if pref!(self.metered_save_data = on) {
                    let was = crate::power::saving_data();
                    crate::power::set_save_data(on);
                    self.saving_data_changed(was);
                }
            }

            AppMsg::Pref(PrefOutput::SetPush(on)) => {
                if pref!(self.push = on) {
                    // Workers read the push setting at startup; restart to apply.
                    self.reconnect_all(&sender);
                }
            }

            AppMsg::Pref(PrefOutput::SetNotifications(on)) => {
                pref!(self.notifications_enabled = on);
            }

            AppMsg::Pref(PrefOutput::SetNotificationContent(on)) => {
                pref!(self.notification_content = on);
            }

            AppMsg::Pref(PrefOutput::SetNotificationButtons(buttons)) => {
                pref!(self.notification_buttons = buttons);
            }

            AppMsg::Pref(PrefOutput::SetRunInBackground(on)) => {
                if self.run_in_background.get() != on {
                    self.run_in_background.set(on);
                    self.save_settings();
                    // Ask the portal as the setting changes, so the permission
                    // dialog appears while the user is looking at the switch they
                    // just moved rather than at some later, unexplained moment.
                    crate::background::request(on && self.autostart);
                    if !on {
                        crate::background::set_status("");
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetAutostart(on)) => {
                if pref!(self.autostart = on) {
                    crate::background::request(self.run_in_background.get() && on);
                }
            }

            AppMsg::Pref(PrefOutput::SetTray(on)) => {
                if pref!(self.tray_enabled = on) {
                    if on {
                        self.start_tray(&sender);
                    } else if let Some(tray) = self.tray.take() {
                        tray.stop();
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetTrayIcon(icon)) => {
                if pref!(self.tray_icon = icon) {
                    // A fresh item rather than a new icon on the old one: the
                    // AppIndicator extension, taken from a picture back to a
                    // symbolic icon's file, kept drawing the picture (#258).
                    // A new item is set up from nothing, as at startup.
                    if let Some(tray) = self.tray.take() {
                        tray.stop();
                        self.start_tray(&sender);
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetAppIcon(id)) => {
                // Picking the stored choice again still changes the desktop
                // when it replaces an icon set outside Hylki (#252).
                let replaces_custom = crate::app_icon::custom_icon().is_some();
                let id = crate::app_icon::set(&id);
                if self.app_icon != id || replaces_custom {
                    self.app_icon = id;
                    if let Some(tray) = &self.tray {
                        tray.set_icon(self.tray_icon, &crate::app_icon::tray_image(&self.app_icon));
                    }
                    self.offer_restart_for_icon(&sender);
                }
            }

            AppMsg::RestartApp => {
                if self.restart_pending {
                    return;
                }
                self.restart_pending = true;
                // GNOME Shell only replaces a launcher's app object some
                // seconds after the launcher changed; quitting before that
                // brings the new window up under the old one, old icon and
                // all. Wait it out with the window still up.
                let wait = crate::app_icon::launcher_settle_remaining();
                self.show_restarting_dialog();
                if wait.is_zero() {
                    sender.input(AppMsg::RestartNow);
                } else {
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(wait, move || s.input(AppMsg::RestartNow));
                }
            }

            AppMsg::RestartNow => match crate::app_icon::launch_restart_helper() {
                Ok(()) => relm4::main_application().activate_action("quit", None),
                Err(e) => {
                    tracing::warn!("restart helper failed: {e}");
                    self.restart_pending = false;
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n(
                            "Couldn't restart automatically — quit and reopen Hylki \
                               to finish switching the icon.",
                        ),
                        error: true,
                        connectivity: false,
                    });
                }
            },

            AppMsg::Pref(PrefOutput::SetTrayMail(on)) => {
                if pref!(self.tray_mail = on) {
                    // A sentinel no real list equals, so the change is sent
                    // whichever way the switch went.
                    *self.tray_mail_key.borrow_mut() =
                        Some(vec![(u32::MAX, u32::MAX, String::new(), String::new())]);
                    self.push_tray_mail();
                }
            }

            AppMsg::Pref(PrefOutput::SetLauncherBadge(badge)) => {
                if pref!(self.launcher_badge = badge) {
                    self.push_launcher_count();
                }
            }

            AppMsg::QuitFromTray => {
                // The same teardown as Ctrl+Q: geometry saved, then exit.
                relm4::main_application().activate_action("quit", None);
            }

            AppMsg::TrayViewUnread => {
                // The tray counts every inbox (#267): when all of its unread
                // mail is in accounts left out of All Inboxes, that view
                // would show none of it.
                let unified = self.unified_inboxes_shown()
                    && (self.unified_inboxes_unread() > 0 || self.inboxes_unread() == 0);
                if unified {
                    // Through the sidebar, so its highlight moves too; it
                    // answers with UnifiedSelected.
                    self.sidebar.emit(SidebarInput::SelectUnifiedRow);
                } else if let Some((account_id, folder_id, name, path)) = self
                    .accounts
                    .iter()
                    .filter_map(|a| self.inbox_of(a.id))
                    .find(|f| self.folder_unread_of(f) > 0)
                    .map(|f| (f.account_id, f.id, f.name.clone(), f.path.clone()))
                {
                    self.select_folder(account_id, folder_id, name, path);
                }
            }

            AppMsg::Pref(PrefOutput::SetSingleKey(on)) => {
                if self.single_key.get() != on {
                    self.single_key.set(on);
                    self.save_settings();
                }
            }

            AppMsg::Pref(PrefOutput::SetKeepDownloads(days)) => {
                if self.keep_downloads_days != days {
                    self.keep_downloads_days = days;
                    self.save_settings();
                    prune_downloads(days);
                }
            }

            AppMsg::Pref(PrefOutput::SetGmailKeys(on)) => {
                if self.gmail_keys.get() != on {
                    self.gmail_keys.set(on);
                    self.message_list.emit(MessageListInput::SetGmailKeys(on));
                    self.save_settings();
                }
            }

            AppMsg::ShowShortcuts => self.show_shortcuts(),
            AppMsg::CommandPalette(actions) => {
                let entries = self.command_palette_entries(&sender);
                let typed = if actions { ">" } else { "" };
                // Over the window it was asked from: a composer of its own
                // window would otherwise hide it behind.
                let tops = gtk::Window::toplevels();
                let active = (0..tops.n_items())
                    .filter_map(|i| tops.item(i))
                    .filter_map(|o| o.downcast::<gtk::Window>().ok())
                    .find(|w| w.is_active() && w.is_visible());
                match active {
                    Some(w) => crate::ui::command_palette::show(&w, entries, typed),
                    None => crate::ui::command_palette::show(&self.window, entries, typed),
                }
            }

            AppMsg::PrintPreview | AppMsg::PrintMessage => {
                // Only the reader can print: it holds the rendered message, and
                // printing from an empty reader would offer a blank page. Say so
                // rather than appearing to do nothing, which is indistinguishable
                // from a broken menu item.
                let preview = matches!(msg, AppMsg::PrintPreview);
                tracing::info!(preview, open = self.current.is_some(), "print requested");
                if self.current.is_none() {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("Open a message first, then print it."),
                        error: false,
                        connectivity: false,
                    });
                    return;
                }
                self.message_view.emit(if preview {
                    MessageViewInput::PrintPreview
                } else {
                    MessageViewInput::Print
                });
            }

            AppMsg::Shortcut(action) => self.run_shortcut(action, &sender),

            AppMsg::Pref(PrefOutput::SetListLayout(layout)) => {
                if pref!(self.list_layout = layout) {
                    self.push_single_line();
                }
            }
            AppMsg::Pref(PrefOutput::SetListHeadings(on)) => {
                if pref!(self.list_headings = on) {
                    self.message_list.emit(MessageListInput::SetHeadings(on));
                }
            }
            AppMsg::Pref(PrefOutput::SetListColumnScroll(on)) => {
                if pref!(self.list_column_scroll = on) {
                    self.message_list
                        .emit(MessageListInput::SetColumnScroll(on));
                }
            }
            AppMsg::Pref(PrefOutput::SetListSearchOpen(on)) => {
                if pref!(self.list_search_open = on) {
                    self.message_list
                        .emit(MessageListInput::SetSearchPinned(on));
                }
            }
            AppMsg::Pref(PrefOutput::SetContactsSearchOpen(on)) => {
                if pref!(self.contacts_search_open = on) {
                    self.contacts_page
                        .emit(ContactsPageInput::SetSearchPinned(on));
                }
            }
            AppMsg::Pref(PrefOutput::SetListSearchBottom(on)) => {
                if pref!(self.list_search_bottom = on) {
                    self.push_list_bars();
                }
            }
            AppMsg::Pref(PrefOutput::SetListBulkBarBottom(on)) => {
                if pref!(self.list_bulk_bar_bottom = on) {
                    self.push_list_bars();
                }
            }
            AppMsg::Pref(PrefOutput::SetGroupByDate(on)) => {
                if pref!(self.list_group_by_date = on) {
                    self.message_list.emit(MessageListInput::SetGroupByDate(on));
                }
            }
            AppMsg::Pref(PrefOutput::SetPillsBelow(on)) => {
                if pref!(self.list_pills_below = on) {
                    self.message_list.emit(MessageListInput::SetPillsBelow(on));
                }
            }
            AppMsg::Pref(PrefOutput::SetListColumns(columns)) => {
                if pref!(self.list_columns = columns.clone()) {
                    self.message_list
                        .emit(MessageListInput::SetColumns(columns));
                }
            }
            AppMsg::ListPaneWidth(width) => {
                // A margin each way, so a drag that rests near the line does
                // not flick the rows back and forth.
                let wide = if self.list_wide {
                    width >= LIST_WIDE_OFF
                } else {
                    width >= LIST_WIDE_ON
                };
                if wide != self.list_wide {
                    self.list_wide = wide;
                    if self.list_layout == config::ListLayout::Automatic {
                        self.push_single_line();
                    }
                }
            }
            AppMsg::Pref(PrefOutput::SetPreviewLines(lines)) => {
                if self.preview_lines != lines {
                    let was_off = self.preview_lines == 0;
                    self.preview_lines = lines;
                    self.save_settings();
                    self.push_list_look(false);
                    // Previews switched back on: IMAP summaries fetched while
                    // they were off carry no preview text (the setting also
                    // stops the body slice being downloaded), so nothing would
                    // show until the next scheduled sync. Refresh now so they
                    // fill in right away. (Graph always has bodyPreview in
                    // hand, which is why Microsoft accounts showed instantly.)
                    if was_off && lines > 0 {
                        sender.input(AppMsg::Refresh);
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetAttachmentsRow(show)) => {
                if pref!(self.show_attachments = show) {
                    self.sidebars_emit(SidebarInput::SetAttachmentsRow(show));
                }
            }

            AppMsg::ListCount(text) => self.list_count = text,
            AppMsg::ListColumnWidths(widths) => {
                pref!(self.list_column_widths = widths);
            }
            AppMsg::Pref(PrefOutput::ResetColumnWidths) => {
                if pref!(self.list_column_widths = Default::default()) {
                    self.message_list
                        .emit(MessageListInput::SetColumnWidths(Default::default()));
                }
            }
            AppMsg::ListSortChanged(key) => {
                if let Some(lh) = self.list_header_widgets.get() {
                    lh.sort.set_state(&key.to_variant());
                }
            }

            AppMsg::Pref(PrefOutput::SetShowUnified(show)) => {
                if pref!(self.show_unified_pref = show) {
                    // Rebuilds the sidebar with or without the unified section.
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetUnifiedChips(chips)) => {
                if pref!(self.unified_chips = chips) {
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetUnifiedKinds(kinds)) => {
                if pref!(self.unified_kinds = kinds) {
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetShowAccounts(show)) => {
                if pref!(self.show_accounts = show) {
                    self.rebuild_sidebar();
                }
                // Both places that offer the switch stay in step.
                if self
                    .show_accounts_action
                    .state()
                    .and_then(|v| v.get::<bool>())
                    != Some(show)
                {
                    self.show_accounts_action.set_state(&show.to_variant());
                }
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::SetShowAccounts(show));
                }
            }

            AppMsg::ToggleAccountTags(account_id) => {
                if let Some(email) = self.email_of(account_id) {
                    if let Some(pos) = self.tags_expanded_accounts.iter().position(|e| *e == email)
                    {
                        self.tags_expanded_accounts.remove(pos);
                    } else {
                        self.tags_expanded_accounts.push(email);
                    }
                    self.save_sidebar_state();
                }
            }

            AppMsg::Pref(PrefOutput::SetSectionStyle(style)) => {
                if pref!(self.section_style = style) {
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetFilteredPlacement(p)) => {
                if pref!(self.filtered_placement = p) {
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetTagsPlacement(p)) => {
                if pref!(self.tags_placement = p) {
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetFavoritesPlacement(p)) => {
                if pref!(self.favorites_placement = p) {
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetContactsPlacement(p)) => {
                if pref!(self.contacts_placement = p) {
                    // Without its books under it in the sidebar, Contacts
                    // picks them itself.
                    self.contacts_page
                        .emit(ContactsPageInput::SetBookPicker(!p.shown()));
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetStartView(view)) => {
                pref!(self.start_view = view);
            }

            AppMsg::Pref(PrefOutput::SetChevronsLeft(left)) => {
                if pref!(self.chevrons_left = left) {
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetAccountClickFolds(on)) => {
                if pref!(self.account_click_folds = on) {
                    self.rebuild_sidebar();
                }
            }

            AppMsg::Pref(PrefOutput::SetThreading(on)) => {
                if pref!(self.threading = on) {
                    self.message_list.emit(MessageListInput::SetThreading(on));
                }
            }

            AppMsg::Pref(PrefOutput::SetThreadExpansion(on)) => {
                if pref!(self.thread_expansion = on) {
                    self.message_list
                        .emit(MessageListInput::SetThreadExpansion(on));
                }
            }

            AppMsg::Pref(PrefOutput::SetThreadRowNewest(on)) => {
                if pref!(self.thread_row_newest = on) {
                    self.message_list
                        .emit(MessageListInput::SetThreadRowNewest(on));
                }
            }

            AppMsg::Pref(PrefOutput::SetConfirmThreadDelete(on)) => {
                pref!(self.confirm_thread_delete = on);
            }

            AppMsg::DeleteThread(messages) => {
                if messages.is_empty() {
                    return;
                }
                if self.confirm_thread_delete {
                    self.confirm_delete_thread(messages, &sender);
                } else {
                    self.delete_messages(messages, &sender);
                }
            }

            AppMsg::DeleteThreadConfirmed(messages) => {
                self.delete_messages(messages, &sender);
            }

            AppMsg::DeleteFromAddress { account_id, addr } => {
                let messages = self.mail_from_address(account_id, &addr);
                tracing::info!(
                    "unsubscribe: deleting {} message(s) from {addr}",
                    messages.len()
                );
                if !messages.is_empty() {
                    self.delete_messages(messages, &sender);
                }
            }

            AppMsg::Pref(PrefOutput::SetCardActionsMode {
                hover_toggle,
                hover_auto,
            }) => {
                if self.card_actions_hover != hover_toggle || self.card_actions_auto != hover_auto {
                    self.card_actions_hover = hover_toggle;
                    self.card_actions_auto = hover_auto;
                    self.save_settings();
                    self.push_card_actions_mode();
                }
            }

            AppMsg::Pref(PrefOutput::SetThreadsExpanded(on)) => {
                if pref!(self.threads_expanded = on) {
                    self.message_list
                        .emit(MessageListInput::SetThreadsExpanded(on));
                }
            }

            AppMsg::Pref(PrefOutput::SetThreadNewestFirst(on)) => {
                if pref!(self.thread_newest_first = on) {
                    // Re-render an open conversation in the new order.
                    if self.current_thread.len() > 1 {
                        self.show_thread();
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetFoldMessages(fold)) => {
                if pref!(self.fold_messages = fold) {
                    self.message_view
                        .emit(MessageViewInput::SetFoldMessages(fold));
                }
            }

            AppMsg::Pref(PrefOutput::SetAlwaysShowRecipients(on)) => {
                if pref!(self.always_show_recipients = on) {
                    self.message_view
                        .emit(MessageViewInput::SetAlwaysShowRecipients(on));
                    // Re-render whatever is open so the header reflects it.
                    if self.current_thread.len() > 1 {
                        self.show_thread();
                    } else if let Some(current) = self.current.clone() {
                        let m = self.with_cached_body(current);
                        self.show_message(Some(m), false);
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetCardAttachments(on)) => {
                if pref!(self.card_attachments = on) {
                    self.message_view
                        .emit(MessageViewInput::SetCardAttachmentsShown(on));
                    for p in self.popouts.values() {
                        p.controller
                            .emit(MessageWindowInput::SetCardAttachmentsShown(on));
                    }
                }
            }
            AppMsg::Pref(PrefOutput::SetAttachmentDrawer(on)) => {
                if pref!(self.drawer_enabled = on) {
                    self.sync_attachment_drawer();
                    self.message_view
                        .emit(MessageViewInput::SetAttachmentDrawer(on));
                    for p in self.popouts.values() {
                        p.controller
                            .emit(MessageWindowInput::SetAttachmentDrawer(on));
                    }
                }
            }
            AppMsg::CardAttachment {
                account_id,
                id,
                index,
                save,
            } => {
                let Some(items) = self
                    .member_key((account_id, id))
                    .and_then(|k| self.attachment_cache.get(&k))
                    .cloned()
                else {
                    return;
                };
                let Some(att) = items.get(index).cloned() else {
                    return;
                };
                if save {
                    save_attachment(att, Some(self.window.clone()));
                } else if crate::ui::attachment_drawer::previewable(&att) {
                    // The lightbox pages through this message's previewable
                    // files, starting at the one clicked.
                    let previewable: Vec<Attachment> = items
                        .iter()
                        .filter(|a| crate::ui::attachment_drawer::previewable(a))
                        .cloned()
                        .collect();
                    let start = items
                        .iter()
                        .take(index + 1)
                        .filter(|a| crate::ui::attachment_drawer::previewable(a))
                        .count()
                        .saturating_sub(1);
                    sender.input(AppMsg::ShowLightbox {
                        items: previewable,
                        start,
                    });
                } else {
                    crate::ui::attachments_gallery::open_bytes(
                        &att.name,
                        &att.data,
                        Some(self.window.upcast_ref::<gtk::Window>()),
                    );
                }
            }
            AppMsg::ShowAttachmentInMessage(att) => {
                // The drawer shows identical (name, size) pairs once, so the
                // first message carrying this one is the one to find.
                let owner = self.conversation_members().into_iter().find(|key| {
                    self.member_key(*key)
                        .and_then(|k| self.attachment_cache.get(&k))
                        .is_some_and(|items| {
                            items
                                .iter()
                                .any(|a| a.name == att.name && a.data.len() == att.data.len())
                        })
                });
                if let Some((account_id, id)) = owner {
                    self.message_view
                        .emit(MessageViewInput::ScrollToAttachments { account_id, id });
                }
            }
            AppMsg::DeleteDrawerAttachment(att) => {
                if let Some(target) = self.attachment_target(&att.name, Some(att.data.len())) {
                    sender.input(AppMsg::AskDeleteAttachment(target));
                }
            }
            AppMsg::ShowcaseDeleteAttachment { name, confirmed } => {
                if let Some(name) = name.strip_prefix("gallery:") {
                    self.showcase_confirm_delete = confirmed;
                    self.gallery
                        .emit(GalleryInput::ShowcaseDelete(name.to_string()));
                    return;
                }
                match self.attachment_target(&name, None) {
                    Some(target) if confirmed => sender.input(AppMsg::DeleteAttachment(target)),
                    Some(target) => sender.input(AppMsg::AskDeleteAttachment(target)),
                    None => tracing::warn!("showcase: no attachment called {name:?} on screen"),
                }
            }
            AppMsg::AskDeleteAttachment(target)
                if std::mem::take(&mut self.showcase_confirm_delete) =>
            {
                sender.input(AppMsg::DeleteAttachment(target));
            }
            AppMsg::AskDeleteAttachment(target) => {
                self.confirm_delete_attachment(target, self.window.upcast_ref(), &sender)
            }
            AppMsg::DeleteAttachment(t) => {
                self.attachment_drawer
                    .emit(AttachmentDrawerInput::MarkDeleting(
                        t.name.clone(),
                        t.size as usize,
                    ));
                if let Some(p) = self.popouts.get(&(t.account_id, t.message_id)) {
                    p.controller.emit(MessageWindowInput::Drawer(
                        AttachmentDrawerInput::MarkDeleting(t.name.clone(), t.size as usize),
                    ));
                }
                self.gallery
                    .emit(GalleryInput::SetDeleting(t.gallery_key(), true));
                self.deleting_attachments.push(t.clone());
                // HYLKI_SHOWCASE_DELETE_HOLD=<seconds> holds the request back,
                // so the file can be captured while it is being deleted.
                if let Some(secs) = std::env::var("HYLKI_SHOWCASE_DELETE_HOLD")
                    .ok()
                    .and_then(|v| v.parse().ok())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(secs, move || {
                        s.input(AppMsg::SendDeleteAttachment(t));
                    });
                    return;
                }
                sender.input(AppMsg::SendDeleteAttachment(t));
            }
            AppMsg::SendDeleteAttachment(t) => {
                self.send_to(
                    t.account_id,
                    MailRequest::DeleteAttachment {
                        message_id: t.message_id,
                        path: t.path,
                        uid: t.uid,
                        name: t.name,
                        size: t.size,
                    },
                );
            }
            AppMsg::AttachmentNotDeleted {
                account_id,
                message_id,
                name,
                size,
            } => {
                if let Some(t) = self.take_deleting(account_id, message_id, &name) {
                    self.gallery
                        .emit(GalleryInput::SetDeleting(t.gallery_key(), false));
                }
                if let Some(p) = self.popouts.get(&(account_id, message_id)) {
                    p.controller.emit(MessageWindowInput::Drawer(
                        AttachmentDrawerInput::NotDeleted(name.clone(), size as usize),
                    ));
                }
                self.attachment_drawer
                    .emit(AttachmentDrawerInput::NotDeleted(name, size as usize));
            }
            AppMsg::AttachmentDeleted {
                account_id,
                message_id,
                path,
                uid,
                new_uid,
                name,
                size,
            } => {
                // The file fades out where it is shown; the lists that
                // follow wait for it.
                let t = self.take_deleting(account_id, message_id, &name);
                self.gallery.emit(GalleryInput::Removed((
                    account_id,
                    path.clone(),
                    uid,
                    name.clone(),
                )));
                let shown = t.map_or(size, |t| t.size) as usize;
                self.attachment_drawer
                    .emit(AttachmentDrawerInput::Deleted(name.clone(), shown));
                if let Some(p) = self.popouts.get(&(account_id, message_id)) {
                    p.controller
                        .emit(MessageWindowInput::Drawer(AttachmentDrawerInput::Deleted(
                            name.clone(),
                            shown,
                        )));
                }
                let key = (account_id, message_id);
                let members = self.conversation_members();
                self.thread_cache
                    .retain(|_, members| !members.iter().any(|m| (m.account_id, m.id) == key));
                let folder = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.path == path))
                    .map(|f| f.id);
                if let Some(fid) = folder {
                    self.body_cache.remove(&(account_id, fid, uid));
                }
                let held = folder.map(|fid| (account_id, fid, uid));
                if new_uid == Some(uid) {
                    // The same message, one file fewer (Microsoft 365): the
                    // reader drops the file where it stands.
                    if let Some(mut items) = held.and_then(|k| self.attachment_cache.remove(&k)) {
                        if let Some(at) = items
                            .iter()
                            .enumerate()
                            .filter(|(_, a)| a.name == name)
                            .min_by_key(|(_, a)| (a.data.len() as u64).abs_diff(size))
                            .map(|(i, _)| i)
                        {
                            items.remove(at);
                        }
                        sender.input(AppMsg::Attachments {
                            account_id,
                            path,
                            message_id,
                            items,
                        });
                    }
                    return;
                }
                // A copy took the message's place under a new UID. The list,
                // loaded again first, may already have carried the selection
                // over to it by Message-ID, the files it held with it; if not,
                // the copy is opened here.
                if let Some(k) = held {
                    self.attachment_cache.remove(&k);
                }
                let Some(new_uid) = new_uid else { return };
                let now = (account_id, new_uid);
                if let Some(fid) = folder {
                    self.attachment_cache.remove(&(account_id, fid, new_uid));
                }
                if members.contains(&now) {
                    self.send_to(
                        account_id,
                        MailRequest::LoadAttachments {
                            message_id: new_uid,
                            path,
                            uid: new_uid,
                            download: true,
                        },
                    );
                } else if members.contains(&key) {
                    self.message_list.emit(MessageListInput::SelectAndLoad(now));
                }
            }
            AppMsg::ZoomKey(step) => {
                // The keys are the application's, so they reach the main
                // window before the composer in it could take them.
                let compose = self.reader_compose.as_ref().filter(|r| {
                    r.window.is_none()
                        && focus_in_compose(&self.window)
                        && r.controller.model().zooms()
                });
                match compose {
                    Some(r) => r.controller.emit(ComposeInput::Zoom(step)),
                    None => sender.input(AppMsg::ZoomMessage(step)),
                }
            }
            AppMsg::ComposerKey(keyval) => {
                let compose = self
                    .reader_compose
                    .as_ref()
                    .filter(|r| r.window.is_none() && focus_in_compose(&self.window));
                let shift_ctrl =
                    gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK;
                match compose.zip(crate::ui::compose::composer_key(keyval, shift_ctrl)) {
                    Some((r, input)) => r.controller.emit(input),
                    // Activated without a value, the stateful action toggles.
                    None if keyval == gtk::gdk::Key::a => {
                        relm4::main_application().activate_action("show-accounts", None)
                    }
                    None => sender.input(AppMsg::OpenConsole),
                }
            }
            AppMsg::ZoomMessage(step) => {
                use config::READER_ZOOM_STEPS as STEPS;
                let at = STEPS.iter().position(|&z| z == self.zoom).unwrap_or(5) as i32;
                let zoom = match step {
                    0 => self.zoom_default,
                    s => {
                        STEPS
                            [(at + i32::from(s.signum())).clamp(0, STEPS.len() as i32 - 1) as usize]
                    }
                };
                self.set_zoom(zoom);
            }
            AppMsg::SetZoomDefault(zoom) => {
                if pref!(self.zoom_default = zoom) {
                    self.message_view
                        .emit(MessageViewInput::SetZoomDefault(zoom));
                    for p in self.popouts.values() {
                        p.controller.emit(MessageWindowInput::SetZoomDefault(zoom));
                    }
                }
                // The setting is also what the user wants to see now.
                self.set_zoom(zoom);
            }
            AppMsg::SetReaderMode(on) => {
                // In Focus Mode with Reader View on, the switch changes the
                // message on screen alone: the saved choice is the ordinary
                // one, and it comes back with the rest of the layout.
                if self.focus.active(config::FocusPart::ReaderView) {
                    // Nothing to remember.
                } else if self.reader_mode != on {
                    self.reader_mode = on;
                    self.save_settings();
                }
                // Always pushed, equal or not: with a per-message default the
                // readers reset themselves on each open, so what they show
                // can differ from the saved state — and a switch flipped in
                // one of them must land there regardless.
                self.message_view.emit(MessageViewInput::SetReaderMode(on));
                for p in self.popouts.values() {
                    p.controller.emit(MessageWindowInput::SetReaderMode(on));
                }
            }
            AppMsg::SetReaderSwitchShown(on) => {
                if pref!(self.reader_switch = on) {
                    self.message_view
                        .emit(MessageViewInput::SetReaderSwitchShown(on));
                    for p in self.popouts.values() {
                        p.controller
                            .emit(MessageWindowInput::SetReaderSwitchShown(on));
                    }
                }
            }
            AppMsg::SetThemeSwitchShown(on) => {
                if pref!(self.theme_switch = on) {
                    self.message_view
                        .emit(MessageViewInput::SetThemeSwitchShown(on));
                    for p in self.popouts.values() {
                        p.controller
                            .emit(MessageWindowInput::SetThemeSwitchShown(on));
                    }
                }
            }
            AppMsg::Pref(PrefOutput::SetDesktopIcons(on)) => {
                if pref!(self.desktop_icons = on) {
                    apply_icon_theme(on);
                }
            }
            AppMsg::Pref(PrefOutput::SetReaderDefault(policy)) => {
                if pref!(self.reader_default = policy) {
                    // Focus Mode's Reader View outranks it while on.
                    let policy = self.effective_reader_default();
                    self.message_view
                        .emit(MessageViewInput::SetReaderDefault(policy));
                    for p in self.popouts.values() {
                        p.controller
                            .emit(MessageWindowInput::SetReaderDefault(policy));
                    }
                }
            }
            AppMsg::Pref(PrefOutput::SetPgpLabels(on)) => {
                if pref!(self.pgp_labels = on) {
                    self.message_view.emit(MessageViewInput::SetPgpLabels(on));
                    for p in self.popouts.values() {
                        p.controller.emit(MessageWindowInput::SetPgpLabels(on));
                    }
                }
            }
            AppMsg::Pref(PrefOutput::SetSingleMessageCard(on)) => {
                if pref!(self.single_message_card = on) {
                    self.message_view
                        .emit(MessageViewInput::SetSingleMessageCard(on));
                    // Only lone messages change; re-render one if it's open.
                    if self.current_thread.len() <= 1 {
                        if let Some(current) = self.current.clone() {
                            let m = self.with_cached_body(current);
                            self.show_message(Some(m), false);
                        }
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetPaletteCollapse(secs)) => {
                if pref!(self.palette_collapse_secs = secs) {
                    self.message_list
                        .emit(MessageListInput::SetPaletteCollapse(secs));
                }
            }

            AppMsg::Pref(PrefOutput::SetCardPaletteCollapse(secs)) => {
                if pref!(self.card_palette_collapse_secs = secs) {
                    self.message_view
                        .emit(MessageViewInput::SetPaletteCollapse(secs));
                }
            }

            AppMsg::Pref(PrefOutput::SetListPalette(on)) => {
                if pref!(self.list_palette = on) {
                    self.message_list.emit(MessageListInput::SetListPalette(on));
                }
            }

            AppMsg::Pref(PrefOutput::SetListPaletteHover(on)) => {
                if pref!(self.list_palette_hover = on) {
                    self.message_list
                        .emit(MessageListInput::SetPaletteHover(on));
                }
            }

            AppMsg::Pref(PrefOutput::SetCardPaletteMenu(on)) => {
                if pref!(self.card_palette_menu = on) {
                    self.message_view
                        .emit(MessageViewInput::SetCardPaletteMenu(on));
                }
            }

            AppMsg::Pref(PrefOutput::SetSwipeEnabled(on)) => {
                if pref!(self.swipe_enabled = on) {
                    self.message_list
                        .emit(MessageListInput::SetSwipeEnabled(on));
                }
            }

            AppMsg::Pref(PrefOutput::SetSwipeReversed(on)) => {
                if pref!(self.swipe_reversed = on) {
                    self.message_list
                        .emit(MessageListInput::SetSwipeReversed(on));
                }
            }

            AppMsg::Pref(PrefOutput::SetSwipeSensitivity(factor)) => {
                let factor =
                    factor.clamp(config::SWIPE_SENSITIVITY_MIN, config::SWIPE_SENSITIVITY_MAX);
                if pref!(self.swipe_sensitivity = factor) {
                    self.message_list
                        .emit(MessageListInput::SetSwipeSensitivity(factor));
                }
            }

            AppMsg::SetRemoteContent {
                account_id,
                id,
                show,
            } => {
                self.remote_override.insert((account_id, id), show);
                // Re-show whatever is open so the decision takes effect now.
                // The reader re-reads `remote_allowed` for every message it
                // paints, so a conversation gets it for the card it applies to.
                if self.current_thread.len() > 1 {
                    self.show_thread();
                } else {
                    let current = self.current.clone();
                    self.show_message(current, false);
                }
            }

            AppMsg::Undo => self.undo_redo(false),

            AppMsg::Redo => self.undo_redo(true),

            AppMsg::ComposeHistory { id, undo, redo } => {
                // Only the inline composer's: a composer in a window of its
                // own answers its keys there and has no menu here to label.
                if self
                    .reader_compose
                    .as_ref()
                    .is_some_and(|r| r.id == id && r.window.is_none())
                {
                    self.compose_history = (undo, redo);
                    self.refresh_undo_menu();
                }
            }

            AppMsg::Pref(PrefOutput::SetComposeInline(on)) => {
                pref!(self.compose_inline = on);
            }

            AppMsg::Pref(PrefOutput::SetReplyInline(on)) => {
                pref!(self.reply_inline = on);
            }

            AppMsg::Pref(PrefOutput::SetReplyFields(on)) => {
                pref!(self.reply_fields = on);
            }

            AppMsg::Pref(PrefOutput::SetFilesPrefs(prefs)) => {
                pref!(self.files_prefs = prefs);
            }

            AppMsg::EditAsNewCurrent => {
                if let Some(m) = self.compose_target() {
                    self.edit_as_new(m, &sender);
                }
            }

            AppMsg::Pref(PrefOutput::SetLinkBrowser(choice)) => {
                if self.link_browser != choice {
                    self.link_browser = choice;
                    crate::ui::launch::set_browser(&self.link_browser);
                    self.save_settings();
                }
            }

            AppMsg::HandOffAction {
                hand_off,
                action,
                remember,
            } => {
                if remember && self.files_prefs.action != action {
                    self.files_prefs.action = action;
                    self.save_settings();
                    self.push_files_prefs();
                }
                self.hand_off_action(hand_off, action, &sender);
            }

            AppMsg::HandOffDraftPickNow => {
                if let Some((hand_off, _)) = self.pending_draft_pick.take() {
                    self.show_draft_picker(hand_off, &sender);
                }
            }

            AppMsg::NewFromTemplate => {
                // As the draft picker does: Templates folders not listed this
                // run are fetched first, and a silent worker gets three
                // seconds.
                let mut waiting = HashSet::new();
                for (account_id, folder) in self.templates_folders() {
                    if !self.message_cache.contains_key(&(account_id, folder.id)) {
                        waiting.insert((account_id, folder.id));
                        self.send_to(
                            account_id,
                            MailRequest::SyncFolder {
                                folder_id: folder.id,
                                path: folder.path.clone(),
                            },
                        );
                    }
                }
                if waiting.is_empty() {
                    self.show_template_picker(&sender);
                } else {
                    self.pending_template_pick = Some(waiting);
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        s.input(AppMsg::TemplatePickNow)
                    });
                }
            }

            AppMsg::TemplatePickNow => {
                if self.pending_template_pick.take().is_some() {
                    self.show_template_picker(&sender);
                }
            }

            AppMsg::TemplatePicked(picked) => match picked {
                Some(m) => self.edit_as_new(m, &sender),
                None => sender.input(AppMsg::Compose),
            },

            AppMsg::HandOffDraft { hand_off, draft } => {
                let target = match draft {
                    Some(m) => HandOffTarget::Draft(m),
                    None => HandOffTarget::New,
                };
                self.hand_off_size_check(hand_off, target, &sender);
            }

            AppMsg::HandOffReply { hand_off, message } => {
                let target = match message {
                    Some(m) => HandOffTarget::Reply(m),
                    None => HandOffTarget::New,
                };
                self.hand_off_size_check(hand_off, target, &sender);
            }

            AppMsg::HandOffOpen {
                hand_off,
                target,
                cloud,
                remember,
            } => {
                if remember {
                    let large = if cloud {
                        config::FilesLarge::Cloud
                    } else {
                        config::FilesLarge::Attach
                    };
                    if pref!(self.files_prefs.large = large) {
                        self.push_files_prefs();
                    }
                }
                self.hand_off_open(hand_off, target, cloud, &sender);
            }

            AppMsg::Pref(PrefOutput::SetComposeDefaultFrom(addr)) => {
                pref!(self.compose_default_from = addr);
            }

            AppMsg::Pref(PrefOutput::SetPastePlain(on)) => {
                pref!(self.paste_plain = on);
            }

            AppMsg::Pref(PrefOutput::SetReturnParagraph(on)) => {
                pref!(self.return_paragraph = on);
            }

            AppMsg::Pref(PrefOutput::SetToolbarExpanded(on)) => {
                pref!(self.toolbar_expanded = on);
            }

            AppMsg::Pref(PrefOutput::SetComposeFont(font)) => {
                pref!(self.compose_font = font);
            }

            AppMsg::Pref(PrefOutput::SetComposeFontSize(pt)) => {
                pref!(self.compose_font_size = pt);
            }

            AppMsg::Pref(PrefOutput::SetPrintOptions(options)) => {
                if pref!(self.print_options = options) {
                    crate::ui::print_preview::set_options(options);
                }
            }

            AppMsg::Pref(PrefOutput::SetSpellcheck(on)) => {
                if pref!(self.spellcheck = on) {
                    // Takes effect in already-open composers too: the shared
                    // web context is live.
                    crate::ui::rich_editor::apply_spellcheck();
                }
            }

            AppMsg::Pref(PrefOutput::SetSpellcheckLangs(langs)) => {
                if pref!(self.spellcheck_langs = langs) {
                    crate::ui::rich_editor::apply_spellcheck();
                    // A composer's right-click menu changes it too (#365):
                    // the Settings window, built once and kept, follows.
                    if let Some(p) = &self.prefs {
                        p.emit(PrefInput::SetSpellcheckLangs(self.spellcheck_langs.clone()));
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetMessageTheme(theme)) => {
                if pref!(self.message_theme = theme) {
                    let dark = theme.dark_override();
                    // Message content only — the reader and any popped-out windows.
                    self.message_view
                        .emit(MessageViewInput::SetContentTheme(dark));
                    for p in self.popouts.values() {
                        p.controller.emit(MessageWindowInput::SetContentTheme(dark));
                    }
                }
            }

            AppMsg::ShowSettingsPage(id) => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::ShowPageById(id));
                }
            }
            AppMsg::ReaderToolbarMenu { x, y } => {
                use crate::ui::context_menu::{show_context_menu, MenuEntry};
                if let Some(header) = self.reader_header.get() {
                    let s = sender.input_sender().clone();
                    let entry = MenuEntry::new(i18n("Customize Toolbar…"), move || {
                        let _ = s.send(AppMsg::CustomizeToolbar);
                    })
                    .icon("preferences-desktop-appearance-symbolic");
                    show_context_menu(header, x, y, vec![vec![entry]]);
                }
            }
            AppMsg::CustomizeToolbar => {
                self.open_settings_window(&sender, false, false);
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::ShowPageById("appearance".to_string()));
                }
            }
            AppMsg::CustomizeColumns => {
                self.open_settings_window(&sender, false, false);
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::RevealColumns);
                }
            }
            AppMsg::ShowcaseToolbarGap { zone, index } => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::ToolbarGapPreview { zone, index });
                }
            }
            AppMsg::ReloadBody(m) => {
                if let Some(path) = self.resolve_folder_path(&m) {
                    self.send_to(
                        m.account_id,
                        MailRequest::LoadBody {
                            message_id: m.id,
                            path,
                            uid: m.uid,
                        },
                    );
                }
            }
            AppMsg::Pref(PrefOutput::SetOverrideFonts(on)) => {
                if pref!(self.override_fonts = on) {
                    self.push_reader_style();
                }
            }
            AppMsg::Pref(PrefOutput::SetReaderFont(font)) => {
                if pref!(self.reader_font = font) {
                    self.push_reader_style();
                }
            }
            AppMsg::Pref(PrefOutput::SetPlainMonospace(on)) => {
                if pref!(self.plain_monospace = on) {
                    self.push_reader_style();
                }
            }
            AppMsg::Pref(PrefOutput::SetPlainFont(font)) => {
                if pref!(self.plain_font = font) {
                    self.push_reader_style();
                }
            }
            AppMsg::ShowcaseComposeMenu { format } => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(if format {
                        ComposeInput::FormatMenu
                    } else {
                        ComposeInput::OverflowMenu
                    });
                }
            }
            AppMsg::ShowcaseComposePreview => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::TogglePreview(true));
                }
            }
            AppMsg::ShowcaseComposeFrom(n) => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::ShowcaseFrom(n));
                }
            }
            AppMsg::ShowcaseForward { account_id, id } => {
                let found = self
                    .current_thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                    .cloned()
                    .or_else(|| {
                        self.message_cache
                            .values()
                            .flatten()
                            .find(|m| m.account_id == account_id && m.id == id)
                            .cloned()
                    });
                match found {
                    Some(m) => self.forward(m, true, &sender),
                    None => tracing::warn!("showcase forward: {account_id}:{id} is not loaded"),
                }
            }
            AppMsg::ShowcaseComposeUndo => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::ShowcaseHistory(0));
                }
            }
            AppMsg::ShowcaseBurger => {
                if let Some(m) = self.sidebar_menu.as_ref() {
                    // Focus first: a click on a MenuButton focuses it before
                    // the popover opens, and popup() alone skips that — the
                    // difference between the two is a bug's hiding place.
                    m.grab_focus();
                    m.popup();
                    let mut chain = Vec::new();
                    let mut node = gtk::prelude::GtkWindowExt::focus(&self.window);
                    while let Some(w) = node {
                        chain.push(format!("{}", w.type_()));
                        node = w.parent();
                    }
                    tracing::info!(
                        target: "hylki::compose::undo",
                        "burger: opened, focus now [{}]",
                        chain.join(" < ")
                    );
                }
            }
            AppMsg::ShowcaseComposeFormat(format) => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::SetFormat(format));
                }
            }
            AppMsg::Pref(PrefOutput::SetComposeFormat(format)) => {
                pref!(self.compose_format = format);
            }
            AppMsg::Pref(PrefOutput::SetFollowPlain(on)) => {
                pref!(self.follow_plain = on);
            }
            AppMsg::Pref(PrefOutput::SetReplyPosition(position)) => {
                // A split reply already open stays where it is; the next
                // one opens in the new place.
                pref!(self.reply_position = position);
            }
            AppMsg::Pref(PrefOutput::SetSignaturePosition(position)) => {
                // Composers already open keep their signature where it is;
                // the next reply or forward opens with the new placement.
                pref!(self.signature_position = position);
            }
            AppMsg::Pref(PrefOutput::SetSignatureDashes(on)) => {
                pref!(self.signature_dashes = on);
            }
            AppMsg::Pref(PrefOutput::SetUndoSend(secs)) => {
                // A message already waiting keeps the delay it was sent with.
                pref!(self.undo_send_secs = secs);
            }
            AppMsg::Pref(PrefOutput::SetOverrideColors(on)) => {
                if pref!(self.override_colors = on) {
                    self.push_reader_style();
                }
            }

            AppMsg::ComposeTo(to) => {
                self.compose_addressed(
                    ComposePrefill {
                        to,
                        ..Default::default()
                    },
                    &sender,
                );
            }

            AppMsg::ComposeBcc(bcc) => {
                self.compose_addressed(
                    ComposePrefill {
                        bcc,
                        ..Default::default()
                    },
                    &sender,
                );
            }

            AppMsg::OpenWithFiles(mut paths) => {
                // Same open-composer flow as OpenMailto, with the handed-in
                // files pre-attached. The relay normalizes every stray
                // command-line argument into a file URI, so keep only the
                // ones that name a real file.
                let named = paths.len();
                paths.retain(|p| {
                    let ok = p.is_file();
                    if !ok {
                        tracing::warn!(
                            "file hand-off ignored (not a readable file): {}",
                            p.display()
                        );
                    }
                    ok
                });
                tracing::info!("file hand-off: {named} named, {} readable", paths.len());
                if paths.is_empty() {
                    return;
                }
                self.begin_hand_off(
                    FileHandOff {
                        base: ComposePrefill::default(),
                        files: paths,
                        dropped: Vec::new(),
                    },
                    &sender,
                );
            }

            AppMsg::DropFilesAs(choice, paths) => {
                use crate::ui::drop_zones::DropChoice;
                tracing::info!(
                    "file drop: {} file(s) for a new message ({choice:?})",
                    paths.len()
                );
                let mut prefill = ComposePrefill::default();
                match choice {
                    // The route a drop always took: the size check first.
                    DropChoice::Attach => {
                        sender.input(AppMsg::DropFiles(paths));
                        return;
                    }
                    DropChoice::Inline => prefill.inline_files = paths,
                    DropChoice::Cloud => prefill.cloud_uploads = paths,
                }
                self.leave_gallery();
                let account = self
                    .current
                    .as_ref()
                    .map(|m| m.account_id)
                    .unwrap_or_else(|| self.active_account());
                let (account, prefill) = self.new_message_from(account, prefill);
                if self.compose_inline {
                    self.open_inline_reply(account, prefill, None, &sender);
                } else {
                    self.open_compose(account, prefill, &sender);
                }
            }

            AppMsg::DropFiles(paths) => {
                tracing::info!("file drop: {} file(s)", paths.len());
                // A composer open in the window is what the files are for,
                // wherever in the window they were let go.
                if let Some(r) = self.reader_compose.as_ref().filter(|r| r.window.is_none()) {
                    r.controller.emit(ComposeInput::AddAttachments(paths));
                    return;
                }
                self.leave_gallery();
                self.hand_off_size_check(
                    FileHandOff {
                        base: ComposePrefill::default(),
                        files: paths,
                        dropped: Vec::new(),
                    },
                    HandOffTarget::New,
                    &sender,
                );
            }

            AppMsg::CopyReaderSelection => {
                self.message_view.emit(MessageViewInput::CopySelection);
            }

            AppMsg::OpenMessageFile(path) => self.open_message_file(&path, &sender),
            AppMsg::PickMessageFile => self.pick_message_file(&sender),

            AppMsg::OpenMid(uri) => {
                // The cache first: it holds every message Hylki has listed,
                // across accounts, and answers at once. Only a miss asks the
                // servers, one HEADER search per folder per account.
                let Some(id) = parse_mid(&uri) else {
                    tracing::warn!("mid: link not understood: {uri}");
                    return;
                };
                let hits: Vec<(u32, String, u32)> = crate::cache::Cache::open()
                    .map(|c| c.locate_by_message_id(&id))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(acc, path, _)| {
                        self.folders
                            .get(acc)
                            .is_some_and(|fs| fs.iter().any(|f| f.path == *path))
                    })
                    .collect();
                // The copy in the account currently in view wins when several
                // accounts hold the message (a mail sent to two of them).
                let preferred = self.current.as_ref().map(|m| m.account_id);
                if let Some((account_id, folder_path, uid)) = hits
                    .iter()
                    .find(|(acc, _, _)| Some(*acc) == preferred)
                    .or_else(|| hits.first())
                    .cloned()
                {
                    tracing::info!(
                        "mid: {id} is cached in account {account_id}, {folder_path} uid {uid}"
                    );
                    sender.input(AppMsg::OpenAttachmentMessage {
                        account_id,
                        folder_path,
                        uid,
                    });
                    return;
                }
                if self.workers.is_empty() {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n_f("No message with the ID {id} was found", &[("id", &id)]),
                        error: true,
                        connectivity: false,
                    });
                    return;
                }
                self.mid_searches.insert(id.clone(), self.workers.len());
                for w in self.workers.values() {
                    let _ = w.send(MailRequest::Locate {
                        message_id: id.clone(),
                    });
                }
            }

            AppMsg::MidLocated {
                account_id,
                message_id,
                hit,
            } => {
                let Some(remaining) = self.mid_searches.get_mut(&message_id) else {
                    return;
                };
                if let Some((folder_path, uid)) = hit {
                    tracing::info!(
                        "mid: {message_id} found on account {account_id}, {folder_path} uid {uid}"
                    );
                    self.mid_searches.remove(&message_id);
                    sender.input(AppMsg::OpenAttachmentMessage {
                        account_id,
                        folder_path,
                        uid,
                    });
                    return;
                }
                *remaining = remaining.saturating_sub(1);
                if *remaining == 0 {
                    tracing::info!("mid: {message_id} not found on any account");
                    self.mid_searches.remove(&message_id);
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n_f(
                            "No message with the ID {id} was found",
                            &[("id", &message_id)],
                        ),
                        error: true,
                        connectivity: false,
                    });
                }
            }

            AppMsg::FindTags => {
                if self.tag_scan.is_some() {
                    return;
                }
                if self.workers.is_empty() {
                    if let Some(a) = &self.accounts_win {
                        a.emit(crate::ui::accounts::AccountsInput::TagFindings(Vec::new()));
                    }
                    return;
                }
                self.tag_scan_gen = self.tag_scan_gen.wrapping_add(1);
                let gen = self.tag_scan_gen;
                self.tag_scan = Some(TagScan {
                    gen,
                    remaining: self.workers.len(),
                    found: Vec::new(),
                });
                for w in self.workers.values() {
                    let _ = w.send(MailRequest::FindKeywords);
                }
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::TagScanning(true));
                }
                // An account that is offline never answers: report what the
                // others found rather than searching forever.
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(120, move || {
                    s.input(AppMsg::TagScanTimeout(gen));
                });
            }

            AppMsg::WantQuota(email) => {
                let worker = self
                    .accounts
                    .iter()
                    .find(|a| a.email.eq_ignore_ascii_case(&email))
                    .and_then(|a| self.workers.get(&a.id));
                if let Some(w) = worker {
                    let _ = w.send(MailRequest::Quota);
                }
            }

            AppMsg::QuotaFound { account_id, quota } => {
                if let (Some(email), Some(a)) = (self.email_of(account_id), &self.accounts_win) {
                    a.emit(crate::ui::accounts::AccountsInput::Quota { email, quota });
                }
            }

            AppMsg::KeywordsFound {
                account_id,
                findings,
            } => {
                let Some(scan) = self.tag_scan.as_mut() else {
                    return;
                };
                scan.found.push((account_id, findings));
                scan.remaining = scan.remaining.saturating_sub(1);
                if scan.remaining == 0 {
                    self.finish_tag_scan(&sender);
                }
            }

            AppMsg::TagScanTimeout(gen) => {
                if self.tag_scan.as_ref().is_some_and(|s| s.gen == gen) {
                    tracing::warn!("find tags: not every account answered in time");
                    self.finish_tag_scan(&sender);
                }
            }

            AppMsg::OpenMailto(uri) => {
                // A mailto: link from anywhere on the system (the desktop file
                // registers the scheme). Same open-composer flow as ComposeTo,
                // with the URI's subject/cc/bcc/body carried along.
                let Some(mut prefill) = parse_mailto(&uri) else {
                    return;
                };
                // attach= names files (#90): keep only the ones that really
                // exist as regular files. Whatever attaches shows up as a
                // normal removable chip in the composer, so nothing rides
                // along invisibly if a web link (rather than Nautilus)
                // carried the parameter. The ones that don't are reported,
                // not dropped in silence: from inside the Flatpak sandbox a
                // path the file manager can see may be one Hylki cannot.
                let (attachments, dropped): (Vec<_>, Vec<_>) =
                    std::mem::take(&mut prefill.attachments)
                        .into_iter()
                        .partition(|p| p.is_file());
                for p in &dropped {
                    tracing::warn!(
                        "mailto attach ignored (not a readable file): {}",
                        p.display()
                    );
                }
                tracing::info!(
                    "mailto hand-off: {} attachment(s) named, {} readable",
                    attachments.len() + dropped.len(),
                    attachments.len()
                );
                if !attachments.is_empty() {
                    // Files came along (Files' own "Email…" entry): the same
                    // choice of destination as "Send with Hylki".
                    self.begin_hand_off(
                        FileHandOff {
                            base: prefill,
                            files: attachments,
                            dropped,
                        },
                        &sender,
                    );
                    return;
                }
                // None of the named files could be read (#339): asking where
                // no files should go helps nobody, so the message opens with
                // the error beside it.
                self.leave_gallery();
                let account = self
                    .current
                    .as_ref()
                    .map(|m| m.account_id)
                    .unwrap_or_else(|| self.active_account());
                let (account, prefill) = self.new_message_from(account, prefill);
                if self.compose_inline {
                    self.open_inline_reply(account, prefill, None, &sender);
                } else {
                    self.open_compose(account, prefill, &sender);
                }
                self.after_hand_off(0, dropped);
            }

            AppMsg::PresentComposers => {
                crate::desktop::present_window(&self.window);
                // The newest standalone composer is the one the alert was
                // about; transient for the main window, it sits above it.
                if let Some(h) = self.composers.last() {
                    crate::desktop::present_window(&h.window);
                }
            }

            AppMsg::SendMessage { from, out } => {
                // A reply sent from the split pane becomes its own card in
                // the conversation, instead of sliding away (send_morph.rs):
                // its card is built now, for the composer's close to carry
                // the editor to.
                self.arriving = None;
                if self.reply_joins(from, &out) {
                    match self.provisional_message(&out) {
                        Some(m) => self.arriving = Some((from, m, motion_wanted())),
                        None => {
                            tracing::info!(target: "hylki::morph", "no send morph: the reply's card could not be built")
                        }
                    }
                }
                let account_id = out.from_account_id;
                // Remember who this went to, now, whatever route it takes
                // from here (straight out, the Outbox, Send Later): the next
                // composer suggests them, and so do the ones already open.
                let recipients: Vec<(String, String)> = [&out.to, &out.cc, &out.bcc]
                    .into_iter()
                    .flat_map(|f| crate::worker::parse_recipients(f))
                    .collect();
                if let Some(c) = self.cache.as_ref() {
                    c.record_addresses(&recipients);
                }
                let own: HashSet<String> =
                    self.config.iter().map(|c| c.email.to_lowercase()).collect();
                let fresh: Vec<crate::contacts::Suggestion> = recipients
                    .into_iter()
                    .filter(|(_, e)| e.contains('@'))
                    .map(|(name, email)| crate::contacts::Suggestion {
                        name: if name.is_empty() { email.clone() } else { name },
                        own: own.contains(&email.to_lowercase()),
                        email,
                        from_contacts: false,
                        from_directory: false,
                        score: 1,
                        group: None,
                    })
                    .collect();
                if !fresh.is_empty() {
                    for host in &self.composers {
                        host.controller.emit(ComposeInput::AddSuggestions {
                            account_id,
                            new: fresh.clone(),
                        });
                    }
                    if let Some(rc) = &self.reader_compose {
                        rc.controller.emit(ComposeInput::AddSuggestions {
                            account_id,
                            new: fresh.clone(),
                        });
                    }
                }
                let sent_path = self.sent_copy_path(account_id);
                if out.send_at.is_some() {
                    // Send Later has its own way back: the message waits in
                    // the Outbox, where it can be edited or dropped.
                    self.send_to(
                        account_id,
                        MailRequest::Send {
                            message: out,
                            sent_path,
                            ticket: 0,
                        },
                    );
                } else {
                    let origin = match self.reader_compose.as_ref().filter(|r| r.id == from) {
                        Some(r) if r.window.is_none() => r.answering.map_or(
                            crate::outgoing::Origin::Pane,
                            crate::outgoing::Origin::Split,
                        ),
                        _ => crate::outgoing::Origin::Window,
                    };
                    self.queue_send(origin, out, sent_path, &sender);
                }
            }

            AppMsg::DispatchSend(ticket) => self.dispatch_send(ticket),

            AppMsg::UndoSend(ticket) => self.undo_send(ticket, &sender),

            AppMsg::SendDone { ticket, sent } => {
                // A failure has said so already, in the status bar, and the
                // message is in the Outbox or back in a composer. Its
                // original is answered once it goes, not before (#454).
                if sent {
                    self.pending_answers.remove(&ticket);
                } else {
                    self.unmark_answered(ticket);
                }
                let done = {
                    let mut list = self.outgoing.borrow_mut();
                    list.iter()
                        .position(|o| o.ticket == ticket)
                        .map(|i| list.remove(i))
                };
                if done.is_some() {
                    self.push_outbox();
                }
            }

            AppMsg::SaveDraftMessage(out) => {
                let account_id = out.from_account_id;
                let Some((folder_id, path)) = self.drafts_target(account_id) else {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("No Drafts folder available for this account"),
                        error: true,
                        connectivity: false,
                    });
                    return;
                };
                self.send_to(
                    account_id,
                    MailRequest::SaveDraft {
                        message: out,
                        folder_id,
                        path,
                        autosave: None,
                        template: false,
                    },
                );
            }

            AppMsg::SaveTemplateMessage(out) => {
                let account_id = out.from_account_id;
                // An edited template goes back to the folder it was kept in;
                // a new one, or one now sent from another account, to the
                // account's Templates folder, which saving makes if need be.
                let target = out
                    .draft_origin
                    .as_ref()
                    .filter(|o| o.account_id == account_id)
                    .map(|o| (o.folder_id, o.path.clone()))
                    .or_else(|| self.templates_target(account_id));
                let Some((folder_id, path)) = target else {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("No Templates folder available for this account"),
                        error: true,
                        connectivity: false,
                    });
                    return;
                };
                self.send_to(
                    account_id,
                    MailRequest::SaveDraft {
                        message: out,
                        folder_id,
                        path,
                        autosave: None,
                        template: true,
                    },
                );
            }

            AppMsg::FillUnifiedSlices => self.fill_unified_slices(),

            AppMsg::Preload => {
                self.preload_due = false;
                self.preload(&sender);
            }

            AppMsg::PrebuildViews => {
                self.prebuild_due = false;
                let mut views: Vec<UnifiedView> = Vec::new();
                if self.multi_account() {
                    if self.unified_inboxes_shown() {
                        views.push(UnifiedView::Kind(FolderKind::Inbox));
                    }
                    // Starred is built from the flags of every folder, and
                    // is small: its first visit builds it.
                    views.extend(
                        self.unified_kinds
                            .listed()
                            .into_iter()
                            .filter(|k| *k != FolderKind::Starred)
                            .map(UnifiedView::Kind),
                    );
                }
                // Taken from the end: the Inboxes first.
                views.reverse();
                self.prebuild_queue = views;
                self.prebuild_next(&sender);
            }

            AppMsg::PrebuildNext => self.prebuild_next(&sender),

            AppMsg::UnifiedFoldersLoaded { lists } => {
                let open = self.unified;
                let mut landed = false;
                for ((account_id, folder_id), messages) in lists {
                    // The worker's own answer may have come first. An empty
                    // folder is recorded as read too: building a unified row
                    // ahead waits on all of its folders.
                    if self
                        .message_cache
                        .get(&(account_id, folder_id))
                        .is_some_and(|c| !c.is_empty())
                    {
                        continue;
                    }
                    if messages.is_empty() {
                        self.message_cache
                            .entry((account_id, folder_id))
                            .or_default();
                        continue;
                    }
                    let messages = self.merge_local_tags(account_id, messages);
                    let messages = self.apply_pending_seen(account_id, folder_id, messages);
                    let messages = self.apply_pending_answers(account_id, folder_id, messages);
                    let messages = self.apply_pending_tags(account_id, folder_id, messages);
                    if open && self.is_unified_target(account_id, folder_id) {
                        if self.unified_reads_through() {
                            self.unified_slices.remove(&(account_id, folder_id));
                            self.unified_unfilled.insert((account_id, folder_id));
                        } else {
                            self.unified_unfilled.remove(&(account_id, folder_id));
                            self.unified_slices
                                .insert((account_id, folder_id), messages.clone());
                        }
                        landed = true;
                    } else if !open
                        && self
                            .selected
                            .as_ref()
                            .is_some_and(|s| (s.account_id, s.folder_id) == (account_id, folder_id))
                    {
                        // The folder on show had only its newest so far.
                        self.message_list.emit(MessageListInput::SetMessages {
                            messages: messages.clone(),
                        });
                    }
                    self.message_cache
                        .insert((account_id, folder_id), messages.into());
                }
                // A view left before its folders landed.
                self.shed_large_folders();
                if landed {
                    self.emit_unified();
                    self.push_index_complete();
                }
                self.schedule_prebuild(&sender);
            }

            AppMsg::PushUnreadCounts => self.push_unread_counts(),

            AppMsg::EmitUnifiedNow => {
                self.list_fp.set(None);
                if self.unified {
                    self.emit_unified();
                }
            }

            AppMsg::EmitUnified => {
                self.unified_emit_queued.set(false);
                if self.thread_links_due.replace(false) {
                    self.push_thread_links();
                }
                if self.unified && self.unified_dirty.replace(false) {
                    self.emit_unified();
                }
            }

            AppMsg::AutoSaveDraft { id, message } => {
                let account_id = message.from_account_id;
                match self.drafts_target(account_id) {
                    Some((folder_id, path)) => self.send_to(
                        account_id,
                        MailRequest::SaveDraft {
                            message,
                            folder_id,
                            path,
                            autosave: Some(id),
                            template: false,
                        },
                    ),
                    None => self.emit_to_composer(
                        id,
                        ComposeInput::Autosaved {
                            saved: false,
                            origin: None,
                        },
                    ),
                }
            }

            AppMsg::DiscardAutosave { id, origin } => {
                self.purge_draft(&origin);
                self.close_compose(id);
                self.message_list.emit(MessageListInput::ReclaimFocus);
            }

            AppMsg::DeleteDraft { id, origin } => {
                // The same move the list's trash button makes for a draft, so
                // it is undoable and the Drafts list updates in place. A draft
                // the list no longer holds (a stale window) is erased by uid.
                let cached = self
                    .message_cache
                    .get(&(origin.account_id, origin.folder_id))
                    .and_then(|msgs| msgs.iter().find(|m| m.uid == origin.uid).cloned());
                match cached {
                    Some(m) => self.move_to(m, FolderKind::Trash),
                    None => {
                        self.send_to(
                            origin.account_id,
                            MailRequest::PurgeMessages {
                                path: origin.path.clone(),
                                uids: vec![origin.uid],
                            },
                        );
                        self.send_to(
                            origin.account_id,
                            MailRequest::LoadMessages {
                                folder_id: origin.folder_id,
                                path: origin.path,
                            },
                        );
                    }
                }
                self.pending_draft = None;
                self.pending_draft_token = None;
                self.close_compose(id);
                self.message_list.emit(MessageListInput::ReclaimFocus);
            }

            AppMsg::DraftSaved {
                account_id,
                autosave,
                message_id,
            } => {
                // The Drafts folder reload already reflects the saved draft; an
                // explicit save's composer has closed. No notification (mirrors
                // silent send). An automatic save tells its composer where the
                // copy is now, so the next save replaces it (#340).
                let Some(id) = autosave else { return };
                let origin = message_id
                    .as_deref()
                    .and_then(|mid| self.find_draft(account_id, mid));
                if !self.composer_exists(id) {
                    // Sent, saved or closed while this save was on its way:
                    // the copy it left is not wanted.
                    if let Some(origin) = &origin {
                        self.purge_draft(origin);
                    }
                    return;
                }
                self.emit_to_composer(
                    id,
                    ComposeInput::Autosaved {
                        saved: message_id.is_some(),
                        origin,
                    },
                );
            }

            AppMsg::ShowcaseComposeClose => {
                if let Some(r) = &self.reader_compose {
                    r.controller.emit(ComposeInput::Cancel);
                }
            }
            AppMsg::SendMorphMeasured => self.send_morph_swap(),
            AppMsg::SendMorphFailed => self.send_morph_failed(),

            AppMsg::ShowcaseCopy => {
                let Some(current) = self.current.clone() else {
                    return;
                };
                let Some(p) = self.provisionals.values().next() else {
                    return;
                };
                let mut copy = p.message.clone();
                copy.uid = 9_999;
                copy.message_id = "showcase-copy@hylki.local".into();
                tracing::info!(target: "hylki::morph", "showcase: the copy of {} arrives", p.message.id);
                self.merge_related(current.account_id, current.id, vec![copy]);
                self.show_thread();
                // The window once the copy has painted, beside the morph's
                // own last frame, to hold the two against each other.
                if let Some(dir) = std::env::var_os("HYLKI_MORPH_FRAMES") {
                    let window: gtk::Widget = self.window.clone().upcast();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(1200),
                        move || {
                            crate::ui::send_morph::capture_next_frame(
                                &window.clone(),
                                move |still| {
                                    if let Some((tex, _)) = still {
                                        let _ = tex.save_to_png(
                                            std::path::Path::new(&dir).join("zz-copy.png"),
                                        );
                                    }
                                },
                            );
                        },
                    );
                }
            }

            AppMsg::ShowcaseSend(text) => {
                // Every frame interval for a second and a half, to compare
                // the send morph with the slide it replaces.
                if let Some(clock) = self.window.frame_clock() {
                    let last = std::rc::Rc::new(std::cell::Cell::new(0i64));
                    let id = clock.connect_after_paint(move |c| {
                        let now = c.frame_time();
                        if last.get() > 0 {
                            tracing::debug!(target: "hylki::frames", "frame {} ms", (now - last.get()) / 1000);
                        }
                        last.set(now);
                    });
                    let c2 = clock.clone();
                    let cell = std::rc::Rc::new(std::cell::Cell::new(Some(id)));
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(2500),
                        move || {
                            if let Some(id) = cell.take() {
                                c2.disconnect(id);
                            }
                        },
                    );
                }
                let Some(rc) = self.reader_compose.as_ref().map(|r| &r.controller) else {
                    return;
                };
                let editor = crate::ui::send_morph::find_widget(rc.widget().upcast_ref(), &|w| {
                    w.is::<webkit6::WebView>()
                })
                .and_downcast::<webkit6::WebView>();
                let Some(editor) = editor else { return };
                let js = format!(
                    "document.body.focus();{}",
                    text.split('\n')
                        .map(|l| format!(
                            "document.execCommand('insertText',false,{});",
                            serde_json::to_string(l).unwrap_or_default()
                        ))
                        .collect::<Vec<_>>()
                        .join("document.execCommand('insertParagraph');")
                );
                webkit6::prelude::WebViewExt::evaluate_javascript(
                    &editor,
                    &js,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |_| {},
                );
                let c = rc.sender().clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(600),
                    move || {
                        c.emit(ComposeInput::Send);
                    },
                );
                // The whole window every quarter second for six seconds from
                // Send, to see what the reader does while the message goes.
                if let Some(dir) = std::env::var_os("HYLKI_MORPH_FRAMES") {
                    let window: gtk::Widget = self.window.clone().upcast();
                    for i in 0..24u64 {
                        let (dir, window) = (dir.clone(), window.clone());
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(600 + i * 250),
                            move || {
                                crate::ui::send_morph::capture_next_frame(
                                    &window.clone(),
                                    move |still| {
                                        if let Some((tex, _)) = still {
                                            let _ = tex.save_to_png(
                                                std::path::Path::new(&dir)
                                                    .join(format!("win-{:05}.png", i * 250)),
                                            );
                                        }
                                    },
                                );
                            },
                        );
                    }
                }
            }

            AppMsg::ComposeClosed(id) => {
                self.close_compose(id);
                self.message_list.emit(MessageListInput::ReclaimFocus);
            }

            AppMsg::ComposeCloseRequested(id) => {
                let controller = self
                    .composers
                    .iter()
                    .find(|h| h.id == id)
                    .map(|h| &h.controller)
                    .or(self
                        .reader_compose
                        .as_ref()
                        .filter(|r| r.id == id)
                        .map(|r| &r.controller));
                match controller {
                    Some(c) => c.emit(ComposeInput::Cancel),
                    None => self.close_compose(id),
                }
            }

            AppMsg::ComposeToggleWindow(id) => self.toggle_compose_window(id, &sender),

            AppMsg::Sent { account_id } => {
                // No success notification: only send failures are surfaced
                // (via WorkerEvent::Error). Just refresh the folder the copy
                // landed in, if that folder is the open one, which is
                // wherever the account files its copies (#199), not always
                // Sent.
                if let Some(sel) = self.selected.clone() {
                    let copied_to = self.sent_copy_path(account_id);
                    let viewing_sent = sel.account_id == account_id
                        && copied_to.is_some_and(|path| path == sel.path);
                    if viewing_sent {
                        self.send_to(
                            account_id,
                            MailRequest::LoadMessages {
                                folder_id: sel.folder_id,
                                path: sel.path,
                            },
                        );
                    }
                }
                // The reply just sent answers the conversation on screen, and
                // its copy is in the cache by now (the worker lists the copy's
                // folder before it says Sent): ask for the rest of the thread
                // again so the reply joins it here, not after the next visit
                // to the Sent folder (#199).
                self.reload_related(account_id);
            }

            AppMsg::Unsent {
                account_id,
                message,
            } => {
                self.drop_provisional(&message.in_reply_to);
                self.compose_unsent(account_id, *message, &sender);
            }

            AppMsg::OpenAccounts => self.open_settings_window(&sender, true, false),

            AppMsg::AddFirstAccount => self.open_settings_window(&sender, true, true),
            AppMsg::AccountSaved {
                original_email,
                account,
            } => {
                // Whether the account joined or left the unified section
                // (#267): the merged views open now are redrawn without it.
                let unified_changed = original_email
                    .as_ref()
                    .and_then(|orig| self.effective_config().iter().find(|c| &c.email == orig))
                    .is_some_and(|old| old.in_unified != account.in_unified);
                // Demo mode: the edit lands on the in-memory stand-in (so a
                // new color, emoji or picture shows in the sidebar and the
                // reader at once) and nothing is written or reconnected.
                if self.config.is_empty() && demo_mode() {
                    let slot = original_email
                        .as_ref()
                        .and_then(|orig| self.demo_config.iter_mut().find(|c| &c.email == orig));
                    match slot {
                        Some(slot) => *slot = *account,
                        None => self.demo_config.push(*account),
                    }
                    if let Err(e) = config::save_demo_accounts(&self.demo_config) {
                        tracing::warn!("could not save the demo accounts: {e}");
                    }
                    self.rebuild_sidebar();
                    self.warm_own_gravatars(&sender);
                    self.refresh_faces();
                    if unified_changed {
                        self.reload_unified_views(
                            self.unified.then_some(self.unified_view),
                            &sender,
                        );
                    }
                    return;
                }
                let open_unified = self.unified.then_some(self.unified_view);
                let new_email = account.email.clone();
                // An edit that only changes how the account looks or signs
                // (#382) is applied in place: reconnecting every account for
                // it emptied the list and the reader until they had synced.
                // Whether its secrets changed too is the keyring's to say,
                // asked now, before the save below replaces them: the keyring
                // thread answers in order. A changed one reconnects then.
                let look_only = original_email
                    .as_ref()
                    .and_then(|orig| self.config.iter().find(|c| &c.email == orig))
                    .filter(|old| look_only_change(old, &account))
                    .cloned();
                let display_only = look_only.is_some();
                if let Some(old) = look_only {
                    let (new, email, sender) =
                        ((*account).clone(), new_email.clone(), sender.clone());
                    config::keyring_then(
                        move || secrets_kept(&old, &new),
                        move |kept| {
                            if kept {
                                tracing::info!("{email}: saved without reconnecting, only its look or signature changed");
                            } else {
                                sender.input(AppMsg::ReconnectAccounts);
                            }
                        },
                    );
                }
                // Remember the secret we expect to persist, so we can verify the
                // keyring actually stored it (a silent keyring failure would
                // otherwise leave the account unable to log in after a restart).
                let expected_secret =
                    (!account.password.is_empty()).then(|| account.password.clone());
                // An alias dropped (or switched back to the account's SMTP) in
                // this edit must not leave its SMTP password behind in the
                // keyring — and an email rename re-keys every alias entry, so
                // the old ones all go. config::save() below stores the current
                // set fresh.
                if let Some(orig) = &original_email {
                    if let Some(old) = self.config.iter().find(|c| &c.email == orig) {
                        for old_alias in &old.aliases {
                            let kept = old.email == new_email
                                && account.aliases.iter().any(|n| {
                                    n.has_own_smtp()
                                        && n.address().eq_ignore_ascii_case(&old_alias.address())
                                });
                            if !kept {
                                let (email, address) = (old.email.clone(), old_alias.address());
                                config::keyring_later(move || {
                                    config::delete_alias_smtp_password(&email, &address)
                                });
                            }
                        }
                    }
                }
                match original_email {
                    // Editing an existing account (matched by its previous email).
                    Some(orig) => {
                        if let Some(slot) = self.config.iter_mut().find(|c| c.email == orig) {
                            *slot = *account;
                        } else {
                            self.config.push(*account);
                        }
                        // Track an email change in the display-order/collapsed lists.
                        if orig != new_email {
                            for e in self
                                .account_order
                                .iter_mut()
                                .chain(self.collapsed.iter_mut())
                            {
                                if *e == orig {
                                    *e = new_email.clone();
                                }
                            }
                        }
                    }
                    // Adding a new account.
                    None => self.config.push(*account),
                }
                match config::save(&self.config) {
                    Ok(()) => {
                        // config::save() only logs keyring errors, so confirm the
                        // password can actually be read back. If not, the Secret
                        // Service isn't persisting it — tell the user how to fix it
                        // instead of silently "saving" an account that won't stay
                        // logged in. Queued after the save's own writes.
                        if let Some(secret) = expected_secret {
                            let (email, sender) = (new_email.clone(), sender.clone());
                            config::keyring_then(
                                move || {
                                    config::load_password(&email).as_deref()
                                        == Some(secret.as_str())
                                },
                                move |stored| {
                                    if !stored {
                                        sender.input(AppMsg::ShowKeyringHelp { problem: true });
                                    }
                                },
                            );
                        }
                        self.save_sidebar_state();
                        // Changed Special Folders assignments (#82) land on the
                        // live lists at once; the reconnect's fresh LIST then
                        // confirms them (and restores auto-detection for any
                        // role set back to Automatic).
                        for (i, cfg) in self.config.iter().enumerate() {
                            if let Some(folders) = self.folders.get_mut(&(i as u32 + 1)) {
                                apply_folder_roles(&cfg.folder_roles, folders);
                            }
                        }
                        self.rebuild_sidebar();
                        // A picture, an emoji or the Gravatar switch may have
                        // changed with this edit (#189).
                        self.warm_own_gravatars(&sender);
                        self.refresh_faces();
                        if !display_only {
                            self.reconnect_all(&sender);
                        }
                        if unified_changed {
                            self.reload_unified_views(open_unified, &sender);
                        }
                    }
                    Err(e) => self.notifications.emit(NotifyInput::Push {
                        text: i18n_f("Could not save account: {e}", &[("e", &(e).to_string())]),
                        error: true,
                        connectivity: false,
                    }),
                }
            }

            AppMsg::ShowKeyringHelp { problem } => self.show_keyring_help(problem),
            AppMsg::ReconnectAccounts => self.reconnect_all(&sender),
            AppMsg::ShowCarryOverNotice(from) => crate::ui::carry_over::show(&self.window, from),

            AppMsg::AccountEnabledChanged { email, enabled } => {
                if let Some(slot) = self.config.iter_mut().find(|c| c.email == email) {
                    if slot.enabled != enabled {
                        slot.enabled = enabled;
                        if let Err(e) = config::save(&self.config) {
                            self.notifications.emit(NotifyInput::Push {
                                text: i18n_f(
                                    "Could not save account: {e}",
                                    &[("e", &(e).to_string())],
                                ),
                                error: true,
                                connectivity: false,
                            });
                        }
                        // Respawn workers so the account starts/stops syncing.
                        self.reconnect_all(&sender);
                    }
                }
            }

            AppMsg::PresentWindow => {
                crate::desktop::present_window(&self.window);
            }

            AppMsg::ShowContactsPage => {
                crate::desktop::present_window(&self.window);
                sender.input(AppMsg::OpenContacts);
            }

            AppMsg::OpenConsole => {
                if self.console_mode {
                    self.notifications.emit(NotifyInput::ShowConsole);
                } else {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n("Enable Console mode in Settings → System & Appearance"),
                        error: false,
                        connectivity: false,
                    });
                }
            }

            AppMsg::SetFilters(rules) => {
                config::save_filters(&rules);
                let listed_before = self.unified_folder_keys();
                if rules.iter().any(|r| r.needs_contacts()) {
                    self.saved_addresses.refresh();
                }
                self.filters = rules;
                // A rule opting its folder in or out of All Inboxes changes
                // the sidebar's Filtered Folders section; nothing else
                // about the rules is drawn there.
                if self.unified_folder_keys() != listed_before {
                    self.rebuild_sidebar();
                }
                // File whatever already sits in the inboxes under the new
                // rules; reuses the blacklist's re-sync sweep.
                self.sweep_blacklisted();
                // The counted folders may have changed with the rules (#116):
                // the total follows at once, and a newly counted folder's
                // list is fetched for the tray menu.
                self.push_unread_counts();
                self.sync_unloaded_counted_folders();
            }

            AppMsg::ApplyFilters(targets) => {
                self.start_filter_run(targets, &sender);
            }

            AppMsg::FilterRunTimeout => {
                self.report_filter_run(true);
            }

            AppMsg::FilterRunToBackground => {
                self.filter_run_dismissed();
            }

            AppMsg::FolderSynced {
                account_id,
                folder_id,
            } => {
                self.filter_run_folder_synced(account_id, folder_id);
            }

            AppMsg::SetTags(tags) => {
                config::save_tags(&tags);
                self.tags = tags;
                // The unified Tags view spans every tag: what it holds is
                // stale once the set changes.
                self.tag_view_cache.retain(|(_, kw), _| kw.is_some());
                self.refresh_tag_css();
                self.message_list
                    .emit(MessageListInput::SetTags(self.tags.clone()));
                self.message_view
                    .emit(MessageViewInput::SetTags(self.tags.clone()));
                for p in self.popouts.values() {
                    p.controller
                        .emit(MessageWindowInput::SetTags(self.tags.clone()));
                }
                // The sidebar lists the tags; a removed one that was the open
                // view falls back to All Inboxes there (SetContents).
                self.rebuild_sidebar();
                // Rules may name a tag that now exists: tag what already sits
                // in the inboxes, through the same sweep the filters use.
                self.sweep_blacklisted();
            }

            AppMsg::TagSelected { keyword, account } => {
                self.close_sidebar_peek();
                self.mirror_selection(match keyword.clone() {
                    Some(k) => crate::ui::sidebar::Sel::Tag(account, k),
                    None => crate::ui::sidebar::Sel::UnifiedTags,
                });
                self.leave_gallery();
                self.showing_contacts = false;
                self.showing_outbox = false;
                self.unified = false;
                self.selected = None;
                self.smart_view = None;
                self.tag_view = Some((account, keyword));
                self.current = None;
                self.current_thread.clear();
                self.attachments.clear();
                self.attachments_loading = false;
                self.sync_attachment_drawer();
                self.show_message(None, false);
                self.message_list.emit(MessageListInput::SetSelected(None));
                self.message_list.emit(MessageListInput::ResetPaging);
                self.message_list
                    .emit(MessageListInput::SetScope(SearchScope::ThisFolder));
                self.message_list.emit(MessageListInput::SetColorize(true));
                self.message_list
                    .emit(MessageListInput::SetShowRecipient(false));
                self.message_list
                    .emit(MessageListInput::SetRestorable(false));
                self.message_list.emit(MessageListInput::SetInJunk(false));
                self.message_list.emit(MessageListInput::SetInDrafts(false));
                self.message_view
                    .emit(MessageViewInput::SetDraftsView(false));
                self.show_tag_view();
                self.refresh_tag_view(&sender);
                self.sync_tag_keywords();
                self.push_index_complete();
            }

            AppMsg::SmartSelected(id) => {
                if !self.smart_folders.iter().any(|f| f.id == id) {
                    return;
                }
                self.close_sidebar_peek();
                self.mirror_selection(crate::ui::sidebar::Sel::Smart(id.clone()));
                self.leave_gallery();
                self.showing_contacts = false;
                self.showing_outbox = false;
                self.unified = false;
                self.selected = None;
                self.tag_view = None;
                self.smart_view = Some(id);
                self.current = None;
                self.current_thread.clear();
                self.attachments.clear();
                self.attachments_loading = false;
                self.sync_attachment_drawer();
                self.show_message(None, false);
                self.message_list.emit(MessageListInput::SetSelected(None));
                self.message_list.emit(MessageListInput::ResetPaging);
                self.message_list
                    .emit(MessageListInput::SetScope(SearchScope::ThisFolder));
                self.message_list.emit(MessageListInput::SetColorize(true));
                // A folder held to Sent lists what you sent: name the
                // recipients, as Sent does.
                let sent = self
                    .open_smart_folder()
                    .is_some_and(|f| f.folder == Some(config::SmartFolderKind::Sent));
                self.message_list
                    .emit(MessageListInput::SetShowRecipient(sent));
                self.message_list
                    .emit(MessageListInput::SetRestorable(false));
                self.message_list.emit(MessageListInput::SetInJunk(false));
                self.message_list.emit(MessageListInput::SetInDrafts(false));
                self.message_view
                    .emit(MessageViewInput::SetDraftsView(false));
                self.show_smart_view();
                self.refresh_smart_view(&sender);
                self.push_index_complete();
            }

            AppMsg::SmartViewLoaded { id, rows } => {
                self.smart_view_loading = false;
                if let Some(folder) = self.smart_folders.iter().find(|f| f.id == id).cloned() {
                    let out = self.assemble_smart_view(&folder, rows);
                    let changed = self.smart_view_cache.get(&id) != Some(&out);
                    if changed {
                        self.smart_view_cache.insert(id.clone(), out.clone());
                    }
                    if self.smart_view.as_ref() == Some(&id) && changed {
                        self.message_list
                            .emit(MessageListInput::SetMessages { messages: out });
                    }
                }
                if std::mem::take(&mut self.smart_view_dirty) {
                    self.refresh_smart_view(&sender);
                }
            }

            AppMsg::RefreshUnifiedStarred => {
                self.unified_starred_due = false;
                self.refresh_unified_starred(&sender);
            }

            AppMsg::UnifiedStarredLoaded { rows } => {
                self.unified_starred_loading = false;
                self.unified_starred_rows.extend(rows);
                if self.fill_unified_starred() {
                    self.unified_dirty.set(true);
                    self.queue_unified_emit(&sender);
                }
                if std::mem::take(&mut self.unified_starred_dirty) {
                    self.refresh_unified_starred(&sender);
                }
            }

            AppMsg::CountSmartFolders => {
                self.smart_counts_due = false;
                self.count_smart_folders(&sender);
            }

            AppMsg::SmartCounts(counts) => {
                self.smart_counts_running = false;
                self.sidebars_emit(SidebarInput::SetSmartUnread(counts));
            }

            AppMsg::SetSmartFolders(folders) => {
                config::save_smart_folders(&folders);
                // What each one keeps may have changed with its conditions.
                self.smart_view_cache.clear();
                self.smart_folders = folders;
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::SetSmartFolders(
                        self.smart_folders.clone(),
                    ));
                }
                self.rebuild_sidebar();
                if self
                    .smart_view
                    .as_ref()
                    .is_some_and(|id| self.smart_folders.iter().any(|f| &f.id == id))
                {
                    self.show_smart_view();
                    self.refresh_smart_view(&sender);
                }
                self.schedule_smart_counts(&sender);
            }

            AppMsg::KeywordsSynced { account_id, paths } => {
                if paths.is_empty() {
                    return;
                }
                // The index changed under the open views: the tag view
                // re-reads it, and an open folder among the changed ones
                // re-syncs so its rows' chips follow.
                if self.tag_view.is_some() {
                    self.refresh_tag_view(&sender);
                }
                if self.smart_view.is_some() {
                    self.refresh_smart_view(&sender);
                }
                if let Some(sel) = self.selected.clone() {
                    if sel.account_id == account_id && paths.contains(&sel.path) {
                        self.send_to(
                            account_id,
                            MailRequest::LoadMessages {
                                folder_id: sel.folder_id,
                                path: sel.path,
                            },
                        );
                    }
                }
            }

            AppMsg::TagViewLoaded { key, rows } => {
                self.tag_view_loading = false;
                let out = self.assemble_tag_view(&key, rows);
                let changed = self.tag_view_cache.get(&key) != Some(&out);
                if changed {
                    self.tag_view_cache.insert(key.clone(), out.clone());
                }
                if self.tag_view.as_ref() == Some(&key) && changed {
                    self.message_list
                        .emit(MessageListInput::SetMessages { messages: out });
                }
                if std::mem::take(&mut self.tag_view_dirty) {
                    self.refresh_tag_view(&sender);
                }
            }

            AppMsg::SetTag {
                message,
                keyword,
                add,
            } => {
                let what = self.tag_label(&keyword, add);
                self.user_set_flags(what, &[*message], FlagKind::Tag(keyword), add);
            }

            AppMsg::SetTagMany {
                messages,
                keyword,
                add,
            } => {
                let what = self.tag_label(&keyword, add);
                self.user_set_flags(what, &messages, FlagKind::Tag(keyword), add);
            }

            AppMsg::MoveToInbox => {
                if self.many_selected() {
                    self.message_list
                        .emit(MessageListInput::Bulk(BulkAction::MoveToInbox));
                    return;
                }
                if let Some(m) = self.reply_target() {
                    self.move_to(m, FolderKind::Inbox);
                }
            }

            AppMsg::ToggleTagCurrent(keyword) => {
                let targets = self.selected_targets();
                self.toggle_tag(keyword, &targets);
            }

            AppMsg::TagKey { n, slots } => {
                // Settings order, first nine. A number past the list does
                // nothing rather than something surprising.
                let Some(keyword) = self
                    .tags
                    .get(usize::from(n).saturating_sub(1))
                    .map(|t| t.keyword.clone())
                else {
                    return;
                };
                // Arrowing quickly, the key can arrive before the app has
                // heard the list moved: the rows selected at the key press are
                // the ones meant, or the tag lands on the row before and the
                // next press takes it back off (#416).
                let keys: Vec<(u32, u32)> = slots.iter().map(|&(a, _, id)| (a, id)).collect();
                let targets =
                    if !keys.is_empty() && keys != self.list_heard && keys != self.list_selection {
                        self.rows_at(&slots)
                    } else {
                        self.selected_targets()
                    };
                self.toggle_tag(keyword, &targets);
            }

            AppMsg::ReaderTagMenu => {
                if let Some(entries) = self.reader_tag_entries(&sender) {
                    let btn = &self.reader_tag_btn;
                    crate::ui::context_menu::show_context_menu(
                        btn,
                        (btn.width() / 2) as f64,
                        btn.height() as f64,
                        vec![entries],
                    );
                }
            }

            AppMsg::MoveToMenu => {
                // A multi-selection lists the folders of its first message's
                // account (the move reports anything from another account);
                // otherwise the target's. The picker leaves out the folder
                // the mail already sits in.
                let (account_id, exclude) = if self.many_selected() {
                    (self.list_selection[0].0, None)
                } else {
                    match self.reply_target() {
                        Some(m) => (m.account_id, self.resolve_folder_path(&m)),
                        None => return,
                    }
                };
                let accounts = self.picker_accounts(account_id, exclude);
                if accounts.is_empty() {
                    return;
                }
                // The reading pane shows a conversation (its row was opened,
                // not one reply of it): offer to move the whole of it.
                let conversation = (!self.many_selected() && self.current_thread.len() > 1)
                    .then_some(self.current_thread.len());
                // Anchored to the toolbar button, or to the overflow button
                // when the toolbar has folded into it.
                let btn = if self.reader_move_btn.is_mapped() {
                    self.reader_move_btn.clone()
                } else {
                    self.reader_overflow_btn.clone()
                };
                let s = sender.input_sender().clone();
                crate::ui::folder_picker::show_folder_picker(
                    &btn,
                    (btn.width() / 2) as f64,
                    btn.height() as f64,
                    accounts,
                    conversation,
                    move |account_id, dest, whole| {
                        let _ = s.send(AppMsg::MoveSelectionTo {
                            account_id,
                            dest,
                            whole,
                        });
                    },
                );
            }

            AppMsg::CardMenu {
                message,
                x,
                y,
                hit,
                popout,
            } => {
                self.show_card_menu(*message, x, y, hit, popout, &sender);
            }

            AppMsg::ListMoveTo {
                messages,
                offer_whole,
                x,
                y,
            } => {
                let Some(first) = messages.first() else {
                    return;
                };
                let account_id = first.account_id;
                // Leave out the folder the mail sits in, when it is one.
                let exclude = self.resolve_folder_path(first).filter(|p| {
                    messages
                        .iter()
                        .all(|m| self.resolve_folder_path(m).as_ref() == Some(p))
                });
                let accounts = self.picker_accounts(account_id, exclude);
                if accounts.is_empty() {
                    return;
                }
                let conversation = offer_whole.then_some(messages.len());
                let s = sender.input_sender().clone();
                let window = self.window.clone();
                crate::ui::folder_picker::show_folder_picker(
                    &window,
                    x,
                    y,
                    accounts,
                    conversation,
                    move |account_id, dest, whole| {
                        let picked = if offer_whole && !whole {
                            messages[..1].to_vec()
                        } else {
                            messages.clone()
                        };
                        let _ = s.send(AppMsg::MoveMessagesTo {
                            account_id,
                            dest,
                            messages: picked,
                        });
                    },
                );
            }

            AppMsg::MoveMessagesTo {
                account_id,
                dest,
                messages,
            } => {
                let items: Vec<(u32, u32, u32, u32)> = messages
                    .iter()
                    .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
                    .collect();
                self.drop_move(account_id, dest, items);
            }

            AppMsg::MoveSelectionTo {
                account_id,
                dest,
                whole,
            } => {
                if whole && self.current_thread.len() > 1 {
                    // The whole conversation, the way a dragged selection
                    // moves: grouped by source folder, undoable, and any
                    // member from another account (a conversation merged
                    // across accounts in a unified view) reported.
                    let items: Vec<(u32, u32, u32, u32)> = self
                        .current_thread
                        .iter()
                        .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
                        .collect();
                    self.drop_move(account_id, dest, items);
                } else if self.rows_selected() {
                    // The list knows the conversations its rows stand for.
                    self.message_list
                        .emit(MessageListInput::MoveSelectedTo { account_id, dest });
                } else if self.many_selected() {
                    // The same path a drag onto the sidebar takes: grouped by
                    // source folder, undoable, foreign accounts reported.
                    let items: Vec<(u32, u32, u32, u32)> = self
                        .list_selection
                        .iter()
                        .filter_map(|(aid, id)| {
                            self.find_cached_message(*aid, *id)
                                .map(|m| (m.account_id, m.folder_id, m.uid, m.id))
                        })
                        .collect();
                    self.drop_move(account_id, dest, items);
                } else if let Some(m) = self.reply_target() {
                    if m.account_id == account_id {
                        self.move_to_path(m, dest);
                    } else {
                        self.drop_move(
                            account_id,
                            dest,
                            vec![(m.account_id, m.folder_id, m.uid, m.id)],
                        );
                    }
                }
            }

            AppMsg::OpenListSearch => {
                // Ctrl+F routes by focus (#102/#103): in the reader it finds
                // within the message, everywhere else it searches the list
                // shown, Contacts' included.
                if self.showing_contacts {
                    self.contacts_page.emit(ContactsPageInput::FocusSearch);
                } else if self.reader_has_focus() && self.current.is_some() {
                    self.message_view.emit(MessageViewInput::OpenFind);
                } else {
                    self.message_list.emit(MessageListInput::FocusSearch);
                }
            }

            AppMsg::OpenReaderFind => {
                self.message_view.emit(MessageViewInput::OpenFind);
            }

            AppMsg::SetQuickFilter(f, on) => self.set_quick_filters(self.quick_filters.with(f, on)),

            AppMsg::ClearQuickFilters => self.set_quick_filters(QuickFilters::default()),

            AppMsg::DeferredMarkRead { message } => {
                let still_current = self
                    .current
                    .as_ref()
                    .is_some_and(|c| c.id == message.id && c.account_id == message.account_id);
                if still_current {
                    self.mark_opened_read(&message);
                }
            }

            AppMsg::Pref(PrefOutput::SetReadMark(policy)) => {
                if pref!(self.read_mark = policy) {
                    self.message_view
                        .emit(MessageViewInput::SetReadMark(policy));
                }
            }

            AppMsg::Pref(PrefOutput::ExportSettings) => {
                let dialog = gtk::FileDialog::builder()
                    .title(&i18n("Export Settings"))
                    .initial_name("hylki-settings.toml")
                    .build();
                let win = self.window.clone();
                let notif = self.notifications.sender().clone();
                dialog.save(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let outcome = crate::config::export_bundle()
                        .and_then(|t| std::fs::write(&path, t).map_err(|e| e.to_string()));
                    let _ = notif.send(match outcome {
                        Ok(()) => NotifyInput::Push {
                            text: i18n_f(
                                "Settings exported to {display}",
                                &[("display", &(path.display()).to_string())],
                            ),
                            error: false,
                            connectivity: false,
                        },
                        Err(e) => NotifyInput::Push {
                            text: i18n_f("Export failed: {e}", &[("e", &(e).to_string())]),
                            error: true,
                            connectivity: false,
                        },
                    });
                });
            }

            AppMsg::Pref(PrefOutput::ExportLog) => {
                let dialog = gtk::FileDialog::builder()
                    .title(&i18n("Export Log"))
                    .initial_name(&format!(
                        "hylki-log-{}.txt",
                        chrono::Local::now().format("%Y%m%d-%H%M")
                    ))
                    .build();
                let win = self.window.clone();
                let notif = self.notifications.sender().clone();
                // Measured now, while the model is at hand: the chooser's
                // callback runs later, without it.
                let memory = self.memory_report();
                dialog.save(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                    let Ok(file) = res else { return };
                    let Some(path) = file.path() else { return };
                    let outcome = std::fs::write(&path, crate::console_log::export_text(&memory))
                        .map_err(|e| e.to_string());
                    let _ = notif.send(match outcome {
                        Ok(()) => NotifyInput::Push {
                            text: i18n_f(
                                "Log exported to {display}",
                                &[("display", &(path.display()).to_string())],
                            ),
                            error: false,
                            connectivity: false,
                        },
                        Err(e) => NotifyInput::Push {
                            text: i18n_f("Export failed: {e}", &[("e", &(e).to_string())]),
                            error: true,
                            connectivity: false,
                        },
                    });
                });
            }

            AppMsg::ExportLogTo(path) => {
                let text = crate::console_log::export_text(&self.memory_report());
                match std::fs::write(&path, text) {
                    Ok(()) => tracing::info!("log exported to {}", path.display()),
                    Err(e) => tracing::warn!("log export to {} failed: {e}", path.display()),
                }
            }

            AppMsg::Pref(PrefOutput::ImportSettings) => {
                let dialog = gtk::FileDialog::builder()
                    .title(&i18n("Import Settings"))
                    .build();
                let win = self.window.clone();
                let s = sender.clone();
                dialog.open(Some(&win), gtk::gio::Cancellable::NONE, move |res| {
                    // Only carry the choice out of the chooser's callback —
                    // the work (and any dialog) runs on a clean loop turn.
                    if let Ok(file) = res {
                        if let Some(path) = file.path() {
                            s.input(AppMsg::ImportSettingsFrom(path));
                        }
                    }
                });
            }

            AppMsg::ImportSettingsFrom(path) => {
                let outcome = std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| crate::config::import_bundle(&t));
                match outcome {
                    Ok(n) => {
                        // Imported files are on disk; the running model still
                        // holds the old world, so a clean restart is the
                        // honest way to apply everything at once.
                        // Parent on whichever window is focused (Settings,
                        // normally) — a modal behind the focused window reads
                        // as a freeze.
                        let parent = relm4::main_application()
                            .active_window()
                            .unwrap_or_else(|| self.window.clone().upcast());
                        let alert = adw::MessageDialog::new(
                            Some(&parent),
                            Some(i18n("Settings Imported").as_str()),
                            Some(&i18n_f(
                                "{n} mail account(s), the cloud storage accounts and all \
                                 preferences were imported. Restart Hylki to apply them. \
                                 Passwords and sign-ins are not part of a backup; re-enter \
                                 them on first use if this is a new machine.",
                                &[("n", &n.to_string())],
                            )),
                        );
                        alert.add_response("later", &i18n("Later"));
                        alert.add_response("restart", &i18n("Restart Hylki"));
                        alert
                            .set_response_appearance("restart", adw::ResponseAppearance::Suggested);
                        alert.connect_response(None, |_, resp| {
                            if resp == "restart" {
                                // A detached shell starts the next instance
                                // once this one has quit and released its
                                // D-Bus name (started immediately, the new
                                // instance would just relay to the dying
                                // primary and exit).
                                if let Ok(exe) = std::env::current_exe() {
                                    let _ = std::process::Command::new("sh")
                                        .arg("-c")
                                        .arg(format!("sleep 1.5; exec '{}'", exe.display()))
                                        .spawn();
                                }
                                relm4::main_application().quit();
                            }
                        });
                        alert.present();
                    }
                    Err(e) => {
                        self.notifications.emit(NotifyInput::Push {
                            text: i18n_f("Import failed: {e}", &[("e", &(e).to_string())]),
                            error: true,
                            connectivity: false,
                        });
                    }
                }
            }

            AppMsg::Pref(PrefOutput::SetStatusBarTop(top)) => {
                if pref!(self.status_bar_top = top) {
                    self.notifications.emit(NotifyInput::SetAtTop(top));
                }
            }

            AppMsg::Pref(PrefOutput::SetConsoleMode(on)) => {
                if pref!(self.console_mode = on) {
                    self.notifications.emit(NotifyInput::SetConsoleEnabled(on));
                    self.rebuild_help_menu();
                }
            }

            AppMsg::ApplyWelcomePrefs(p) => {
                config::mark_wizard_completed();
                // Route every choice through its normal handler (each saves and
                // updates the live UI); drop the wizard controller afterwards.
                sender.input(AppMsg::Pref(PrefOutput::SetAutoRemoteContent(
                    !p.block_remote,
                )));
                sender.input(AppMsg::Pref(PrefOutput::SetGravatar(p.gravatar)));
                sender.input(AppMsg::Pref(PrefOutput::SetSenderLogos(p.sender_logos)));
                sender.input(AppMsg::Pref(PrefOutput::SetNotificationContent(
                    p.notification_content,
                )));
                sender.input(AppMsg::Pref(PrefOutput::SetPreviewLines(p.preview_lines)));
                sender.input(AppMsg::Pref(PrefOutput::SetAvatars(p.avatars)));
                sender.input(AppMsg::Pref(PrefOutput::SetThreading(p.threading)));
                if !p.app_icon.is_empty() {
                    sender.input(AppMsg::Pref(PrefOutput::SetAppIcon(p.app_icon)));
                }
                self.welcome = None;
            }

            AppMsg::ImportGoaAccount(account) => {
                // Enable a GNOME Online Account in Hylki (or re-enable if already
                // imported). Its password came from GOA and is stored in the keyring.
                let email = account.email.clone();
                if let Some(slot) = self.config.iter_mut().find(|c| c.email == email) {
                    slot.enabled = true;
                    // Re-importing refreshes the GOA-owned connection details in
                    // place — e.g. a broken pre-#36 Microsoft 365 import (empty
                    // hosts, IMAP) repairs into a Graph account — keeping the
                    // slot (account ids are config indices) and the cosmetics.
                    if account.goa_id.is_some() {
                        slot.protocol = account.protocol;
                        slot.imap_host = account.imap_host.clone();
                        slot.imap_port = account.imap_port;
                        slot.smtp_host = account.smtp_host.clone();
                        slot.smtp_port = account.smtp_port;
                        slot.username = account.username.clone();
                        slot.smtp_separate = account.smtp_separate;
                        slot.smtp_username = account.smtp_username.clone();
                        slot.oauth = account.oauth;
                        slot.oauth_settings = account.oauth_settings.clone();
                        slot.oauth_refresh = account.oauth_refresh.clone();
                        slot.goa_id = account.goa_id.clone();
                    }
                } else {
                    self.config.push(*account);
                }
                match config::save(&self.config) {
                    Ok(()) => self.reconnect_all(&sender),
                    Err(e) => self.notifications.emit(NotifyInput::Push {
                        text: i18n_f("Could not import account: {e}", &[("e", &(e).to_string())]),
                        error: true,
                        connectivity: false,
                    }),
                }
            }

            AppMsg::AccountRemoved { email } => {
                // Drop the keyring entries while the config (which knows the
                // alias addresses) is still around.
                forget_account_secrets(std::slice::from_ref(&email), &self.config);
                self.config.retain(|c| c.email != email);
                self.account_order.retain(|e| *e != email);
                self.collapsed.retain(|e| *e != email);
                if let Err(e) = config::save(&self.config) {
                    tracing::error!("could not save config: {e}");
                }
                self.save_sidebar_state();
                self.reconnect_all(&sender);
            }

            AppMsg::GoaChanged(live) => {
                // An account added there shows in Settings' import list.
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::RefreshGoa);
                }
                // GOA changed in GNOME Settings. Drop any imported account that
                // no longer exists there; pause/resume any whose Mail service
                // was toggled. (Adding an account to GOA never auto-imports —
                // that stays a manual choice.)
                // Snapshot first: reconcile removes accounts from the config,
                // and the alias-password keyring entries are keyed by data
                // (the alias addresses) only the config holds.
                let before = self.config.clone();
                let outcome = reconcile_goa(&mut self.config, &live);
                if !outcome.removed.is_empty() {
                    forget_account_secrets(&outcome.removed, &before);
                    for email in &outcome.removed {
                        self.account_order.retain(|e| e != email);
                        self.collapsed.retain(|e| e != email);
                        self.folders_expanded.retain(|e| e != email);
                        self.filtered_expanded_accounts.retain(|e| e != email);
                        self.tags_expanded_accounts.retain(|e| e != email);
                    }
                    self.save_sidebar_state();
                }
                if !outcome.removed.is_empty() || outcome.paused_changed || outcome.servers_changed
                {
                    if let Err(e) = config::save(&self.config) {
                        tracing::error!("could not save config after GOA change: {e}");
                    }
                    self.reconnect_all(&sender);
                }
            }

            AppMsg::SystemResumed => {
                // The dock may have come back from the lock screen without
                // the badge; nothing else would send it again.
                self.resend_launcher_count();
                // logind and the clock check both report the same wake.
                let now = std::time::Instant::now();
                if self
                    .last_wake
                    .is_some_and(|t| now - t < std::time::Duration::from_secs(30))
                {
                    return;
                }
                self.last_wake = Some(now);
                // Realign the auto-fetch timer to now; its monotonic countdown
                // did not advance during sleep.
                self.arm_auto_fetch(&sender);
                // Still rejoining the Wi-Fi: a reconnect now would fail and
                // leave the account offline until the next auto-fetch.
                // NetworkBack does it once the network is up.
                if !crate::power::online() {
                    return;
                }
                self.reconnect_workers(&sender);
                // The network monitor can lag the wake by a moment and call a
                // dead network up; one more pass shortly after catches that.
                let input = sender.input_sender().clone();
                gtk::glib::timeout_add_seconds_local_once(20, move || {
                    let _ = input.send(AppMsg::Refresh);
                });
            }

            AppMsg::NetworkBack => {
                self.show_network_state();
                // Errors from while it was down say nothing about now.
                self.notifications.emit(NotifyInput::ClearConnectivity);
                self.reconnect_workers(&sender);
                // What waited in the Outbox goes now. An IMAP account sends
                // it as its session comes back, but JMAP, Graph and POP3 only
                // send when asked, so their mail sat there until a manual
                // retry (#442).
                let queued: Vec<u32> = self
                    .outbox_by_account
                    .iter()
                    .filter(|(_, items)| !items.is_empty())
                    .map(|(id, _)| *id)
                    .collect();
                for account_id in queued {
                    self.send_to(account_id, MailRequest::FlushOutbox { id: None });
                }
            }

            AppMsg::NetworkLost => self.show_network_state(),

            AppMsg::MeteredChanged => {
                // The flag has already flipped, so before it the connection
                // was the other way.
                let was = self.metered_save_data && !crate::power::metered();
                self.saving_data_changed(was);
            }

            AppMsg::LauncherCountLost => self.resend_launcher_count(),

            AppMsg::WindowInFront => {
                self.arrivals.clear();
                self.resend_launcher_count();
            }

            AppMsg::OpenSettings => {
                // The "opens to" preference decides the first open of the
                // session; after that the window returns to where it was.
                let on_accounts = self.settings_open_accounts && self.last_settings_page.is_none();
                self.open_settings_window(&sender, on_accounts, false);
            }

            AppMsg::SettingsPageShown(id) => {
                self.last_settings_page = Some(id);
            }

            AppMsg::OpenPreferences => self.open_settings_window(&sender, false, false),

            AppMsg::Pref(PrefOutput::SetSettingsOpenAccounts(on)) => {
                pref!(self.settings_open_accounts = on);
            }

            // Closing the combined Settings window hides it (the window's
            // own hide-on-close); both panels stay for the next open, which
            // then only rebuilds the accounts panel.
            AppMsg::ClosePreferences => {
                // The window only hides; let the signature editor's web
                // process go with it (#221).
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::ReleaseSignatureEditor);
                }
            }

            // Build the Settings window ahead of its first open, hidden and
            // realized, a moment after startup: its first appearance is
            // then a present() rather than a build, a layout and a paint.
            AppMsg::DebugCloseSettings => {
                if let Some(p) = &self.prefs {
                    p.widget().close();
                }
            }

            AppMsg::PrewarmSettings => {
                if self.prefs.is_none() {
                    self.build_settings_window(&sender, self.settings_open_accounts);
                    if let Some(p) = &self.prefs {
                        let w = p.widget();
                        WidgetExt::realize(w);
                        // Measuring runs the first CSS pass and Pango layouts
                        // now, so the first frame after present has less to do.
                        let _ = w.measure(gtk::Orientation::Horizontal, -1);
                        let _ = w.measure(gtk::Orientation::Vertical, 920);
                    }
                }
            }

            AppMsg::ShowcaseNewAccount(provider) => {
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::AddAccount);
                    if let Some(label) = provider {
                        acc.emit(crate::ui::accounts::AccountsInput::DebugProvider(label));
                    }
                }
            }
            AppMsg::ShowcaseDirtyEditor => {
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::DebugEditLabel(
                        "Edited in the capture".into(),
                    ));
                }
            }

            AppMsg::SettingsEditorOpen(open) => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::EditorOpen(open));
                }
            }

            AppMsg::SettingsLeaveEditor { page, ask } => {
                if let Some(p) = &self.prefs {
                    p.emit(if ask {
                        PrefInput::LeaveEditorPrompt(page)
                    } else {
                        PrefInput::LeaveEditorTo(page)
                    });
                }
            }

            AppMsg::ServerIdentities {
                account_id,
                identities,
            } => {
                if let (Some(email), Some(a)) = (self.email_of(account_id), &self.accounts_win) {
                    a.emit(crate::ui::accounts::AccountsInput::ServerIdentities {
                        email,
                        identities: identities.clone(),
                    });
                }
                self.server_identities.insert(account_id, identities);
            }
            AppMsg::WantIdentities(email) => {
                let Some(id) = self
                    .accounts
                    .iter()
                    .find(|a| a.email.eq_ignore_ascii_case(&email))
                    .map(|a| a.id)
                else {
                    return;
                };
                let identities = self.server_identities.get(&id).cloned().unwrap_or_default();
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::ServerIdentities {
                        email,
                        identities,
                    });
                }
            }

            AppMsg::SetAccount(account) => {
                if let Some(existing) = self.accounts.iter_mut().find(|a| a.id == account.id) {
                    *existing = account;
                } else {
                    // Layout not remembered: an account starts folded up,
                    // wherever it came from (config, GOA, the demo). Only
                    // on arrival, so opening it by hand sticks for the
                    // session.
                    if !self.remember_sidebar && !self.collapsed.contains(&account.email) {
                        self.collapsed.push(account.email.clone());
                    }
                    self.accounts.push(account);
                    self.accounts.sort_by_key(|a| a.id);
                }
                self.rebuild_sidebar();
            }

            AppMsg::SetFolders {
                account_id,
                folders,
            } => {
                self.notifications.emit(NotifyInput::ClearConnectivity);
                // Defence in depth against a wedged session's empty LIST (the
                // worker filters these too): never trade a real folder list
                // for nothing.
                if folders.is_empty()
                    && self
                        .folders
                        .get(&account_id)
                        .is_some_and(|old| !old.is_empty())
                {
                    return;
                }
                let mut folders = folders;
                // The first listing of an account is looked over once for
                // Exchange's non-mail folders (#239), which are hidden the
                // way any folder is: the account editor lists them, and can
                // bring any of them back. Noted as done either way, so a
                // folder brought back stays back.
                let seed = self
                    .config
                    .get(account_id as usize - 1)
                    .filter(|cfg| !cfg.folders_seeded)
                    .map(|_| crate::models::exchange_non_mail_folders(&folders));
                if let Some(seed) = seed {
                    if let Some(cfg) = self.config.get_mut(account_id as usize - 1) {
                        cfg.folders_seeded = true;
                        if let Err(e) = config::save(&self.config) {
                            tracing::warn!("could not note the folder look-over: {e}");
                        }
                    }
                    if !seed.is_empty() {
                        tracing::info!(
                            "account {account_id}: hiding Exchange's non-mail folders {seed:?}"
                        );
                        self.hide_folders(account_id, seed);
                    }
                }
                // A listing from before a hide (the cache at startup, an
                // answer already on its way) still carries what is hidden.
                if let Some(cfg) = self.config.get(account_id as usize - 1) {
                    if !cfg.hidden_folders.is_empty() {
                        let hidden = cfg.hidden_folders.clone();
                        folders
                            .retain(|f| !crate::models::folder_is_hidden(&f.path, None, &hidden));
                    }
                }
                // Manual special-folder assignments (#82) ride over whatever
                // the worker detected.
                if let Some(cfg) = self.config.get(account_id as usize - 1) {
                    apply_folder_roles(&cfg.folder_roles.clone(), &mut folders);
                }
                // After the look-over above, which goes by the English names.
                crate::models::localize_special_folder_names(&mut folders);
                // Keep the settings editor's folder choices current while open.
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::SetFolderChoices(
                        self.folder_choice_map_with(account_id, &folders),
                    ));
                }
                let unread = merged_unread(
                    account_id,
                    self.folders.get(&account_id).map(Vec::as_slice),
                    &self.folder_unread,
                    &folders,
                );
                self.folder_unread.retain(|(a, _), _| *a != account_id);
                for (folder_id, n) in unread {
                    self.folder_unread.insert((account_id, folder_id), n);
                }
                // An identical list — above all the refresh confirming an
                // optimistic folder move, whose local reshape mirrors the
                // worker exactly — changes nothing on screen: skip the rebuild
                // so the sidebar's rows don't vanish and reappear for it.
                let unchanged = self
                    .folders
                    .get(&account_id)
                    .is_some_and(|old| same_folder_list(old, &folders));
                // Ids are list positions, so a folder another client added,
                // renamed or removed shifts them (a refresh re-lists the
                // folders). The open folder and the cached lists follow their
                // folder by path; a list whose folder is gone is dropped.
                // The first listing has nothing to follow from.
                if !unchanged
                    && self
                        .folders
                        .get(&account_id)
                        .is_some_and(|old| !old.is_empty())
                {
                    let old_paths: HashMap<u32, String> = self
                        .folders
                        .get(&account_id)
                        .map(|old| old.iter().map(|f| (f.id, f.path.clone())).collect())
                        .unwrap_or_default();
                    let new_ids: HashMap<&str, u32> =
                        folders.iter().map(|f| (f.path.as_str(), f.id)).collect();
                    let cached = std::mem::take(&mut self.message_cache);
                    for ((a, fid), mut list) in cached {
                        if a != account_id {
                            self.message_cache.insert((a, fid), list);
                            continue;
                        }
                        let Some(&id) = old_paths.get(&fid).and_then(|p| new_ids.get(p.as_str()))
                        else {
                            continue;
                        };
                        for m in &mut list {
                            m.folder_id = id;
                        }
                        self.message_cache.insert((a, id), list);
                    }
                    if let Some(sel) = self
                        .selected
                        .as_mut()
                        .filter(|s| s.account_id == account_id)
                    {
                        if let Some(&id) = new_ids.get(sel.path.as_str()) {
                            sel.folder_id = id;
                        }
                    }
                }
                self.folders.insert(account_id, folders);
                self.push_template_folders();
                if unchanged {
                    self.push_unread_counts();
                } else {
                    self.rebuild_sidebar();
                }
                // Launch with "All Inboxes" already selected: UnifiedSelected
                // fired before any folder list existed, so its inbox requests
                // went nowhere and the list sat empty until the user visited a
                // folder by hand. The first time an account's folders arrive
                // while the unified view is up, ask for its inbox — the cache
                // priming already painted its slice, and this is what brings
                // in whatever changed since the app last ran.
                if self.unified && self.unified_boot_requested.insert(account_id) {
                    let mine: Vec<(u32, u32, String)> = self
                        .unified_targets()
                        .into_iter()
                        .filter(|(a, _, _)| *a == account_id)
                        .collect();
                    for (_, folder_id, path) in mine {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                }
                self.schedule_preload(&sender);
                if self.flag_starred_accounts().contains(&account_id) {
                    self.refresh_unified_starred(&sender);
                }
                self.index_sent_folders(account_id);
                // A unified view merges one folder per account, and this
                // account's folders may only now have arrived — so the set the
                // completeness flag is measured over just changed. It is
                // otherwise recomputed on `BackfillDone` alone, which meant an
                // account joining after every other one had finished left the
                // list claiming a complete index, or (once its own load
                // landed) spinning with no event left to clear it (#218).
                self.push_index_complete();
            }

            AppMsg::FolderUnread {
                account_id,
                folder_id,
                unread,
            } => {
                // A count fetched ahead of a read mark still in the worker's
                // queue: the app's own count (adjusted when the mark was
                // made) stands until the mark is stored, and the server is
                // asked again once it is (#333).
                if self.pending_seen_in_folder(account_id, folder_id) {
                    self.dropped_unread.insert((account_id, folder_id));
                    return;
                }
                // Nothing unread left where the notification points: it is
                // answered, wherever the mail was read (#333).
                if unread == 0
                    && crate::notify::posted_for(account_id).is_some_and(|(f, _)| f == folder_id)
                {
                    crate::notify::withdraw_mail(account_id);
                }
                // Likewise while mail is being taken out of the folder: each
                // move changes the server's count before the app hears the
                // move is done, and every change started another sync of the
                // folder, which put the mail back on the list (#255).
                if self
                    .folder_path(account_id, folder_id)
                    .is_some_and(|p| self.pending_moves_in(account_id, &p))
                {
                    self.dropped_unread.insert((account_id, folder_id));
                    return;
                }
                let prev = self.folder_unread.insert((account_id, folder_id), unread);
                if prev != Some(unread) {
                    self.sync_background_folder(account_id, folder_id);
                }
                self.push_unread_counts();
            }

            AppMsg::SeenSettled {
                account_id,
                path,
                uid,
            } => {
                self.pending_seen.remove(&(account_id, path, uid));
                self.pending_seen
                    .retain(|_, (_, at)| at.elapsed() < PENDING_SEEN_MAX);
                // Counts the server sent while the marks were on their way
                // were set aside; with the last one stored, ask for fresh
                // ones, or the badge keeps the app's guess until the next
                // change (#333).
                if !self.pending_seen.keys().any(|(a, _, _)| *a == account_id) {
                    self.resync_dropped_unread(account_id);
                }
            }

            AppMsg::Answered {
                account_id,
                path,
                uid,
                forwarded,
            } => {
                let fid = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.path == path))
                    .map(|f| f.id);
                if let Some(fid) = fid {
                    self.show_answered(account_id, fid, uid, forwarded, true);
                }
            }

            AppMsg::TagSettled {
                account_id,
                path,
                uid,
                keyword,
            } => {
                let soon = std::time::Instant::now() + PENDING_TAG_SETTLED;
                if let Some((_, until, _)) =
                    self.pending_tags
                        .get_mut(&(account_id, path, uid, keyword.to_lowercase()))
                {
                    *until = (*until).min(soon);
                }
            }

            AppMsg::RawExported { token, raw } => {
                if let Some(name) = self.pending_saves.remove(&token) {
                    match raw {
                        Ok(raw) => self.save_message_file(name, raw),
                        Err(e) => self.notifications.emit(NotifyInput::Push {
                            text: i18n_f("Could not save the message: {e}", &[("e", &e)]),
                            error: true,
                            connectivity: false,
                        }),
                    }
                    return;
                }
                if self.pending_draft_token == Some(token) {
                    self.pending_draft_token = None;
                    let Some((m, inline, extra)) = self.pending_draft.take() else {
                        return;
                    };
                    match raw {
                        Ok(raw) => self.compose_from_draft_raw(m, &raw, inline, extra, &sender),
                        // Offline, say: the body the reader has is still
                        // better than nothing.
                        Err(e) => {
                            tracing::warn!("draft {} could not be fetched whole: {e}", m.uid);
                            self.open_draft_body(m, inline, extra, &sender);
                        }
                    }
                    return;
                }
                let Some(t) = self.transfers.get(&token) else {
                    return;
                };
                match raw {
                    Ok(raw) => self.send_to(
                        t.dest_account,
                        MailRequest::ImportRaw {
                            token,
                            path: t.dest_path.clone(),
                            raw,
                            seen: t.seen,
                            flagged: t.flagged,
                        },
                    ),
                    Err(e) => self.transfer_failed(token, e),
                }
            }

            AppMsg::RawImported { token, result } => match result {
                Ok(()) => self.transfer_landed(token),
                Err(e) => self.transfer_failed(token, e),
            },

            AppMsg::MovesSettled {
                account_id,
                path,
                uids,
            } => {
                let mut pending = self.pending_moves.borrow_mut();
                for uid in uids {
                    pending.remove(&(account_id, path.clone(), uid));
                }
                pending.retain(|_, at| at.elapsed() < PENDING_MOVE_MAX);
                drop(pending);
                if !self.pending_moves_in(account_id, &path)
                    && !self.pending_seen_in(account_id, &path)
                {
                    self.resync_dropped_unread(account_id);
                }
            }

            AppMsg::FolderUnreadByPath {
                account_id,
                path,
                unread,
            } => {
                // Resolve against the current list; a path the app no longer
                // knows (folder deleted/renamed under a live watcher) is
                // dropped rather than guessed at.
                let id = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.path == path))
                    .map(|f| f.id);
                if self.pending_seen_in(account_id, &path)
                    || self.pending_moves_in(account_id, &path)
                {
                    if let Some(folder_id) = id {
                        self.dropped_unread.insert((account_id, folder_id));
                    }
                    return;
                }
                if let Some(folder_id) = id {
                    if unread == 0
                        && crate::notify::posted_for(account_id)
                            .is_some_and(|(f, _)| f == folder_id)
                    {
                        crate::notify::withdraw_mail(account_id);
                    }
                    let prev = self.folder_unread.insert((account_id, folder_id), unread);
                    if prev != Some(unread) {
                        self.sync_background_folder(account_id, folder_id);
                    }
                    self.push_unread_counts();
                }
            }

            AppMsg::ShowcaseEditFilter(i) => {
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::EditFilter(i));
                }
            }

            AppMsg::BodyHits {
                account_id,
                folder_id,
                hits,
            } => {
                // The whole answer for the sync about to land; the previous
                // one described mail that sync lists again anyway.
                self.body_hits.insert((account_id, folder_id), hits);
            }

            AppMsg::Messages {
                account_id,
                folder_id,
                messages,
            } => {
                self.notifications.emit(NotifyInput::ClearConnectivity);
                let messages = self.merge_local_tags(account_id, messages);
                // Mail already moved or deleted here, in a list fetched before
                // the worker got to the move.
                let messages = self.drop_pending_moves(account_id, folder_id, messages);
                // Auto-delete blacklisted senders from the inbox before anything
                // else sees them.
                let messages = self.apply_blacklist(account_id, folder_id, messages);
                let (messages, filed) = self.apply_filters(account_id, folder_id, messages);
                // A list fetched ahead of a read mark still in the worker's
                // queue shows the message unread again; keep the app's state.
                let messages = self.apply_pending_seen(account_id, folder_id, messages);
                let messages = self.apply_pending_answers(account_id, folder_id, messages);
                let messages = self.apply_pending_tags(account_id, folder_id, messages);
                // A message a move has just brought home arrives with a new
                // UID — that is what a move does — which leaves its body filed
                // under the id the old one became. Move the body across before
                // anything asks for it (#200).
                if !self.carried_bodies.is_empty() {
                    for m in messages.iter().filter(|m| !m.message_id.is_empty()) {
                        if let Some(body) = self
                            .carried_bodies
                            .remove(&(account_id, m.message_id.clone()))
                        {
                            self.body_cache.insert(body_key(m), body);
                        }
                    }
                }
                // The reader may still be holding the copy from before the
                // move. Point it at the row that is really there now: every
                // action on it addresses a UID, and the check just below would
                // otherwise read it as a message deleted from under us.
                let renumbered: HashMap<&str, (u32, u32)> = messages
                    .iter()
                    .filter(|m| !m.message_id.is_empty())
                    .map(|m| (m.message_id.as_str(), (m.uid, m.id)))
                    .collect();
                let renumber = |m: &mut Message| {
                    if m.account_id != account_id || m.folder_id != folder_id {
                        return;
                    }
                    if let Some(&(uid, id)) = renumbered.get(m.message_id.as_str()) {
                        m.uid = uid;
                        m.id = id;
                    }
                };
                if let Some(cur) = self.current.as_mut() {
                    renumber(cur);
                }
                for tm in self.current_thread.iter_mut() {
                    renumber(tm);
                }
                // Did this sync remove the message currently open in the reader
                // (deleted/moved on another device)? Scope the check to the reader's
                // own folder so a folder switch or another folder's sync doesn't
                // count. Capture where it sat so we can advance to that slot.
                let vanished = self.current.as_ref().is_some_and(|c| {
                    c.account_id == account_id
                        && c.folder_id == folder_id
                        && !messages.iter().any(|m| m.uid == c.uid)
                });

                let next_after_vanish = if vanished {
                    let cur_uid = self.current.as_ref().unwrap().uid;
                    next_after_vanish(
                        self.message_cache
                            .get(&(account_id, folder_id))
                            .map(|c| &**c),
                        &messages,
                        cur_uid,
                    )
                } else {
                    None
                };
                // Desktop-notify for genuinely new inbox mail. Only when Hylki
                // isn't the active window (no point notifying about mail you're
                // watching arrive), only for the Inbox, and never on the first load
                // of a folder (no prior cache) — that would fire for every existing
                // message on startup. "New" = unread and not in the previous sync.
                // Mail a filter just filed elsewhere still counts (#47 feedback):
                // it is new mail even though it never lands in the inbox list.
                // With the window in front, the sound alone, when Settings asks
                // for it there (#337): see announce_new_mail.
                if self.notifications_enabled
                    && (!self.window.is_active() || crate::config::sound_when_focused())
                {
                    let is_inbox =
                        self.folder_kind(account_id, folder_id) == Some(FolderKind::Inbox);
                    if let (true, Some(old)) =
                        (is_inbox, self.message_cache.get(&(account_id, folder_id)))
                    {
                        let old_uids: std::collections::HashSet<u32> =
                            old.iter().map(|m| m.uid).collect();
                        let fresh: Vec<&Message> = messages
                            .iter()
                            .filter(|m| m.unread && !old_uids.contains(&m.uid))
                            .collect();
                        let fresh_filed: Vec<&(Message, String)> = filed
                            .iter()
                            .filter(|(m, _)| m.unread && !old_uids.contains(&m.uid))
                            .collect();
                        let others = (fresh.len() + fresh_filed.len()).saturating_sub(1);
                        let newest = fresh.iter().max_by_key(|m| m.timestamp);
                        let newest_filed = fresh_filed.iter().max_by_key(|(m, _)| m.timestamp);
                        match (newest, newest_filed) {
                            // The newest arrival is still in the inbox: anchor
                            // there. Ties go to the inbox copy — its click
                            // target and action buttons actually work.
                            (Some(m), nf) if nf.is_none_or(|(f, _)| f.timestamp <= m.timestamp) => {
                                self.announce_new_mail(
                                    account_id,
                                    folder_id,
                                    m.id,
                                    &m.from_name,
                                    &m.subject,
                                    others,
                                    true,
                                );
                            }
                            // The newest arrival was filed by a filter: anchor
                            // the notification on its destination folder so a
                            // click lands where the mail went. Its buttons stay
                            // off — the message is no longer where Mark as
                            // Read/Archive would look for it.
                            (_, Some((m, dest_path))) => {
                                let dest_id = self
                                    .folders
                                    .get(&account_id)
                                    .and_then(|fs| fs.iter().find(|f| &f.path == dest_path))
                                    .map_or(folder_id, |f| f.id);
                                self.announce_new_mail(
                                    account_id,
                                    dest_id,
                                    m.id,
                                    &m.from_name,
                                    &m.subject,
                                    others,
                                    false,
                                );
                            }
                            _ => {}
                        }
                    }
                }
                // Cache for instant display when revisiting this folder.
                // The worker answers a load from its cache first and from the
                // server after, and the view was seeded from the same cache
                // when it opened — so most arrivals change nothing. Only a
                // changed list is worth re-threading and re-emitting: a
                // rebuild of a large (or merged) list is not free, and it
                // used to happen several times over per visit.
                let unchanged = self
                    .message_cache
                    .get(&(account_id, folder_id))
                    .map(|c| &**c)
                    == Some(&messages);
                // What the badge counts when it counts arrivals (#431): the
                // same new inbox mail as the notification above, whatever
                // the notification settings, and only what is still in the
                // inbox, as the badge's other choice counts.
                if !self.window.is_active()
                    && self.inbox_of(account_id).is_some_and(|f| f.id == folder_id)
                {
                    let old: Option<HashSet<u32>> = self
                        .message_cache
                        .get(&(account_id, folder_id))
                        .map(|old| old.iter().map(|m| m.uid).collect());
                    self.arrivals.note(
                        account_id,
                        folder_id,
                        old.as_ref(),
                        messages.iter().map(|m| (m.uid, m.unread)),
                    );
                }
                // The notified mail was read somewhere else, on the phone or
                // in another client: the notification goes too (#333).
                if let Some((f, mid)) = crate::notify::posted_for(account_id) {
                    if f == folder_id && messages.iter().any(|m| m.id == mid && !m.unread) {
                        crate::notify::withdraw_mail(account_id);
                    }
                }
                // An Archive answers each visit with the same tens of
                // thousands of messages: copying them over themselves only
                // churned the heap (#504).
                if !unchanged {
                    // Nor is a large folder off screen taken in: the worker
                    // lists a folder again after every move into it.
                    let key = (account_id, folder_id);
                    let on_show = self.folders_on_show();
                    if self.message_cache.contains_key(&key)
                        || !self.too_large_off_screen(key, messages.len(), &on_show)
                    {
                        self.message_cache.insert(key, messages.clone().into());
                    }
                    self.shed_large_folders();
                }
                // A draft picker waiting on this folder's list (Send with
                // Hylki → Continue a Draft…) opens once every Drafts folder
                // has answered.
                if let Some((_, waiting)) = self.pending_draft_pick.as_mut() {
                    waiting.remove(&(account_id, folder_id));
                    if waiting.is_empty() {
                        sender.input(AppMsg::HandOffDraftPickNow);
                    }
                }
                if let Some(waiting) = self.pending_template_pick.as_mut() {
                    waiting.remove(&(account_id, folder_id));
                    if waiting.is_empty() {
                        sender.input(AppMsg::TemplatePickNow);
                    }
                }
                if !unchanged {
                    // A sync can add a reply to a conversation already
                    // assembled, so what was stored is no longer necessarily
                    // the conversation.
                    self.forget_threads(account_id);
                    // In a unified view, with the next merge, once for a run
                    // of answers; a folder's own list takes them now, in the
                    // same pass as its messages.
                    if self.unified {
                        self.thread_links_due.set(true);
                        self.queue_unified_emit(&sender);
                    } else {
                        self.push_thread_links();
                    }
                    // A sync that brought in a reply also changed how big the
                    // conversations are; the badges have to be counted again
                    // rather than kept from before it (#222).
                    self.message_list
                        .emit(MessageListInput::RecheckThreadSummaries(account_id));
                }
                // After that sweep, not before it: a conversation carried over
                // a move (#200) goes back under the id the message now has,
                // with the member that moved renumbered to match. The reader
                // paints a remembered conversation with no lookup and no
                // spinner, which is the whole point of carrying it.
                if !self.carried_threads.is_empty() {
                    for m in messages.iter().filter(|m| !m.message_id.is_empty()) {
                        let Some(mut thread) = self
                            .carried_threads
                            .remove(&(account_id, m.message_id.clone()))
                        else {
                            continue;
                        };
                        for tm in thread.iter_mut() {
                            if tm.account_id == account_id && tm.message_id == m.message_id {
                                tm.uid = m.uid;
                                tm.id = m.id;
                                tm.folder_id = m.folder_id;
                            }
                        }
                        tracing::debug!(
                            target: "hylki::undo",
                            "re-filed conversation under id {}",
                            m.id,
                        );
                        self.remember_thread_for(body_key(m), thread);
                    }
                }
                if self.unified {
                    // Accept only the folders the view merges, by recency.
                    if self.is_unified_target(account_id, folder_id) {
                        let key = (account_id, folder_id);
                        let through = self.unified_reads_through();
                        let same_slice = unchanged
                            && if through {
                                self.unified_unfilled.contains(&key)
                            } else {
                                self.unified_slices
                                    .get(&key)
                                    .is_some_and(|s| *s == messages)
                            };
                        if !same_slice {
                            if through {
                                self.unified_slices.remove(&key);
                                self.unified_unfilled.insert(key);
                            } else {
                                self.unified_unfilled.remove(&key);
                                self.unified_slices.insert(key, messages);
                            }
                            // Accounts answer one after another, a large or
                            // slow one in several goes: one merge for the lot
                            // that lands together, not one each.
                            self.unified_dirty.set(true);
                            self.queue_unified_emit(&sender);
                        }
                    }
                } else if let Some(sel) = self.selected.as_ref() {
                    if sel.account_id == account_id && sel.folder_id == folder_id && !unchanged {
                        self.message_list
                            .emit(MessageListInput::SetMessages { messages });
                    }
                } else if self.tag_view.is_some() {
                    // The index just took this folder's flags: re-read the
                    // tag, off the main thread.
                    self.refresh_tag_view(&sender);
                } else if self.smart_view.is_some() {
                    self.refresh_smart_view(&sender);
                }
                if !unchanged && self.flag_starred_accounts().contains(&account_id) {
                    // The index just took this folder's flags: read them
                    // again now for the view on show, else once a sync's
                    // burst of folders has passed.
                    if self.starred_by_flag().is_empty() {
                        self.refresh_unified_starred_soon(&sender);
                    } else {
                        self.refresh_unified_starred(&sender);
                    }
                }
                self.schedule_smart_counts(&sender);
                // The reader's message was removed by this sync (deleted/moved on
                // another device). Clear it right away so nothing stale lingers, then
                // advance to whatever now sits in its place if there is one — the
                // SelectAndLoad runs after SetMessages, so the target row exists.
                if vanished {
                    sender.input(AppMsg::ClearReader);
                    if let Some(key) = next_after_vanish {
                        self.message_list.emit(MessageListInput::SelectAndLoad(key));
                    }
                }
                // Refresh unread badges with the freshly-synced counts.
                self.push_unread_counts();
            }

            AppMsg::UndoRestored {
                account_id,
                folder_id,
                message_ids,
            } => {
                // The undone move landed and the folder's reload has already
                // been processed (the worker sends Messages first): put the
                // user on the restored message — selected, loaded in the
                // reader, and scrolled into view — instead of wherever the
                // reload left the list. If the restored message isn't in the
                // current view (folder switched meanwhile), SelectAndLoad
                // finds no row and nothing moves.
                // Unless the reader is already on it, which it will be when
                // the undo put the row back itself (#200): selecting it again
                // would tear the message down and render it a second time for
                // no change the reader can see.
                let already_open = self.current.as_ref().is_some_and(|c| {
                    !c.message_id.is_empty() && message_ids.contains(&c.message_id)
                });
                if already_open {
                    return;
                }
                let restored = self
                    .message_cache
                    .get(&(account_id, folder_id))
                    .and_then(|msgs| msgs.iter().find(|m| message_ids.contains(&m.message_id)))
                    .map(|m| (m.account_id, m.id));
                if let Some(key) = restored {
                    self.message_list.emit(MessageListInput::SelectAndLoad(key));
                }
            }

            AppMsg::MessagesAppend {
                account_id,
                folder_id,
                messages,
            } => {
                // Background backfill: grow the folder's search index without
                // disturbing the current view (no title/query reset).
                let messages = self.merge_local_tags(account_id, messages);
                let messages = self.drop_pending_moves(account_id, folder_id, messages);
                let messages = self.apply_blacklist(account_id, folder_id, messages);
                // Only onto a folder listed whole already. An entry started
                // here would hold just the newest arrivals, and a search or
                // a unified view would take it for the folder (#472); one
                // not open has its rows on disk, where the worker put them.
                let Some(entry) = self.message_cache.get_mut(&(account_id, folder_id)) else {
                    return;
                };
                let existing: std::collections::HashSet<u32> =
                    entry.iter().map(|m| m.uid).collect();
                let fresh: Vec<Message> = messages
                    .into_iter()
                    .filter(|m| !existing.contains(&m.uid))
                    .collect();
                if fresh.is_empty() {
                    return;
                }
                entry.extend(fresh.iter().cloned());
                // Feed the visible list so search covers the new messages live.
                if self.unified {
                    if self.is_unified_target(account_id, folder_id) {
                        // A folder read where it is has them already.
                        if !self.unified_unfilled.contains(&(account_id, folder_id)) {
                            self.unified_slices
                                .entry((account_id, folder_id))
                                .or_default()
                                .extend(fresh.iter().cloned());
                        }
                        self.message_list
                            .emit(MessageListInput::AppendMessages { messages: fresh });
                    }
                } else if let Some(sel) = self.selected.as_ref() {
                    if sel.account_id == account_id && sel.folder_id == folder_id {
                        self.message_list
                            .emit(MessageListInput::AppendMessages { messages: fresh });
                    }
                }
            }

            AppMsg::BackfillDone {
                account_id,
                folder_id,
            } => {
                self.indexed_folders.insert((account_id, folder_id));
                self.push_index_complete();
            }

            AppMsg::SenderChecked {
                account_id,
                path,
                message_id,
                check,
            } => {
                // Remember it: prefetch delivers the verdict long before the
                // message is opened, and opening it renders from the in-memory
                // body cache without asking the worker for anything.
                let key = self.event_key(account_id, &path, message_id);
                if let Some(k) = key {
                    self.sender_cache.insert(k, check.clone());
                }
                let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
                // Only the message actually on screen; a verdict that arrives
                // from a background prefetch must not relabel a different one.
                if self.current.as_ref().is_some_and(is_target) {
                    self.message_view
                        .emit(MessageViewInput::SetSenderCheck(check.clone()));
                }
                // Light the header seal on whichever on-screen card this
                // verdict belongs to (#88) — the open single message included
                // (it never fills current_thread). The card goes by the id
                // it has here, which for a member from another folder is not
                // its UID.
                let card = self
                    .current_thread
                    .iter()
                    .chain(self.current.iter())
                    .find(|m| is_target(m))
                    .map(|m| m.id);
                if let Some(id) = card {
                    self.message_view.emit(MessageViewInput::SenderCheckFor {
                        account_id,
                        id,
                        check: check.clone(),
                    });
                }
                for p in self.popouts.values().filter(|p| key == Some(p.message)) {
                    p.controller
                        .emit(MessageWindowInput::SetSenderCheck(check.clone()));
                }
            }

            AppMsg::Body {
                account_id,
                message_id,
                path,
                body,
            } => {
                // Interior NULs abort glib string conversion (labels and the
                // WebView document alike); decoded mail bodies can carry them.
                let body = if body.contains('\0') {
                    body.replace('\0', " ")
                } else {
                    body
                };
                // A UID is unique only within its folder, and the background
                // prefetch pushes bodies from every folder it syncs, so the
                // body is filed by folder and UID. A folder the app does not
                // know yet has no key; the body is still on disk for when it
                // does.
                let key = self.event_key(account_id, &path, message_id);
                if let Some(k) = key {
                    self.body_cache.insert(k, body.clone());
                }
                // If this body was fetched to open a draft, open the editor now.
                // One waiting for its bytes ignores a body the prefetch sent.
                if let Some((pd, inline, extra)) = self
                    .pending_draft
                    .take_if(|_| self.pending_draft_token.is_none())
                {
                    if pd.account_id == account_id && pd.id == message_id {
                        self.compose_from_draft(pd, body, inline, extra, &sender);
                        return;
                    }
                    self.pending_draft = Some((pd, inline, extra));
                }
                // Likewise a copy being edited as a new message: the body is
                // in the cache now, so the second pass finds it there.
                if let Some(pending) = self.pending_edit_as_new.take() {
                    if pending.account_id == account_id && pending.id == message_id {
                        self.edit_as_new(pending, &sender);
                        return;
                    }
                    self.pending_edit_as_new = Some(pending);
                }
                // And a forward waiting on the body it quotes.
                if let Some((pending, inline)) = self.pending_forward.take() {
                    if pending.account_id == account_id && pending.id == message_id {
                        self.forward(pending, inline, &sender);
                        return;
                    }
                    self.pending_forward = Some((pending, inline));
                }
                // A reply from a notification's button (#244): the message
                // is opening in the reader too, so the body goes on to be
                // shown as usual.
                if let Some(mut m) = self.pending_notified_reply.take() {
                    if m.account_id == account_id && m.id == message_id {
                        m.body = body.clone();
                        self.open_reply(
                            m.account_id,
                            self.reply_context(&m, reply_prefill(&m)),
                            (m.account_id, m.id),
                            &sender,
                        );
                    } else {
                        self.pending_notified_reply = Some(m);
                    }
                }
                // Likewise a reply picked for handed-in files.
                if let Some((mut m, extra)) = self.pending_reply.take() {
                    if m.account_id == account_id && m.id == message_id {
                        m.body = body.clone();
                        self.open_hand_off_reply(m, extra, &sender);
                        return;
                    }
                    self.pending_reply = Some((m, extra));
                }
                let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
                // Keep the primary's body up to date in either mode.
                if let Some(current) = self.current.as_mut() {
                    if is_target(current) {
                        current.body = body.clone();
                    }
                }
                if self.current_thread.len() > 1 {
                    // Conversation mode: fill the matching message's body. The
                    // whole conversation renders as one document holding every
                    // body, so a burst of arrivals is coalesced into a single
                    // re-render rather than one per body.
                    let mut changed = false;
                    for tm in self.current_thread.iter_mut() {
                        if is_target(tm) {
                            tm.body = body.clone();
                            changed = true;
                        }
                    }
                    if changed {
                        self.queue_thread_render(&sender);
                    }
                } else if self.current.as_ref().is_some_and(is_target) {
                    let current = self.current.clone();
                    self.show_message(current, false);
                }
                // Re-render any popped-out window showing this message — a
                // conversation window may hold it as any member of its thread,
                // so every popout gets the offer (non-members ignore it).
                for p in self.popouts.values() {
                    p.controller.emit(MessageWindowInput::SetBody {
                        account_id,
                        id: message_id,
                        body: body.clone(),
                    });
                }
            }

            AppMsg::MessageGone {
                account_id,
                message_id,
                path,
                uid,
            } => {
                // A conversation member the cache still listed turned out to
                // be gone from the server (#257): take it out of the open
                // conversation rather than show it as a blank card.
                let folder = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.path == path))
                    .map(|f| f.id);
                if let Some(fid) = folder {
                    if let Some(msgs) = self.message_cache.get_mut(&(account_id, fid)) {
                        msgs.retain(|m| m.uid != uid);
                    }
                }
                // The message the reader was opened on stays: that one is
                // the list's to settle, by its own sync.
                let opened = self
                    .current
                    .as_ref()
                    .map(|c| (c.account_id, c.id, c.folder_id));
                let before = self.current_thread.len();
                self.current_thread.retain(|m| {
                    let gone = m.account_id == account_id
                        && m.id == message_id
                        && folder.is_none_or(|fid| m.folder_id == fid);
                    !gone || opened == Some((m.account_id, m.id, m.folder_id))
                });
                // Remembered conversations may hold it too, and the list's
                // badges counted it.
                self.forget_threads(account_id);
                self.message_list
                    .emit(MessageListInput::RecheckThreadSummaries(account_id));
                if self.current_thread.len() != before {
                    if self.current_thread.len() > 1 {
                        self.queue_thread_render(&sender);
                    } else {
                        // Nothing left to make a conversation of.
                        self.current_thread.clear();
                        let current = self.current.clone();
                        let loading = current.as_ref().is_some_and(|c| c.body.is_empty());
                        self.show_message(current, loading);
                    }
                }
            }

            AppMsg::RefsRepaired {
                account_id,
                folder_id,
            } => {
                // Only the folder on screen, and only its cached copy: the repair
                // rewrote rows on disk, and re-reading is what lets the list see
                // the conversations they now form.
                let showing = self
                    .selected
                    .as_ref()
                    .is_some_and(|sel| sel.account_id == account_id && sel.folder_id == folder_id);
                if showing || self.unified {
                    if let Some(path) = self
                        .folders
                        .get(&account_id)
                        .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
                        .map(|f| f.path.clone())
                    {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                }
            }

            AppMsg::SelectionKeys(slots) => {
                let keys: Vec<(u32, u32)> = slots.iter().map(|&(a, _, id)| (a, id)).collect();
                self.list_slots = slots;
                self.list_heard = keys.clone();
                self.list_selection = keys.clone();
                self.selection_from_cards = false;
                self.message_view
                    .emit(MessageViewInput::SetSelectedCards(keys));
            }

            AppMsg::ListChecked(count) => {
                self.checked = count;
                // Checked rows are not read: whatever was open closes, and
                // the empty pane counts them.
                if count.is_some() && self.current.is_some() {
                    sender.input(AppMsg::ClearReader);
                }
                self.message_view.emit(MessageViewInput::SetChecked {
                    count: count.unwrap_or(0),
                    conversations: self.threading,
                });
            }

            AppMsg::SelectCards(keys) => {
                self.list_selection = keys.clone();
                self.list_slots.clear();
                self.selection_from_cards = !keys.is_empty();
                // Reading is click-driven: scrolling past a conversation
                // message no longer marks it, so an arriving reply stays
                // unread until the user actually clicks its card. A single
                // deliberate selection is that click.
                if let [(aid, id)] = keys.as_slice() {
                    if let Some(m) = self
                        .current_thread
                        .iter()
                        .find(|m| m.account_id == *aid && m.id == *id)
                        .filter(|m| m.unread)
                        .cloned()
                    {
                        self.set_read(&m, true);
                    }
                }
                // The conversation as the reader has it, Sent members and all,
                // so a card the list never listed still keeps its row (#220).
                let conversation = self
                    .current_thread
                    .iter()
                    .map(|m| (m.account_id, m.id))
                    .collect();
                self.message_list
                    .emit(MessageListInput::SelectFromReader { keys, conversation });
            }

            AppMsg::ThreadMessageSeen { account_id, id } => {
                // Reading one message of a conversation marks only that message,
                // exactly as opening it on its own would.
                let target = self
                    .current_thread
                    .iter_mut()
                    .find(|m| m.account_id == account_id && m.id == id)
                    .filter(|m| m.unread)
                    .map(|m| {
                        m.unread = false;
                        (m.uid, m.folder_id)
                    });
                let Some((uid, folder_id)) = target else {
                    return;
                };
                if let Some(c) = self
                    .current
                    .as_mut()
                    .filter(|c| (c.account_id, c.id) == (account_id, id))
                {
                    c.unread = false;
                }
                if let Some(path) = self.folder_path(account_id, folder_id) {
                    self.pending_seen.insert(
                        (account_id, path.clone(), uid),
                        (true, std::time::Instant::now()),
                    );
                    self.send_to(
                        account_id,
                        MailRequest::SetSeen {
                            path,
                            uid,
                            seen: true,
                        },
                    );
                }
                // Reading the new mail inside its conversation answers the
                // notification as opening it on its own does (#333).
                crate::notify::withdraw_mail(account_id);
                self.message_list
                    .emit(MessageListInput::MarkRead((account_id, folder_id, id)));
                self.mark_cached_read(account_id, folder_id, id);
                // The card's dot clears in place as the viewport observer
                // marks it (#100) — this path never told the view before.
                self.message_view
                    .emit(MessageViewInput::ClearDot { account_id, id });
                if let Some(n) = self.folder_unread.get_mut(&(account_id, folder_id)) {
                    *n = n.saturating_sub(1);
                }
                self.remember_thread();
                self.push_unread_counts();
            }

            AppMsg::RenderThread => {
                self.thread_render_queued = false;
                if self.current_thread.len() > 1 {
                    // Still assembling and still within the grace period: come
                    // back rather than paint a conversation that is about to be
                    // replaced. The re-queue is what makes the deadline fire.
                    let unsettled = self.current_thread.iter().any(|m| m.body.is_empty())
                        || self.thread_related_pending;
                    let waiting = self
                        .thread_opened_at
                        .is_some_and(|opened| opened.elapsed() < THREAD_BODY_WAIT);
                    if unsettled && waiting {
                        self.queue_thread_render(&sender);
                        return;
                    }
                    self.thread_painted = true;
                    self.show_thread();
                    self.remember_thread();
                }
            }

            AppMsg::Source { text } => {
                // Source is only fetched on explicit request (toolbar or context
                // menu), so always show it — even for a message that isn't open.
                self.show_source_window(&text);
            }

            AppMsg::Attachments {
                account_id,
                path,
                message_id,
                items,
            } => {
                let key = self.event_key(account_id, &path, message_id);
                if let Some(k) = key {
                    self.attachment_cache.insert(k, items.clone());
                }
                let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
                // A file the gallery asked for: hand the bytes straight over so
                // the thumbnail and the preview fill in without a reload, which
                // would throw away the user's place in the list.
                if self.showing_gallery {
                    self.gallery.emit(GalleryInput::SetFetching(false));
                    self.gallery.emit(GalleryInput::Fetched {
                        account_id,
                        path: path.clone(),
                        uid: key.map_or(message_id, |k| k.2),
                        items: items.clone(),
                    });
                }
                if self.current.as_ref().is_some_and(is_target) {
                    self.attachments_loading = false;
                    self.attachments = items.clone();
                    self.sync_attachment_drawer();
                }
                // With a conversation open the drawer spans the whole thread, so
                // any member's arrival re-merges the union (this supersedes the
                // single-message assignment above when both apply).
                if self.current_thread.len() > 1 && self.current_thread.iter().any(is_target) {
                    self.attachments_loading = false;
                    self.refresh_thread_attachments();
                }
                for p in self.popouts.values() {
                    let member = p.members.iter().find(|(k, (a, id))| {
                        key.map_or(*a == account_id && *id == message_id, |key| *k == key)
                    });
                    if let Some((_, (a, id))) = member {
                        p.controller.emit(MessageWindowInput::SetAttachments {
                            account_id: *a,
                            id: *id,
                            items: items.clone(),
                        });
                    }
                }
                // These files were fetched to be carried into a copy of the
                // message; they are cached now, so the second pass stages them.
                if let Some(pending) = self.pending_edit_as_new.take() {
                    if is_target(&pending) {
                        self.edit_as_new(pending, &sender);
                    } else {
                        self.pending_edit_as_new = Some(pending);
                    }
                }
                // The same for a forward that carries them (#240).
                if let Some((pending, inline)) = self.pending_forward.take() {
                    if is_target(&pending) {
                        self.forward(pending, inline, &sender);
                    } else {
                        self.pending_forward = Some((pending, inline));
                    }
                }
            }

            AppMsg::AttachmentsPending {
                account_id,
                path,
                message_id,
            } => {
                // Attachments exist but weren't on disk. Opening the message
                // was the request — fetch them now rather than asking for a
                // click (the old "load attachments" button). Every reader path
                // now downloads outright, so this is a safety net for any
                // cache-only probe that still answers "present, not fetched".
                let key = self.event_key(account_id, &path, message_id);
                let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
                let msg = self
                    .current
                    .as_ref()
                    .filter(|c| is_target(c))
                    .cloned()
                    .or_else(|| self.current_thread.iter().find(|m| is_target(m)).cloned());
                if let Some(m) = msg {
                    if let Some(path) = self.resolve_folder_path(&m) {
                        self.attachments_loading = true;
                        self.send_to(
                            account_id,
                            MailRequest::LoadAttachments {
                                message_id,
                                path,
                                uid: m.uid,
                                download: true,
                            },
                        );
                    }
                }
                for p in self.popouts.values() {
                    let member = p.members.iter().find(|(k, (a, id))| {
                        key.map_or(*a == account_id && *id == message_id, |key| *k == key)
                    });
                    if let Some((_, (a, id))) = member {
                        p.controller.emit(MessageWindowInput::AttachmentsPending {
                            account_id: *a,
                            id: *id,
                        });
                    }
                }
            }

            AppMsg::NoAttachments {
                account_id,
                path,
                message_id,
            } => {
                // Clear a false paperclip live: on this message, in its own
                // folder. A UID names a message only within its folder, so
                // the same number elsewhere is a different message.
                let key = self.event_key(account_id, &path, message_id);
                let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
                self.set_has_attachment(key, false);
                if let Some(c) = self.current.as_mut() {
                    if is_target(c) {
                        c.has_attachment = false;
                    }
                }
                // A copy waiting on files that do not exist: open it anyway,
                // rather than leave the menu entry looking dead.
                if let Some(mut pending) = self.pending_edit_as_new.take() {
                    if is_target(&pending) {
                        pending.has_attachment = false;
                        self.edit_as_new(pending, &sender);
                    } else {
                        self.pending_edit_as_new = Some(pending);
                    }
                }
                if let Some((mut pending, inline)) = self.pending_forward.take() {
                    if is_target(&pending) {
                        pending.has_attachment = false;
                        self.forward(pending, inline, &sender);
                    } else {
                        self.pending_forward = Some((pending, inline));
                    }
                }
            }

            AppMsg::HasAttachments {
                account_id,
                path,
                message_id,
            } => {
                // The mirror of NoAttachments: a message whose structure didn't
                // advertise its attachments (an inline PDF, say — issue #9) is
                // given its paperclip once the body has proved they are there.
                let key = self.event_key(account_id, &path, message_id);
                let is_target = |m: &Message| is_event_target(m, key, account_id, message_id);
                self.set_has_attachment(key, true);
                // If it is the message on screen, fetch the files too: the reader
                // only asks for them when the flag was already set, which by
                // definition it wasn't.
                let already = self
                    .current
                    .as_ref()
                    .is_some_and(|c| is_target(c) && c.has_attachment);
                let open = self.current.as_mut().filter(|c| is_target(c));
                if let (false, Some(current)) = (already, open) {
                    current.has_attachment = true;
                    let message = current.clone();
                    if let Some(cached) = self.attachment_cache.get(&body_key(&message)).cloned() {
                        self.attachments = cached;
                        self.sync_attachment_drawer();
                    } else if let Some(path) = self.resolve_folder_path(&message) {
                        self.attachments_loading = true;
                        self.send_to(
                            account_id,
                            MailRequest::LoadAttachments {
                                message_id,
                                path,
                                uid: message.uid,
                                download: true,
                            },
                        );
                    }
                }
            }

            AppMsg::Status { account_id, text } => {
                if text.is_empty() {
                    self.busy.remove(&account_id);
                } else {
                    self.busy.insert(account_id);
                }
                self.update_busy_indicator();
                // A sync going quiet must not blank the status while background
                // bulk operations are still reporting there.
                if !text.is_empty() || self.bulk_pending == 0 {
                    self.notifications.emit(NotifyInput::SetStatus(text));
                }
            }

            AppMsg::Error {
                account_id,
                text,
                connectivity,
            } => {
                // The log gets the whole error (a parser dump included, which
                // is what diagnoses #226-style replies); the bar gets it
                // readable.
                tracing::error!("[account {account_id}] {text}");
                // A connection that died with the network: the Refresh
                // button already says it is down (#442).
                if connectivity && !crate::power::reaches_internet() {
                    return;
                }
                let text = crate::worker::readable_error(&text);
                let label = self.account_label(account_id);
                // Desktop-notify only genuine failures (not transient connectivity
                // blips that auto-recover), and only when unfocused — the in-app bar
                // already surfaces it while you're looking.
                if self.notifications_enabled && !connectivity && !self.window.is_active() {
                    crate::notify::error(account_id, &format!("{label}: mail error"), &text);
                }
                self.notifications.emit(NotifyInput::Push {
                    text: format!("{label}: {text}"),
                    error: true,
                    connectivity,
                });
            }

            AppMsg::DirectoryError(text) => {
                self.notifications.emit(NotifyInput::Push {
                    text,
                    error: true,
                    connectivity: false,
                });
            }

            AppMsg::NotifyCount(n) => self.notify_count = n,
            AppMsg::ToggleNotifications => self.notifications.emit(NotifyInput::ToggleBar),
            AppMsg::StatusBarShown(shown) => {
                self.status_bar_shown = shown;
                self.rebuild_help_menu();
            }

            AppMsg::OpenContacts => {
                self.mirror_selection(crate::ui::sidebar::Sel::Contacts);
                self.show_contacts_view(&sender);
            }

            AppMsg::ContactTree(tree) => {
                self.sidebar
                    .emit(SidebarInput::SetContactTree(tree.clone()));
                self.peek_sidebar.emit(SidebarInput::SetContactTree(tree));
            }

            AppMsg::ContactScope(scope) => {
                // A book or group under the sidebar's Contacts row opens
                // Contacts on it.
                if !self.showing_contacts {
                    self.show_contacts_view(&sender);
                } else {
                    self.close_sidebar_peek();
                }
                self.contacts_page.emit(ContactsPageInput::SetScope(scope));
            }

            AppMsg::ContactBooksSynced => {
                // EDS writes what it fetched just after the sync returns.
                // Read even while Contacts is hidden: the page keeps its
                // list between visits, and this keeps that list current.
                reload_contacts(&sender, 1500);
                if self.filters.iter().any(|r| r.needs_contacts()) {
                    self.saved_addresses.refresh();
                }
            }

            AppMsg::Pref(PrefOutput::ContactSourcesChanged) => {
                // A book switched on or off, or renamed, counts at once
                // everywhere, the sidebar's Contacts list among them.
                reload_contacts(&sender, 0);
                if self.filters.iter().any(|r| r.needs_contacts()) {
                    self.saved_addresses.refresh();
                }
                // Settings synced a saved account's books already; any it
                // did not are opened and synced now.
                sync_contact_books(&sender, false, 0);
            }

            AppMsg::ContactBooksLoaded { books, local_count } => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::SetContactBooks(books.clone()));
                    p.emit(PrefInput::SetLocalContactCount(local_count));
                }
                self.contacts_page.emit(ContactsPageInput::SetBooks(books));
            }

            AppMsg::ContactSuggestions => {
                let own: Vec<(String, String)> = self
                    .config
                    .iter()
                    .map(|c| (c.name.clone(), c.email.clone()))
                    .collect();
                let page = self.contacts_page.sender().clone();
                std::thread::spawn(move || {
                    let _ = page.send(ContactsPageInput::SuggestionsLoaded(
                        crate::contacts::suggestions(&own),
                    ));
                });
            }

            AppMsg::ContactsLoaded(contacts) => {
                self.contacts_page
                    .emit(ContactsPageInput::SetContacts(contacts));
                // A contact was saved, deleted or imported: the rules on
                // whether a sender is in Contacts should know.
                if self.filters.iter().any(|r| r.needs_contacts()) {
                    self.saved_addresses.refresh();
                }
            }

            AppMsg::LaunchGnomeContacts => crate::ui::contacts_browser::launch_gnome_contacts(),

            AppMsg::SaveContact { book_uid, vcard } => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    let result = crate::contacts::modify_contact(&book_uid, &vcard);
                    s.input(AppMsg::ContactWriteDone(result.err()));
                });
            }

            AppMsg::CreateContact { book_uid, vcard } => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    // No book picked (none was listed yet): the default one.
                    let book_uid = match book_uid {
                        Some(uid) => Some(uid),
                        None => crate::contacts::writable_books()
                            .first()
                            .map(|b| b.uid.clone()),
                    };
                    let result = match book_uid {
                        Some(uid) => crate::contacts::create_contact(&uid, &vcard),
                        None => Err(i18n("No address book available")),
                    };
                    s.input(AppMsg::ContactWriteDone(result.err()));
                });
            }

            AppMsg::SaveGroup {
                book_uid,
                format,
                original,
                edit,
            } => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    let result =
                        crate::contacts::save_group(&book_uid, format, original.as_deref(), edit);
                    s.input(AppMsg::ContactWriteDone(result.err()));
                });
            }

            AppMsg::DeleteContacts(keys) => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    // One call per book.
                    let mut by_book: Vec<(String, Vec<String>)> = Vec::new();
                    for (book, uid) in keys {
                        match by_book.iter_mut().find(|(b, _)| *b == book) {
                            Some((_, uids)) => uids.push(uid),
                            None => by_book.push((book, vec![uid])),
                        }
                    }
                    let err = by_book
                        .iter()
                        .filter_map(|(book, uids)| {
                            crate::contacts::delete_contacts(book, uids).err()
                        })
                        .next();
                    s.input(AppMsg::ContactWriteDone(err));
                });
            }

            AppMsg::MoveGroup {
                from,
                uid,
                to,
                format,
                edit,
            } => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    let result = crate::contacts::move_group(&from, &uid, &to, format, edit);
                    s.input(AppMsg::ContactWriteDone(result.err()));
                });
            }

            AppMsg::ContactDrop { keys, on, moving } => {
                self.contacts_page
                    .emit(ContactsPageInput::DropContacts { keys, on, moving });
            }

            AppMsg::ContactTreeAction(action) => {
                self.close_sidebar_peek();
                self.contacts_page.emit(ContactsPageInput::Tree(action));
            }

            AppMsg::ContactsFolded(keys) => {
                self.contacts_folded = keys;
                self.save_sidebar_state();
            }

            AppMsg::RenameBook { uid, name } => {
                if let Err(e) = crate::contact_sources::set_name(&uid, name.as_deref()) {
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n_f("Could not rename the address book: {e}", &[("e", &e)]),
                        error: true,
                        connectivity: false,
                    });
                }
                // Every name of the book is read again with the books.
                reload_contacts(&sender, 0);
            }

            AppMsg::CopyContacts { to, cards } => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    let failed: Vec<String> = cards
                        .iter()
                        .filter_map(|card| crate::contacts::create_contact(&to, card).err())
                        .collect();
                    let err = failed.first().map(|e| {
                        let n = failed.len() as u32;
                        ni18n_f(
                            "{n} contact was not copied: {e}",
                            "{n} contacts were not copied: {e}",
                            n,
                            &[("n", &n.to_string()), ("e", e)],
                        )
                    });
                    s.input(AppMsg::ContactWriteDone(err));
                });
            }

            AppMsg::MoveContacts { to, cards } => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    let failed: Vec<String> = cards
                        .iter()
                        .filter_map(|(book, uid, vcard)| {
                            crate::contacts::move_contact(book, uid, vcard, &to).err()
                        })
                        .collect();
                    let err = failed.first().map(|e| {
                        let n = failed.len() as u32;
                        ni18n_f(
                            "{n} contact was not moved: {e}",
                            "{n} contacts were not moved: {e}",
                            n,
                            &[("n", &n.to_string()), ("e", e)],
                        )
                    });
                    s.input(AppMsg::ContactWriteDone(err));
                });
            }

            AppMsg::ContactWriteDone(err) => {
                if let Some(e) = err {
                    // What was taken off the list may still be there.
                    self.contacts_page.emit(ContactsPageInput::WriteFailed);
                    self.notifications.emit(NotifyInput::Push {
                        text: i18n_f("Could not update contact: {e}", &[("e", &(e).to_string())]),
                        error: true,
                        connectivity: false,
                    });
                }
                // Success or not, re-read so the card shows what EDS holds.
                // A short pause lets EDS flush the write to its SQLite cache
                // (that is what the read goes through).
                reload_contacts(&sender, 500);
            }

            AppMsg::ImportContacts(paths) => {
                let s = sender.clone();
                std::thread::spawn(move || {
                    s.input(AppMsg::ContactsImported(crate::contacts::import_vcf_files(
                        &paths,
                    )));
                });
            }

            AppMsg::ContactsImported(result) => {
                let (text, error) = match &result {
                    Err(e) => (i18n_f("Could not import contacts: {e}", &[("e", e)]), true),
                    Ok(o) => import_summary(o),
                };
                // The bar opens for errors only; a good import shows in the
                // list, and its counts wait in the bar for whoever opens it.
                if error {
                    self.notifications.emit(NotifyInput::Push {
                        text,
                        error,
                        connectivity: false,
                    });
                } else {
                    self.notifications.emit(NotifyInput::SetStatus(text));
                }
                if let Ok(o) = &result {
                    if !o.failed_files.is_empty() {
                        self.notifications.emit(NotifyInput::Push {
                            text: i18n_f(
                                "Could not read: {files}",
                                &[("files", &o.failed_files.join("; "))],
                            ),
                            error: true,
                            connectivity: false,
                        });
                    }
                }
                reload_contacts(&sender, 0);
            }

            // The rest of Preferences' outputs are mapped to their own
            // messages before they get here.
            AppMsg::Pref(_) => {}
        }
    }
}
