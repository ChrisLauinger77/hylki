//! Attachments gallery: a grid of every cached attachment across the connected
//! inboxes, with a lightbox preview (prev/next, open, go to message).
//!
//! Data comes from the SQLite cache (what the background prefetch has already
//! downloaded), fed in a page at a time via [`GalleryInput::Page`]. Image attachments and PDFs
//! (rendered from their first page) show as thumbnails; other files show a type
//! icon. Clicking a cell opens a large overlay preview.

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;
use relm4::prelude::*;

use std::collections::HashMap;

use crate::i18n::i18n;
use crate::models::{GalleryItem, GallerySort};
use crate::ui::context_menu::{show_context_menu, MenuEntry};

mod cell;
mod labels;
mod msg;
mod open_file;
mod ratio_box;
mod scope;
mod table;
mod thumbnails;

use cell::build_cell;
pub(crate) use cell::{deleting_label, deleting_over, deleting_veil, fade_out};
use labels::{caption, column_header, count_text, scan_text};
use msg::key_of;
pub use msg::{FileKey, GalleryInput, GalleryOutput, GalleryRequest};

pub(crate) use open_file::open_bytes;
pub use open_file::purge_attachment_dir;
use ratio_box::RatioBox;
pub use scope::{GalleryAccount, GalleryFolder};
use table::build_row;
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

/// Whether a scroller has come close enough to its end to be worth asking for
/// the next page. `upper - page_size` is the furthest the view can scroll, so
/// this is "within a couple of rows of the bottom".
fn near_end(a: &gtk::Adjustment) -> bool {
    a.upper() - a.page_size() - a.value() < LOAD_MORE_MARGIN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_index_roundtrips_every_criterion() {
        for i in 0..10 {
            assert_eq!(GallerySort::from_index(i).index(), i);
        }
    }
}
