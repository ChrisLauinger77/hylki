//! Attachments gallery: a grid of every cached attachment across the connected
//! inboxes, with a lightbox preview (prev/next, open, go to message).
//!
//! Data comes from the SQLite cache (what the background prefetch has already
//! downloaded), fed in via [`GalleryInput::SetItems`]. Image attachments and PDFs
//! (rendered from their first page) show as thumbnails; other files show a type
//! icon. Clicking a cell opens a large overlay preview.

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;
use relm4::prelude::*;

use std::collections::HashMap;

use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::models::{ext_of, FolderKind, GalleryItem, GallerySort};
use crate::ui::context_menu::{show_context_menu, MenuEntry};

mod open_file;
mod ratio_box;
mod thumbnails;

pub(crate) use open_file::open_bytes;
pub use open_file::purge_attachment_dir;
use ratio_box::RatioBox;
pub(crate) use thumbnails::{
    cache_stats, cached_pdf_preview, content_key, icon_color_class, icon_for, is_pdf_name,
    lightbox_pdf_texture, pdf_page_texture, pdf_render_lock, spawn_thumbnail_render, texture_from,
    thumbnail_spinner, thumbnail_texture, Thumbnail,
};
use thumbnails::{thumb_cache_key, PDF_PREVIEWS};

/// Width of the table's trailing quick-actions column (three icon buttons);
/// the header carries a spacer of the same width so the columns line up.
const TABLE_ACTIONS_WIDTH: i32 = 100;

/// How many attachments one page holds. Big enough that scrolling rarely waits,
/// small enough that the first screen appears at once and a page's worth of
/// cached thumbnails is not a burden.
const PAGE_SIZE: u32 = 120;

/// How close to the end of the list the scroll has to come before the next page
/// is asked for, in px. Roughly two rows of thumbnails, so the page is usually
/// already there by the time the user reaches it.
const LOAD_MORE_MARGIN: f64 = 700.0;

/// How long typing has to pause before the search runs, in ms. Each search is a
/// query over the whole archive and a rebuilt grid, so running one per keystroke
/// would spend most of its time on words half-typed.
const SEARCH_DEBOUNCE_MS: u32 = 250;

/// One page's worth of question for the cache: everything the database needs to
/// narrow and order the archive before cutting the page out of it.
#[derive(Debug, Clone)]
pub struct GalleryRequest {
    /// `(account id, folder path)` for every folder in scope.
    pub folders: Vec<(u32, String)>,
    pub account_id: Option<u32>,
    pub tokens: Vec<String>,
    pub bucket: u32,
    pub sort: GallerySort,
    pub offset: u32,
    pub limit: u32,
}

/// One folder offered in the gallery's folder list.
#[derive(Debug, Clone)]
pub struct GalleryFolder {
    pub path: String,
    /// Display name, as the sidebar spells it.
    pub name: String,
    pub kind: FolderKind,
}

/// One account's share of the folder list: the account's own label and the
/// folders whose attachments the gallery could draw on.
#[derive(Debug, Clone)]
pub struct GalleryAccount {
    pub id: u32,
    pub label: String,
    pub folders: Vec<GalleryFolder>,
}

/// Whether a folder of this kind feeds the gallery unless the user says
/// otherwise. Sent is the one eligible kind that starts off: what you sent is
/// rarely what you are looking for, and its attachments are usually copies of
/// files you already have. Drafts, Junk and Trash never reach the gallery at
/// all (the cache query drops them), so they are not offered here.
fn kind_default(kind: FolderKind) -> bool {
    !matches!(kind, FolderKind::Sent)
}

/// Which master switch governs a folder kind: Archive folders follow "Include
/// Archive", anything that is neither an inbox nor an archive follows "Include
/// other folders", and inboxes follow neither (they are always on).
fn kind_master(kind: FolderKind) -> Option<Master> {
    match kind {
        FolderKind::Inbox => None,
        FolderKind::Archive => Some(Master::Archive),
        _ => Some(Master::Other),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Master {
    Archive,
    Other,
}

/// Whether a folder's attachments are in scope: its master switch has to be on
/// *and* it has to be ticked. A master that is off wins over a tick rather than
/// clearing it, so turning the master back on restores each folder to whatever
/// the user had chosen. `tick` is the user's own choice, or `None` when they
/// have not touched this folder and its kind's default stands.
fn in_scope(
    kind: FolderKind,
    include_archive: bool,
    include_other: bool,
    tick: Option<bool>,
) -> bool {
    let master_on = match kind_master(kind) {
        Some(Master::Archive) => include_archive,
        Some(Master::Other) => include_other,
        None => true,
    };
    master_on && tick.unwrap_or_else(|| kind_default(kind))
}

pub struct AttachmentsGallery {
    /// The pages loaded so far, already filtered and ordered by the database.
    /// This is a window onto `total`, not the whole set: an archive of two
    /// decades holds far more attachments than the UI can hold widgets for, so
    /// the list grows a page at a time as the view is scrolled.
    all_items: Vec<GalleryItem>,
    /// How many attachments match the current query in total, across every
    /// page — what the footer counts and how `has_more` is known.
    total: u32,
    /// A page request is in flight; the footer shows its spinner and no second
    /// request is sent until it lands.
    loading_more: bool,
    /// Messages the scan has still to describe across the folders in scope.
    /// Non-zero means the gallery is not yet showing everything there is.
    scan_remaining: u32,
    /// An attachment that was never downloaded is on its way from the server.
    fetching: bool,
    query: String,
    sort: GallerySort,
    /// Index into `items` currently shown in the lightbox, if any.
    preview: Option<usize>,
    /// What the lightbox shows for the current item: a decoded image, or a
    /// PDF's first page once its full-size render lands (None while a PDF
    /// render is in flight, and for types with nothing to show).
    preview_texture: Option<gdk::Texture>,
    loading: bool,
    /// Show the sortable table instead of the thumbnail grid (persisted).
    view_table: bool,
    /// Grid thumbnail cell width in px, driven by the footer slider (persisted).
    thumb_width: i32,
    /// The footer type dropdown's row: 0 = all, then one bucket per row.
    type_filter: u32,
    /// Show only one account's attachments; `None` shows every account. Unlike
    /// the folder scope this is a view filter, not a setting, so it resets to
    /// "All accounts" each time the gallery is opened.
    account_filter: Option<u32>,
    /// Every account and the folders it offers, as fed by the app.
    accounts: Vec<GalleryAccount>,
    /// Pull from Archive folders (persisted master switch).
    include_archive: bool,
    /// Pull from folders that are neither Inbox nor Archive (persisted).
    include_other: bool,
    /// The folders the user ticked or unticked by hand, overriding
    /// [`kind_default`]: account id -> path -> on. Persisted.
    overrides: HashMap<u32, HashMap<String, bool>>,
    /// Effective per-folder verdict, recomputed from the master switches and
    /// `overrides` whenever either changes, so filtering is a lookup.
    included: HashMap<u32, HashMap<String, bool>>,
    /// Debounce for the size slider — one rebuild after the drag settles.
    resize_timer: Option<glib::SourceId>,
    /// Debounce for the search box — one query after the typing settles.
    query_timer: Option<glib::SourceId>,
    flow: gtk::FlowBox,
    /// The table view's rows (the grid's sibling stack page).
    table: gtk::ListBox,
    /// The folder list inside the footer's "Folders" popover, rebuilt whenever
    /// the account/folder set changes.
    folder_list: gtk::ListBox,
    /// The account filter dropdown's model: "All accounts" then one row per
    /// account, in `accounts` order.
    account_names: gtk::StringList,
    /// The component's root widget, used to anchor the right-click context menu.
    root: gtk::Widget,
    /// Files being deleted from the server (#289), drawn pale red with
    /// "Deleting…" until the server answers.
    deleting: Vec<FileKey>,
}

/// One attachment in the gallery: account, folder, message UID, file name.
pub type FileKey = (u32, String, u32, String);

fn key_of(item: &GalleryItem) -> FileKey {
    (
        item.account_id,
        item.folder_path.clone(),
        item.uid,
        item.name.clone(),
    )
}

#[derive(Debug)]
pub enum GalleryInput {
    /// A page came back from the cache: `offset` 0 replaces the list, anything
    /// else appends. `total` is how many the query matches altogether.
    Page {
        items: Vec<GalleryItem>,
        total: u32,
        offset: u32,
    },
    SetLoading(bool),
    /// The scroll came near the end — ask for the next page if there is one.
    LoadMore,
    /// The scan described more attachments. Refreshes the view only while the
    /// user is still on the first page, so a scan landing mid-scroll cannot
    /// pull the ground out from under them.
    ScanProgress,
    /// How many messages the scan still has to describe, across every folder in
    /// scope; 0 once the archive is fully indexed.
    ScanStatus(u32),
    /// A file the gallery knew of but had not downloaded is being fetched, so
    /// the lightbox can show a spinner instead of an empty frame.
    SetFetching(bool),
    /// A message's attachments arrived: fill in the bytes for every loaded row
    /// of that message, so its thumbnail and preview appear in place.
    Fetched {
        account_id: u32,
        path: String,
        uid: u32,
        items: Vec<crate::models::Attachment>,
    },
    /// Filter the grid to items matching this search text (sender, subject,
    /// filename, folder, and type keywords like "pdf" or "spreadsheet").
    /// Debounced — the query itself runs on [`GalleryInput::ApplyQuery`].
    SetQuery(String),
    /// The typing settled — run the search.
    ApplyQuery,
    /// Re-sort the grid; the value is the sort dropdown's selected row index.
    SetSort(u32),
    /// A table column header was clicked: sort by that column, or flip its
    /// direction when it is already the active column (0 name, 1 sender,
    /// 2 date, 3 size, 4 type).
    SortColumn(u8),
    /// Switch between the thumbnail grid and the table.
    SetViewTable(bool),
    /// The footer size slider moved (grid thumbnail width, px).
    SetThumbWidth(f64),
    /// The size slider settled — rebuild the grid at the new width.
    ApplyThumbWidth,
    /// Show only one type bucket (the footer type dropdown's row; 0 = all).
    SetTypeFilter(u32),
    /// The accounts and folders the gallery may draw on, sent by the app when
    /// the gallery opens. Rebuilds the folder list and the account dropdown.
    SetAccounts(Vec<GalleryAccount>),
    /// Show only one account (the footer account dropdown's row; 0 = all).
    SetAccountFilter(u32),
    /// Master switch: pull from Archive folders.
    SetIncludeArchive(bool),
    /// Master switch: pull from folders that are neither Inbox nor Archive.
    SetIncludeOther(bool),
    /// One folder was ticked or unticked in the folder list.
    SetFolder {
        account_id: u32,
        path: String,
        on: bool,
    },
    /// A grid cell was activated (single click) — open the lightbox on that item.
    Activate(u32),
    Prev,
    Next,
    ClosePreview,
    /// Open the current (previewed) item's file in its default application.
    OpenCurrent,
    /// Jump to the current (previewed) item's source message.
    GoToCurrent,
    /// Open item `index` externally (double-click / context menu / lightbox).
    OpenItem(usize),
    /// Save item `index` to a file the user picks.
    DownloadItem(usize),
    /// Jump to item `index`'s source message.
    GoToItem(usize),
    /// Remove item `index` from its message on the server (#289).
    DeleteItem(usize),
    /// A file is being deleted from the server (`true`), or the server
    /// refused (`false`).
    SetDeleting(FileKey, bool),
    /// An attachment was removed from a message on the server: its item
    /// fades out and leaves the view (the next load reads the cache as it
    /// is now).
    Removed(FileKey),
    /// The fade is over: take the item away.
    RemoveNow(FileKey),
    /// Showcase only (HYLKI_SHOWCASE_DELETE_ATTACHMENT=gallery:<name>):
    /// Delete from Server… on the first file of that name.
    ShowcaseDelete(String),
    /// A cell was double-clicked: open it externally, closing any preview.
    OpenExternal(usize),
    /// Right-click on cell `index` at `(x, y)` (cell-relative) — show its menu there.
    ContextMenu {
        index: usize,
        x: f64,
        y: f64,
    },
    /// A lightbox-size PDF render finished (keyed by content hash) — show it
    /// if that PDF is still the one being previewed.
    PreviewRendered(u64),
    /// Showcase only (HYLKI_SHOWCASE_GALLERY_FOLDERS): drop the footer's
    /// folder popover open so a capture can see it.
    ShowcaseFolders,
    /// Showcase only (HYLKI_SHOWCASE_GALLERY_SEARCH): focus the search box and
    /// put text in it, as typing would, then report whether the box still has
    /// the focus once the search has run. Guards the bug where a search that
    /// matched nothing hid the toolbar the box lives in.
    ShowcaseSearch(String),
}

#[derive(Debug)]
pub enum GalleryOutput {
    /// Open the source message of an attachment in the reader.
    OpenMessage {
        account_id: u32,
        folder_path: String,
        uid: u32,
    },
    /// Run this query against the cache and send the page back.
    Load(GalleryRequest),
    /// Fetch an attachment whose bytes were never downloaded, so it can be
    /// opened or previewed.
    Fetch {
        account_id: u32,
        folder_path: String,
        uid: u32,
    },
    /// Take an attachment out of its message on the server (#289); the app
    /// asks first.
    DeleteFromServer {
        account_id: u32,
        folder_path: String,
        uid: u32,
        name: String,
        size: u64,
    },
}

#[relm4::component(pub)]
impl Component for AttachmentsGallery {
    type Init = ();
    type Input = GalleryInput;
    type Output = GalleryOutput;
    type CommandOutput = ();

    view! {
        gtk::Overlay {
            add_css_class: "attachments-gallery",

            // Base layer: a search/sort toolbar above the scrolling grid.
            #[wrap(Some)]
            set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                gtk::Box {
                    add_css_class: "gallery-toolbar",
                    set_spacing: 8,
                    #[watch]
                    set_visible: model.show_chrome(),

                    #[name = "search_entry"]
                    gtk::SearchEntry {
                        set_hexpand: true,
                        set_placeholder_text: Some(i18n("Search by sender, subject, type, filename…").as_str()),
                        connect_search_changed[sender] => move |e| {
                            sender.input(GalleryInput::SetQuery(e.text().to_string()));
                        },
                    },
                },

                gtk::Stack {
                    set_vexpand: true,
                    #[watch]
                    set_visible_child_name: model.page(),

                    add_named[Some("loading")] = &gtk::Box {
                        set_halign: gtk::Align::Center,
                        set_valign: gtk::Align::Center,
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 14,
                        gtk::Spinner { set_spinning: true, set_width_request: 36, set_height_request: 36 },
                        gtk::Label { set_label: &i18n("Loading attachments…"), add_css_class: "dim-label" },
                    },

                    add_named[Some("empty")] = &adw::StatusPage {
                        set_icon_name: Some("mail-attachment-symbolic"),
                        set_title: &i18n("No attachments"),
                        set_description: Some(i18n("Attachments from your inboxes will appear here.").as_str()),
                    },

                    add_named[Some("noresults")] = &adw::StatusPage {
                        set_icon_name: Some("system-search-symbolic"),
                        set_title: &i18n("No matching attachments"),
                        set_description: Some(i18n("Try a different search, or check which accounts and folders the footer is pulling from.").as_str()),
                    },

                    add_named[Some("grid")] = &gtk::ScrolledWindow {
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    set_vexpand: true,
                    connect_edge_reached[sender] => move |_, pos| {
                        if pos == gtk::PositionType::Bottom {
                            sender.input(GalleryInput::LoadMore);
                        }
                    },
                    #[wrap(Some)]
                    set_vadjustment = &gtk::Adjustment {
                        connect_value_changed[sender] => move |a| {
                            if near_end(a) {
                                sender.input(GalleryInput::LoadMore);
                            }
                        },
                    },

                    #[local_ref]
                    flow -> gtk::FlowBox {
                        set_valign: gtk::Align::Start,
                        set_max_children_per_line: 8,
                        set_min_children_per_line: 3,
                        set_row_spacing: 14,
                        set_column_spacing: 14,
                        set_homogeneous: true,
                        set_selection_mode: gtk::SelectionMode::None,
                        set_activate_on_single_click: true,
                        add_css_class: "gallery-flow",
                        connect_child_activated[sender] => move |_, child| {
                            sender.input(GalleryInput::Activate(child.index() as u32));
                        },
                    },
                    },

                    // Table view: fixed sortable column headers over the rows.
                    add_named[Some("table")] = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,

                        gtk::Box {
                            add_css_class: "gallery-table-header",
                            set_spacing: 10,

                            // Aligns the headers with the rows' leading thumbnail.
                            gtk::Box { set_width_request: 28 },
                            gtk::Button {
                                add_css_class: "flat",
                                set_hexpand: true,
                                connect_clicked => GalleryInput::SortColumn(0),
                                gtk::Label {
                                    #[watch]
                                    set_label: &column_header(&i18n("Name"), model.sort, GallerySort::Name, GallerySort::NameDesc),
                                    set_xalign: 0.0,
                                },
                            },
                            gtk::Button {
                                add_css_class: "flat",
                                set_width_request: 170,
                                set_hexpand: false,
                                connect_clicked => GalleryInput::SortColumn(1),
                                gtk::Label {
                                    #[watch]
                                    set_label: &column_header(&i18n("Sender"), model.sort, GallerySort::Sender, GallerySort::SenderDesc),
                                    set_xalign: 0.0,
                                },
                            },
                            gtk::Button {
                                add_css_class: "flat",
                                set_width_request: 90,
                                set_hexpand: false,
                                connect_clicked => GalleryInput::SortColumn(4),
                                gtk::Label {
                                    #[watch]
                                    set_label: &column_header(&i18n("Type"), model.sort, GallerySort::Type, GallerySort::TypeDesc),
                                    set_xalign: 0.0,
                                },
                            },
                            gtk::Button {
                                add_css_class: "flat",
                                set_width_request: 110,
                                set_hexpand: false,
                                connect_clicked => GalleryInput::SortColumn(2),
                                gtk::Label {
                                    #[watch]
                                    set_label: &column_header(&i18n("Date"), model.sort, GallerySort::Oldest, GallerySort::Newest),
                                    set_xalign: 0.0,
                                },
                            },
                            // Only Name expands, as in the rows: a header
                            // that also took spare width (a child's hexpand
                            // propagates to its button) would shift every
                            // column between them off its cells.
                            gtk::Button {
                                add_css_class: "flat",
                                set_width_request: 90,
                                set_hexpand: false,
                                connect_clicked => GalleryInput::SortColumn(3),
                                gtk::Label {
                                    #[watch]
                                    set_label: &column_header(&i18n("Size"), model.sort, GallerySort::Smallest, GallerySort::Largest),
                                    set_xalign: 1.0,
                                },
                            },
                            // Aligns with the rows' trailing actions column.
                            gtk::Box { set_width_request: TABLE_ACTIONS_WIDTH },
                        },

                        gtk::ScrolledWindow {
                            set_hscrollbar_policy: gtk::PolicyType::Never,
                            set_vexpand: true,
                            connect_edge_reached[sender] => move |_, pos| {
                                if pos == gtk::PositionType::Bottom {
                                    sender.input(GalleryInput::LoadMore);
                                }
                            },
                            #[wrap(Some)]
                            set_vadjustment = &gtk::Adjustment {
                                connect_value_changed[sender] => move |a| {
                                    if near_end(a) {
                                        sender.input(GalleryInput::LoadMore);
                                    }
                                },
                            },

                            #[local_ref]
                            table -> gtk::ListBox {
                                set_selection_mode: gtk::SelectionMode::None,
                                set_activate_on_single_click: true,
                                add_css_class: "gallery-table",
                                connect_row_activated[sender] => move |_, row| {
                                    sender.input(GalleryInput::Activate(row.index() as u32));
                                },
                            },
                        },
                    },
                },

                // Footer: view toggle, filtering/ordering, size, count — the
                // gallery's controls in one place, out of the content's way.
                gtk::ActionBar {
                    add_css_class: "gallery-footer",
                    #[watch]
                    set_revealed: model.show_chrome(),

                    pack_start = &gtk::Box {
                        add_css_class: "linked",

                        gtk::ToggleButton {
                            set_icon_name: "view-grid-symbolic",
                            set_tooltip_text: Some(i18n("Thumbnail grid").as_str()),
                            #[watch]
                            #[block_signal(grid_toggle)]
                            set_active: !model.view_table,
                            connect_clicked[sender] => move |_| {
                                sender.input(GalleryInput::SetViewTable(false));
                            } @grid_toggle,
                        },
                        gtk::ToggleButton {
                            set_icon_name: "view-list-bullet-symbolic",
                            set_tooltip_text: Some(i18n("Table").as_str()),
                            #[watch]
                            #[block_signal(table_toggle)]
                            set_active: model.view_table,
                            connect_clicked[sender] => move |_| {
                                sender.input(GalleryInput::SetViewTable(true));
                            } @table_toggle,
                        },
                    },

                    // Scope: which account and which folders feed the gallery.
                    #[name = "account_dropdown"]
                    pack_start = &gtk::DropDown {
                        set_tooltip_text: Some(i18n("Show only this account").as_str()),
                        // Pointless with one account, and it would be the only
                        // control there that could never change anything.
                        #[watch]
                        set_visible: model.accounts.len() > 1,
                        set_model: Some(&model.account_names),
                        connect_selected_notify[sender] => move |d| {
                            sender.input(GalleryInput::SetAccountFilter(d.selected()));
                        },
                    },

                    #[name = "folders_button"]
                    pack_start = &gtk::MenuButton {
                        set_icon_name: "folder-symbolic",
                        set_tooltip_text: Some(i18n("Folders to pull from").as_str()),

                        #[wrap(Some)]
                        set_popover = &gtk::Popover {
                            add_css_class: "gallery-folders-popover",

                            #[name = "folders_box"]
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 10,
                                set_width_request: 320,

                                gtk::ListBox {
                                    add_css_class: "boxed-list",
                                    set_selection_mode: gtk::SelectionMode::None,

                                    #[name = "archive_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Include Archive"),
                                        #[watch]
                                        #[block_signal(archive_toggle)]
                                        set_active: model.include_archive,
                                        connect_active_notify[sender] => move |r| {
                                            sender.input(GalleryInput::SetIncludeArchive(r.is_active()));
                                        } @archive_toggle,
                                    },
                                    #[name = "other_row"]
                                    adw::SwitchRow {
                                        set_title: &i18n("Include other folders"),
                                        set_subtitle: &i18n("Everything that is not an inbox or an archive"),
                                        #[watch]
                                        #[block_signal(other_toggle)]
                                        set_active: model.include_other,
                                        connect_active_notify[sender] => move |r| {
                                            sender.input(GalleryInput::SetIncludeOther(r.is_active()));
                                        } @other_toggle,
                                    },
                                },

                                gtk::Label {
                                    set_label: &i18n("Pulling attachments from"),
                                    set_xalign: 0.0,
                                    add_css_class: "heading",
                                },

                                gtk::ScrolledWindow {
                                    set_hscrollbar_policy: gtk::PolicyType::Never,
                                    set_propagate_natural_height: true,
                                    set_max_content_height: 380,

                                    #[local_ref]
                                    folder_list -> gtk::ListBox {
                                        add_css_class: "boxed-list",
                                        set_selection_mode: gtk::SelectionMode::None,
                                    },
                                },
                            },
                        },
                    },

                    pack_start = &gtk::DropDown {
                        set_tooltip_text: Some(i18n("Show only this type").as_str()),
                        #[wrap(Some)]
                        set_model = &gtk::StringList::new(&[
                            i18n("All types").as_str(),
                            i18n("Images").as_str(),
                            i18n("PDFs").as_str(),
                            i18n("Documents").as_str(),
                            i18n("Archives").as_str(),
                            i18n("Audio & Video").as_str(),
                            i18n("Other").as_str(),
                        ]),
                        connect_selected_notify[sender] => move |d| {
                            sender.input(GalleryInput::SetTypeFilter(d.selected()));
                        },
                    },

                    #[name = "sort_dropdown"]
                    pack_start = &gtk::DropDown {
                        set_tooltip_text: Some(i18n("Sort").as_str()),
                        set_selected: model.sort.index(),
                        #[wrap(Some)]
                        set_model = &gtk::StringList::new(&[
                            i18n("Newest first").as_str(),
                            i18n("Oldest first").as_str(),
                            i18n("Name (A–Z)").as_str(),
                            i18n("Name (Z–A)").as_str(),
                            i18n("Sender (A–Z)").as_str(),
                            i18n("Sender (Z–A)").as_str(),
                            i18n("Largest first").as_str(),
                            i18n("Smallest first").as_str(),
                            i18n("Type (A–Z)").as_str(),
                            i18n("Type (Z–A)").as_str(),
                        ]),
                        connect_selected_notify[sender] => move |d| {
                            sender.input(GalleryInput::SetSort(d.selected()));
                        },
                    },

                    pack_end = &gtk::Label {
                        add_css_class: "dim-label",
                        #[watch]
                        set_label: &count_text(model.all_items.len(), model.total as usize),
                    },

                    // What the network is doing, in the one place the gallery
                    // keeps its status: a page on its way, or the scan still
                    // working back through the archive.
                    pack_end = &gtk::Box {
                        set_spacing: 6,
                        set_valign: gtk::Align::Center,
                        #[watch]
                        set_visible: model.loading_more || model.scan_remaining > 0,

                        gtk::Spinner {
                            #[watch]
                            set_spinning: model.loading_more || model.scan_remaining > 0,
                            set_width_request: 16,
                            set_height_request: 16,
                        },
                        gtk::Label {
                            add_css_class: "dim-label",
                            #[watch]
                            set_label: &scan_text(model.loading_more, model.scan_remaining),
                        },
                    },

                    pack_end = &gtk::Scale {
                        set_range: (140.0, 380.0),
                        set_value: model.thumb_width as f64,
                        set_increments: (10.0, 40.0),
                        set_width_request: 140,
                        set_draw_value: false,
                        set_tooltip_text: Some(i18n("Thumbnail size").as_str()),
                        #[watch]
                        set_visible: !model.view_table,
                        connect_value_changed[sender] => move |s| {
                            sender.input(GalleryInput::SetThumbWidth(s.value()));
                        },
                    },
                },
            },

            // Lightbox overlay, shown while previewing an item.
            add_overlay = &gtk::Box {
                add_css_class: "gallery-lightbox",
                set_orientation: gtk::Orientation::Vertical,
                #[watch]
                set_visible: model.preview.is_some(),

                // Top bar: title + close.
                gtk::CenterBox {
                    add_css_class: "gallery-lightbox-bar",
                    #[wrap(Some)]
                    set_start_widget = &gtk::Label {
                        #[watch]
                        set_label: &model.current().map(|i| i.name.clone()).unwrap_or_default(),
                        set_ellipsize: gtk::pango::EllipsizeMode::Middle,
                        set_halign: gtk::Align::Start,
                        add_css_class: "gallery-lightbox-title",
                    },
                    #[wrap(Some)]
                    set_end_widget = &gtk::Button {
                        set_icon_name: "window-close-symbolic",
                        set_tooltip_text: Some(i18n("Close").as_str()),
                        add_css_class: "circular",
                        add_css_class: "flat",
                        connect_clicked => GalleryInput::ClosePreview,
                    },
                },

                // Middle: prev  |  image/icon  |  next.
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_vexpand: true,
                    set_spacing: 8,

                    gtk::Button {
                        set_icon_name: "go-previous-symbolic",
                        set_tooltip_text: Some(i18n("Previous").as_str()),
                        set_valign: gtk::Align::Center,
                        add_css_class: "circular",
                        add_css_class: "osd",
                        #[watch]
                        set_sensitive: model.all_items.len() > 1,
                        connect_clicked => GalleryInput::Prev,
                    },

                    #[name = "preview_stack"]
                    gtk::Stack {
                        set_hexpand: true,
                        set_vexpand: true,
                        #[watch]
                        set_visible_child_name: if model.preview_texture.is_some() {
                            "image"
                        } else if model.current().is_some_and(|i| is_pdf_name(&i.name) && i.data.is_some()) {
                            // A PDF whose full-size render is still on its way.
                            "rendering"
                        } else {
                            "file"
                        },

                        #[name = "preview_picture"]
                        add_named[Some("image")] = &gtk::Picture {
                            set_can_shrink: true,
                            set_content_fit: gtk::ContentFit::Contain,
                            #[watch]
                            set_paintable: model.preview_texture.as_ref(),
                        },

                        add_named[Some("rendering")] = &gtk::Box {
                            set_halign: gtk::Align::Center,
                            set_valign: gtk::Align::Center,
                            gtk::Spinner {
                                set_spinning: true,
                                set_width_request: 36,
                                set_height_request: 36,
                            },
                        },

                        add_named[Some("file")] = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_halign: gtk::Align::Center,
                            set_valign: gtk::Align::Center,
                            set_spacing: 12,
                            gtk::Image {
                                #[watch]
                                set_icon_name: model.current().map(|i| icon_for(&i.name)),
                                set_pixel_size: 96,
                                #[watch]
                                set_css_classes: &[
                                    "gallery-file-icon",
                                    model.current().map(|i| icon_color_class(&i.name)).unwrap_or("ftype-generic"),
                                ],
                            },
                            gtk::Label {
                                #[watch]
                                set_label: &model.current().map(|i| i.name.clone()).unwrap_or_default(),
                                set_ellipsize: gtk::pango::EllipsizeMode::Middle,
                                add_css_class: "title-3",
                            },
                        },
                    },

                    gtk::Button {
                        set_icon_name: "go-next-symbolic",
                        set_tooltip_text: Some(i18n("Next").as_str()),
                        set_valign: gtk::Align::Center,
                        add_css_class: "circular",
                        add_css_class: "osd",
                        #[watch]
                        set_sensitive: model.all_items.len() > 1,
                        connect_clicked => GalleryInput::Next,
                    },
                },

                // Bottom bar: caption + actions.
                gtk::CenterBox {
                    add_css_class: "gallery-lightbox-bar",
                    #[wrap(Some)]
                    set_start_widget = &gtk::Label {
                        #[watch]
                        set_label: &model.current().map(caption).unwrap_or_default(),
                        set_halign: gtk::Align::Start,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        add_css_class: "dim-label",
                    },
                    #[wrap(Some)]
                    set_end_widget = &gtk::Box {
                        set_spacing: 8,
                        gtk::Button {
                            set_label: &i18n("Open"),
                            set_tooltip_text: Some(i18n("Open in the default app").as_str()),
                            #[watch]
                            set_sensitive: model.current().is_some_and(|i| i.data.is_some()),
                            connect_clicked => GalleryInput::OpenCurrent,
                        },
                        gtk::Button {
                            set_label: &i18n("Go to Message"),
                            add_css_class: "suggested-action",
                            connect_clicked => GalleryInput::GoToCurrent,
                        },
                    },
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let (view_table, thumb_width, sort_index) = crate::config::load_gallery_view();
        let (include_archive, include_other, saved_folders) = crate::config::load_gallery_scope();
        let mut overrides: HashMap<u32, HashMap<String, bool>> = HashMap::new();
        for (id, path, on) in saved_folders {
            overrides.entry(id).or_default().insert(path, on);
        }
        let model = AttachmentsGallery {
            all_items: Vec::new(),
            total: 0,
            loading_more: false,
            scan_remaining: 0,
            fetching: false,
            query: String::new(),
            sort: GallerySort::from_index(sort_index),
            preview: None,
            preview_texture: None,
            loading: false,
            view_table,
            thumb_width,
            type_filter: 0,
            account_filter: None,
            accounts: Vec::new(),
            include_archive,
            include_other,
            overrides,
            included: HashMap::new(),
            resize_timer: None,
            query_timer: None,
            flow: gtk::FlowBox::new(),
            table: gtk::ListBox::new(),
            folder_list: gtk::ListBox::new(),
            account_names: gtk::StringList::new(&[i18n("All accounts").as_str()]),
            root: root.clone().upcast(),
            deleting: Vec::new(),
        };
        let flow = &model.flow;
        let table = &model.table;
        let folder_list = &model.folder_list;
        let widgets = view_output!();

        // Double-clicking the preview opens the document in its external app.
        let dbl = gtk::GestureClick::new();
        dbl.set_button(gtk::gdk::BUTTON_PRIMARY);
        let ds = sender.clone();
        dbl.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                ds.input(GalleryInput::OpenCurrent);
            }
        });
        widgets.preview_picture.add_controller(dbl);

        // Arrow keys navigate the lightbox; Escape closes it.
        let key = gtk::EventControllerKey::new();
        let ks = sender.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            match keyval {
                gdk::Key::Left => ks.input(GalleryInput::Prev),
                gdk::Key::Right => ks.input(GalleryInput::Next),
                gdk::Key::Escape => ks.input(GalleryInput::ClosePreview),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        root.add_controller(key);

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            GalleryInput::Page {
                items,
                total,
                offset,
            } => {
                if offset == 0 {
                    // The first page of a fresh query replaces everything.
                    self.all_items = items;
                } else if offset as usize == self.all_items.len() {
                    self.all_items.extend(items);
                } else {
                    // A page for a query that has since changed: its offset no
                    // longer lines up with what is loaded, so appending it
                    // would interleave rows from two different orderings. Drop
                    // it and wait for the page the current query asked for.
                    self.loading_more = false;
                    self.update_view(widgets, sender);
                    return;
                }
                self.total = total;
                self.loading = false;
                self.loading_more = false;
                self.rebuild_view(&sender);
            }
            GalleryInput::LoadMore => self.load_more(&sender),
            GalleryInput::ScanProgress => {
                if self.all_items.len() <= PAGE_SIZE as usize {
                    self.reload(&sender);
                }
            }
            GalleryInput::ScanStatus(remaining) => self.scan_remaining = remaining,
            GalleryInput::SetFetching(on) => self.fetching = on,
            GalleryInput::Fetched {
                account_id,
                path,
                uid,
                items,
            } => {
                let mut touched = false;
                for row in self.all_items.iter_mut().filter(|i| {
                    i.account_id == account_id
                        && i.folder_path == path
                        && i.uid == uid
                        && i.data.is_none()
                }) {
                    // Match on the name rather than the index: the scan numbers
                    // parts as the server describes them and the fetch numbers
                    // them as the parser finds them, which need not agree.
                    if let Some(found) = items.iter().find(|a| a.name == row.name) {
                        row.data = Some(found.data.clone());
                        row.size = found.data.len() as u64;
                        row.downloaded = true;
                        touched = true;
                    }
                }
                if touched {
                    self.rebuild_view(&sender);
                    if self.preview.is_some() {
                        self.refresh_preview(&sender);
                    }
                }
            }
            GalleryInput::SetDeleting(key, on) => {
                self.deleting.retain(|k| *k != key);
                if on {
                    self.deleting.push(key);
                }
                self.rebuild_view(&sender);
            }
            GalleryInput::Removed(key) => {
                let at = self.all_items.iter().position(|i| key_of(i) == key);
                let shown: Option<gtk::Widget> = at.and_then(|at| {
                    if self.view_table {
                        self.table.row_at_index(at as i32).map(|r| r.upcast())
                    } else {
                        self.flow.child_at_index(at as i32).map(|c| c.upcast())
                    }
                });
                let s = sender.clone();
                let done = move || s.input(GalleryInput::RemoveNow(key.clone()));
                match shown {
                    Some(w) => fade_out(&w, done),
                    None => done(),
                }
            }
            GalleryInput::RemoveNow(key) => {
                self.deleting.retain(|k| *k != key);
                // In place, rather than a reload that would lose the user's
                // place in a long list.
                let Some(at) = self.all_items.iter().position(|i| key_of(i) == key) else {
                    self.rebuild_view(&sender);
                    return;
                };
                self.all_items.remove(at);
                self.total = self.total.saturating_sub(1);
                self.preview = match self.preview {
                    Some(p) if p == at => None,
                    Some(p) if p > at => Some(p - 1),
                    p => p,
                };
                self.rebuild_view(&sender);
            }
            GalleryInput::SetQuery(q) => {
                if self.query != q {
                    self.query = q;
                    if let Some(t) = self.query_timer.take() {
                        t.remove();
                    }
                    let s = sender.clone();
                    self.query_timer = Some(glib::timeout_add_local_once(
                        std::time::Duration::from_millis(SEARCH_DEBOUNCE_MS as u64),
                        move || s.input(GalleryInput::ApplyQuery),
                    ));
                }
            }
            GalleryInput::ApplyQuery => {
                self.query_timer = None;
                self.reload(&sender);
            }
            GalleryInput::SetSort(i) => {
                let sort = GallerySort::from_index(i);
                if self.sort != sort {
                    self.sort = sort;
                    crate::config::save_gallery_sort(i);
                    self.reload(&sender);
                }
            }
            GalleryInput::SortColumn(col) => {
                // First click sorts a column its natural way; a second flips it.
                let sort = match (col, self.sort) {
                    (0, GallerySort::Name) => GallerySort::NameDesc,
                    (0, _) => GallerySort::Name,
                    (1, GallerySort::Sender) => GallerySort::SenderDesc,
                    (1, _) => GallerySort::Sender,
                    (2, GallerySort::Newest) => GallerySort::Oldest,
                    (2, _) => GallerySort::Newest,
                    (3, GallerySort::Largest) => GallerySort::Smallest,
                    (3, _) => GallerySort::Largest,
                    (4, GallerySort::Type) => GallerySort::TypeDesc,
                    (4, _) | (_, _) => GallerySort::Type,
                };
                // The dropdown follows; its notify handler sees the same value
                // and does nothing further.
                widgets.sort_dropdown.set_selected(sort.index());
                if self.sort != sort {
                    self.sort = sort;
                    crate::config::save_gallery_sort(sort.index());
                    self.reload(&sender);
                }
            }
            GalleryInput::SetViewTable(table) => {
                if self.view_table != table {
                    self.view_table = table;
                    self.rebuild_view(&sender);
                    crate::config::save_gallery_table_view(table);
                }
            }
            GalleryInput::SetThumbWidth(v) => {
                let width = (v.round() as i32).clamp(140, 380);
                if width != self.thumb_width {
                    self.thumb_width = width;
                    // One rebuild once the drag settles, not one per pixel.
                    if let Some(id) = self.resize_timer.take() {
                        id.remove();
                    }
                    let s = sender.clone();
                    self.resize_timer = Some(glib::timeout_add_local_once(
                        std::time::Duration::from_millis(150),
                        move || s.input(GalleryInput::ApplyThumbWidth),
                    ));
                }
            }
            GalleryInput::ApplyThumbWidth => {
                self.resize_timer = None;
                if !self.view_table {
                    self.rebuild_view(&sender);
                }
                crate::config::save_gallery_thumb_width(self.thumb_width);
            }
            GalleryInput::SetTypeFilter(bucket) => {
                if self.type_filter != bucket {
                    self.type_filter = bucket;
                    self.reload(&sender);
                }
            }
            GalleryInput::SetAccounts(accounts) => {
                self.accounts = accounts;
                self.rebuild_account_names();
                // The dropdown's rows just changed under it; start from "All
                // accounts" rather than whichever row the old list had there.
                self.account_filter = None;
                widgets.account_dropdown.set_selected(0);
                self.recompute_scope();
                self.rebuild_folder_list(&sender);
                self.reload(&sender);
            }
            GalleryInput::SetAccountFilter(row) => {
                // Row 0 is "All accounts"; the rest index `accounts` in order.
                let filter = (row as usize)
                    .checked_sub(1)
                    .and_then(|i| self.accounts.get(i))
                    .map(|a| a.id);
                if self.account_filter != filter {
                    self.account_filter = filter;
                    // Keeps the dropdown honest when the message came from
                    // somewhere other than the dropdown itself. Setting it to
                    // the row it already holds emits nothing, so this cannot
                    // loop back round.
                    widgets.account_dropdown.set_selected(row);
                    self.reload(&sender);
                }
            }
            GalleryInput::SetIncludeArchive(on) => {
                if self.include_archive != on {
                    self.include_archive = on;
                    self.scope_changed(&sender);
                }
            }
            GalleryInput::SetIncludeOther(on) => {
                if self.include_other != on {
                    self.include_other = on;
                    self.scope_changed(&sender);
                }
            }
            GalleryInput::SetFolder {
                account_id,
                path,
                on,
            } => {
                let entry = self.overrides.entry(account_id).or_default();
                if entry.insert(path, on) != Some(on) {
                    // The list already draws this tick; rebuilding it here
                    // would tear down the check button mid-signal.
                    self.recompute_scope();
                    self.save_scope();
                    self.reload(&sender);
                }
            }
            GalleryInput::SetLoading(on) => self.loading = on,
            GalleryInput::Activate(i) => {
                if (i as usize) < self.all_items.len() {
                    self.preview = Some(i as usize);
                    self.refresh_preview(&sender);
                }
            }
            GalleryInput::Prev => self.step(-1, &sender),
            GalleryInput::Next => self.step(1, &sender),
            GalleryInput::ClosePreview => {
                self.preview = None;
                self.preview_texture = None;
            }
            GalleryInput::PreviewRendered(key) => {
                // Only meaningful if the rendered PDF is still on show.
                let still_current = self
                    .current()
                    .and_then(|i| i.data.as_ref())
                    .is_some_and(|d| thumb_cache_key(d) == key);
                if still_current {
                    self.refresh_preview(&sender);
                }
            }
            GalleryInput::OpenCurrent => {
                if let Some(i) = self.preview {
                    self.open_item(i, &sender);
                }
            }
            GalleryInput::GoToCurrent => {
                if let Some(i) = self.preview {
                    self.goto_item(i, &sender);
                }
            }
            GalleryInput::OpenItem(i) => self.open_item(i, &sender),
            GalleryInput::DownloadItem(i) => self.download_item(i, &sender),
            GalleryInput::GoToItem(i) => self.goto_item(i, &sender),
            GalleryInput::ShowcaseDelete(name) => {
                match self.all_items.iter().position(|i| i.name == name) {
                    Some(i) => sender.input(GalleryInput::DeleteItem(i)),
                    None => tracing::warn!("showcase: no {name:?} in the gallery"),
                }
            }
            GalleryInput::DeleteItem(i) => {
                if let Some(item) = self.item_at(i) {
                    let _ = sender.output(GalleryOutput::DeleteFromServer {
                        account_id: item.account_id,
                        folder_path: item.folder_path.clone(),
                        uid: item.uid,
                        name: item.name.clone(),
                        size: item.size,
                    });
                }
            }
            GalleryInput::OpenExternal(i) => {
                // Double-click: skip/close the preview and open the file directly.
                self.preview = None;
                self.open_item(i, &sender);
            }
            GalleryInput::ContextMenu { index, x, y } => {
                self.show_context_menu(index, x, y, &sender)
            }
            GalleryInput::ShowcaseSearch(text) => {
                let entry = widgets.search_entry.clone();
                entry.grab_focus();
                // Goes through `search-changed`, so this is the same path a
                // typed character takes, debounce and all.
                entry.set_text(&text);
                glib::timeout_add_seconds_local_once(2, move || {
                    // `has_focus` also needs the window to be the active one,
                    // which it need not be under a private bus; `is_focus` is
                    // the question actually being asked — is this still the
                    // toplevel's focus widget.
                    let focus = entry
                        .root()
                        .and_downcast::<gtk::Window>()
                        .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w));
                    // A GtkSearchEntry is composite: the focus widget is the
                    // GtkText inside it, so ask whether the focus is anywhere
                    // within the entry rather than whether it *is* the entry.
                    let kept = focus.as_ref().is_some_and(|f| {
                        f == entry.upcast_ref::<gtk::Widget>() || f.is_ancestor(&entry)
                    });
                    tracing::info!(
                        target: "hylki::showcase",
                        "gallery search focus: kept={kept} holder={} toolbar_visible={} text={:?}",
                        focus.map(|w| w.type_().name().to_string()).unwrap_or_else(|| "none".into()),
                        entry.parent().is_some_and(|p| p.is_visible()),
                        entry.text()
                    );
                });
            }
            GalleryInput::ShowcaseFolders => {
                widgets.folders_button.popup();
                // A popover is its own surface, so the window snapshot never
                // has it: capture its contents directly, as the context menu
                // does (see `ui::context_menu`).
                if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
                    let content = widgets.folders_box.clone();
                    glib::timeout_add_seconds_local_once(1, move || {
                        crate::app::showcase_capture(content.upcast_ref(), &path);
                    });
                }
            }
        }
        self.update_view(widgets, sender);
    }
}

impl AttachmentsGallery {
    /// What the gallery holds in RAM, for the memory section of an export:
    /// items listed, of which carrying their file bytes, and those bytes.
    pub fn memory_stats(&self) -> (usize, usize, u64) {
        let with_data = self.all_items.iter().filter(|i| i.data.is_some()).count();
        let bytes = self
            .all_items
            .iter()
            .filter_map(|i| i.data.as_ref())
            .map(|d| d.len() as u64)
            .sum();
        (self.all_items.len(), with_data, bytes)
    }

    fn page(&self) -> &'static str {
        if self.loading && self.all_items.is_empty() {
            "loading"
        } else if !self.all_items.is_empty() {
            if self.view_table {
                "table"
            } else {
                "grid"
            }
        } else if self.is_narrowed() {
            // Nothing matched, but something would without the search or the
            // filters — a different message from an empty gallery.
            "noresults"
        } else {
            "empty"
        }
    }

    /// Whether the search bar and the footer are on show. They stay up while a
    /// search narrows the gallery to nothing: the entry being typed into lives
    /// in the toolbar, and a toolbar that hid itself the moment a query matched
    /// nothing would take the focus with it mid-word.
    fn show_chrome(&self) -> bool {
        !self.all_items.is_empty() || self.is_narrowed() || self.scan_remaining > 0
    }

    /// Whether the user has narrowed the view at all. Used only to choose
    /// between the two empty states, so an archive with no attachments in it
    /// doesn't tell the user to try a different search they never made.
    fn is_narrowed(&self) -> bool {
        !self.query.trim().is_empty() || self.type_filter != 0 || self.account_filter.is_some()
    }

    /// Ask for the first page again, dropping whatever is loaded: what every
    /// change to the scope, the search, the type filter or the sort has to do,
    /// because all four are applied by the database and not here.
    /// Ask for the first page again: what every change to the scope, the
    /// search, the type filter or the sort has to do, because all four are
    /// applied by the database and not here.
    ///
    /// What is already on screen deliberately stays there until the
    /// replacement page arrives. Emptying the list first would take the
    /// toolbar and footer down with it — and the search entry lives in the
    /// toolbar, so a search would lose the focus of whoever was typing it
    /// after the first letter.
    fn reload(&mut self, sender: &ComponentSender<Self>) {
        self.preview = None;
        self.preview_texture = None;
        self.loading = true;
        self.request_page(0, sender);
    }

    /// Ask for the next page, unless one is already on its way or the last page
    /// has already landed.
    fn load_more(&mut self, sender: &ComponentSender<Self>) {
        if self.loading_more || !self.has_more() {
            return;
        }
        let offset = self.all_items.len() as u32;
        self.request_page(offset, sender);
    }

    fn has_more(&self) -> bool {
        (self.all_items.len() as u32) < self.total
    }

    /// The query as it stands, for the app to run against the cache.
    fn request_page(&mut self, offset: u32, sender: &ComponentSender<Self>) {
        self.loading_more = true;
        let folders: Vec<(u32, String)> = self
            .accounts
            .iter()
            .flat_map(|a| {
                a.folders
                    .iter()
                    .filter(|f| self.folder_included(a.id, &f.path))
                    .map(|f| (a.id, f.path.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        let _ = sender.output(GalleryOutput::Load(GalleryRequest {
            folders,
            account_id: self.account_filter,
            tokens: self
                .query
                .split_whitespace()
                .map(|t| t.to_lowercase())
                .collect(),
            bucket: self.type_filter,
            sort: self.sort,
            offset,
            limit: PAGE_SIZE,
        }));
    }

    /// Whether attachments from this folder are in scope. A folder the app has
    /// not listed (a new one on the server, or the list not having arrived yet)
    /// counts as in scope, matching the worker's own default.
    fn folder_included(&self, account_id: u32, path: &str) -> bool {
        self.included
            .get(&account_id)
            .and_then(|m| m.get(path))
            .copied()
            .unwrap_or(true)
    }

    /// Recompute the effective per-folder verdict from the two master switches
    /// and the user's own ticks. A master switch that is off wins over a tick,
    /// so turning "Include Archive" back on restores each archive folder to
    /// whatever the user had chosen for it.
    fn recompute_scope(&mut self) {
        self.included = self
            .accounts
            .iter()
            .map(|acct| {
                let ticks = self.overrides.get(&acct.id);
                let folders = acct
                    .folders
                    .iter()
                    .map(|f| {
                        let tick = ticks.and_then(|m| m.get(&f.path)).copied();
                        let on = in_scope(f.kind, self.include_archive, self.include_other, tick);
                        (f.path.clone(), on)
                    })
                    .collect();
                (acct.id, folders)
            })
            .collect();
    }

    /// The user's own tick for a folder, before the master switches have their
    /// say — what the folder list shows in its check box.
    fn folder_ticked(&self, account_id: u32, path: &str, kind: FolderKind) -> bool {
        self.overrides
            .get(&account_id)
            .and_then(|m| m.get(path))
            .copied()
            .unwrap_or_else(|| kind_default(kind))
    }

    /// Rebuild the footer popover's folder list: every account's folders, each
    /// with a check box, under the account's own label when there is more than
    /// one account. Rows whose master switch is off are shown insensitive, so
    /// it is clear the switch and not the tick is what is holding them back.
    fn rebuild_folder_list(&self, sender: &ComponentSender<Self>) {
        while let Some(row) = self.folder_list.first_child() {
            self.folder_list.remove(&row);
        }
        let multi = self.accounts.len() > 1;
        for acct in &self.accounts {
            if multi {
                let header = gtk::ListBoxRow::new();
                header.set_activatable(false);
                header.set_selectable(false);
                header.add_css_class("gallery-folder-heading");
                let label = gtk::Label::new(Some(&acct.label));
                label.set_xalign(0.0);
                label.add_css_class("heading");
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                header.set_child(Some(&label));
                self.folder_list.append(&header);
            }
            for folder in &acct.folders {
                let row = adw::ActionRow::new();
                row.set_title(&glib::markup_escape_text(&folder.name));
                row.add_prefix(&gtk::Image::from_icon_name(folder.kind.icon()));
                let check = gtk::CheckButton::new();
                check.set_active(self.folder_ticked(acct.id, &folder.path, folder.kind));
                check.set_valign(gtk::Align::Center);
                let s = sender.clone();
                let account_id = acct.id;
                let path = folder.path.clone();
                check.connect_toggled(move |c| {
                    s.input(GalleryInput::SetFolder {
                        account_id,
                        path: path.clone(),
                        on: c.is_active(),
                    });
                });
                row.add_suffix(&check);
                row.set_activatable_widget(Some(&check));
                // An inbox always counts; the rest answer to a master switch.
                row.set_sensitive(match kind_master(folder.kind) {
                    Some(Master::Archive) => self.include_archive,
                    Some(Master::Other) => self.include_other,
                    None => true,
                });
                self.folder_list.append(&row);
            }
        }
    }

    /// Refill the account dropdown's rows from `accounts`, keeping "All
    /// accounts" first.
    fn rebuild_account_names(&self) {
        while self.account_names.n_items() > 1 {
            self.account_names.remove(self.account_names.n_items() - 1);
        }
        for acct in &self.accounts {
            self.account_names.append(&acct.label);
        }
    }

    /// Persist the folder scope, dropping ticks that agree with their kind's
    /// default so the file only records genuine choices.
    fn save_scope(&self) {
        let kinds: HashMap<(u32, &str), FolderKind> = self
            .accounts
            .iter()
            .flat_map(|a| {
                a.folders
                    .iter()
                    .map(move |f| ((a.id, f.path.as_str()), f.kind))
            })
            .collect();
        let mut folders: Vec<(u32, String, bool)> = self
            .overrides
            .iter()
            .flat_map(|(id, m)| m.iter().map(move |(path, on)| (*id, path.clone(), *on)))
            .filter(|(id, path, on)| {
                // Keep a tick for a folder we have not been told about: it may
                // belong to an account that is offline right now.
                kinds
                    .get(&(*id, path.as_str()))
                    .is_none_or(|k| kind_default(*k) != *on)
            })
            .collect();
        folders.sort();
        crate::config::save_gallery_scope(self.include_archive, self.include_other, &folders);
    }

    /// Re-apply the scope to the loaded items and repaint both the grid/table
    /// and the folder list's tick marks.
    fn scope_changed(&mut self, sender: &ComponentSender<Self>) {
        self.recompute_scope();
        self.save_scope();
        self.rebuild_folder_list(sender);
        self.reload(sender);
    }

    /// The `GalleryItem` at display position `display`. The database returns
    /// rows already filtered and in order, so position is a direct index.
    fn item_at(&self, display: usize) -> Option<&GalleryItem> {
        self.all_items.get(display)
    }

    fn current(&self) -> Option<&GalleryItem> {
        self.preview.and_then(|i| self.item_at(i))
    }

    fn step(&mut self, delta: i32, sender: &ComponentSender<Self>) {
        if self.all_items.is_empty() {
            return;
        }
        if let Some(i) = self.preview {
            let n = self.all_items.len() as i32;
            self.preview = Some((((i as i32 + delta) % n + n) % n) as usize);
            self.refresh_preview(sender);
        }
    }

    /// Work out what the lightbox shows for the current item: an image decodes
    /// on the spot; a PDF's first page comes from the full-size render cache,
    /// or a worker renders it now and [`GalleryInput::PreviewRendered`] circles
    /// back here. Anything else has no texture — the file-icon page shows.
    fn refresh_preview(&mut self, sender: &ComponentSender<Self>) {
        self.preview_texture = None;
        let Some(item) = self.current() else { return };
        let Some(data) = item.data.as_ref() else {
            // Never downloaded (or too big to have ridden along with the page):
            // ask for it, and the spinner shows until `Fetched` circles back.
            if !self.fetching {
                self.fetch_item(item, sender);
            }
            return;
        };
        if item.is_image() {
            self.preview_texture = texture_from(data);
            return;
        }
        if !is_pdf_name(&item.name) {
            return;
        }
        let key = thumb_cache_key(data);
        match PDF_PREVIEWS.with(|c| c.borrow().get(&key).cloned()) {
            Some(texture) => self.preview_texture = texture,
            None => {
                let s = sender.clone();
                lightbox_pdf_texture(data, move |_| {
                    s.input(GalleryInput::PreviewRendered(key));
                });
            }
        }
    }

    /// Repopulate whichever view is showing. The other keeps stale children;
    /// switching to it rebuilds it, so only one view's widgets are ever built
    /// for a given change.
    fn rebuild_view(&mut self, sender: &ComponentSender<Self>) {
        if self.view_table {
            self.rebuild_table(sender);
        } else {
            self.rebuild_grid(sender);
        }
    }

    fn rebuild_grid(&mut self, sender: &ComponentSender<Self>) {
        // Remove existing cells; only FlowBoxChild children (not, say, a popover
        // that happens to be parented nearby).
        let mut child = self.flow.first_child();
        while let Some(c) = child {
            let next = c.next_sibling();
            if c.downcast_ref::<gtk::FlowBoxChild>().is_some() {
                self.flow.remove(&c);
            }
            child = next;
        }
        for display in 0..self.all_items.len() {
            if let Some(item) = self.item_at(display) {
                let deleting = self.deleting.contains(&key_of(item));
                self.flow.append(&build_cell(
                    display,
                    item,
                    self.thumb_width,
                    deleting,
                    sender,
                ));
            }
        }
    }

    fn rebuild_table(&mut self, sender: &ComponentSender<Self>) {
        while let Some(row) = self.table.first_child() {
            self.table.remove(&row);
        }
        for display in 0..self.all_items.len() {
            if let Some(item) = self.item_at(display) {
                let deleting = self.deleting.contains(&key_of(item));
                self.table
                    .append(&build_row(display, item, deleting, sender));
            }
        }
    }

    /// Open item `index` in its default application. A file whose bytes were
    /// never downloaded — most of an archive — is fetched first, and opens when
    /// [`GalleryInput::Fetched`] brings it back.
    fn open_item(&self, index: usize, sender: &ComponentSender<Self>) {
        let Some(item) = self.item_at(index) else {
            return;
        };
        match &item.data {
            Some(data) => {
                let parent = self.flow.root().and_downcast::<gtk::Window>();
                open_bytes(&item.name, data, parent.as_ref());
            }
            None => self.fetch_item(item, sender),
        }
    }

    /// Ask the app to download the message this item belongs to.
    fn fetch_item(&self, item: &GalleryItem, sender: &ComponentSender<Self>) {
        let _ = sender.output(GalleryOutput::Fetch {
            account_id: item.account_id,
            folder_path: item.folder_path.clone(),
            uid: item.uid,
        });
    }

    /// Save item `index` to a file the user chooses.
    fn download_item(&self, index: usize, sender: &ComponentSender<Self>) {
        let Some(item) = self.item_at(index) else {
            return;
        };
        let Some(data) = item.data.clone() else {
            // Not in hand yet: fetch it, and the user can save it once the
            // thumbnail fills in.
            self.fetch_item(item, sender);
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title(&i18n("Save Attachment"))
            .initial_name(&item.name)
            .modal(true)
            .build();
        let parent = self.flow.root().and_downcast::<gtk::Window>();
        dialog.save(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(path) = file.path() {
                    if let Err(e) = std::fs::write(&path, &data) {
                        tracing::warn!("could not save attachment: {e}");
                    }
                }
            }
        });
    }

    fn goto_item(&mut self, index: usize, sender: &ComponentSender<Self>) {
        if let Some(item) = self.item_at(index) {
            let _ = sender.output(GalleryOutput::OpenMessage {
                account_id: item.account_id,
                folder_path: item.folder_path.clone(),
                uid: item.uid,
            });
            self.preview = None;
        }
    }

    /// Pop up the right-click menu (Download / Open / Go to Message) at the click
    /// point `(x, y)` (relative to cell `index`). Download/Open are only enabled
    /// when the file's bytes are cached.
    fn show_context_menu(&self, index: usize, x: f64, y: f64, sender: &ComponentSender<Self>) {
        if self.item_at(index).is_none() {
            return;
        }

        // Both act on a file that was never downloaded too: they fetch it
        // first. Greying them out would be wrong now that most of an archive's
        // attachments are known but not held.
        let s = sender.clone();
        let open = MenuEntry::new(i18n("Open"), move || s.input(GalleryInput::OpenItem(index)))
            .icon("document-open-symbolic");
        let s = sender.clone();
        let download = MenuEntry::new(i18n("Download…"), move || {
            s.input(GalleryInput::DownloadItem(index))
        })
        .icon("folder-download-symbolic");
        let s = sender.clone();
        let goto = MenuEntry::new(i18n("Go to Message"), move || {
            s.input(GalleryInput::GoToItem(index))
        })
        .icon("mail-unread-symbolic");
        let s = sender.clone();
        let delete = MenuEntry::new(i18n("Delete from Server…"), move || {
            s.input(GalleryInput::DeleteItem(index))
        })
        .icon("user-trash-symbolic");
        let sections = vec![vec![open, download, goto], vec![delete]];

        // Anchor on the clicked cell/row itself so the click point (already
        // relative to it) needs no coordinate translation.
        let source: Option<gtk::Widget> = if self.view_table {
            self.table.row_at_index(index as i32).map(|r| r.upcast())
        } else {
            self.flow.child_at_index(index as i32).map(|c| c.upcast())
        };
        show_context_menu(source.as_ref().unwrap_or(&self.root), x, y, sections);
    }
}

/// One grid cell: a thumbnail (image or PDF first page) or type icon, plus name
/// + size.
fn build_cell(
    index: usize,
    item: &GalleryItem,
    width: i32,
    deleting: bool,
    sender: &ComponentSender<AttachmentsGallery>,
) -> gtk::Widget {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 6);
    cell.add_css_class("gallery-cell");
    cell.set_hexpand(true);
    cell.set_halign(gtk::Align::Fill);
    cell.set_tooltip_text(Some(&format!("{} — {}", item.name, item.human_size())));

    let thumb_holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    thumb_holder.add_css_class("gallery-thumb");
    thumb_holder.set_halign(gtk::Align::Fill);
    thumb_holder.set_valign(gtk::Align::Fill);

    let thumb = match item.data.as_ref() {
        Some(d) => thumbnail_texture(&item.name, d),
        None => Thumbnail::Fallback,
    };
    match thumb {
        Thumbnail::Ready(tex) => thumb_holder.append(&gallery_picture(&tex)),
        Thumbnail::Fallback => thumb_holder.append(&gallery_icon(&item.name)),
        Thumbnail::Pending => {
            thumb_holder.append(&thumbnail_spinner());
            let holder = thumb_holder.downgrade();
            let name = item.name.clone();
            let data = item.data.clone().unwrap_or_default();
            spawn_thumbnail_render(&item.name, data, move |tex| {
                // The cell may be gone by now (search narrowed, list rebuilt);
                // the render still landed in the cache for the next build.
                let Some(holder) = holder.upgrade() else {
                    return;
                };
                while let Some(child) = holder.first_child() {
                    holder.remove(&child);
                }
                match tex {
                    Some(tex) => holder.append(&gallery_picture(&tex)),
                    None => holder.append(&gallery_icon(&name)),
                }
            });
        }
    }

    // Lock the thumbnail section to a 4:3 aspect ratio; its width tracks the
    // (responsive) column width and the height follows, filling the cell.
    let aspect = RatioBox::new(&thumb_holder);
    // Preferred column width (the footer slider's value) — the FlowBox packs at
    // least 3 per row and adds more as the window widens.
    aspect.set_width_request(width);
    aspect.set_hexpand(true);

    // Overlay quick-action buttons at the thumbnail's bottom-right corner; they
    // fade in on hover (via CSS). Download and Open need the file's bytes cached;
    // "Go to Message" always works, so it shows even for uncached attachments.
    let thumb_overlay = gtk::Overlay::new();
    thumb_overlay.set_child(Some(&aspect));

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    actions.set_valign(gtk::Align::End);
    actions.set_margin_end(6);
    actions.set_margin_bottom(6);
    let action_btn = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("gallery-open");
        b.add_css_class("circular");
        b.add_css_class("osd");
        b.set_tooltip_text(Some(tip));
        b
    };
    if item.data.is_some() {
        let download = action_btn("folder-download-symbolic", &i18n("Download"));
        let s = sender.clone();
        download.connect_clicked(move |_| s.input(GalleryInput::DownloadItem(index)));
        actions.append(&download);

        let open = action_btn("document-open-symbolic", &i18n("Open"));
        let s = sender.clone();
        open.connect_clicked(move |_| s.input(GalleryInput::OpenItem(index)));
        actions.append(&open);
    }
    let goto = action_btn("mail-unread-symbolic", &i18n("Go to Message"));
    let s = sender.clone();
    goto.connect_clicked(move |_| s.input(GalleryInput::GoToItem(index)));
    actions.append(&goto);
    if deleting {
        cell.add_css_class("attachment-deleting");
        thumb_overlay.add_overlay(&deleting_veil(true));
    } else {
        thumb_overlay.add_overlay(&actions);
    }

    cell.append(&thumb_overlay);

    let name = gtk::Label::new(Some(&item.name));
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_max_width_chars(18);
    name.add_css_class("gallery-name");
    cell.append(&name);

    let from = item.from_name.trim();
    if !from.is_empty() {
        let sender = gtk::Label::new(Some(from));
        sender.set_ellipsize(gtk::pango::EllipsizeMode::End);
        sender.set_max_width_chars(18);
        sender.add_css_class("gallery-from");
        cell.append(&sender);
    }

    let subject = item.subject.trim();
    if !subject.is_empty() {
        let subj = gtk::Label::new(Some(subject));
        subj.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subj.set_max_width_chars(18);
        subj.add_css_class("gallery-subject");
        subj.add_css_class("dim-label");
        cell.append(&subj);
    }

    let mut meta = vec![folder_label(&item.folder_path)];
    let date = item.date_label();
    if !date.is_empty() {
        meta.push(date);
    }
    meta.push(item.human_size());
    let sub = gtk::Label::new(Some(&meta.join(" · ")));
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sub.set_max_width_chars(18);
    sub.add_css_class("gallery-size");
    sub.add_css_class("dim-label");
    cell.append(&sub);

    let child = gtk::FlowBoxChild::new();
    child.set_child(Some(&cell));
    // Nothing more to do with a file on its way off the server.
    if deleting {
        child.set_can_target(false);
        return child.upcast();
    }

    // Right-click → context menu at the click point.
    let right = gtk::GestureClick::new();
    right.set_button(gtk::gdk::BUTTON_SECONDARY);
    let s = sender.clone();
    right.connect_pressed(move |_, _, x, y| {
        s.input(GalleryInput::ContextMenu { index, x, y });
    });
    child.add_controller(right);

    // Double-click (primary) → open externally. Single click keeps the FlowBox's
    // built-in activation (which opens the preview).
    let dbl = gtk::GestureClick::new();
    dbl.set_button(gtk::gdk::BUTTON_PRIMARY);
    let s = sender.clone();
    dbl.connect_pressed(move |_, n, _, _| {
        if n == 2 {
            s.input(GalleryInput::OpenExternal(index));
        }
    });
    child.add_controller(dbl);

    child.upcast()
}

/// How long a file deleted from the server takes to fade out (#289).
const DELETE_FADE_MS: u32 = 420;

/// "Deleting…", in place of a file's actions while it is removed from the
/// server (#289).
pub(crate) fn deleting_label() -> gtk::Label {
    let l = gtk::Label::new(Some(&i18n("Deleting…")));
    l.add_css_class("attachment-deleting-label");
    l.add_css_class("caption");
    l.set_valign(gtk::Align::Center);
    l
}

/// A row's actions while its file is removed from the server: still there,
/// unseen and out of reach, so the row keeps its size, with "Deleting…"
/// over them.
pub(crate) fn deleting_over(actions: &impl IsA<gtk::Widget>) -> gtk::Overlay {
    actions.set_opacity(0.0);
    actions.set_can_target(false);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(actions));
    let label = deleting_label();
    label.set_halign(gtk::Align::End);
    overlay.add_overlay(&label);
    overlay
}

/// A thumbnail's cover while its file is removed from the server: a pale
/// red veil, with "Deleting…" across the middle when `labelled` (a
/// thumbnail too small for it says so beneath instead).
pub(crate) fn deleting_veil(labelled: bool) -> gtk::Widget {
    let veil = gtk::Box::new(gtk::Orientation::Vertical, 0);
    veil.add_css_class("attachment-deleting-veil");
    veil.set_hexpand(true);
    veil.set_vexpand(true);
    veil.set_overflow(gtk::Overflow::Hidden);
    if labelled {
        let label = deleting_label();
        label.set_halign(gtk::Align::Center);
        label.set_vexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        veil.append(&label);
    }
    veil.upcast()
}

/// Fade a deleted file's cell or row out, then call `done` to take it away.
/// Libadwaita skips the fade when animations are off or the widget is not
/// on screen, and `done` comes at once.
pub(crate) fn fade_out(widget: &impl IsA<gtk::Widget>, done: impl Fn() + 'static) {
    let w = widget.clone().upcast::<gtk::Widget>();
    let target = adw::CallbackAnimationTarget::new({
        let w = w.clone();
        move |v| w.set_opacity(v)
    });
    let anim = adw::TimedAnimation::new(&w, 1.0, 0.0, DELETE_FADE_MS, target);
    anim.set_easing(adw::Easing::EaseOutCubic);
    anim.connect_done(move |_| done());
    anim.play();
}

fn caption(item: &GalleryItem) -> String {
    let unknown = i18n("Unknown");
    let who = if item.from_name.trim().is_empty() {
        unknown.as_str()
    } else {
        item.from_name.trim()
    };
    let subject = item.subject.trim();
    let mut parts = vec![who.to_string()];
    if !subject.is_empty() {
        parts.push(subject.to_string());
    }
    parts.push(folder_label(&item.folder_path));
    let date = item.date_label();
    if !date.is_empty() {
        parts.push(date);
    }
    parts.push(item.human_size());
    parts.join(" · ")
}

/// A friendly folder name from a mailbox path (the last path segment).
fn folder_label(path: &str) -> String {
    let name = path.rsplit(['/', '.']).next().unwrap_or(path);
    if name.eq_ignore_ascii_case("inbox") {
        "Inbox".to_string()
    } else {
        // Paths are stored as the server names them, in modified UTF-7.
        crate::mutf7::decode(name)
    }
}

/// One table row: mini thumbnail/type icon, name, sender, type, date, size —
/// the same widths as the header buttons, so the columns line up.
fn build_row(
    index: usize,
    item: &GalleryItem,
    deleting: bool,
    sender: &ComponentSender<AttachmentsGallery>,
) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    line.add_css_class("gallery-table-row");

    // Cache-only mini thumbnail: a decode already paid for is shown, but a
    // 24px row never spawns a render of its own.
    let lead: gtk::Widget = match item.data.as_ref().map(|d| thumbnail_texture(&item.name, d)) {
        Some(Thumbnail::Ready(tex)) => {
            let pic = gtk::Picture::for_paintable(&tex);
            pic.set_content_fit(gtk::ContentFit::Cover);
            pic.set_size_request(28, 28);
            pic.add_css_class("gallery-table-thumb");
            pic.upcast()
        }
        _ => {
            let img = gtk::Image::from_icon_name(icon_for(&item.name));
            img.set_pixel_size(20);
            img.set_size_request(28, 28);
            img.add_css_class("gallery-file-icon");
            img.add_css_class(icon_color_class(&item.name));
            img.upcast()
        }
    };
    line.append(&lead);

    let name = gtk::Label::new(Some(&item.name));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_tooltip_text(Some(&format!("{} — {}", item.subject, item.human_size())));
    line.append(&name);

    let sender_label = gtk::Label::new(Some(&item.from_name));
    sender_label.set_xalign(0.0);
    sender_label.set_width_request(170);
    sender_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sender_label.set_max_width_chars(1);
    line.append(&sender_label);

    // Every fixed-width cell is clipped to its column (the width request is
    // the floor, the one-char max the ceiling): a value wider than its
    // column would otherwise widen the cell, and with the row's width
    // fixed, the expanding Name cell would give up the difference — sliding
    // Sender left of its header on that row alone.
    let fixed = |text: &str, width: i32, xalign: f32| {
        let l = gtk::Label::new(Some(text));
        l.set_xalign(xalign);
        l.set_width_request(width);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.set_max_width_chars(1);
        l.add_css_class("dim-label");
        l
    };
    // No extension: say so, rather than showing the name again.
    let ext = ext_of(&item.name);
    let kind_text = if ext.is_empty() {
        i18n("File")
    } else {
        ext.to_ascii_uppercase()
    };
    let kind = fixed(&kind_text, 90, 0.0);
    line.append(&kind);

    let date = fixed(&date_text(item.timestamp), 110, 0.0);
    line.append(&date);

    let size = fixed(&item.human_size(), 90, 1.0);
    size.add_css_class("numeric");
    line.append(&size);

    // The same quick actions the grid cells carry, as a trailing column.
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    actions.set_width_request(TABLE_ACTIONS_WIDTH);
    actions.set_halign(gtk::Align::End);
    let act = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("flat");
        b.set_valign(gtk::Align::Center);
        b.set_tooltip_text(Some(tip));
        b
    };
    if item.data.is_some() {
        let download = act("folder-download-symbolic", &i18n("Download"));
        let s = sender.clone();
        download.connect_clicked(move |_| s.input(GalleryInput::DownloadItem(index)));
        actions.append(&download);
        let open = act("document-open-symbolic", &i18n("Open"));
        let s = sender.clone();
        open.connect_clicked(move |_| s.input(GalleryInput::OpenItem(index)));
        actions.append(&open);
    }
    let goto = act("mail-unread-symbolic", &i18n("Go to Message"));
    let s = sender.clone();
    goto.connect_clicked(move |_| s.input(GalleryInput::GoToItem(index)));
    actions.append(&goto);
    if deleting {
        line.append(&deleting_over(&actions));
    } else {
        line.append(&actions);
    }

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&line));
    if deleting {
        // The ground on the row itself, where the hover and selection
        // highlights are drawn, so it has their shape.
        row.add_css_class("attachment-deleting");
        row.set_can_target(false);
        return row;
    }

    // The same gestures the grid cells carry: right-click for the context
    // menu, double-click to open externally (single click previews).
    let right = gtk::GestureClick::new();
    right.set_button(gtk::gdk::BUTTON_SECONDARY);
    let s = sender.clone();
    right.connect_pressed(move |_, _, x, y| {
        s.input(GalleryInput::ContextMenu { index, x, y });
    });
    row.add_controller(right);
    let dbl = gtk::GestureClick::new();
    dbl.set_button(gtk::gdk::BUTTON_PRIMARY);
    let s = sender.clone();
    dbl.connect_pressed(move |_, n, _, _| {
        if n == 2 {
            s.input(GalleryInput::OpenExternal(index));
        }
    });
    row.add_controller(dbl);
    row
}

/// A column header's label, carrying the sort arrow when it is the active
/// column: `asc`/`desc` are the two criteria that column maps to.
fn column_header(label: &str, current: GallerySort, asc: GallerySort, desc: GallerySort) -> String {
    if current == asc {
        format!("{label} \u{2191}")
    } else if current == desc {
        format!("{label} \u{2193}")
    } else {
        label.to_string()
    }
}

/// Whether a scroller has come close enough to its end to be worth asking for
/// the next page. `upper - page_size` is the furthest the view can scroll, so
/// this is "within a couple of rows of the bottom".
fn near_end(a: &gtk::Adjustment) -> bool {
    a.upper() - a.page_size() - a.value() < LOAD_MORE_MARGIN
}

/// The footer's network status: a page arriving, or how much of the archive the
/// scan has still to describe. Empty when there is nothing happening.
fn scan_text(loading_more: bool, scan_remaining: u32) -> String {
    if scan_remaining > 0 {
        // Deliberately counts messages, not attachments: until a message has
        // been described there is no telling how many files it holds.
        i18n_f(
            "Indexing {n} messages…",
            &[("n", &scan_remaining.to_string())],
        )
    } else if loading_more {
        i18n("Loading more…")
    } else {
        String::new()
    }
}

/// The footer's item count: what's shown of what's there.
fn count_text(shown: usize, total: usize) -> String {
    if shown == total {
        ni18n_f(
            "{n} attachment",
            "{n} attachments",
            total as u32,
            &[("n", &total.to_string())],
        )
    } else {
        i18n_f(
            "{shown} of {total}",
            &[("shown", &shown.to_string()), ("total", &total.to_string())],
        )
    }
}

/// "Aug 26, 2026" for a table row, or nothing when the timestamp is unknown.
fn date_text(timestamp: i64) -> String {
    glib::DateTime::from_unix_local(timestamp)
        .ok()
        .and_then(|d| d.format("%b %e, %Y").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The footer type dropdown's bucket for a filename. Row 0 is "All types";
/// the rest must match the `StringList` built in the view.
/// The gallery's cover-cropped thumbnail picture for a ready texture.
fn gallery_picture(tex: &gdk::Texture) -> gtk::Picture {
    let pic = gtk::Picture::for_paintable(tex);
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_hexpand(true);
    pic.set_vexpand(true);
    pic.add_css_class("gallery-thumb-image");
    pic
}

/// The gallery's centred type icon for anything without a thumbnail.
fn gallery_icon(name: &str) -> gtk::Image {
    let img = gtk::Image::from_icon_name(icon_for(name));
    img.set_pixel_size(56);
    img.set_hexpand(true);
    img.add_css_class("gallery-file-icon");
    img.add_css_class(icon_color_class(name));
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gallery's default catch: inboxes, archives and custom folders, but
    /// not Sent.
    #[test]
    fn sent_is_the_only_kind_off_by_default() {
        for kind in [
            FolderKind::Inbox,
            FolderKind::Starred,
            FolderKind::Archive,
            FolderKind::Custom,
        ] {
            assert!(
                in_scope(kind, true, true, None),
                "{kind:?} should be on by default"
            );
        }
        assert!(!in_scope(FolderKind::Sent, true, true, None));
    }

    #[test]
    fn a_tick_overrides_the_kind_default_either_way() {
        assert!(in_scope(FolderKind::Sent, true, true, Some(true)));
        assert!(!in_scope(FolderKind::Custom, true, true, Some(false)));
    }

    #[test]
    fn each_master_switch_governs_only_its_own_kinds() {
        // Archive off takes the archive down and leaves the rest alone.
        assert!(!in_scope(FolderKind::Archive, false, true, None));
        assert!(in_scope(FolderKind::Inbox, false, true, None));
        assert!(in_scope(FolderKind::Custom, false, true, None));
        // "Other" off takes everything that is neither inbox nor archive.
        assert!(!in_scope(FolderKind::Custom, true, false, None));
        assert!(!in_scope(FolderKind::Starred, true, false, None));
        assert!(in_scope(FolderKind::Inbox, true, false, None));
        assert!(in_scope(FolderKind::Archive, true, false, None));
    }

    /// An inbox answers to neither switch: there is always something to show.
    #[test]
    fn an_inbox_survives_both_switches_being_off() {
        assert!(in_scope(FolderKind::Inbox, false, false, None));
    }

    /// A master switch that is off beats a tick, but does not erase it: the
    /// tick is what comes back when the switch returns.
    #[test]
    fn a_master_switch_outranks_a_tick_without_clearing_it() {
        assert!(!in_scope(FolderKind::Archive, false, true, Some(true)));
        assert!(in_scope(FolderKind::Archive, true, true, Some(true)));
    }

    fn item(
        name: &str,
        from: &str,
        subject: &str,
        folder: &str,
        ts: i64,
        size: u64,
    ) -> GalleryItem {
        GalleryItem {
            account_id: 1,
            downloaded: true,
            folder_path: folder.into(),
            uid: 1,
            name: name.into(),
            size,
            from_name: from.into(),
            subject: subject.into(),
            timestamp: ts,
            data: None,
        }
    }

    #[test]
    fn a_caption_names_sender_subject_folder_and_size() {
        assert_eq!(
            caption(&item("a.pdf", "  ", "  Hi ", "INBOX", 0, 2048)),
            "Unknown · Hi · Inbox · 2.0\u{a0}kB"
        );
        assert_eq!(
            caption(&item("a.pdf", " Ann ", "", "[Gmail]/Sent Mail", -5, 10)),
            format!("Ann · Sent Mail · {}", crate::models::human_size(10))
        );
    }

    #[test]
    fn folder_labels_are_the_last_segment_decoded() {
        assert_eq!(folder_label("INBOX"), "Inbox");
        assert_eq!(folder_label("inbox"), "Inbox");
        assert_eq!(folder_label("INBOX/Work"), "Work");
        assert_eq!(folder_label("INBOX.Inbox"), "Inbox");
        assert_eq!(folder_label("Archives.2024"), "2024");
        assert_eq!(folder_label("Entw&APw-rfe"), "Entwürfe");
        assert_eq!(folder_label(""), "");
    }

    #[test]
    fn the_footer_says_what_the_network_is_doing() {
        assert_eq!(scan_text(false, 0), "");
        assert_eq!(scan_text(true, 0), "Loading more…");
        assert_eq!(scan_text(true, 5), "Indexing 5 messages…");
        assert_eq!(scan_text(false, 1), "Indexing 1 messages…");
    }

    /// The table's date ignores the date-style setting (REFACTOR_NOTES.md,
    /// suspected bug B5).
    #[test]
    fn table_dates_use_a_fixed_pattern() {
        assert_eq!(date_text(1_787_745_600), "Aug 26, 2026");
    }

    #[test]
    fn sort_index_roundtrips_every_criterion() {
        for i in 0..10 {
            assert_eq!(GallerySort::from_index(i).index(), i);
        }
    }

    #[test]
    fn column_headers_carry_the_sort_arrow() {
        use super::column_header;
        assert_eq!(
            column_header(
                "Name",
                GallerySort::Name,
                GallerySort::Name,
                GallerySort::NameDesc
            ),
            "Name ↑"
        );
        assert_eq!(
            column_header(
                "Name",
                GallerySort::NameDesc,
                GallerySort::Name,
                GallerySort::NameDesc
            ),
            "Name ↓"
        );
        assert_eq!(
            column_header(
                "Name",
                GallerySort::Newest,
                GallerySort::Name,
                GallerySort::NameDesc
            ),
            "Name"
        );
    }

    #[test]
    fn counts_read_naturally() {
        use super::count_text;
        assert_eq!(count_text(1, 1), "1 attachment");
        assert_eq!(count_text(42, 42), "42 attachments");
        assert_eq!(count_text(12, 42), "12 of 42");
    }
}
