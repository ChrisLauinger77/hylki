//! Left pane: an optional "All Inboxes" (unified) row, then one section per
//! account. Each account has a colored avatar circle + header button (chevron
//! on the right) and an animated `gtk::Revealer` holding its folder list, so
//! expanding/collapsing slides smoothly. Exactly one thing is selected across
//! the unified row and all account folder lists.
//!
//! Account order is managed in the Accounts window, not here; folder order is
//! set here (a folder dragged between its siblings) and handed to the app to keep, which sends it back in `SectionData`. Collapse state is
//! owned by the app (persisted); collapse is animated locally and reported for
//! persistence WITHOUT a full rebuild (which would interrupt the animation).

use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f, i18n_noop};
use crate::models::{Account, Folder, FolderKind};
use crate::ui::context_menu::{show_context_menu, show_context_menu_with_header, MenuEntry};

mod dnd;
mod menus;
mod messages;
mod open_state;
mod rebuild;
mod selection;
mod tree;
mod types;
mod unified;
mod widgets;

use dnd::parse_move_payload;
pub use messages::{CtxAction, SidebarInput, SidebarOutput};
pub use tree::Hierarchy;
pub(crate) use tree::{folder_depth, order_folders, placed_run, TREE_EXPANDER_WIDTH};
pub(crate) use types::kind_label;
use types::{chevron_icon, InboxRef, KindWidgets, SectionWidgets, UnifiedRow};
pub use types::{SectionData, Sel, SidebarInit, Slot, StartTarget, UnifiedFolderRef};
use widgets::slide_tree_row;
pub(crate) use widgets::{account_initials, build_unified_sub_row, pin_icon_size};

pub struct Sidebar {
    /// Last sections received, in display order.
    sections: Vec<SectionData>,
    /// Whether to show the unified "All Inboxes" row.
    show_unified: bool,
    /// Whether the collapsed-up "All Inboxes" row wears its total-unread chip
    /// (while expanded, the per-inbox sub-list carries the counts instead).
    unified_chips: crate::config::UnifiedChips,
    /// Whether the disclosure chevrons LEAD their rows (Settings: Chevron
    /// placement). Off restores the classic trailing position.
    chevrons_left: bool,
    /// Whether a click anywhere on an account's header folds it (Settings →
    /// Sidebar); off, its chevron or a double click does.
    account_click_folds: bool,
    /// Icon rail: a dot for unread mail in place of the count.
    rail_dots: bool,
    /// Icon rail: the sections that fold up by themselves when the sidebar
    /// collapses (Settings → Sidebar → Icon rail).
    rail_fold: crate::config::RailFold,
    /// Per-account widgets, rebuilt on each SetContents.
    revealers: HashMap<u32, gtk::Revealer>,
    chevrons: HashMap<u32, gtk::Image>,
    folder_lists: HashMap<u32, gtk::ListBox>,
    /// Per-account list box holding just the custom (user-created) folders, shown
    /// under a collapsible "Folders" section. Selection indices into these are
    /// offset past the account's essential folders.
    custom_folder_lists: HashMap<u32, gtk::ListBox>,
    /// Per-account custom folders, in row order, for tree-visibility math.
    custom_folders: HashMap<u32, Vec<Folder>>,
    /// Collapsed tree nodes per account (paths), mirrored from SectionData and
    /// flipped locally as chevrons are clicked (#51).
    tree_collapsed: HashMap<u32, std::collections::HashSet<String>>,
    /// Each parent row's expander image, for flipping without a rebuild.
    tree_chevrons: HashMap<(u32, String), gtk::Image>,
    /// Per-account custom-row revealers (row order), for animated tree
    /// collapse/expand.
    tree_row_revealers: HashMap<u32, Vec<gtk::Revealer>>,
    /// The folders inside the Inbox, listed under its row (#345): each
    /// account's main-list folders in row order, and the revealers of the
    /// nested ones by row.
    main_folders: HashMap<u32, Vec<Folder>>,
    main_row_revealers: HashMap<u32, HashMap<usize, gtk::Revealer>>,
    /// The rebuild freeze-frame Picture and its pending lift timer.
    freeze_frame: Option<gtk::Picture>,
    freeze_timer: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>>,
    /// The freeze-frame's fade during a rail toggle (see `rebuild_normal`).
    freeze_fade: std::rc::Rc<std::cell::RefCell<Option<adw::TimedAnimation>>>,
    /// The rail state the rows on screen were built for, so a rebuild can
    /// tell a full ↔ rail toggle from an in-place refresh.
    built_collapsed: bool,
    /// The "Folders" section revealer and its chevron, per account.
    custom_revealers: HashMap<u32, gtk::Revealer>,
    custom_chevrons: HashMap<u32, gtk::Image>,
    /// The unified-row list box (one row), when shown.
    unified_list: Option<gtk::ListBox>,
    /// The pinned footer's single list box (Contacts + Attachments rows),
    /// for selection management.
    footer_list: Option<gtk::ListBox>,
    /// The address books and groups the Contacts row lists.
    contact_tree: Option<crate::ui::contacts_page::ContactTree>,
    /// That list's rows, by the scope each picks; shared with its
    /// selection handler.
    tree_rows: std::rc::Rc<
        std::cell::RefCell<Vec<(crate::ui::contacts_page::ContactScope, gtk::ListBoxRow)>>,
    >,
    attachments_row: Option<gtk::ListBoxRow>,
    /// The footer's Contacts row, when Contacts is placed there.
    contacts_row: Option<gtk::ListBoxRow>,
    /// Refresh/spinner stack + spinner beside the "New Message" button.
    sync_stack: Option<gtk::Stack>,
    sync_spinner: Option<gtk::Spinner>,
    /// Whether any account is syncing (drives the refresh spinner).
    busy: bool,
    /// Whether the network is down (turns the refresh button orange).
    offline: bool,
    /// The rail's refresh button, for its offline face.
    sync_button: Option<gtk::Button>,
    /// The "Outbox" row list box (one row), while anything is queued.
    outbox_list: Option<gtk::ListBox>,
    /// Display-wide provider holding each account's avatar color rules.
    color_provider: crate::ui::DisplayCss,
    selected: Sel,
    /// Icon-only mode: hide all text, show just icons and account pills.
    collapsed: bool,
    /// Set while rows are selected *programmatically* (restoring after a
    /// rebuild, following the app's navigation): the list boxes' selection
    /// signals then stay silent. Otherwise a signal-driven input would be
    /// queued and judged against a selection that has since moved on — two
    /// programmatic selections in a row used to oscillate forever through
    /// the app that way.
    quiet: std::rc::Rc<std::cell::Cell<bool>>,
    /// See `SidebarInit::mirror`.
    mirror: bool,
    /// The launch view still to be picked (#256): kept until the account
    /// it names has its folders listed, then taken by the first pick.
    start: Option<StartTarget>,
    /// The view to pick again, and announce, once its account is listed
    /// again: the app dropped what it showed when the accounts reconnected
    /// (an account saved), while this list still showed it selected. The
    /// account is held by address: ids follow the account order, which a
    /// removal shifts.
    reannounce: Option<(Sel, Option<String>)>,
    /// Whether the "Attachments" row is shown (in the pinned footer).
    show_attachments: bool,
    /// How many messages are waiting in the Outbox across all accounts. The row
    /// only exists while this is non-zero — an empty Outbox is the normal state
    /// and does not deserve permanent furniture.
    outbox_count: u32,
    /// Total unread across all inboxes, for the "All Inboxes" badge.
    unified_unread: u32,
    /// The folders whose unread count is new since last looked at, while
    /// "Highlight only new unread mail" is on (#343); `None` when it is off.
    fresh: Option<std::collections::HashSet<(u32, u32)>>,
    /// Unread badge labels by (account_id, folder_id), updated in place.
    folder_badges: HashMap<(u32, u32), gtk::Label>,
    /// The "All Inboxes" unread badge label, when shown.
    unified_badge: Option<gtk::Label>,
    /// Whether the "All Inboxes" per-account inbox sub-list is expanded.
    unified_expanded: bool,
    unified_revealer: Option<gtk::Revealer>,
    unified_chevron: Option<gtk::Image>,
    unified_inbox_list: Option<gtk::ListBox>,
    /// Per-account inboxes shown under "All Inboxes", in sub-list row order.
    unified_inboxes: Vec<InboxRef>,
    /// Unread badges for the sub-list rows, by (account_id, inbox folder_id).
    unified_inbox_badges: HashMap<(u32, u32), gtk::Label>,
    /// The filtered folders listed in the unified section, in row order.
    unified_folders: Vec<UnifiedFolderRef>,
    /// Whether the unified "Filtered Folders" section is open.
    unified_folders_expanded: bool,
    /// The favorite folders (#367), in the order they were added.
    favorite_folders: Vec<UnifiedFolderRef>,
    favorites_expanded: bool,
    /// The smart folders (#417) as (id, name), in their saved order, with
    /// each one's unread count and the badge showing it.
    smart_folders: Vec<(String, String)>,
    smart_expanded: bool,
    /// Whether the Contacts row's address books are shown.
    contacts_expanded: bool,
    /// The accounts and books folded under it, by key.
    contacts_folded: HashSet<String>,
    /// Its rows that fold, and the carets that fold them.
    contact_layout: Vec<crate::ui::contact_tree::TreeRow>,
    contact_chevrons: Vec<(String, gtk::Image)>,
    smart_unread: HashMap<String, u32>,
    smart_badges: HashMap<String, gtk::Label>,
    /// The rows of every Filtered Folders section (the unified one and each
    /// account's own), in row order, with the sections' widgets and badges.
    filtered_rows: HashMap<Slot, Vec<UnifiedFolderRef>>,
    filtered_badges: HashMap<Slot, HashMap<(u32, u32), gtk::Label>>,
    /// Inbox unread badge overlaid on each account's avatar circle, shown only
    /// while that account's section is collapsed (its Inbox row — and normal
    /// chip — is then hidden inside the revealer). Keyed by account_id.
    account_circle_badges: HashMap<u32, gtk::Label>,
    /// The tags (#71), listed in the unified Tags section and in each
    /// account's own; the sections exist only while there is a tag.
    tags: Vec<crate::config::Tag>,
    /// Whether the unified "Tags" section is open.
    tags_expanded: bool,
    tag_sections: HashMap<Slot, SectionWidgets>,
    /// The rail's own open states: anything opened or folded while the
    /// sidebar is the icon rail lands here, the saved state untouched, so
    /// the full sidebar comes back as it was left. Cleared whenever the
    /// sidebar changes width. An item absent here shows its saved state —
    /// or folded, when "Fold up expanded items" covers it.
    rail_open: HashMap<UnifiedRow, bool>,
    rail_open_accounts: HashMap<u32, bool>,
    /// Which unified Starred / Sent / Drafts rows are shown, whether their
    /// account lists are open, and their widgets.
    unified_kinds: crate::config::UnifiedKinds,
    kind_expanded: HashMap<FolderKind, bool>,
    kind_widgets: HashMap<UnifiedRow, KindWidgets>,
    /// Whether the account sections are shown at all.
    show_accounts: bool,
    /// Where the Filters, Tags, Favorites and Contacts sections sit
    /// (Settings → Sidebar), and whether they are drawn as unified rows or
    /// as a tree.
    section_style: crate::config::SectionStyle,
    filtered_placement: crate::config::SectionPlacement,
    tags_placement: crate::config::SectionPlacement,
    favorites_placement: crate::config::SectionPlacement,
    contacts_placement: crate::config::SectionPlacement,
    /// Focus Mode: the account sections are hidden.
    focus_hide_accounts: bool,
    /// Focus Mode: the unified rows show folded, whatever their saved
    /// state; a click opens one for the mode alone (`rail_open`), the way
    /// the icon rail does.
    focus_fold_unified: bool,
    /// The revealer holding every account section, so Focus Mode can slide
    /// them away as one and bring them back the same way.
    accounts_revealer: Option<gtk::Revealer>,
    /// The next rebuild draws the accounts folded away and slides them in
    /// (Focus Mode has just given them back).
    reveal_accounts_late: bool,
}

#[relm4::component(pub)]
impl Component for Sidebar {
    type Init = SidebarInit;
    type Input = SidebarInput;
    type Output = SidebarOutput;
    type CommandOutput = ();

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,

            #[name = "body_overlay"]
            gtk::Overlay {
                set_vexpand: true,

                // The top block (compose bar, All Inboxes) is pinned above
                // the scroller so it never scrolls away; only the per-account
                // sections below scroll. Contacts/Attachments pin below it,
                // in the footer.
                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,

                    #[name = "pinned_box"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                    },

                    #[name = "sidebar_scroller"]
                    gtk::ScrolledWindow {
                        set_vexpand: true,
                        // External, not Never (see the message list's scroller): row
                        // content — a deeply indented folder tree, say — must not force
                        // the window's minimum width past what edge-tiling allows. The
                        // split view's min/max sidebar widths govern instead.
                        set_hscrollbar_policy: gtk::PolicyType::External,

                        #[name = "normal_box"]
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                        },
                    },

                    // Pinned footer, below the scroller: the Contacts and
                    // Attachments rows stay put against the sidebar's bottom
                    // edge no matter how tall the account list above grows.
                    #[name = "footer_box"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                    },
                },

                // Freeze-frame for rebuilds: the sidebar's last-rendered pixels,
                // shown over the swap so recreating every row never shimmers.
                // Anchored at the start and sized to the pixels it holds, so
                // the pane animating to or from the rail beneath it can never
                // stretch or squash the picture (the overlay clips it instead).
                #[name = "freeze_frame"]
                add_overlay = &gtk::Picture {
                    set_visible: false,
                    set_can_target: false,
                    set_halign: gtk::Align::Start,
                    set_content_fit: gtk::ContentFit::Fill,
                },
            },

        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let color_provider = crate::ui::DisplayCss::new();

        let mut model = Sidebar {
            sections: Vec::new(),
            show_unified: false,
            unified_chips: crate::config::UnifiedChips::default(),
            chevrons_left: false,
            account_click_folds: false,
            rail_dots: false,
            rail_fold: crate::config::RailFold::default(),
            revealers: HashMap::new(),
            chevrons: HashMap::new(),
            folder_lists: HashMap::new(),
            custom_folder_lists: HashMap::new(),
            custom_folders: HashMap::new(),
            tree_collapsed: HashMap::new(),
            tree_chevrons: HashMap::new(),
            tree_row_revealers: HashMap::new(),
            main_folders: HashMap::new(),
            main_row_revealers: HashMap::new(),
            freeze_frame: None,
            freeze_timer: std::rc::Rc::new(std::cell::RefCell::new(None)),
            freeze_fade: std::rc::Rc::new(std::cell::RefCell::new(None)),
            built_collapsed: init.collapsed,
            custom_revealers: HashMap::new(),
            custom_chevrons: HashMap::new(),
            unified_list: None,
            footer_list: None,
            contact_tree: None,
            tree_rows: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            attachments_row: None,
            contacts_row: None,
            sync_stack: None,
            sync_spinner: None,
            busy: false,
            offline: !crate::power::online(),
            sync_button: None,
            outbox_list: None,
            color_provider,
            selected: Sel::None,
            collapsed: init.collapsed,
            quiet: std::rc::Rc::new(std::cell::Cell::new(false)),
            mirror: init.mirror,
            start: if init.mirror { None } else { init.start },
            reannounce: None,
            show_attachments: init.show_attachments,
            outbox_count: 0,
            unified_unread: 0,
            fresh: None,
            folder_badges: HashMap::new(),
            unified_badge: None,
            unified_expanded: init.unified_expanded,
            unified_revealer: None,
            unified_chevron: None,
            unified_inbox_list: None,
            unified_inboxes: Vec::new(),
            unified_inbox_badges: HashMap::new(),
            unified_folders: Vec::new(),
            unified_folders_expanded: init.filtered_expanded,
            favorite_folders: Vec::new(),
            favorites_expanded: init.favorites_expanded,
            smart_folders: Vec::new(),
            smart_expanded: init.smart_expanded,
            contacts_expanded: init.contacts_expanded,
            contacts_folded: init.contacts_folded.iter().cloned().collect(),
            contact_layout: Vec::new(),
            contact_chevrons: Vec::new(),
            smart_unread: HashMap::new(),
            smart_badges: HashMap::new(),
            filtered_rows: HashMap::new(),
            filtered_badges: HashMap::new(),
            account_circle_badges: HashMap::new(),
            tags: Vec::new(),
            tags_expanded: init.tags_expanded,
            tag_sections: HashMap::new(),
            unified_kinds: crate::config::UnifiedKinds::NONE,
            kind_expanded: HashMap::from([
                (FolderKind::Starred, init.starred_expanded),
                (FolderKind::Sent, init.sent_expanded),
                (FolderKind::Drafts, init.drafts_expanded),
                (FolderKind::Archive, init.archive_expanded),
                (FolderKind::Junk, init.junk_expanded),
                (FolderKind::Trash, init.trash_expanded),
            ]),
            kind_widgets: HashMap::new(),
            rail_open: HashMap::new(),
            rail_open_accounts: HashMap::new(),
            show_accounts: true,
            section_style: crate::config::SectionStyle::default(),
            filtered_placement: crate::config::SectionPlacement::AboveAccounts,
            tags_placement: crate::config::SectionPlacement::AboveAccounts,
            favorites_placement: crate::config::SectionPlacement::AboveAccounts,
            contacts_placement: crate::config::SectionPlacement::AboveAccounts,
            focus_hide_accounts: false,
            focus_fold_unified: false,
            accounts_revealer: None,
            reveal_accounts_late: false,
        };

        let widgets = view_output!();
        model.freeze_frame = Some(widgets.freeze_frame.clone());
        widgets
            .body_overlay
            .set_clip_overlay(&widgets.freeze_frame, true);
        // Never scroll-to-focus: a rebuild (folder drag-and-drop) destroys
        // the focused row, GTK hands focus to some early widget, and the
        // viewport would yank the sidebar to the top to show it — the
        // "jumps up then back down" on every drop. Sidebar scrolling is the
        // user's alone.
        if let Some(viewport) = widgets
            .sidebar_scroller
            .child()
            .and_downcast::<gtk::Viewport>()
        {
            viewport.set_scroll_to_focus(false);
        }

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        self.update_inner(widgets, msg, sender, root);
    }
}

impl Sidebar {
    fn update_inner(
        &mut self,
        widgets: &mut <Self as Component>::Widgets,
        msg: SidebarInput,
        sender: ComponentSender<Self>,
        _root: &<Self as Component>::Root,
    ) {
        match msg {
            SidebarInput::DropStart => {
                let waited = self.start.take().is_some() | self.reannounce.take().is_some();
                if waited && self.selected == Sel::None {
                    self.restore_selection();
                }
            }

            SidebarInput::Reannounce => {
                if self.mirror || self.selected == Sel::None {
                    return;
                }
                let sel = std::mem::replace(&mut self.selected, Sel::None);
                let email = sel.account().and_then(|acc| {
                    self.sections
                        .iter()
                        .find(|s| s.account.id == acc)
                        .map(|s| s.account.email.clone())
                });
                self.reannounce = Some((sel, email));
                self.quiet.set(true);
                self.clear_other_selections(Sel::None);
                self.quiet.set(false);
                // An account that never comes back online must not leave
                // the list without a view.
                let s = sender.input_sender().clone();
                gtk::glib::timeout_add_seconds_local_once(8, move || {
                    let _ = s.send(SidebarInput::DropStart);
                });
            }

            SidebarInput::MirrorSelection(sel) => {
                if self.selected != sel {
                    self.selected = sel.clone();
                    self.quiet.set(true);
                    self.clear_other_selections(sel);
                    self.quiet.set(false);
                    self.restore_selection();
                }
            }

            SidebarInput::SetContents {
                mut sections,
                show_unified,
                unified_kinds,
                show_accounts,
                unified_chips,
                chevrons_left,
                account_click_folds,
                rail_dots,
                rail_fold,
                unified_unread,
                unified_folders,
                favorite_folders,
                tags,
                section_style,
                filtered_placement,
                tags_placement,
                favorites_placement,
                contacts_placement,
                smart_folders,
            } => {
                // Order each account's folders essential-first, then custom, so
                // the essential/custom split lines up with row indices (the main
                // list holds indices 0..E, the custom list E..), each part in
                // the user's own order where they set one.
                for s in &mut sections {
                    s.folders = order_folders(
                        std::mem::take(&mut s.folders),
                        &s.folder_order,
                        s.folder_sort,
                    );
                }
                // Seed the tree's collapsed nodes from the persisted state;
                // later chevron clicks flip the local copy.
                self.tree_collapsed = sections
                    .iter()
                    .map(|s| (s.account.id, s.tree_collapsed.iter().cloned().collect()))
                    .collect();
                self.sections = sections;
                self.show_unified = show_unified;
                self.unified_chips = unified_chips;
                self.chevrons_left = chevrons_left;
                self.account_click_folds = account_click_folds;
                self.rail_dots = rail_dots;
                // "Fold up expanded items" is a view of the rail, not a
                // change to what is saved: the rebuild below draws the
                // ticked items folded while the sidebar is collapsed and
                // as they are once it expands (see `locked_row`).
                self.rail_fold = rail_fold;
                self.unified_unread = unified_unread;
                self.unified_folders = unified_folders;
                self.favorite_folders = favorite_folders;
                self.tags = tags;
                self.section_style = section_style;
                self.filtered_placement = filtered_placement;
                self.tags_placement = tags_placement;
                self.favorites_placement = favorites_placement;
                self.contacts_placement = contacts_placement;
                self.smart_folders = smart_folders;
                // The open smart folder was deleted: fall back to the
                // default view.
                if let Sel::Smart(id) = &self.selected {
                    if !self.smart_folders.iter().any(|(i, _)| i == id) {
                        self.selected = Sel::None;
                    }
                }
                self.unified_kinds = unified_kinds;
                self.show_accounts = show_accounts;
                // Every Filtered Folders section's rows: the unified one's
                // (narrowed by the app to the rules that opted in) and each
                // account's own (every rule's destination).
                self.filtered_rows.clear();
                self.filtered_rows
                    .insert(Slot::Unified, self.unified_folders.clone());
                for s in &self.sections {
                    let rows = s
                        .filtered
                        .iter()
                        .map(|f| UnifiedFolderRef {
                            account_id: s.account.id,
                            folder: f.clone(),
                        })
                        .collect();
                    self.filtered_rows.insert(Slot::Account(s.account.id), rows);
                }
                // The open tag was removed (or its account): fall back to
                // the default view.
                if let Sel::Tag(acc, kw) = &self.selected {
                    let gone = !self.tags.iter().any(|t| t.keyword.eq_ignore_ascii_case(kw))
                        || acc.is_some_and(|a| !self.sections.iter().any(|s| s.account.id == a));
                    if gone {
                        self.selected = Sel::None;
                    }
                }
                // A unified row switched off in Settings (or with nothing
                // left to list): its view stays, nothing shows selected.
                if let Sel::UnifiedKind(kind) = &self.selected {
                    if !self.unified_kinds.has(*kind) {
                        self.selected = Sel::None;
                    }
                }
                if self.selected == Sel::UnifiedFiltered && self.unified_folders.is_empty() {
                    self.selected = Sel::None;
                }
                if self.selected == Sel::UnifiedFavorites && !self.favorites_shown() {
                    self.selected = Sel::None;
                }
                // All Inboxes gone (one account or none left in the unified
                // section, #267): the usual first view is picked again.
                if self.selected == Sel::Unified && !self.show_unified {
                    self.selected = Sel::None;
                }
                // An inbox or folder picked under a unified row whose account
                // has just left the unified section is still the open
                // folder: carry the highlight to the account's own row.
                let left_unified = |acc: u32| {
                    self.sections
                        .iter()
                        .find(|s| s.account.id == acc)
                        .map(|s| !s.in_unified)
                };
                let moved = match &self.selected {
                    Sel::UnifiedInbox(acc) => Some((*acc, FolderKind::Inbox)),
                    Sel::UnifiedKindRow(kind, acc) => Some((*acc, *kind)),
                    _ => None,
                };
                if let Some((acc, kind)) = moved.filter(|(acc, _)| left_unified(*acc) == Some(true))
                {
                    let path = self
                        .sections
                        .iter()
                        .find(|s| s.account.id == acc)
                        .and_then(|s| s.folders.iter().find(|f| f.kind == kind))
                        .map(|f| f.path.clone());
                    self.selected = path.map_or(Sel::None, |p| Sel::Folder(acc, p));
                }
                if self.selected == Sel::UnifiedTags && !self.unified_tags_shown() {
                    self.selected = Sel::None;
                }
                // Contacts placed off takes its selection with it; in the
                // footer, its one row is every scope.
                if matches!(self.selected, Sel::Contacts | Sel::ContactScope(_)) {
                    if self.contacts_placement == crate::config::SectionPlacement::Off {
                        self.selected = Sel::None;
                    } else if self.contacts_in_footer() {
                        self.selected = Sel::Contacts;
                    }
                }
                // A folder picked in a switched-off row's account list is
                // still the open folder: carry the highlight to the
                // account's own row.
                if let Sel::UnifiedKindRow(kind, acc) = self.selected.clone() {
                    if !self.unified_kinds.has(kind) {
                        let path = self
                            .sections
                            .iter()
                            .find(|s| s.account.id == acc)
                            .and_then(|s| s.folders.iter().find(|f| f.kind == kind))
                            .map(|f| f.path.clone());
                        self.selected = path.map_or(Sel::None, |p| Sel::Folder(acc, p));
                    }
                }
                // A folder taken out of Favorites is still the open folder.
                if let Sel::Favorite(acc, path) = &self.selected {
                    if !self
                        .favorite_folders
                        .iter()
                        .any(|r| r.account_id == *acc && r.folder.path == *path)
                    {
                        self.selected = Sel::Folder(*acc, path.clone());
                    }
                }
                // Likewise a folder that left an account's own Filtered
                // Folders section (its rule was removed).
                if let Sel::AccountFiltered(acc, path) = &self.selected {
                    let listed = self
                        .filtered_rows
                        .get(&Slot::Account(*acc))
                        .is_some_and(|rows| rows.iter().any(|r| r.folder.path == *path));
                    if !listed {
                        self.selected = Sel::Folder(*acc, path.clone());
                    }
                }
                // A selected filtered folder that just left the section (its
                // rule opted out, or the section was switched off) is still
                // the open folder: carry the highlight to the account
                // section's own row for it.
                if let Sel::UnifiedFolder(acc, path) = &self.selected {
                    let listed = self
                        .unified_folders
                        .iter()
                        .any(|r| r.account_id == *acc && r.folder.path == *path);
                    if !listed {
                        self.selected = Sel::Folder(*acc, path.clone());
                    }
                }
                self.rebuild_normal(
                    &widgets.pinned_box,
                    &widgets.normal_box,
                    &widgets.footer_box,
                    &sender,
                );
                self.restore_selection();
            }

            SidebarInput::UnifiedRowSelected => {
                if self.selected == Sel::Unified {
                    return;
                }
                self.selected = Sel::Unified;
                self.clear_other_selections(Sel::Unified);
                let _ = sender.output(SidebarOutput::UnifiedSelected);
            }

            SidebarInput::SelectUnifiedRow => {
                if self.selected == Sel::Unified {
                    return;
                }
                self.selected = Sel::Unified;
                self.quiet.set(true);
                self.clear_other_selections(Sel::Unified);
                if let Some(l) = &self.unified_list {
                    l.select_row(l.row_at_index(0).as_ref());
                }
                self.quiet.set(false);
                let _ = sender.output(SidebarOutput::UnifiedSelected);
            }

            SidebarInput::AttachmentsRowSelected => {
                if self.selected == Sel::Attachments {
                    return;
                }
                self.selected = Sel::Attachments;
                self.clear_other_selections(Sel::Attachments);
                let _ = sender.output(SidebarOutput::AttachmentsSelected);
            }

            SidebarInput::ContactsRowClicked => {
                if self.selected == Sel::Contacts {
                    return;
                }
                self.selected = Sel::Contacts;
                self.clear_other_selections(Sel::Contacts);
                let _ = sender.output(SidebarOutput::ContactsClicked);
                // The row itself is every contact. The footer's opens
                // Contacts on the book its own picker last showed.
                if !self.contacts_in_footer() {
                    let _ = sender.output(SidebarOutput::ContactScope(
                        crate::ui::contacts_page::ContactScope::All,
                    ));
                }
            }

            SidebarInput::SetContactTree(tree) => {
                if self.contact_tree.as_ref() == Some(&tree) {
                    return;
                }
                // The highlight follows a scope the contact page picked
                // itself (a book's New Contact opens that book).
                if matches!(self.selected, Sel::Contacts | Sel::ContactScope(_)) {
                    self.selected = match &tree.scope {
                        _ if self.contacts_in_footer() => Sel::Contacts,
                        crate::ui::contacts_page::ContactScope::All => Sel::Contacts,
                        scope => Sel::ContactScope(scope.clone()),
                    };
                    self.quiet.set(true);
                    self.clear_other_selections(self.selected.clone());
                    if self.selected == Sel::Contacts {
                        self.select_contacts(None);
                    }
                    self.quiet.set(false);
                }
                self.contact_tree = Some(tree);
                self.fill_contact_tree(&sender);
            }

            SidebarInput::ToggleContactsExpand => {
                self.toggle_row(UnifiedRow::Contacts, &sender);
            }

            SidebarInput::ToggleContactNode(key) => {
                let was_folded = self.contacts_folded.remove(&key);
                if !was_folded {
                    self.contacts_folded.insert(key.clone());
                }
                for (k, chevron) in &self.contact_chevrons {
                    if *k == key {
                        if was_folded {
                            chevron.add_css_class("open");
                        } else {
                            chevron.remove_css_class("open");
                        }
                    }
                }
                for t in &self.contact_layout {
                    if t.under.contains(&key) {
                        let hidden = t.under.iter().any(|k| self.contacts_folded.contains(k));
                        slide_tree_row(&t.row, &t.revealer, hidden);
                    }
                }
                let mut keys: Vec<String> = self.contacts_folded.iter().cloned().collect();
                keys.sort();
                let _ = sender.output(SidebarOutput::ContactsFolded(keys));
            }

            SidebarInput::ContactTreeSelected(scope) => {
                let key = Sel::ContactScope(scope.clone());
                if self.selected == key {
                    return;
                }
                self.selected = key.clone();
                self.clear_other_selections(key);
                let _ = sender.output(SidebarOutput::ContactScope(scope));
            }

            SidebarInput::ContactTreeDrop { keys, on, moving } => {
                let _ = sender.output(SidebarOutput::ContactDrop { keys, on, moving });
            }

            SidebarInput::ContactTreeMenu(action) => {
                let _ = sender.output(SidebarOutput::ContactTreeAction(action));
            }

            SidebarInput::OutboxRowSelected => {
                if self.selected == Sel::Outbox {
                    return;
                }
                self.selected = Sel::Outbox;
                self.clear_other_selections(Sel::Outbox);
                let _ = sender.output(SidebarOutput::OutboxSelected);
            }

            SidebarInput::SetOutboxCount(count) => {
                if self.outbox_count == count {
                    return;
                }
                let appearing = (self.outbox_count == 0) != (count == 0);
                self.outbox_count = count;
                // The row itself comes and goes with the count, so the sidebar
                // only needs rebuilding when it crosses zero; otherwise just
                // refresh the badge in place.
                if appearing {
                    // Leaving the Outbox selected when its row disappears would
                    // strand the view on an empty list.
                    if count == 0 && self.selected == Sel::Outbox {
                        self.selected = Sel::None;
                    }
                    self.rebuild_normal(
                        &widgets.pinned_box,
                        &widgets.normal_box,
                        &widgets.footer_box,
                        &sender,
                    );
                    self.restore_selection();
                } else if let Some(list) = &self.outbox_list {
                    if let Some(row) = list.row_at_index(0) {
                        if let Some(badge) = row.child().and_downcast::<gtk::Box>() {
                            if let Some(label) = badge.last_child().and_downcast::<gtk::Label>() {
                                label.set_label(&count.to_string());
                            }
                        }
                    }
                }
            }

            SidebarInput::UnifiedInboxRowSelected(index) => {
                if let Some(r) = self.unified_inboxes.get(index as usize).cloned() {
                    let key = Sel::UnifiedInbox(r.account_id);
                    if self.selected == key {
                        return;
                    }
                    self.selected = key.clone();
                    self.clear_other_selections(key);
                    let _ = sender.output(SidebarOutput::FolderSelected {
                        account_id: r.account_id,
                        folder_id: r.folder_id,
                        name: r.name,
                        path: r.path,
                    });
                }
            }

            SidebarInput::ToggleUnifiedExpand => {
                self.toggle_row(UnifiedRow::Kind(FolderKind::Inbox), &sender)
            }

            SidebarInput::FilteredRowSelected { slot, index } => {
                let Some(r) = self
                    .filtered_rows
                    .get(&slot)
                    .and_then(|rows| rows.get(index as usize))
                    .cloned()
                else {
                    return;
                };
                let key = match slot {
                    Slot::Unified => Sel::UnifiedFolder(r.account_id, r.folder.path.clone()),
                    Slot::Account(_) => Sel::AccountFiltered(r.account_id, r.folder.path.clone()),
                };
                if self.selected == key {
                    return;
                }
                self.selected = key.clone();
                self.clear_other_selections(key);
                let _ = sender.output(SidebarOutput::FolderSelected {
                    account_id: r.account_id,
                    folder_id: r.folder.id,
                    name: r.folder.name,
                    path: r.folder.path,
                });
            }

            SidebarInput::TagRowSelected { slot, index } => {
                if let Some(t) = self.tags.get(index as usize).cloned() {
                    let account = slot.account();
                    let key = Sel::Tag(account, t.keyword.clone());
                    if self.selected == key {
                        return;
                    }
                    self.selected = key.clone();
                    self.clear_other_selections(key);
                    let _ = sender.output(SidebarOutput::TagSelected {
                        keyword: t.keyword,
                        account,
                    });
                }
            }

            // The unified slot is a unified row; an account's is a heading.
            SidebarInput::ToggleTagsExpand(slot) => {
                if slot == Slot::Unified {
                    self.toggle_row(UnifiedRow::Tags, &sender);
                } else {
                    let open = !self.tags_open(slot);
                    self.set_tags_open(slot, open, &sender);
                    if let Some(w) = self.tag_sections.get(&slot) {
                        w.revealer.set_reveal_child(open);
                        w.chevron.set_icon_name(Some(chevron_icon(open)));
                    }
                }
            }

            SidebarInput::ToggleFavoritesExpand => {
                self.toggle_row(UnifiedRow::Favorites, &sender);
            }

            SidebarInput::ToggleSmartExpand => {
                self.toggle_row(UnifiedRow::Smart, &sender);
                // The header is a toggle, not a view: it never stays picked.
                if let Some(w) = self.kind_widgets.get(&UnifiedRow::Smart) {
                    let header = w.header.clone();
                    let quiet = self.quiet.clone();
                    gtk::glib::idle_add_local_once(move || {
                        quiet.set(true);
                        header.unselect_all();
                        quiet.set(false);
                    });
                }
            }

            SidebarInput::SmartRowSelected(index) => {
                let Some((id, _)) = self.smart_folders.get(index as usize).cloned() else {
                    return;
                };
                let key = Sel::Smart(id.clone());
                if self.selected == key {
                    return;
                }
                self.selected = key.clone();
                self.clear_other_selections(key);
                let _ = sender.output(SidebarOutput::SmartSelected(id));
            }

            SidebarInput::SetSmartUnread(counts) => {
                for (id, badge) in &self.smart_badges {
                    let n = counts.get(id).copied().unwrap_or(0);
                    badge.set_label(&n.to_string());
                    badge.set_visible(n > 0);
                }
                self.smart_unread = counts;
                if let Some(w) = self.kind_widgets.get(&UnifiedRow::Smart) {
                    if let Some(b) = &w.badge {
                        let total = self.row_unread(UnifiedRow::Smart);
                        b.set_label(&total.to_string());
                        b.set_visible(total > 0 && !self.row_shown_open(UnifiedRow::Smart));
                    }
                }
            }

            SidebarInput::FavoriteRowSelected(index) => {
                let Some(r) = self.favorite_folders.get(index as usize).cloned() else {
                    return;
                };
                let key = Sel::Favorite(r.account_id, r.folder.path.clone());
                if self.selected == key {
                    return;
                }
                self.selected = key.clone();
                self.clear_other_selections(key);
                let _ = sender.output(SidebarOutput::FolderSelected {
                    account_id: r.account_id,
                    folder_id: r.folder.id,
                    name: r.folder.name,
                    path: r.folder.path,
                });
            }

            SidebarInput::ToggleFilteredExpand(slot) => {
                if slot == Slot::Unified {
                    self.toggle_row(UnifiedRow::Filtered, &sender);
                }
            }

            SidebarInput::UnifiedKindRowSelected(kind) => {
                let key = Sel::UnifiedKind(kind);
                if self.selected == key {
                    return;
                }
                self.selected = key.clone();
                self.clear_other_selections(key);
                let _ = sender.output(SidebarOutput::UnifiedKindSelected(kind));
            }

            SidebarInput::UnifiedFavoritesSelected => {
                if self.selected == Sel::UnifiedFavorites {
                    return;
                }
                self.selected = Sel::UnifiedFavorites;
                self.clear_other_selections(Sel::UnifiedFavorites);
                let _ = sender.output(SidebarOutput::UnifiedFavoritesSelected);
            }

            SidebarInput::UnifiedFilteredSelected => {
                if self.selected == Sel::UnifiedFiltered {
                    return;
                }
                self.selected = Sel::UnifiedFiltered;
                self.clear_other_selections(Sel::UnifiedFiltered);
                let _ = sender.output(SidebarOutput::UnifiedFilteredSelected);
            }

            SidebarInput::UnifiedTagsSelected => {
                if self.selected == Sel::UnifiedTags {
                    return;
                }
                self.selected = Sel::UnifiedTags;
                self.clear_other_selections(Sel::UnifiedTags);
                let _ = sender.output(SidebarOutput::UnifiedTagsSelected);
            }

            SidebarInput::KindSubRowSelected { kind, index } => {
                let Some(r) = self
                    .kind_widgets
                    .get(&UnifiedRow::Kind(kind))
                    .and_then(|w| w.rows.get(index as usize))
                    .cloned()
                else {
                    return;
                };
                let key = Sel::UnifiedKindRow(kind, r.account_id);
                if self.selected == key {
                    return;
                }
                self.selected = key.clone();
                self.clear_other_selections(key);
                if r.path.is_empty() {
                    let _ = sender.output(SidebarOutput::AccountStarredSelected(r.account_id));
                    return;
                }
                let _ = sender.output(SidebarOutput::FolderSelected {
                    account_id: r.account_id,
                    folder_id: r.folder_id,
                    name: r.name,
                    path: r.path,
                });
            }

            SidebarInput::ToggleKindExpand(kind) => {
                self.toggle_row(UnifiedRow::Kind(kind), &sender)
            }

            SidebarInput::FolderRowSelected { account_id, index } => {
                let folder = self
                    .sections
                    .iter()
                    .find(|s| s.account.id == account_id)
                    .and_then(|s| s.folders.get(index as usize))
                    .cloned();
                if let Some(folder) = folder {
                    let key = Sel::Folder(account_id, folder.path.clone());
                    if self.selected == key {
                        return;
                    }
                    self.selected = key.clone();
                    self.clear_other_selections(key);
                    let _ = sender.output(SidebarOutput::FolderSelected {
                        account_id,
                        folder_id: folder.id,
                        name: folder.name.clone(),
                        path: folder.path.clone(),
                    });
                }
            }

            SidebarInput::SelectFolderRow { account_id, path } => {
                // A click on a per-account inbox under "All Inboxes" echoes
                // back here as its plain folder path; that sub-row already
                // shows this exact folder, so keep its highlight instead of
                // jumping to the account section's own Inbox row.
                if self.selected == Sel::UnifiedInbox(account_id)
                    && self
                        .unified_inboxes
                        .iter()
                        .any(|r| r.account_id == account_id && r.path == path)
                {
                    return;
                }
                // Likewise a click on a filtered folder in either kind of
                // Filtered Folders section, on a favorite, or on an account's
                // folder in a unified Starred / Sent / Drafts row's list.
                if self.selected == Sel::UnifiedFolder(account_id, path.clone())
                    || self.selected == Sel::AccountFiltered(account_id, path.clone())
                    || self.selected == Sel::Favorite(account_id, path.clone())
                {
                    return;
                }
                if let Sel::UnifiedKindRow(kind, acc) = &self.selected {
                    if *acc == account_id
                        && self
                            .kind_widgets
                            .get(&UnifiedRow::Kind(*kind))
                            .is_some_and(|w| {
                                w.rows
                                    .iter()
                                    .any(|r| r.account_id == account_id && r.path == path)
                            })
                    {
                        return;
                    }
                }
                let key = Sel::Folder(account_id, path.clone());
                if self.selected != key {
                    self.selected = key.clone();
                    self.quiet.set(true);
                    self.clear_other_selections(key);
                    self.select_folder(account_id, &path);
                    self.quiet.set(false);
                }
                // A highlight inside a folded account shows nothing (#170:
                // a notification click seemed to land nowhere); unfold it.
                self.reveal_account(account_id, &sender);
            }

            SidebarInput::SetUnread {
                folders,
                unified,
                fresh,
            } => {
                self.fresh = fresh;
                // Mirror the fresh counts into every row list too, so a
                // rebuild draws them right (the account folders are done
                // below).
                let fresh =
                    |aid: u32, fid: u32, cur: u32| folders.get(&(aid, fid)).copied().unwrap_or(cur);
                for r in self
                    .unified_folders
                    .iter_mut()
                    .chain(self.favorite_folders.iter_mut())
                {
                    r.folder.unread = fresh(r.account_id, r.folder.id, r.folder.unread);
                }
                for rows in self.filtered_rows.values_mut() {
                    for r in rows.iter_mut() {
                        r.folder.unread = fresh(r.account_id, r.folder.id, r.folder.unread);
                    }
                }
                for s in &mut self.sections {
                    let aid = s.account.id;
                    for f in &mut s.filtered {
                        f.unread = fresh(aid, f.id, f.unread);
                    }
                }
                for ((aid, fid), label) in self
                    .folder_badges
                    .iter()
                    .chain(&self.unified_inbox_badges)
                    .chain(self.filtered_badges.values().flatten())
                    .chain(self.kind_widgets.values().flat_map(|w| &w.row_badges))
                {
                    let n = folders.get(&(*aid, *fid)).copied().unwrap_or(0);
                    label.set_text(&n.to_string());
                    label.set_visible(n > 0);
                }
                if let Some(label) = &self.unified_badge {
                    label.set_text(&unified.to_string());
                    label.set_visible(
                        unified > 0
                            && !self.row_shown_open(UnifiedRow::Kind(FolderKind::Inbox))
                            && self.unified_chips.all_inboxes,
                    );
                }
                for (row, w) in &self.kind_widgets {
                    let total: u32 = w
                        .row_badges
                        .keys()
                        .map(|k| folders.get(k).copied().unwrap_or(0))
                        .sum();
                    if let Some(b) = &w.badge {
                        b.set_text(&total.to_string());
                        b.set_visible(
                            total > 0 && !self.row_shown_open(*row) && self.chip_shown(*row),
                        );
                    }
                }
                // Keep the avatar-circle badges in sync too. They only show while
                // the account is collapsed (toggled live in ToggleCollapseLocal),
                // so we just refresh the number and re-apply that visibility rule.
                for section in &self.sections {
                    if let Some(label) = self.account_circle_badges.get(&section.account.id) {
                        let n = section
                            .folders
                            .iter()
                            .find(|f| f.kind == FolderKind::Inbox)
                            .and_then(|f| folders.get(&(section.account.id, f.id)))
                            .copied()
                            .unwrap_or(0);
                        label.set_text(&n.to_string());
                        label.set_visible(self.account_shown_folded(section) && n > 0);
                    }
                }
                self.unified_unread = unified;
                self.restyle_seen();
                // Persist the fresh counts into `sections` as well. Otherwise the
                // next rebuild_normal (e.g. toggling the sidebar collapse) recreates
                // every badge from the folder unread values captured at the last
                // SetContents, reverting in-place updates — so a read inbox's chip
                // reappears on collapse. Keep `sections` a faithful mirror.
                for section in &mut self.sections {
                    for folder in &mut section.folders {
                        folder.unread = folders
                            .get(&(section.account.id, folder.id))
                            .copied()
                            .unwrap_or(0);
                    }
                }
            }

            SidebarInput::SetAttachmentsRow(show) => {
                self.show_attachments = show;
                // The row is about to disappear from under the selection; fall
                // back the same way an emptied selection does.
                if !show && self.selected == Sel::Attachments {
                    self.selected = Sel::None;
                }
                self.rebuild_normal(
                    &widgets.pinned_box,
                    &widgets.normal_box,
                    &widgets.footer_box,
                    &sender,
                );
                self.restore_selection();
            }

            SidebarInput::SetBusy(busy) => {
                self.busy = busy;
                if let Some(sp) = &self.sync_spinner {
                    sp.set_spinning(busy);
                }
                if let Some(stack) = &self.sync_stack {
                    stack.set_visible_child_name(if busy { "spinner" } else { "icon" });
                }
            }

            SidebarInput::SetOffline(offline) => {
                self.offline = offline;
                if let Some(button) = &self.sync_button {
                    crate::ui::show_refresh_offline(button, offline);
                }
            }

            SidebarInput::SetFocus {
                hide_accounts,
                fold_unified,
                animate,
            } => {
                let hiding = hide_accounts && !self.focus_hide_accounts;
                let showing = !hide_accounts && self.focus_hide_accounts;
                let folding = fold_unified && !self.focus_fold_unified;
                if !hiding && !showing && fold_unified == self.focus_fold_unified {
                    return;
                }
                self.focus_hide_accounts = hide_accounts;
                self.focus_fold_unified = fold_unified;
                // Whatever was opened for the mode alone goes with it.
                self.rail_open.clear();
                self.rail_open_accounts.clear();
                if animate && (hiding || folding) {
                    // Slide what goes: the accounts up and away, every open
                    // unified row shut. The rebuild that draws the sidebar
                    // without them follows once they have gone — the pixels
                    // it replaces are then the ones it draws.
                    if hiding {
                        // SlideDown folding: the block slides up under the
                        // unified section, the way a folder list folds.
                        if let Some(r) = &self.accounts_revealer {
                            r.set_transition_duration(crate::ui::FOCUS_ANIM_MS);
                            r.set_reveal_child(false);
                        }
                    }
                    if folding {
                        self.fold_unified_rows_now();
                    }
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(u64::from(crate::ui::FOCUS_ANIM_MS) + 40),
                        move || s.input(SidebarInput::FocusSettled),
                    );
                } else {
                    // Accounts coming back slide in after the rebuild.
                    self.reveal_accounts_late = animate && showing;
                    self.rebuild_normal(
                        &widgets.pinned_box,
                        &widgets.normal_box,
                        &widgets.footer_box,
                        &sender,
                    );
                    self.restore_selection();
                }
            }

            SidebarInput::FocusSettled => {
                self.rebuild_normal(
                    &widgets.pinned_box,
                    &widgets.normal_box,
                    &widgets.footer_box,
                    &sender,
                );
                self.restore_selection();
            }

            SidebarInput::SetCollapsed(collapsed) => {
                // Driven by the app's narrow-window breakpoint: same visual
                // change as the user's own toggle, but no CollapsedChanged
                // output — automatic switches must not overwrite the user's
                // persisted preference.
                if self.collapsed != collapsed {
                    self.collapsed = collapsed;
                    self.rail_open.clear();
                    self.rail_open_accounts.clear();
                    self.rebuild_normal(
                        &widgets.pinned_box,
                        &widgets.normal_box,
                        &widgets.footer_box,
                        &sender,
                    );
                    self.restore_selection();
                }
            }

            SidebarInput::ToggleCollapsed => {
                self.collapsed = !self.collapsed;
                self.rail_open.clear();
                self.rail_open_accounts.clear();
                self.rebuild_normal(
                    &widgets.pinned_box,
                    &widgets.normal_box,
                    &widgets.footer_box,
                    &sender,
                );
                self.restore_selection();
                let _ = sender.output(SidebarOutput::CollapsedChanged(self.collapsed));
            }

            SidebarInput::ToggleCollapseLocal(id) => {
                // In the rail this opens or folds the account for the rail
                // alone — the saved state stays as it is for the full
                // sidebar, which comes back as it was left.
                let rail_only = self.collapsed;
                if let Some(rev) = self.revealers.get(&id) {
                    let expanded = !rev.reveals_child();
                    rev.set_reveal_child(expanded);
                    if let Some(ch) = self.chevrons.get(&id) {
                        ch.set_icon_name(Some(if expanded {
                            "pan-down-symbolic"
                        } else {
                            "pan-end-symbolic"
                        }));
                    }
                    if rail_only {
                        self.rail_open_accounts.insert(id, expanded);
                    } else if let Some(s) = self.sections.iter_mut().find(|s| s.account.id == id) {
                        s.collapsed = !expanded;
                    }
                    // The Inbox chip lives inside the folder list we just hid/shown,
                    // so mirror it onto the avatar: visible only while collapsed.
                    if let Some(label) = self.account_circle_badges.get(&id) {
                        let n = self
                            .sections
                            .iter()
                            .find(|s| s.account.id == id)
                            .and_then(|s| s.folders.iter().find(|f| f.kind == FolderKind::Inbox))
                            .map(|f| f.unread)
                            .unwrap_or(0);
                        label.set_text(&n.to_string());
                        label.set_visible(!expanded && n > 0);
                    }
                    if !rail_only {
                        let _ = sender.output(SidebarOutput::AccountFolded {
                            id,
                            folded: !expanded,
                        });
                    }
                }
            }

            SidebarInput::ToggleCustomFoldersLocal(id) => {
                if let Some(rev) = self.custom_revealers.get(&id) {
                    let expanded = !rev.reveals_child();
                    rev.set_reveal_child(expanded);
                    if let Some(ch) = self.custom_chevrons.get(&id) {
                        ch.set_icon_name(Some(if expanded {
                            "pan-down-symbolic"
                        } else {
                            "pan-end-symbolic"
                        }));
                    }
                    if let Some(s) = self.sections.iter_mut().find(|s| s.account.id == id) {
                        s.custom_expanded = expanded;
                    }
                    let _ = sender.output(SidebarOutput::ToggleCustomFolders(id));
                }
            }

            SidebarInput::ToggleFolderNode { account_id, path } => {
                self.toggle_folder_node(account_id, path, &sender);
            }

            SidebarInput::ReorderFolder {
                account_id,
                path,
                target,
                after,
            } => {
                let Some(section) = self.sections.iter().find(|s| s.account.id == account_id)
                else {
                    return;
                };
                let h = &section.hierarchy;
                let level = h.parent(&target);
                let custom = section
                    .folders
                    .iter()
                    .any(|f| f.path == path && f.kind == FolderKind::Custom);
                if !custom || level == h.parent(&path) {
                    // Already one of the target's siblings: only the order changes.
                    if let Some(paths) = placed_run(&section.folders, h, &path, &target, after) {
                        let _ = sender.output(SidebarOutput::ReorderFolders { account_id, paths });
                    }
                } else {
                    // From another level: the server moves it there first.
                    let _ = sender.output(SidebarOutput::PlaceFolder {
                        account_id,
                        path,
                        dest: level,
                        target,
                        after,
                    });
                }
            }

            SidebarInput::ExpandForDrop(id) => {
                // Expand a collapsed account so its folders become drop targets.
                self.reveal_account(id, &sender);
            }

            SidebarInput::DropOnFolder {
                account_id: dest_account,
                path: dest,
                payload,
            } => {
                // A dragged folder, not messages: reparent it (#51). Folders
                // never cross accounts — mailboxes belong to one server.
                if let Some(rest) = payload.strip_prefix("vireo-folder\t") {
                    let mut it = rest.splitn(2, '\t');
                    let src_account = it.next().and_then(|s| s.parse::<u32>().ok());
                    let src_path = it.next().map(String::from);
                    if let (Some(src_account), Some(src_path)) = (src_account, src_path) {
                        if src_account == dest_account && src_path != dest {
                            let _ = sender.output(SidebarOutput::MoveFolder {
                                account_id: dest_account,
                                path: src_path,
                                dest,
                            });
                        }
                    }
                    return;
                }
                let items = parse_move_payload(&payload);
                // "" is the Folders header (a folder-move destination only);
                // messages need a real mailbox.
                if !items.is_empty() && !dest.is_empty() {
                    let _ = sender.output(SidebarOutput::MoveMessages {
                        dest_account,
                        dest,
                        items,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
