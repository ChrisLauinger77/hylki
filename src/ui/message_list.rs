//! Middle pane: the scrollable list of messages in the selected folder,
//! with a search field and live filtering.

use self::layout::{schedule_midnight_refresh, BarSlots, LIST_MIN_WIDTH};
use self::rows::LIST_WINDOW;
use self::search::{ScopePicker, SearchHits, SearchText};
use self::sort::column_sort;
use self::threading::{
    reader_conversation, row_for_reader_key, source_threads_of, thread_inputs, thread_slot,
    unasked_threads, SourceThreads,
};
use self::views::ViewSnapshot;

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

use crate::i18n::i18n;
use crate::models::{Message, ThreadSummary};
pub use crate::ui::message_row::{tag_menu_entries, RowAction};
use crate::ui::message_row::{RowData, RowShared};

mod conversations;
mod filters;
mod layout;
mod menus;
mod nav;
mod rows;
mod search;
mod sort;
mod threading;
mod types;
mod views;

pub use filters::{CheckFilter, QuickFilter, QuickFilters};
pub(crate) use layout::after_next_paint;
pub(crate) use search::search_text_of;
pub use search::{PoolFolder, SearchScope};
pub use sort::SortOrder;
pub use types::{BulkAction, MessageListInput, MessageListOutput};
pub use views::{KEEP_AT_MOST, KEPT_VIEWS};

pub struct MessageList {
    /// The rows: a `ListView` builds only the ones on screen, so a folder
    /// of any size is one list (#323).
    list_view: gtk::ListView,
    /// The model behind it, its selection, and what every row shares.
    shared: Rc<RowShared>,
    /// The list's own input sender, for work it schedules on the main loop
    /// (coalesced rebuilds).
    input: relm4::Sender<MessageListInput>,
    /// A rebuild asked for and not yet run: `Some(preserve_scroll)`. Several
    /// arrivals in one main-loop pass (a folder's cached copy, its synced
    /// copy, fresh thread links, a view switch's flag changes) collapse into
    /// one rebuild instead of one each.
    rebuild_queued: Option<bool>,
    /// The view on show, as the app named it (`SetView`), and its content's
    /// fingerprint: a unified view is kept as built when left, and shown
    /// again from that if nothing in it changed.
    view_key: Option<u64>,
    view_fp: u64,
    views: std::collections::VecDeque<ViewSnapshot>,
    /// A kept view to show at the next queued rebuild, in place of one.
    pending_restore: Option<ViewSnapshot>,
    /// A view being built ahead (`Prebuild`) while its conversations are
    /// worked out off the main thread: its number, key, fingerprint and
    /// messages.
    prebuild: Option<(u64, u64, u64, Vec<Rc<Message>>)>,
    prebuild_gen: u64,
    /// Another view was opened: its first rebuild waits for a frame, and
    /// fills the rows for the new look in passing (#401).
    switched: bool,
    /// The look changed during a switch and the rows on screen were left
    /// for the rebuild to fill.
    look_stale: std::cell::Cell<bool>,
    /// A `SelectAndLoad` that arrived while a rebuild was queued: the rows it
    /// must find are not there yet, so it waits for that rebuild and runs
    /// after it (a notification click follows the folder's list into the
    /// channel in the same pass, and the list is only built on the idle).
    pending_select: Option<(u32, u32)>,
    /// A message asked for from outside (a notification click) that the list
    /// does not hold yet, and when: it is selected once a rebuild brings it,
    /// unless the user picks something first (#332).
    late_select: Option<((u32, u32), std::time::Instant)>,
    /// All messages for the current folder (full searchable index). Shared
    /// with the rows rather than copied: a large folder is listed whole.
    all: Vec<Rc<Message>>,
    /// Every folder's messages (all accounts), supplied by the app while a search
    /// is active, so `AllFolders` scope can filter across the whole mailbox. Empty
    /// when not searching.
    search_pool: Vec<Rc<Message>>,
    /// The pool's folders, by `(account_id, folder_id)`.
    pool_folders: Rc<std::collections::HashMap<(u32, u32), PoolFolder>>,
    /// What a search looks in, for the folder (`[0]`) and for the pool
    /// (`[1]`), made once for a search rather than at every keystroke.
    search_text: std::cell::RefCell<[Option<SearchText>; 2]>,
    /// The newest search sent to another thread. One still running for an
    /// older query sees it has been overtaken and stops (#515).
    search_gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The answer of the last search that ran on another thread.
    search_hits: Option<SearchHits>,
    /// Which messages the search field filters over.
    scope: SearchScope,
    /// The search scope's button and popover.
    scope_picker: Option<ScopePicker>,
    /// The search field widget, kept so a folder switch can clear its text.
    search_entry: Option<gtk::SearchEntry>,
    /// The search toolbar is hidden until asked for (#102).
    search_open: bool,
    /// When the search closed itself (empty entry losing focus): the button
    /// click that caused that blur arrives right after and must not reopen.
    search_closed_at: Option<std::time::Instant>,
    /// The search toolbar never hides (#446): closing only clears it.
    search_pinned: bool,
    /// The search field and the bulk bar open under the rows instead of
    /// above them (#496).
    search_bottom: bool,
    bulk_bottom: bool,
    /// The two bars and the boxes above and below the rows they move
    /// between.
    bars: Option<BarSlots>,
    /// The rows, in order: what the model shows, one message each.
    shown: Vec<Rc<Message>>,
    /// Total messages matching the current filter.
    total_matches: usize,
    query: String,
    gravatar: bool,
    /// Lines of preview text per row (1–3), from Preferences.
    preview_lines: u32,
    /// Whether rows draw their subject line (Focus Mode can take it away).
    show_subject: bool,
    /// Whether the colored avatars are drawn (#29).
    avatars: bool,
    /// Whether a sender's site icon may fill one (#30).
    sender_logos: bool,
    /// Ring each avatar in its account's color (the unified inbox view).
    colorize: bool,
    /// account_id → avatar color, for the rings.
    account_colors: std::collections::HashMap<u32, String>,
    /// Display-wide provider with each account's ring rule.
    color_provider: crate::ui::DisplayCss,
    /// Bumped when the circles must be looked up again, and when the tag
    /// definitions change (see `RowLook`).
    face_gen: u64,
    tags_gen: u64,
    /// The bulk bar's tag button, which its menu hangs from (#313).
    bulk_tag_btn: gtk::Button,
    /// The message currently being viewed, kept selected across list rebuilds.
    /// Keyed by (account_id, id) since UIDs collide across accounts in the
    /// unified "All Inboxes" view.
    selected_id: Option<(u32, u32)>,
    /// (account, message-id, references) for mail in the account's *other*
    /// folders. A conversation is often joined through messages that aren't on
    /// screen — every reply in an Inbox answers something in Sent — so those
    /// links are needed to see that the replies belong together.
    thread_links: Vec<(u32, String, String)>,
    /// The conversations of everything the list holds, worked out once and
    /// kept until the messages or the links change. Opening a message asks
    /// which conversation it belongs to, and grouping a large folder from
    /// scratch on every click held each one up by a visible beat (#323).
    source_threads: std::cell::RefCell<Option<Rc<SourceThreads>>>,
    /// What each conversation really is, read across the account's other
    /// folders and handed down by the app: its size (#222) and its newest
    /// message (#236). The list can only see its own folder, so a thread whose
    /// replies live in Sent would otherwise wear a badge that undercounts it
    /// and a row that never mentions your answer. Keyed by thread key, as
    /// `rebuild` groups them.
    thread_summaries: std::collections::HashMap<(u32, String), ThreadSummary>,
    /// Whether a row may speak for a message in another folder at all (#236).
    /// Off by default: the row describes the newest message this folder holds,
    /// as it always has. The sizes on the badges are not affected either way.
    thread_row_newest: bool,
    /// Every address you send from, lower case: with `thread_row_newest`
    /// off, the mail you sent is not nested under a conversation's row
    /// either (#388).
    own_addresses: std::collections::HashSet<String>,
    /// Gmail's keys are on (#392): Enter opens a row in the reader.
    gmail_keys: bool,
    /// Each conversation the last rebuild listed, with the folder's own
    /// members: what the app needs to look its real size up when a row
    /// showing it comes on screen.
    groups: std::collections::HashMap<(u32, String), Vec<Rc<Message>>>,
    /// The folders the last rebuild listed: a conversation's members in them
    /// are rows of their own, so only the rest are nested (#309).
    listed_folders: std::collections::HashSet<(u32, u32)>,
    /// Which conversations have already been asked about. A rebuild runs on
    /// every keystroke of a search and on every sync, and each ask is a scan of
    /// the account's index — so a thread is asked about once and remembered,
    /// not re-asked whenever its row is redrawn. Cleared per account by
    /// [`MessageListInput::RecheckThreadSummaries`] when that account's mail moves.
    asked_threads: std::collections::HashSet<(u32, String)>,
    /// Accounts whose conversations on screen are to be asked about again
    /// once the rebuild under way has run.
    recheck_accounts: std::collections::HashSet<u32>,
    /// Every selected message key, so the whole selection survives list rebuilds
    /// (background syncs) until the user clicks away.
    selected_ids: Vec<(u32, u32)>,
    /// The conversation the last `Selected` carried, as keys: after a
    /// rebuild, a selected head whose conversation now holds more is
    /// reported (`ThreadGrew`) so the reader shows the new reply at once.
    emitted_thread: Vec<(u32, u32)>,
    /// Selection changes still expected from a reader-driven selection, and what
    /// that selection is. GTK reports each selection change separately and a
    /// rebuild adds more, so a single flag would be consumed by the first and
    /// let a later one re-open the message; only a change that matches what
    /// the reader asked for is suppressed, and anything else ends it at once.
    from_reader: u8,
    reader_keys: Vec<(u32, u32)>,
    /// How many rows are currently selected (drives the bulk-action bar).
    selection_count: usize,
    /// Gmail's x is checking rows for an action rather than opening them
    /// (#392): a selection of one stays closed too, and the reader says how
    /// many are selected. Opening a row, or a plain click, ends it.
    checking: bool,
    /// Which way the user last moved through the list: +1 down, -1 up.
    /// Deleting the viewed message advances in this direction (like Apple
    /// Mail): triaging downward selects the message below the deleted one,
    /// and after moving up the list, deletion selects the one above instead.
    /// Updated only by the user's own selection movement — the programmatic
    /// post-delete advance keeps its index and never flips it.
    nav_direction: i32,
    /// Conversation keys the user has toggled away from the default state
    /// (expanded when the default is collapsed, and vice versa).
    expanded_threads: std::collections::HashSet<(u32, String)>,
    /// Whether conversations start expanded (user preference; collapsed default).
    default_expanded: bool,
    /// The open folder is Sent: rows name recipients instead of senders (#27).
    show_recipient: bool,
    /// The list shows Trash or Junk, where menus offer "Move to Inbox" (#138).
    restorable: bool,
    /// The list shows Junk: "Not Spam" stands where "Mark as Spam" would.
    in_junk: bool,
    /// The list shows Drafts (a folder or the unified row): drafts are
    /// neither read nor unread, so the toggles are not offered.
    in_drafts: bool,
    /// The list shows a Templates folder (#360): a row's menu offers to use
    /// or edit the template, not to answer it.
    in_templates: bool,
    /// Rendered thread membership: message key → conversation key, rebuilt with
    /// the rows. Lets a read-state change on a hidden reply refresh its head.
    msg_thread: std::collections::HashMap<(u32, u32), (u32, String)>,
    /// Conversation key → member message keys (multi-message threads only).
    thread_members: std::collections::HashMap<(u32, String), Vec<(u32, u32)>>,
    /// Members of the conversations on the list that live in other folders
    /// (your replies in Sent, the archived parts), by message key: rows a
    /// conversation opens out into without being part of this folder (#309).
    /// Whole-conversation actions look members up in the folder's own index,
    /// so they never reach these.
    nested: std::collections::HashMap<(u32, u32), Rc<Message>>,
    /// Whether the folder's background index is fully loaded. When false, more
    /// rows may still stream in, so reaching the bottom shows a spinner.
    index_complete: bool,
    /// Whether a SetMessages has arrived since the last SetLoading — gates the
    /// empty-folder placeholder so it never flashes during a folder switch.
    loaded: bool,
    /// The list is scrolled to its bottom: with the index still streaming
    /// in, the spinner there says more is on its way.
    at_bottom: bool,
    /// How many rows the view holds: a large list is put on screen a window
    /// at a time, the rest added as it is scrolled toward its end, so
    /// tens of thousands of messages (All Archive) never stall the window
    /// filling a view nobody is looking at. `shown` and the view stay row
    /// for row the same; `tail` is what follows them.
    window: usize,
    tail: Vec<Rc<RowData>>,
    /// Bumped whenever the source changes, so a background answer about
    /// conversations (`threads_pending`) is used only for the source it was
    /// worked out from; and the generation a job is running for.
    threads_gen: std::cell::Cell<u64>,
    threads_job: std::cell::Cell<Option<u64>>,
    /// Threads whose replies are sliding shut. The rows stay in the list until
    /// the paired timer fires and drops them — otherwise they'd simply vanish
    /// rather than animate away. (PR #79)
    collapsing_threads: std::collections::HashMap<(u32, String), gtk::glib::SourceId>,
    /// The list's scroller.
    scroller: Option<gtk::ScrolledWindow>,
    /// Current sort order for the list.
    sort: SortOrder,
    /// The header's quick filters (#97, #441).
    quick: QuickFilters,
    /// Your contacts' addresses, lower case, while "From Contacts" is on.
    contact_addresses: Option<std::sync::Arc<std::collections::HashSet<String>>>,
    /// The last count string sent to the header bar, to emit only on change.
    last_count: String,
    /// Group messages into conversation threads (user preference).
    threading: bool,
    /// Whether a conversation row may expand into its member rows. Off: the
    /// row keeps its count chip and chevron, but never opens — the thread is
    /// read through the reader's cards instead.
    thread_expansion: bool,
    /// Whether rows carry the actions palette line at all (preference).
    list_palette: bool,
    /// One line per message (#334): set by the Layout setting, or by the
    /// pane's width when it is Automatic.
    single_line: bool,
    /// The single-line columns, in order, as the setting has them (#334).
    columns: Vec<crate::config::ListColumn>,
    /// The Microsoft 365 accounts: only their mail has a due date, so the
    /// Due column takes room only while the list holds some of it.
    graph_accounts: std::collections::HashSet<u32>,
    /// The list holds mail of one of `graph_accounts`.
    graph_in_view: bool,
    /// The sort runs the other way: a heading clicked twice (#334).
    sort_reversed: bool,
    /// Headings divide a list sorted by date into Today, Yesterday and so
    /// on (#435), and whether the view draws them now: only a date sort
    /// has them.
    group_by_date: bool,
    day_headings: bool,
    /// A card's pills on a line under its text (#374).
    pills_below: bool,
    /// Show the column headings over a single-line list (#334).
    headings: bool,
    headings_bar: gtk::Box,
    /// The widths columns were dragged to by their headings (#334).
    widths: std::collections::HashMap<crate::config::ListColumn, i32>,
    /// Columns keep their widths and a list too narrow for them scrolls
    /// sideways; off, the name columns narrow to fit the pane (#443).
    column_scroll: bool,
    /// How wide the rows are, for fitting the columns into it.
    pane_width: i32,
    /// The headings' sized cells, which a drag resizes in place.
    heading_bins: std::cell::RefCell<
        std::collections::HashMap<crate::config::ListColumn, crate::ui::column_bin::ColumnBin>,
    >,
}

#[relm4::component(pub)]
impl SimpleComponent for MessageList {
    type Init = ();
    type Input = MessageListInput;
    type Output = MessageListOutput;

    view! {
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            add_css_class: "message-list-pane",

            // The bars set to open above the rows (#496): `place_bars`
            // moves the search field and the bulk bar between this and
            // the foot.
            #[name = "bars_top"]
            gtk::Box {
                add_css_class: "list-toolbar",
                set_orientation: gtk::Orientation::Vertical,
            },

            // The single line's column headings (#334), filled by
            // `sync_headings`; a click on one sorts by its column.
            #[name = "headings_area"]
            gtk::Box {
                add_css_class: "list-headings-bar",
                #[watch]
                set_visible: model.single_line && model.headings,
                // Scrolled sideways with the rows, never by itself: no bar
                // of its own, its place set from the list's (#443).
                #[name = "headings_scroller"]
                gtk::ScrolledWindow {
                    set_hexpand: true,
                    set_hscrollbar_policy: gtk::PolicyType::External,
                    set_vscrollbar_policy: gtk::PolicyType::Never,
                    set_propagate_natural_height: true,
                    #[local_ref]
                    headings_bar -> gtk::Box {
                        set_hexpand: true,
                    },
                },
            },

            gtk::Overlay {
                #[wrap(Some)]
                #[name = "scroller"]
                set_child = &gtk::ScrolledWindow {
                    set_vexpand: true,
                    // External, not Never: with Never the widest row's minimum (the
                    // actions palette reservation plus the avatar column) propagates
                    // all the way up and becomes part of the window's minimum width,
                    // which pushed it past half of a 1920px screen — at which point
                    // GNOME refuses to tile the window to the left/right edge. Rows
                    // ellipsize, so a narrow pane clips gracefully instead.
                    // Automatic does not propagate it either: a single line
                    // too wide for the pane scrolls to its columns past the
                    // edge (#443), with a bar that says so.
                    #[watch]
                    set_hscrollbar_policy: if model.single_line && model.column_scroll { gtk::PolicyType::Automatic } else { gtk::PolicyType::External },
                    // The pane's own floor, now that rows no longer set one: room
                    // for a row's full actions palette (avatar + dot + the reserved
                    // actions line), so opening the palette never needs to clip —
                    // the narrow-window breakpoint rails the sidebar in time to
                    // afford this even in a half-screen tile. (Grows by the thread
                    // indent while a conversation is expanded — see the rebuild.)
                    set_size_request: (LIST_MIN_WIDTH, -1),

                    // The view must be the scroller's own child to build only
                    // the rows on screen.
                    #[local_ref]
                    list_view -> gtk::ListView {},
                },

                // At the bottom while the rest of the folder streams in.
                add_overlay = &gtk::Box {
                    add_css_class: "list-loading",
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::End,
                    set_spacing: 8,
                    set_margin_bottom: 14,
                    set_can_target: false,
                    #[watch]
                    set_visible: model.is_loading_more(),

                    // Spun only while shown: a spinning spinner redraws every
                    // frame for as long as it is mapped (#275).
                    gtk::Spinner {
                        set_width_request: 18,
                        set_height_request: 18,
                        #[watch]
                        set_spinning: model.is_loading_more(),
                    },
                    gtk::Label {
                        set_label: &i18n("Loading more…"),
                        add_css_class: "dim-label",
                    },
                },

                // Placeholder when the folder has loaded and holds nothing.
                // Same full-size AdwStatusPage styling as the reader's
                // "No message selected", so the two placeholders match.
                add_overlay = &adw::StatusPage {
                    set_icon_name: Some("mail-inbox-symbolic"),
                    #[watch]
                    set_title: &(if model.quick.is_empty() { i18n("No Messages") } else { i18n("No Matching Messages") }),
                    #[watch]
                    set_description: Some(if model.quick.is_empty() {
                        i18n("There's nothing here right now.")
                    } else {
                        i18n("No message here passes the filters.")
                    }.as_str()),
                    #[watch]
                    set_visible: model.is_empty_state(),
                },
            },

            // The bars set to open below the rows (#496): one coming up
            // shrinks the list from its foot and never moves the rows being
            // read or clicked, as the status bar does (#433). The view
            // builds both bars here; `place_bars` puts them where the
            // settings say.
            #[name = "bars_bottom"]
            gtk::Box {
                add_css_class: "list-footer",
                set_orientation: gtk::Orientation::Vertical,

                // Bulk-action bar, revealed while more than one message is selected.
                #[name = "bulk_revealer"]
                gtk::Revealer {
                    #[watch]
                    set_reveal_child: model.selection_count > 1 || (model.checking && model.selection_count > 0),

                    // A collapsed revealer still reserves its child's width,
                    // so this bar of seven buttons was setting the whole
                    // pane's minimum width — 340px — however narrow the rows
                    // became (#29). Scrolled, its minimum is nothing and its
                    // natural size is unchanged, so it only clips when the
                    // list is genuinely too narrow to hold it.
                    gtk::ScrolledWindow {
                        set_vscrollbar_policy: gtk::PolicyType::Never,
                        set_hscrollbar_policy: gtk::PolicyType::External,
                        set_propagate_natural_width: true,
                        set_propagate_natural_height: true,

                    gtk::Box {
                        add_css_class: "bulk-bar",
                        set_spacing: 2,
                        // The buttons never take focus from the list: the
                        // selection keeps its focused highlight and the
                        // single-key shortcuts keep working after a click.

                        gtk::Label {
                            #[watch]
                            set_label: &format!("{} selected", model.selection_count),
                            set_hexpand: true,
                            set_halign: gtk::Align::Start,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            add_css_class: "bulk-count",
                        },
                        // One toggle, like the star: it shows what a click will
                        // do. Drafts are neither read nor unread: it goes when
                        // the list shows them.
                        gtk::Button {
                            #[watch]
                            set_icon_name: if model.selection_any_unread() {
                                "hylki-mail-read-symbolic"
                            } else {
                                "mail-unread-symbolic"
                            },
                            #[watch]
                            set_tooltip_text: Some(if model.selection_any_unread() { i18n("Mark as Read") } else { i18n("Mark as Unread") }.as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            #[watch]
                            set_visible: !model.in_drafts,
                            connect_clicked => MessageListInput::BulkToggleRead,
                        },
                        // Mapped to Unflag by the Bulk handler once every
                        // selected message is starred (#313).
                        gtk::Button {
                            #[watch]
                            set_icon_name: if model.selection_all_starred() {
                                "hylki-non-starred-symbolic"
                            } else {
                                "starred-symbolic"
                            },
                            #[watch]
                            set_tooltip_text: Some(if model.selection_all_starred() { i18n("Remove Star") } else { i18n("Star") }.as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            connect_clicked => MessageListInput::Bulk(BulkAction::Flag),
                        },
                        // Only once a tag exists, like the row menu's Tags.
                        #[local_ref]
                        bulk_tag_btn -> gtk::Button {
                            set_icon_name: "tag-outline-symbolic",
                            set_tooltip_text: Some(i18n("Tags").as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            #[watch]
                            set_visible: !model.shared.tags.borrow().is_empty(),
                            connect_clicked => MessageListInput::BulkTagMenu,
                        },
                        gtk::Button {
                            set_icon_name: "mail-archive-symbolic",
                            set_tooltip_text: Some(i18n("Archive").as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            connect_clicked => MessageListInput::Bulk(BulkAction::Archive),
                        },
                        gtk::Button {
                            #[watch]
                            set_icon_name: if model.in_junk {
                                "mail-mark-notjunk-symbolic"
                            } else {
                                "mail-mark-junk-symbolic"
                            },
                            #[watch]
                            set_tooltip_text: Some(if model.in_junk { i18n("Not Spam") } else { i18n("Mark as Spam") }.as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            // Mapped to NotSpam in Junk by the Bulk handler.
                            connect_clicked => MessageListInput::Bulk(BulkAction::Spam),
                        },
                        gtk::Button {
                            set_icon_name: "user-trash-symbolic",
                            set_tooltip_text: Some(i18n("Delete").as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            connect_clicked => MessageListInput::Bulk(BulkAction::Delete),
                        },
                        gtk::Separator {
                            set_orientation: gtk::Orientation::Vertical,
                        },
                        gtk::Button {
                            set_icon_name: "edit-clear-symbolic",
                            set_tooltip_text: Some(i18n("Clear selection").as_str()),
                            add_css_class: "flat",
                            set_focus_on_click: false,
                            connect_clicked => MessageListInput::ClearSelection,
                        },
                    },
                    },
                },

                // Search stays hidden until asked for (#102).
                #[name = "search_revealer"]
                gtk::Revealer {
                    #[watch]
                    set_reveal_child: model.search_open || model.search_pinned,

                gtk::Box {
                    set_spacing: 6,
                    add_css_class: "list-search-row",

                    #[name = "search_entry"]
                    gtk::SearchEntry {
                        set_hexpand: true,
                        // Its own default minimum is wider than the rows need.
                        set_width_chars: 3,
                        #[watch]
                        set_placeholder_text: Some(model.search_placeholder().as_str()),
                        connect_search_changed[sender] => move |entry| {
                            sender.input(MessageListInput::Search(entry.text().to_string()));
                        },
                        // Esc closes (SearchEntry's own stop signal).
                        connect_stop_search[sender] => move |_| {
                            sender.input(MessageListInput::CloseSearch);
                        },
                        // Leaving an empty entry closes too.
                        add_controller = gtk::EventControllerFocus {
                            connect_leave[sender] => move |ctl| {
                                let Some(entry) = ctl.widget().and_downcast::<gtk::SearchEntry>() else { return };
                                let sender = sender.clone();
                                crate::ui::close_if_focus_left_row(&entry, move || sender.input(MessageListInput::CloseSearch));
                            },
                        },
                    },

                    // The scope picker; its popover is built in `init`
                    // (`ScopePicker`).
                    #[name = "scope_button"]
                    gtk::MenuButton {
                        set_valign: gtk::Align::Center,
                        set_tooltip_text: Some(i18n("Choose which folders to search").as_str()),
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
        let shared = RowShared::new(sender.input_sender().clone());
        let list_view = gtk::ListView::new(
            Some(shared.selection.clone()),
            Some(crate::ui::message_row::factory(&shared)),
        );
        shared.view.set(Some(&list_view));
        Self::wire_list(&list_view, &shared, sender.input_sender());
        crate::ui::message_row::make_spare_rows(&shared);

        let color_provider = crate::ui::DisplayCss::new();

        let mut model = MessageList {
            list_view: list_view.clone(),
            shared,
            input: sender.input_sender().clone(),
            rebuild_queued: None,
            view_key: None,
            view_fp: 0,
            views: std::collections::VecDeque::new(),
            pending_restore: None,
            prebuild: None,
            prebuild_gen: 0,
            switched: false,
            look_stale: Default::default(),
            pending_select: None,
            late_select: None,
            all: Vec::new(),
            search_pool: Vec::new(),
            pool_folders: Default::default(),
            search_text: Default::default(),
            search_gen: Default::default(),
            search_hits: None,
            scope: SearchScope::AllFolders,
            search_entry: None,
            scope_picker: None,
            search_open: false,
            search_closed_at: None,
            search_pinned: false,
            search_bottom: false,
            bulk_bottom: false,
            bars: None,
            shown: Vec::new(),
            total_matches: 0,
            index_complete: true,
            loaded: false,
            at_bottom: false,
            window: LIST_WINDOW,
            tail: Vec::new(),
            threads_gen: std::cell::Cell::new(0),
            threads_job: std::cell::Cell::new(None),
            collapsing_threads: std::collections::HashMap::new(),
            query: String::new(),
            gravatar: false,
            avatars: true,
            sender_logos: false,
            preview_lines: 1,
            show_subject: true,
            colorize: false,
            account_colors: std::collections::HashMap::new(),
            color_provider,
            face_gen: 0,
            tags_gen: 0,
            thread_links: Vec::new(),
            source_threads: std::cell::RefCell::new(None),
            thread_summaries: std::collections::HashMap::new(),
            thread_row_newest: false,
            own_addresses: Default::default(),
            gmail_keys: crate::config::load_privacy().gmail_keys,
            groups: std::collections::HashMap::new(),
            listed_folders: std::collections::HashSet::new(),
            asked_threads: std::collections::HashSet::new(),
            recheck_accounts: std::collections::HashSet::new(),
            selected_id: None,
            selected_ids: Vec::new(),
            emitted_thread: Vec::new(),
            selection_count: 0,
            checking: false,
            nav_direction: 1,
            from_reader: 0,
            reader_keys: Vec::new(),
            expanded_threads: std::collections::HashSet::new(),
            show_recipient: false,
            restorable: false,
            in_junk: false,
            in_drafts: false,
            in_templates: false,
            default_expanded: false,
            msg_thread: std::collections::HashMap::new(),
            thread_members: std::collections::HashMap::new(),
            nested: std::collections::HashMap::new(),
            scroller: None,
            sort: SortOrder::DateNewest,
            quick: QuickFilters::default(),
            contact_addresses: None,
            last_count: String::new(),
            threading: true,
            thread_expansion: true,
            list_palette: true,
            single_line: false,
            columns: crate::config::ListColumn::DEFAULT.to_vec(),
            graph_accounts: std::collections::HashSet::new(),
            graph_in_view: false,
            sort_reversed: false,
            group_by_date: crate::config::load_privacy().list_group_by_date,
            day_headings: false,
            pills_below: crate::config::load_privacy().list_pills_below,
            headings: false,
            headings_bar: gtk::Box::new(gtk::Orientation::Horizontal, 8),
            widths: std::collections::HashMap::new(),
            column_scroll: false,
            pane_width: 0,
            heading_bins: Default::default(),
            bulk_tag_btn: gtk::Button::new(),
        };

        let bulk_tag_btn = model.bulk_tag_btn.clone();
        let headings_bar = model.headings_bar.clone();
        let widgets = view_output!();
        model.scroller = Some(widgets.scroller.clone());
        {
            // The headings follow the rows sideways, and a scroll over the
            // headings moves the rows. Setting the value it already has
            // emits nothing, so the two do not chase each other.
            let rows = widgets.scroller.hadjustment();
            let heads = widgets.headings_scroller.hadjustment();
            let h = heads.clone();
            rows.connect_value_changed(move |a| h.set_value(a.value()));
            let r = rows.clone();
            heads.connect_value_changed(move |a| r.set_value(a.value()));
            // The headings take their full width a frame after the rows
            // may have scrolled: put them back in step then.
            let r = rows.clone();
            heads.connect_changed(move |a| a.set_value(r.value()));
        }
        {
            // A right-click on the headings offers the way to the columns'
            // settings (#443).
            let click = gtk::GestureClick::new();
            click.set_button(3);
            let area = widgets.headings_area.clone();
            let s = sender.clone();
            click.connect_pressed(move |g, _, x, y| {
                g.set_state(gtk::EventSequenceState::Claimed);
                let s = s.clone();
                let entry = crate::ui::context_menu::MenuEntry::new(
                    i18n("Customize Columns…"),
                    move || {
                        let _ = s.output(MessageListOutput::CustomizeColumns);
                    },
                )
                .icon("view-list-bullet-symbolic");
                crate::ui::context_menu::show_context_menu(&area, x, y, vec![vec![entry]]);
            });
            widgets.headings_area.add_controller(click);
        }
        {
            // The rows' width, which fitted columns fit into: the view's
            // page across, set as it is allocated.
            let input = sender.input_sender().clone();
            widgets.scroller.hadjustment().connect_notify_local(
                Some("page-size"),
                move |adj, _| {
                    let _ = input.send(MessageListInput::PaneWidth(adj.page_size() as i32));
                },
            );
        }
        {
            // Whether the list sits at its bottom, for the spinner that says
            // more of the folder is on its way.
            let adj = widgets.scroller.vadjustment();
            let input = sender.input_sender().clone();
            let at_bottom = std::rc::Rc::new(std::cell::Cell::new(false));
            let near = std::rc::Rc::new(std::cell::Cell::new(false));
            let check = move |adj: &gtk::Adjustment| {
                let bottom = adj.upper() > adj.page_size()
                    && adj.value() + adj.page_size() >= adj.upper() - 1.0;
                if at_bottom.replace(bottom) != bottom {
                    let _ = input.send(MessageListInput::AtBottom(bottom));
                }
                // Two screens from the end, the next rows are added, so
                // they are there before the scroll reaches them.
                let close = adj.upper() > adj.page_size()
                    && adj.value() + adj.page_size() >= adj.upper() - 2.0 * adj.page_size();
                if near.replace(close) != close && close {
                    let _ = input.send(MessageListInput::NearEnd);
                }
            };
            let c = check.clone();
            adj.connect_value_changed(move |a| c(a));
            adj.connect_changed(move |a| check(a));
        }
        model.sync_look();
        model.search_entry = Some(widgets.search_entry.clone());
        model.bars = Some(BarSlots {
            top: widgets.bars_top.clone(),
            bottom: widgets.bars_bottom.clone(),
            search: widgets.search_revealer.clone(),
            bulk: widgets.bulk_revealer.clone(),
        });

        model.scope_picker = Some(ScopePicker::new(
            &widgets.scope_button,
            sender.input_sender(),
        ));
        model.place_bars();

        schedule_midnight_refresh(&sender);

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            MessageListInput::SetView { key, fp, restore } => {
                let kept = if restore {
                    self.take_view(key, fp)
                } else {
                    None
                };
                self.trim_views();
                self.view_key = key;
                self.view_fp = fp;
                match kept {
                    Some(snap) => {
                        self.pending_restore = Some(snap);
                        self.queue_rebuild(false);
                    }
                    None if restore => {
                        let _ = sender.output(MessageListOutput::ViewMissed);
                    }
                    None => {}
                }
            }
            MessageListInput::SetViewFp(fp) => self.view_fp = fp,
            MessageListInput::Prebuild { key, fp, messages } => {
                if self.view_key == Some(key)
                    || self.has_view(key, fp)
                    || messages.len() > KEEP_AT_MOST
                {
                    return;
                }
                let all: Vec<Rc<Message>> = messages.into_iter().map(Rc::new).collect();
                self.prebuild_gen += 1;
                let gen = self.prebuild_gen;
                if !self.threading {
                    self.prebuild_view(key, fp, all, None);
                    return;
                }
                let input = thread_inputs(&all);
                let links = self.thread_links.clone();
                let sender = self.input.clone();
                self.prebuild = Some((gen, key, fp, all));
                std::thread::spawn(move || {
                    let threads = source_threads_of(&input, &links, false);
                    let _ = sender.send(MessageListInput::PrebuildThreads {
                        gen,
                        threads: Box::new(threads),
                    });
                });
            }
            MessageListInput::PrebuildThreads { gen, threads } => {
                if let Some((g, key, fp, all)) = self.prebuild.take() {
                    if g == gen {
                        self.prebuild_view(key, fp, all, Some(Rc::new(*threads)));
                    } else {
                        self.prebuild = Some((g, key, fp, all));
                    }
                }
            }
            MessageListInput::SetMessages { messages } => {
                self.pending_restore = None;
                let t = std::time::Instant::now();
                let n = messages.len();
                self.all = messages.into_iter().map(Rc::new).collect();
                self.drop_threads();
                self.loaded = true;
                // Keep any active search query: this also fires for a background
                // re-sync of the folder you're viewing, which shouldn't drop your
                // search. Folder switches clear the query via `ResetPaging` first.
                self.queue_rebuild(true);
                tracing::debug!("list: set {n} messages in {:?}", t.elapsed());
            }
            MessageListInput::AppendMessages { messages } => {
                self.pending_restore = None;
                // Grow the searchable index in place. Dedup by (account, uid) since
                // UIDs collide across accounts in the unified inbox.
                let existing: std::collections::HashSet<(u32, u32)> =
                    self.all.iter().map(|m| (m.account_id, m.uid)).collect();
                let before = self.all.len();
                for m in messages {
                    if !existing.contains(&(m.account_id, m.uid)) {
                        self.all.push(Rc::new(m));
                    }
                }
                // The list holds the whole folder, so what arrives joins it.
                if self.all.len() != before {
                    self.drop_threads();
                    self.queue_rebuild(true);
                }
            }
            MessageListInput::SetLoading => {
                self.pending_restore = None;
                self.all.clear();
                self.drop_threads();
                self.loaded = false;
                self.clear_search(&sender);
                // Queued: when the folder's list follows in the same pass
                // (served from cache), the old rows are torn down once, for
                // the new ones, not first for nothing.
                self.queue_rebuild(false);
            }
            MessageListInput::ResetPaging => {
                // The view being left is kept as built, to come back to.
                self.keep_view();
                self.view_key = None;
                // Folder switch: drop any active search, scrolled to the top.
                self.switched = true;
                self.window = LIST_WINDOW;
                self.late_select = None;
                if std::mem::take(&mut self.checking) {
                    let _ = sender.output(MessageListOutput::Checked(None));
                }
                self.clear_search(&sender);
                self.emitted_thread.clear();
                // However the new view's list arrives, a rebuild follows, and
                // the top is scrolled to then, not over the old rows first.
                self.queue_rebuild(false);
            }
            MessageListInput::SetIndexComplete(complete) => {
                self.index_complete = complete;
            }
            MessageListInput::AtBottom(bottom) => self.at_bottom = bottom,
            MessageListInput::NearEnd => self.grow_window(LIST_WINDOW),
            MessageListInput::ThreadsReady { gen, threads } => {
                if self.threads_job.get() == Some(gen) {
                    self.threads_job.set(None);
                }
                if gen == self.threads_gen.get() && threads.pool == self.searching_pool() {
                    *self.source_threads.borrow_mut() = Some(std::rc::Rc::new(*threads));
                    self.queue_rebuild(true);
                }
            }
            MessageListInput::AskThreads => {
                // Only the conversations rows have come to show, and only once
                // each: the answer arrives as a refresh of those rows, so
                // asking again on the strength of it would never settle.
                let wanted = self.shared.take_wanted();
                if !self.threading {
                    return;
                }
                let mut listed: Vec<(u32, String, Vec<String>)> = Vec::new();
                for key in wanted {
                    if listed.iter().any(|(a, r, _)| (*a, r) == (key.0, &key.1)) {
                        continue;
                    }
                    let Some(members) = self.groups.get(&key) else {
                        continue;
                    };
                    let ids = crate::models::thread_ids(members.iter().map(|m| &**m));
                    if !ids.is_empty() {
                        listed.push((key.0, key.1, ids));
                    }
                }
                let fresh = unasked_threads(&listed, &self.asked_threads);
                if !fresh.is_empty() {
                    for (aid, root, _) in &fresh {
                        self.asked_threads.insert((*aid, root.clone()));
                    }
                    let _ = sender.output(MessageListOutput::ThreadsListed { groups: fresh });
                }
            }
            MessageListInput::RunQueuedRebuild => {
                self.switched = false;
                if let Some(preserve) = self.rebuild_queued.take() {
                    match self.pending_restore.take() {
                        Some(snap) => self.restore_view(snap),
                        None => self.rebuild(),
                    }
                    if !preserve {
                        self.scroll_top();
                    }
                }
                self.recheck_bound_rows();
                // The rows exist now: run the selection that waited for them.
                if let Some(key) = self.pending_select.take() {
                    let _ = self.input.send(MessageListInput::SelectAndLoad(key));
                } else if let Some((key, at)) = self.late_select {
                    if at.elapsed() > std::time::Duration::from_secs(30) {
                        self.late_select = None;
                    } else if self.shown.iter().any(|m| (m.account_id, m.id) == key)
                        || self.thread_head_for(key).is_some()
                    {
                        let _ = self.input.send(MessageListInput::SelectAndLoad(key));
                    }
                }
                self.report_thread_growth(&sender);
            }
            MessageListInput::SetThreadLinks(links) => {
                if self.thread_links != links {
                    self.thread_links = links;
                    self.drop_threads();
                    if self.threading {
                        self.queue_rebuild(true);
                    }
                }
            }
            MessageListInput::SetThreadSummaries(summaries) => {
                // They arrive a beat after the rows that asked are shown (the
                // cache is the worker's, not ours). Only what those rows say
                // moves; which conversations are listed does not — which is
                // what keeps this from asking again and looping.
                let mut changed = Vec::new();
                for (key, summary) in summaries {
                    if self.thread_summaries.get(&key) != Some(&summary) {
                        self.thread_summaries.insert(key.clone(), summary);
                        changed.push(key);
                    }
                }
                if !changed.is_empty() && self.threading {
                    // A rebuild already on its way reads them anyway.
                    if self.rebuild_queued.is_none() {
                        self.apply_summaries(changed);
                    }
                }
            }
            MessageListInput::RecheckThreadSummaries(account_id) => {
                // The summaries are kept, not dropped: dropping them turned
                // every conversation row on screen back into its folder-only
                // self, and a row kept across the rebuild is not bound again,
                // so it never asked again either (#330). The answers replace
                // whatever has changed.
                self.asked_threads.retain(|(aid, _)| *aid != account_id);
                self.recheck_accounts.insert(account_id);
                if self.rebuild_queued.is_none() {
                    self.recheck_bound_rows();
                }
            }
            MessageListInput::ConversationSize {
                account_id,
                id,
                size,
            } => {
                if !self.threading || size < 2 {
                    return;
                }
                let key = (account_id, id);
                let row = self
                    .shown
                    .iter()
                    .position(|m| (m.account_id, m.id) == key)
                    .or_else(|| {
                        let head = self.thread_head_for(key)?;
                        self.shown.iter().position(|m| (m.account_id, m.id) == head)
                    })
                    .and_then(|pos| self.shared.model.row(pos));
                let Some(row) = row.filter(|r| r.meta.count < size) else {
                    return;
                };
                if let Some(group) = row.meta.group.clone() {
                    self.asked_threads.remove(&group);
                    self.shared.want(group);
                }
            }
            MessageListInput::SetThreadRowNewest(on) => {
                if self.thread_row_newest != on {
                    self.thread_row_newest = on;
                    if self.threading {
                        self.queue_rebuild(true);
                    }
                }
            }
            MessageListInput::SetOwnAddresses(addresses) => {
                if self.own_addresses != addresses {
                    self.own_addresses = addresses;
                    if self.threading && self.thread_expansion && !self.thread_row_newest {
                        self.queue_rebuild(true);
                    }
                }
            }
            MessageListInput::SetThreading(on) => {
                if self.threading != on {
                    self.threading = on;
                    self.rebuild();
                    self.scroll_top();
                }
            }
            MessageListInput::SetThreadExpansion(on) => {
                if self.thread_expansion != on {
                    self.thread_expansion = on;
                    // Turning expansion off folds every open thread (the
                    // `expanded` computation ignores the stored toggles while
                    // off); turning it on restores them.
                    self.sync_look();
                    self.rebuild();
                }
            }
            MessageListInput::SetListPalette(on) => {
                if self.list_palette != on {
                    self.list_palette = on;
                    self.sync_look();
                }
            }
            MessageListInput::ResolveDelete => {
                // A lone selected row that heads a multi-message conversation
                // stands for the whole thread: hand every member to the app
                // (which confirms before deleting). Anything else — multiple
                // rows, a reply row, a plain message — is an ordinary bulk
                // delete of exactly what is selected.
                let selected = self.selected_messages();
                if let [m] = selected.as_slice() {
                    let key = (m.account_id, m.id);
                    if let Some(tkey) = self.msg_thread.get(&key) {
                        let members = self.thread_members.get(tkey).cloned().unwrap_or_default();
                        if members.len() > 1 && members.first() == Some(&key) {
                            let messages: Vec<Message> = members
                                .iter()
                                .filter_map(|mk| {
                                    self.active_source()
                                        .iter()
                                        .find(|x| (x.account_id, x.id) == *mk)
                                        .map(|m| Message::clone(m))
                                })
                                .collect();
                            let _ = sender.output(MessageListOutput::DeleteThread { messages });
                            return;
                        }
                    }
                }
                sender.input(MessageListInput::Bulk(BulkAction::Delete));
            }
            MessageListInput::SetThreadsExpanded(on) => {
                if self.default_expanded != on {
                    self.default_expanded = on;
                    // Per-thread toggles were exceptions to the old default;
                    // drop them so everything follows the new one.
                    self.expanded_threads.clear();
                    self.rebuild();
                    self.scroll_top();
                }
            }
            // Every row's date, and a conversation's latest, is worked out
            // by the rebuild.
            MessageListInput::RefreshDates => self.rebuild(),
            MessageListInput::SetSenderLogos(on) => {
                if self.sender_logos != on {
                    self.sender_logos = on;
                    self.face_gen += 1;
                    self.sync_look();
                }
            }
            MessageListInput::SetSingleLine(on) => {
                if self.single_line != on {
                    self.single_line = on;
                    // The date headings line up with a single line's text.
                    if on {
                        self.list_view.add_css_class("single-line");
                    } else {
                        self.list_view.remove_css_class("single-line");
                    }
                    self.sync_look();
                }
            }
            MessageListInput::SetColumns(columns) => {
                if self.columns != columns {
                    // The Correspondents column is worked out by the rebuild.
                    let people = |c: &[crate::config::ListColumn]| {
                        c.contains(&crate::config::ListColumn::Correspondents)
                    };
                    let rebuild = people(&columns) != people(&self.columns);
                    self.columns = columns;
                    self.sync_look();
                    if rebuild {
                        self.queue_rebuild(true);
                    }
                }
            }
            MessageListInput::SetAccountNames { names, graph } => {
                let changed =
                    *self.shared.account_names.borrow() != names || self.graph_accounts != graph;
                if changed {
                    *self.shared.account_names.borrow_mut() = names;
                    self.graph_accounts = graph;
                    self.graph_in_view = self
                        .listed_folders
                        .iter()
                        .any(|(a, _)| self.graph_accounts.contains(a));
                    self.sync_look();
                }
            }
            MessageListInput::SetLook {
                avatars,
                preview_lines,
                subject,
                animate,
            } => {
                let preview_lines = preview_lines.min(3);
                let avatars_changed = self.avatars != avatars;
                let lines_changed = self.preview_lines != preview_lines;
                let subject_changed = self.show_subject != subject;
                if !avatars_changed && !lines_changed && !subject_changed {
                    return;
                }
                self.show_subject = subject;
                if animate && avatars_changed && !avatars {
                    // Slide every circle on screen away (and the preview to
                    // its new height in place); the rows give the slot back
                    // once they have gone.
                    self.preview_lines = preview_lines;
                    for row in self.shared.bound_rows() {
                        row.slide_avatar_away();
                        if lines_changed {
                            row.set_preview_lines(preview_lines);
                        }
                    }
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(u64::from(crate::ui::FOCUS_ANIM_MS) + 40),
                        move || s.input(MessageListInput::LookSettled),
                    );
                } else {
                    self.avatars = avatars;
                    self.preview_lines = preview_lines;
                    self.sync_look();
                    // Circles coming back start folded and slide in.
                    if animate && avatars_changed && avatars {
                        for row in self.shared.bound_rows() {
                            row.slide_avatar_in();
                        }
                    }
                }
            }
            MessageListInput::LookSettled => {
                self.avatars = false;
                self.sync_look();
            }
            MessageListInput::SetAvatars(on) => {
                if self.avatars != on {
                    self.avatars = on;
                    self.sync_look();
                }
            }
            MessageListInput::SetGravatar(on) => {
                if self.gravatar != on {
                    self.gravatar = on;
                    self.face_gen += 1;
                    self.sync_look();
                }
            }
            MessageListInput::SetShowRecipient(on) => {
                if self.show_recipient != on {
                    self.show_recipient = on;
                    self.sync_look();
                }
            }
            MessageListInput::SetRestorable(on) => self.restorable = on,
            MessageListInput::SetInJunk(on) => {
                if self.in_junk != on {
                    self.in_junk = on;
                    self.sync_look();
                }
            }
            MessageListInput::SetInDrafts(on) => {
                if self.in_drafts != on {
                    self.in_drafts = on;
                    self.sync_look();
                }
            }
            MessageListInput::SetInTemplates(on) => self.in_templates = on,
            MessageListInput::ContactPhotosChanged => {
                // The circles on screen look again; the rest do when shown.
                self.face_gen += 1;
                self.sync_look();
            }
            MessageListInput::SetColorize(on) => {
                if self.colorize != on {
                    self.colorize = on;
                    self.sync_look();
                }
            }
            MessageListInput::DayChanged => {
                // Re-render so relative labels like "Today" reflect the new date.
                self.rebuild();
                schedule_midnight_refresh(&sender);
            }
            MessageListInput::SetAccountColors(colors) => {
                self.account_colors = colors;
                self.refresh_tint_css();
                self.sync_look();
            }
            MessageListInput::Search(q) => {
                let was_active = self.wants_pool();
                self.query = q;
                let now_active = self.wants_pool();
                // On the empty↔non-empty edge, tell the app to supply or drop the
                // cross-folder pool.
                if was_active != now_active {
                    let _ = sender.output(MessageListOutput::SearchActive(now_active));
                }
                if self.query.is_empty() {
                    *self.search_text.borrow_mut() = Default::default();
                    self.search_hits = None;
                }
                // A large list is searched on another thread, and its rows
                // stay as they are until the answer is in: typing held the
                // main loop for as long as each search took (#515).
                if !self.start_search() {
                    self.rebuild();
                    self.scroll_top();
                }
            }
            MessageListInput::SearchMatched {
                gen,
                query,
                text,
                hits,
            } => {
                if gen == self.search_gen.load(std::sync::atomic::Ordering::Relaxed)
                    && query == self.query
                {
                    self.search_hits = Some(SearchHits { query, text, hits });
                    self.rebuild();
                    self.scroll_top();
                }
            }
            MessageListInput::SetScope(scope) => {
                if let Some(picker) = &self.scope_picker {
                    picker.sync(scope);
                }
                if self.scope != scope {
                    let was_active = self.wants_pool();
                    self.scope = scope;
                    // "This folder" has no use for the pool, which can run to
                    // every message of every account (#472).
                    if was_active != self.wants_pool() {
                        let _ = sender.output(MessageListOutput::SearchActive(!was_active));
                    }
                    // Scope only affects the view while a query is present.
                    if self.searching() {
                        self.sync_look();
                        self.rebuild();
                        self.scroll_top();
                    }
                }
            }
            MessageListInput::SetSearchPool(pool, folders, text) => {
                self.search_pool = pool.into_iter().map(Rc::new).collect();
                self.search_text.borrow_mut()[1] = (text.len() == self.search_pool.len()
                    && !text.is_empty())
                .then(|| SearchText {
                    of: self.search_pool.iter().map(Rc::as_ptr).collect(),
                    text: std::sync::Arc::new(text),
                });
                self.pool_folders = Rc::new(folders);
                self.drop_threads();
                self.sync_look();
                if self.searching() && self.scope != SearchScope::ThisFolder {
                    self.rebuild();
                }
            }
            MessageListInput::SelectFromReader { keys, conversation } => {
                // An empty set means the reader cleared its card selection (a
                // click on the document's empty space). The list keeps the
                // viewed message highlighted rather than losing its anchor —
                // the focus-within CSS dims the highlight instead.
                let keys = if keys.is_empty() {
                    self.selected_id.into_iter().collect()
                } else {
                    keys
                };
                // The list shows only a thread's head while it is collapsed, so
                // selecting a reply has to open the thread first — otherwise
                // there is no row to select and only the head would ever answer.
                let hidden: Vec<(u32, String)> = keys
                    .iter()
                    .filter(|k| !self.shown.iter().any(|m| (m.account_id, m.id) == **k))
                    .filter_map(|k| self.msg_thread.get(k).cloned())
                    .collect();
                if !hidden.is_empty() && self.thread_expansion {
                    for thread_key in hidden {
                        // `expanded_threads` records the departure from the
                        // default, so which way to move it depends on that.
                        if self.default_expanded {
                            self.expanded_threads.remove(&thread_key);
                        } else {
                            self.expanded_threads.insert(thread_key);
                        }
                    }
                    self.rebuild();
                }
                // The conversation as the reader shows it: what this list
                // handed over plus what the app merged in from other folders
                // since (#220). Either way it was opened from the viewed row.
                let conversation = reader_conversation(&self.emitted_thread, &conversation);
                let positions: Vec<usize> = keys
                    .iter()
                    .filter_map(|key| {
                        row_for_reader_key(
                            key,
                            &self.shown,
                            &self.msg_thread,
                            &conversation,
                            self.selected_id,
                        )
                    })
                    .collect();
                // What the reader asked for, as this list can represent it, so
                // the change GTK is about to report is recognised as ours
                // rather than the user's.
                self.reader_keys = self.keys_at(&positions);
                self.from_reader = 8;
                self.select_positions(&positions);
                sender.input(MessageListInput::SelectionChanged);
            }
            MessageListInput::SelectionChanged => {
                let positions = self.selected_positions();
                let keys = self.keys_at(&positions);
                self.selection_count = keys.len();
                // Set from the reader, which is already showing these messages:
                // mirror the selection but leave the reader alone. Reporting it
                // back would also drop whatever the reader has selected that this
                // list has no row for.
                if self.from_reader > 0 {
                    self.from_reader -= 1;
                    if keys == self.reader_keys {
                        self.selected_ids = keys;
                        return;
                    }
                    // Something else moved the selection — stop expecting ours.
                    self.from_reader = 0;
                }
                // A selection the user made here; the reader outlines it, and
                // it wins over one still waiting for its row.
                if !keys.is_empty() {
                    self.late_select = None;
                }
                let _ = sender.output(MessageListOutput::SelectionKeys(self.slots_at(&positions)));
                if self.checking {
                    // Checked rows are not opened, one or many (#392).
                    self.selected_id = None;
                    let _ = sender.output(MessageListOutput::Checked(Some(keys.len())));
                    self.selected_ids = keys;
                    return;
                }
                match keys.as_slice() {
                    [] => self.selected_id = None,
                    [key] => {
                        // Exactly one selected → show it in the reader. Skip when
                        // it's already the viewed row (e.g. programmatic restore
                        // after a rebuild) so it isn't needlessly reloaded.
                        if self.selected_id != Some(*key) {
                            // Which way did the user move? Only a change with a
                            // known previous row says anything — the post-delete
                            // advance clears selected_id first, so it can never
                            // flip the direction it is itself steering by.
                            let pos = |k: &(u32, u32)| {
                                self.shown.iter().position(|m| (m.account_id, m.id) == *k)
                            };
                            if let (Some(old), Some(new)) =
                                (self.selected_id.as_ref().and_then(&pos), pos(key))
                            {
                                if new != old {
                                    self.nav_direction = if new > old { 1 } else { -1 };
                                }
                            }
                            self.selected_id = Some(*key);
                            if let Some(m) = self
                                .shown
                                .iter()
                                .find(|m| (m.account_id, m.id) == *key)
                                .map(|m| Message::clone(m))
                            {
                                let (thread, solo) = self.conversation_for(&m);
                                self.emitted_thread =
                                    thread.iter().map(|t| (t.account_id, t.id)).collect();
                                let _ = sender.output(MessageListOutput::Selected {
                                    message: m,
                                    thread,
                                    solo,
                                });
                            }
                        }
                    }
                    // Multiple selected → keep the reader on the primary message.
                    _ => {}
                }
                self.selected_ids = keys;
            }
            MessageListInput::Bulk(action) => {
                // The bulk bar's spam button is one button: in Junk it means
                // the reverse.
                let action = if self.in_junk && action == BulkAction::Spam {
                    BulkAction::NotSpam
                } else {
                    action
                };
                let rows = self.selected_messages();
                // Likewise the star: it clears a selection that is starred
                // throughout, or it could never be taken off again (#313).
                let action = if action == BulkAction::Flag
                    && !rows.is_empty()
                    && rows.iter().all(|m| m.starred)
                {
                    BulkAction::Unflag
                } else {
                    action
                };
                // A conversation's row stands for all of it, as it does for a
                // lone Delete: marking several read reached only the first
                // message of each (#392). A star goes on the row's message.
                let messages = if action == BulkAction::Flag {
                    rows
                } else {
                    self.selected_conversations()
                };
                if !messages.is_empty() {
                    let _ = sender.output(MessageListOutput::Bulk { action, messages });
                }
                // The selection stays, and with it the bulk bar: read and star
                // change rows in place, so a second action (or the same one
                // again, to undo it) can follow without selecting anew (#313).
                // Row-removing actions need it too: the RemoveMany that
                // follows reads it to know the viewed message is going away
                // and to advance the selection (and reader) in its place.
            }
            MessageListInput::MoveSelection(delta) => {
                if self.shown.is_empty() {
                    return;
                }
                if delta != 0 {
                    self.nav_direction = delta.signum();
                }
                // From the current row, or from the top/bottom when nothing is
                // selected yet, so the first keypress always lands somewhere.
                let current = self
                    .selected_positions()
                    .first()
                    .map(|&p| p as i64)
                    .unwrap_or(if delta > 0 {
                        -1
                    } else {
                        self.shown.len() as i64
                    });
                let next = (current + delta as i64).clamp(0, self.shown.len() as i64 - 1) as usize;
                self.shared.set_cursor(None);
                self.shared.selection.select_item(next as u32, true);
                self.stop_checking(&sender);
                self.focus_row(next);
            }

            MessageListInput::SetGmailKeys(on) => {
                self.gmail_keys = on;
                if !on {
                    self.stop_checking(&sender);
                }
            }
            MessageListInput::MoveCursor(delta) => {
                if self.shown.is_empty() {
                    return;
                }
                if delta != 0 {
                    self.nav_direction = delta.signum();
                }
                // From the row the keyboard is on, else the open one, else
                // the top or the bottom.
                let from = self
                    .cursor_position()
                    .or_else(|| self.selected_positions().first().copied());
                let next = self.conversation_step(from, delta);
                self.focus_row(next);
                // Drawn by the row itself: the list's focus ring is off, and
                // the window's focus-visible flag is not reliably set by a key
                // a shortcut handler consumed.
                self.shared
                    .set_cursor(self.shown.get(next).map(|m| (m.account_id, m.id)));
            }
            MessageListInput::OpenCursor => {
                // Several checked are for an action, not for reading: which
                // one to open is anyone's guess (#392).
                if self.checking && self.selection_count > 1 {
                    return;
                }
                let Some(pos) = self
                    .cursor_position()
                    .or_else(|| self.selected_positions().first().copied())
                else {
                    return;
                };
                self.shared.set_cursor(None);
                let already_open = !self.checking && self.selected_positions() == [pos];
                self.shared.selection.select_item(pos as u32, true);
                self.stop_checking(&sender);
                self.focus_row(pos);
                let _ = sender.output(MessageListOutput::OpenedByKey { already_open });
            }
            MessageListInput::PointerPressed { extend } => {
                self.shared.set_cursor(None);
                if !extend {
                    self.stop_checking(&sender);
                }
            }
            MessageListInput::MoveSelectedTo { account_id, dest } => {
                let messages = self.selected_conversations();
                if !messages.is_empty() {
                    let _ = sender.output(MessageListOutput::MoveMessages {
                        account_id,
                        dest,
                        messages,
                    });
                }
            }
            MessageListInput::CloseOpen => {
                // Only an open row closes: with rows checked, or none
                // selected, the keyboard just comes back to the list.
                let selected = self.selected_positions();
                let [pos] = selected[..] else {
                    sender.input(MessageListInput::FocusList);
                    return;
                };
                if self.checking {
                    sender.input(MessageListInput::FocusList);
                    return;
                }
                self.selected_id = None;
                self.selected_ids.clear();
                self.selection_count = 0;
                self.shared.selection.unselect_all();
                self.focus_row(pos);
                // The cursor stays on the row just read, so j, k and o carry
                // on from there.
                self.shared
                    .set_cursor(self.shown.get(pos).map(|m| (m.account_id, m.id)));
                let _ = sender.output(MessageListOutput::SelectionCleared);
            }
            MessageListInput::StepConversation(delta) => {
                if self.shown.is_empty() {
                    return;
                }
                if delta != 0 {
                    self.nav_direction = delta.signum();
                }
                let from = self.selected_positions().first().copied();
                let next = self.conversation_step(from, delta);
                if from == Some(next) {
                    return;
                }
                self.shared.set_cursor(None);
                // Selecting opens it; the keyboard stays where it is, in the
                // reader, so the next j or k goes on from there.
                self.shared.selection.select_item(next as u32, true);
                self.list_view
                    .scroll_to(next as u32, gtk::ListScrollFlags::NONE, None);
                self.reveal_first_heading(next);
            }
            MessageListInput::SelectAllShown => {
                if !self.shown.is_empty() {
                    // Checked, as Gmail's * a checks them: nothing opens.
                    self.checking = true;
                    self.selected_id = None;
                    self.shared.selection.select_all();
                    sender.input(MessageListInput::SelectionChanged);
                }
            }
            MessageListInput::SelectWhere(filter) => {
                let is_child = |p: usize| self.shared.model.row(p).is_some_and(|r| r.meta.is_child);
                let picked = gtk::Bitset::new_empty();
                for (pos, m) in self.shown.iter().enumerate() {
                    if !is_child(pos) && filter.matches(&self.conversation_for(m).0) {
                        picked.add(pos as u32);
                    }
                }
                // Checked, as with * a; none matching leaves none checked.
                self.checking = true;
                self.selected_id = None;
                let all = gtk::Bitset::new_range(0, self.shown.len() as u32);
                self.shared.selection.set_selection(&picked, &all);
                sender.input(MessageListInput::SelectionChanged);
            }
            MessageListInput::SetNavDirection(delta) => {
                if delta != 0 {
                    self.nav_direction = delta.signum();
                }
            }
            MessageListInput::ToggleSelection => {
                // The row the keyboard is on, or the first selected one.
                let Some(pos) = self
                    .cursor_position()
                    .or_else(|| self.selected_positions().first().copied())
                else {
                    return;
                };
                let selection = &self.shared.selection;
                if self.gmail_keys && !self.checking {
                    // Gmail's x checks a conversation without opening it
                    // (#392): the one open closes, and the checks start
                    // from this row.
                    self.checking = true;
                    self.selected_id = None;
                    selection.select_item(pos as u32, true);
                } else if selection.is_selected(pos as u32) {
                    selection.unselect_item(pos as u32);
                } else {
                    selection.select_item(pos as u32, false);
                }
                if self.checking {
                    // The keyboard stays on the row, for the next j, k or x.
                    self.focus_row(pos);
                    self.shared
                        .set_cursor(self.shown.get(pos).map(|m| (m.account_id, m.id)));
                    // Told even when GTK's selection did not change, as when
                    // the row checked is the one that was open.
                    sender.input(MessageListInput::SelectionChanged);
                }
            }

            MessageListInput::SetQuickFilters { filters, contacts } => {
                let contacts_changed = filters.has(QuickFilter::Contacts)
                    && match (&self.contact_addresses, &contacts) {
                        (Some(a), Some(b)) => !std::sync::Arc::ptr_eq(a, b),
                        (a, b) => a.is_some() != b.is_some(),
                    };
                self.contact_addresses = contacts;
                if self.quick != filters || contacts_changed {
                    self.quick = filters;
                    self.rebuild();
                }
            }

            MessageListInput::FocusList => {
                // The "back to list" shortcut: deliberately returns to the
                // selected row (that's the point — resume j/k navigation
                // from what you were reading), so it's allowed to scroll.
                if !self.shown.is_empty() {
                    let pos = self.selected_positions().first().copied().unwrap_or(0);
                    self.shared.set_cursor(None);
                    self.focus_row(pos);
                }
            }
            MessageListInput::ReclaimFocus => {
                // Housekeeping focus restore (e.g. after a compose window
                // closes) so keyboard shortcuts keep targeting the list —
                // unlike `FocusList`, this isn't a "go back to what I was
                // reading" action, so it shouldn't scroll the viewport away
                // from wherever the user was browsing.
                self.preserving_scroll(|this| {
                    this.list_view.grab_focus();
                });
                self.hide_focus_ring();
            }

            MessageListInput::ShowcaseSearch(text, scope) => {
                self.search_open = true;
                if let Some(scope) = SearchScope::ALL.get(scope as usize) {
                    sender.input(MessageListInput::SetScope(*scope));
                }
                if let Some(entry) = &self.search_entry {
                    entry.set_text(&text);
                }
                // HYLKI_SHOWCASE_SCOPE_MENU=<png> opens the scope popover and
                // captures it.
                if let (Ok(path), Some(picker)) = (
                    std::env::var("HYLKI_SHOWCASE_SCOPE_MENU"),
                    &self.scope_picker,
                ) {
                    picker.button.popup();
                    if let Some(popover) = picker.button.popover() {
                        gtk::glib::timeout_add_seconds_local_once(1, move || {
                            crate::app::showcase_capture(popover.upcast_ref(), &path);
                        });
                    }
                    // HYLKI_SHOWCASE_SCOPE_INFO=<png> opens the first scope's
                    // info popover on top and captures that.
                    if let (Ok(path), Some(info)) = (
                        std::env::var("HYLKI_SHOWCASE_SCOPE_INFO"),
                        picker.infos.first(),
                    ) {
                        let info = info.clone();
                        gtk::glib::timeout_add_seconds_local_once(2, move || {
                            info.popup();
                            if let Some(popover) = info.popover() {
                                gtk::glib::timeout_add_seconds_local_once(1, move || {
                                    crate::app::showcase_capture(popover.upcast_ref(), &path);
                                });
                            }
                        });
                    }
                }
            }
            MessageListInput::FocusSearch => {
                // A field that is always there has nothing to open or close.
                if self.search_pinned {
                    if let Some(entry) = &self.search_entry {
                        entry.grab_focus();
                    }
                    return;
                }
                // Toggle (#102): the header button closes an open search too.
                if self.search_open {
                    sender.input(MessageListInput::CloseSearch);
                    return;
                }
                // Clicking the button blurs an empty entry, whose focus-leave
                // just closed the bar — that same click must not reopen it.
                if self
                    .search_closed_at
                    .take()
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300))
                {
                    return;
                }
                self.search_open = true;
                if let Some(entry) = &self.search_entry {
                    let entry = entry.clone();
                    // After the revealer maps it; focusing an unmapped entry
                    // is a no-op.
                    gtk::glib::idle_add_local_once(move || {
                        entry.grab_focus();
                    });
                }
            }

            MessageListInput::CloseSearch => {
                if self.search_pinned {
                    // Esc in the field hands the keyboard back to the list;
                    // an empty field losing focus has gone somewhere already.
                    let had_focus = self
                        .search_entry
                        .as_ref()
                        .is_some_and(|e| e.has_focus() || e.focus_child().is_some());
                    if !self.query.is_empty() {
                        self.clear_search(&sender);
                        self.rebuild();
                        self.scroll_top();
                    }
                    if had_focus {
                        self.preserving_scroll(|this| {
                            this.list_view.grab_focus();
                        });
                    }
                    return;
                }
                self.search_open = false;
                self.search_closed_at = Some(std::time::Instant::now());
                self.clear_search(&sender);
                self.rebuild();
            }

            MessageListInput::BulkToggleRead => {
                let action = if self.selection_any_unread() {
                    BulkAction::MarkRead
                } else {
                    BulkAction::MarkUnread
                };
                sender.input(MessageListInput::Bulk(action));
            }
            MessageListInput::BulkTagMenu => {
                let btn = self.bulk_tag_btn.clone();
                let entries = self.bulk_tag_entries(&sender);
                if !entries.is_empty() {
                    // The menu takes the keyboard while it is open and does
                    // not hand it back, which greys the selection and stops
                    // the single-key shortcuts: give it back to the list.
                    let before = btn.root().and_then(|r| r.focus());
                    let popover = crate::ui::context_menu::show_context_menu_popover(
                        &btn,
                        (btn.width() / 2) as f64,
                        btn.height() as f64,
                        None,
                        vec![entries],
                    );
                    popover.connect_closed(move |_| {
                        if let Some(w) = &before {
                            w.grab_focus();
                        }
                    });
                }
            }
            MessageListInput::ClearSelection => {
                self.shared.selection.unselect_all();
                self.selected_id = None;
                self.selected_ids.clear();
                self.selection_count = 0;
            }
            MessageListInput::SetSort(order) => {
                if self.sort != order || self.sort_reversed {
                    self.sort = order;
                    self.sort_reversed = false;
                    self.rebuild();
                    self.scroll_top();
                    self.sync_headings();
                }
            }
            MessageListInput::ResizeColumn {
                column,
                width,
                done,
            } => {
                match width {
                    Some(px) => self.widths.insert(column, px),
                    None => self.widths.remove(&column),
                };
                // The rows follow every step; the headings are left alone
                // while their handle is held, as rebuilding them would drop
                // it mid-drag. It has moved its own heading already.
                let min_width = self.row_min_width();
                let fitted = self.fitted_widths();
                for (c, bin) in self.heading_bins.borrow().iter() {
                    bin.set_width(
                        fitted
                            .get(c)
                            .copied()
                            .unwrap_or_else(|| crate::ui::message_row::default_width(*c)),
                    );
                }
                self.headings_bar.set_size_request(min_width, -1);
                {
                    let mut look = self.shared.look.borrow_mut();
                    look.widths = fitted;
                    look.min_width = min_width;
                }
                self.shared.refresh_all();
                if done {
                    let input = self.shared.input.clone();
                    glib::idle_add_local_once(move || {
                        let _ = input.send(MessageListInput::RefreshHeadings);
                    });
                    let _ = sender.output(MessageListOutput::ColumnWidths(self.widths.clone()));
                }
            }
            MessageListInput::SetColumnWidths(widths) => {
                if self.widths != widths {
                    self.widths = widths;
                    self.sync_look();
                }
            }
            MessageListInput::RefreshHeadings => self.sync_headings(),
            MessageListInput::SetBarsPlacement {
                search_bottom,
                bulk_bottom,
            } => {
                if (self.search_bottom, self.bulk_bottom) != (search_bottom, bulk_bottom) {
                    self.search_bottom = search_bottom;
                    self.bulk_bottom = bulk_bottom;
                    self.place_bars();
                }
            }
            MessageListInput::SetSearchPinned(on) => {
                self.search_pinned = on;
                // Unpinned, it stays only while it holds a search.
                if !on {
                    self.search_open = !self.query.is_empty();
                }
            }
            MessageListInput::SetColumnScroll(on) => {
                if self.column_scroll != on {
                    self.column_scroll = on;
                    self.sync_look();
                }
            }
            MessageListInput::PaneWidth(width) => {
                if self.pane_width != width {
                    let fits = self.single_line && !self.column_scroll;
                    let before = fits.then(|| self.fitted_widths());
                    self.pane_width = width;
                    if before.is_some_and(|b| b != self.fitted_widths()) {
                        self.sync_look();
                    }
                }
            }
            MessageListInput::SetHeadings(on) => {
                self.headings = on;
                self.sync_headings();
            }
            MessageListInput::SetGroupByDate(on) => {
                if self.group_by_date != on {
                    self.group_by_date = on;
                    self.queue_rebuild(true);
                }
            }
            MessageListInput::SetPillsBelow(on) => {
                if self.pills_below != on {
                    self.pills_below = on;
                    self.sync_look();
                }
            }
            MessageListInput::SortByColumn(column) => {
                let order = column_sort(column, self.show_recipient);
                match (order, self.sort) {
                    // Date has an order each way of its own.
                    (SortOrder::DateNewest, SortOrder::DateNewest) => {
                        self.sort = SortOrder::DateOldest
                    }
                    (SortOrder::DateNewest, SortOrder::DateOldest) => {
                        self.sort = SortOrder::DateNewest
                    }
                    (order, current) if order == current => {
                        self.sort_reversed = !self.sort_reversed
                    }
                    (order, _) => {
                        self.sort = order;
                        self.sort_reversed = false;
                    }
                }
                self.rebuild();
                self.scroll_top();
                self.sync_headings();
                let _ = sender.output(MessageListOutput::SortChanged(self.sort.key()));
            }
            MessageListInput::ToggleThread(key) => {
                // Expansion disabled: the chevron stays but does nothing — the
                // conversation is read through the reader's cards.
                if !self.thread_expansion {
                    return;
                }
                let was_expanded = self.expanded_threads.contains(&key) != self.default_expanded;
                if was_expanded {
                    // Slide the replies shut in place; the list only drops
                    // them once that animation has actually finished (see
                    // `start_collapse_thread`) — dropping them right away
                    // would just make them vanish instead of collapsing.
                    self.start_collapse_thread(key, &sender);
                } else {
                    if !self.expanded_threads.remove(&key) {
                        self.expanded_threads.insert(key.clone());
                    }
                    // Insert just this thread's replies rather than rebuilding
                    // the whole list — on a long list a full rebuild tears
                    // down and recreates every row (up to RENDER_CAP), which
                    // stutters right as the reveal animation is trying to run.
                    self.expand_thread(&key);
                }
            }
            MessageListInput::FinishCollapseThread(key) => {
                self.collapsing_threads.remove(&key);
                if !self.expanded_threads.remove(&key) {
                    self.expanded_threads.insert(key.clone());
                }
                // Same reasoning as `expand_thread`: drop just these rows.
                self.collapse_thread_rows(&key);
            }
            MessageListInput::RowActivated(index) => {
                // With Gmail's keys, Enter on the row the cursor reached opens
                // it here, as o does, checked or not (#392); a row already
                // open goes to a window.
                if self.gmail_keys
                    && (self.checking || !self.shared.selection.is_selected(index as u32))
                {
                    sender.input(MessageListInput::OpenCursor);
                    return;
                }
                if let Some(m) = self.shown.get(index as usize).map(|m| Message::clone(m)) {
                    let (thread, _solo) = self.conversation_for(&m);
                    let _ = sender.output(MessageListOutput::Activated { message: m, thread });
                }
            }
            MessageListInput::MarkRead(slot) => {
                self.update_message(|m| thread_slot(m) == slot, |m| m.unread = false);
                self.refresh_thread_head(slot);
            }
            MessageListInput::SetRead { slot, read } => {
                self.update_message(|m| thread_slot(m) == slot, |m| m.unread = !read);
                self.refresh_thread_head(slot);
            }
            MessageListInput::SetStarred { slot, starred } => {
                self.update_message(|m| thread_slot(m) == slot, |m| m.starred = starred);
                self.refresh_thread_head(slot);
            }
            MessageListInput::SetAnswered {
                slot,
                forwarded,
                on,
            } => {
                self.update_message(
                    |m| thread_slot(m) == slot,
                    |m| {
                        if forwarded {
                            m.forwarded = on
                        } else {
                            m.answered = on
                        }
                    },
                );
                self.refresh_thread_head(slot);
            }
            MessageListInput::SetKeywords {
                slot,
                keywords,
                labels,
            } => {
                self.update_message(
                    |m| thread_slot(m) == slot,
                    |m| {
                        m.keywords = keywords.clone();
                        m.labels = labels.clone();
                    },
                );
            }
            MessageListInput::SetTags(tags) => {
                if *self.shared.tags.borrow() != tags {
                    *self.shared.tags.borrow_mut() = tags;
                    // Chips and the palette's tag button follow the
                    // definitions.
                    self.tags_gen += 1;
                    self.sync_look();
                }
            }
            MessageListInput::SetTagFor {
                message,
                keyword,
                add,
            } => {
                let _ = sender.output(MessageListOutput::SetTag {
                    message,
                    keyword,
                    add,
                });
            }
            MessageListInput::SetHasAttachment {
                account_id,
                folder_id,
                uid,
                has,
            } => {
                self.update_message(
                    |m| m.account_id == account_id && m.folder_id == folder_id && m.uid == uid,
                    |m| m.has_attachment = has,
                );
            }
            MessageListInput::Remove(id) => {
                // Was the removed message the one shown in the reader? If so we'll
                // advance to whatever row slides into its place.
                let was_viewed = self.selected_id.map(|(_, i)| i) == Some(id);
                if was_viewed {
                    self.selected_id = None;
                }
                self.selected_ids.retain(|(_, i)| *i != id);
                self.all.retain(|m| m.id != id);
                self.drop_threads();
                self.tail.retain(|r| r.msg.id != id);
                let removed_idx = self.shown.iter().position(|m| m.id == id);
                // Was the row about to go the one holding keyboard focus? If
                // so, and it isn't the viewed row handled below, focus is
                // put on its neighbour rather than left to GTK's fallback.
                let had_focus = removed_idx
                    .and_then(|idx| self.shared.row_at(idx))
                    .is_some_and(|row| row.has_focus_within());
                if let Some(idx) = removed_idx {
                    self.shown.remove(idx);
                    self.shared.model.splice(idx, 1, Vec::new());
                    // No rebuild follows — keep the header count honest.
                    self.total_matches = self.total_matches.saturating_sub(1);
                }
                if self.checking {
                    self.selection_count = self.selected_ids.len();
                    let _ = sender.output(MessageListOutput::SelectionKeys(
                        self.slots_at(&self.selected_positions()),
                    ));
                    let _ = sender.output(MessageListOutput::Checked(Some(self.selection_count)));
                }

                if was_viewed {
                    match removed_idx {
                        // Advance in the direction the user was triaging: the
                        // row now at the removed slot when moving down the
                        // list, the one above it when moving up (Apple Mail's
                        // behaviour). Selecting it fires SelectionChanged,
                        // which loads it in the reader.
                        Some(idx) if !self.shown.is_empty() => {
                            let next = self.advance_index(idx);
                            self.select_and_focus(next);
                        }
                        // Nothing left to show → clear the reader.
                        _ => {
                            let _ = sender.output(MessageListOutput::SelectionCleared);
                        }
                    }
                } else if had_focus {
                    if let Some(idx) = removed_idx {
                        if !self.shown.is_empty() {
                            self.focus_only(self.advance_index(idx));
                        }
                    }
                }
            }
            MessageListInput::LeaveConversation { gone, head } => {
                // The cache's count of the conversation still holds the
                // message until the server has moved it, so the badge is
                // brought down here rather than asked for again. A reply
                // filed in another folder is no row of this list: the
                // conversation is found by its row.
                let group = [gone, head]
                    .iter()
                    .find_map(|k| self.msg_thread.get(k))
                    .cloned()
                    .or_else(|| {
                        self.groups
                            .iter()
                            .find(|(_, ms)| ms.iter().any(|m| (m.account_id, m.id) == head))
                            .map(|(k, _)| k.clone())
                    });
                // A message of this folder goes by another id there, so by
                // folder and UID; one filed elsewhere by the id the reader
                // gave it.
                let slot = self
                    .all
                    .iter()
                    .find(|m| (m.account_id, m.id) == gone)
                    .map(|m| (m.account_id, m.folder_id, m.uid));
                if let Some(s) = group.and_then(|k| self.thread_summaries.get_mut(&k)) {
                    let before = s.members.len();
                    s.members.retain(|m| {
                        (m.account_id, m.id) != gone
                            && Some((m.account_id, m.folder_id, m.uid)) != slot
                    });
                    if s.members.len() < before {
                        s.count = s.count.saturating_sub(before - s.members.len());
                        // The row may have been quoting it as the newest.
                        s.latest = s.members.iter().max_by_key(|m| m.timestamp).map(|m| {
                            crate::models::ThreadLatest {
                                from_name: m.from_name.clone(),
                                from_addr: m.from_addr.clone(),
                                preview: m.preview.clone(),
                                timestamp: m.timestamp,
                                date: m.date.clone(),
                            }
                        });
                    }
                }
                self.all.retain(|m| (m.account_id, m.id) != gone);
                self.tail.retain(|r| (r.msg.account_id, r.msg.id) != gone);
                self.emitted_thread.retain(|k| *k != gone);
                self.drop_threads();
                // The row is selected under its new head before the rebuild
                // lights it, so the selection reads as the one already open
                // and the reader is not loaded again.
                for k in self.selected_ids.iter_mut().filter(|k| **k == gone) {
                    *k = head;
                }
                self.selected_ids.dedup();
                if self.selected_id == Some(gone) {
                    self.selected_id = Some(head);
                }
                self.queue_rebuild(true);
            }
            MessageListInput::RemoveMany(ids) => {
                if ids.is_empty() {
                    return;
                }
                let set: std::collections::HashSet<u32> = ids.into_iter().collect();
                let was_viewed = self
                    .selected_id
                    .map(|(_, i)| i)
                    .is_some_and(|i| set.contains(&i));
                if was_viewed {
                    self.selected_id = None;
                }
                self.selected_ids.retain(|(_, i)| !set.contains(i));
                self.all.retain(|m| !set.contains(&m.id));
                self.drop_threads();
                self.tail.retain(|r| !set.contains(&r.msg.id));
                // Where the first removed row sat, so we can re-select in its place.
                let first_removed = self.shown.iter().position(|m| set.contains(&m.id));
                let had_focus = self.shared.bound_rows().into_iter().any(|r| {
                    r.has_focus_within() && r.data().is_some_and(|d| set.contains(&d.msg.id))
                });
                // Out in runs, back to front, so the view hears of each run
                // once and the positions still to come stay valid.
                let shown_before = self.shown.len();
                let mut idx = self.shown.len();
                while idx > 0 {
                    idx -= 1;
                    if !set.contains(&self.shown[idx].id) {
                        continue;
                    }
                    let end = idx + 1;
                    while idx > 0 && set.contains(&self.shown[idx - 1].id) {
                        idx -= 1;
                    }
                    self.shown.drain(idx..end);
                    self.shared.model.splice(idx, end - idx, Vec::new());
                }
                self.selection_count = self.selected_ids.len();
                if self.checking {
                    let _ = sender.output(MessageListOutput::SelectionKeys(
                        self.slots_at(&self.selected_positions()),
                    ));
                    let _ = sender.output(MessageListOutput::Checked(Some(self.selection_count)));
                }
                // Keep the header count honest; no rebuild follows.
                self.total_matches = self
                    .total_matches
                    .saturating_sub(shown_before - self.shown.len());
                if self.checking {
                    // Checked rows deleted (#392): the keyboard goes to the
                    // conversation above them, else the one below, outlined
                    // but neither checked nor opened.
                    let cursor_gone = self.shared.cursor().is_some_and(|(_, i)| set.contains(&i));
                    if let Some(idx) = first_removed
                        .filter(|_| (cursor_gone || had_focus) && !self.shown.is_empty())
                    {
                        let pos = if idx > 0 {
                            self.conversation_step(Some(idx), -1)
                        } else {
                            0
                        };
                        self.focus_row(pos);
                        self.shared
                            .set_cursor(self.shown.get(pos).map(|m| (m.account_id, m.id)));
                    }
                } else if was_viewed {
                    match first_removed {
                        Some(idx) if !self.shown.is_empty() => {
                            let next = self.advance_index(idx);
                            self.select_and_focus(next);
                        }
                        _ => {
                            let _ = sender.output(MessageListOutput::SelectionCleared);
                        }
                    }
                } else if had_focus {
                    if let Some(idx) = first_removed {
                        if !self.shown.is_empty() {
                            self.focus_only(self.advance_index(idx));
                        }
                    }
                }
            }
            MessageListInput::RowAction { action, message } => {
                // Starring a conversation row stars the conversation (#star):
                // every member together, toggled as a unit — individual
                // members keep their own stars via their own rows/cards.
                if matches!(action, RowAction::ToggleStar) {
                    let members = self.thread_members(&message);
                    let is_head = members
                        .first()
                        .is_some_and(|h| (h.account_id, h.id) == (message.account_id, message.id));
                    if is_head && members.len() > 1 {
                        // ANY starred member reads as a starred conversation
                        // (matching the head's indicator), so the toggle can
                        // always clear — all-starred semantics deadlocked the
                        // moment one member was individually unstarred.
                        let any = members.iter().any(|m| m.starred);
                        let _ = sender.output(MessageListOutput::Bulk {
                            action: if any {
                                BulkAction::Unflag
                            } else {
                                BulkAction::Flag
                            },
                            messages: members,
                        });
                        return;
                    }
                }
                let conversation = self.row_conversation(&message);
                let _ = sender.output(MessageListOutput::Action {
                    action,
                    message,
                    conversation,
                });
            }
            MessageListInput::SetPaletteCollapse(secs) => {
                self.shared.palette_collapse_secs.set(secs)
            }
            MessageListInput::SetPaletteHover(on) => self.shared.palette_hover.set(on),
            MessageListInput::SetSwipeReversed(on) => self.shared.swipe_reversed.set(on),
            MessageListInput::SetSwipeEnabled(on) => {
                self.shared.swipe_enabled.set(on);
                // The rows on screen switch their trackers; the rest do so
                // when shown.
                self.shared.refresh_all();
            }
            MessageListInput::SetSwipeSensitivity(factor) => {
                self.shared.swipe_sensitivity.set(factor);
                self.shared.refresh_all();
            }
            MessageListInput::SetSelected(id) => {
                match id {
                    // Account-less id resolved against the shown list (the app
                    // only sends `None` today; `Some` kept for completeness).
                    Some(i) => {
                        if let Some(m) = self.shown.iter().find(|m| m.id == i) {
                            let key = (m.account_id, m.id);
                            self.selected_id = Some(key);
                            self.selected_ids = vec![key];
                            self.select_current();
                        }
                    }
                    None => {
                        self.selected_id = None;
                        self.selected_ids.clear();
                        self.selection_count = 0;
                        self.shared.selection.unselect_all();
                    }
                }
            }
            MessageListInput::DebugOpenPalette(idx) => {
                if let Some(row) = self.shared.row_at(idx) {
                    row.toggle_palette();
                }
            }
            MessageListInput::DebugSwipe { index, left } => {
                if let Some(row) = self.shared.row_at(index) {
                    row.debug_swipe(left);
                }
            }
            MessageListInput::DebugRowMenu => {
                if let Some(m) = self.shown.first().map(|m| Message::clone(m)) {
                    self.show_context_menu(&m, 120.0, 40.0, &sender);
                }
            }
            MessageListInput::SelectAndLoad(key) => {
                // Rows not built yet (the list landed in this same pass):
                // wait for the queued rebuild rather than find nothing.
                if self.rebuild_queued.is_some() {
                    self.pending_select = Some(key);
                    return;
                }
                self.late_select = None;
                let asked = key;
                self.stop_checking(&sender);
                // A row further down than the window reaches comes on screen.
                if let Some(at) = self
                    .tail
                    .iter()
                    .position(|r| (r.msg.account_id, r.msg.id) == key)
                {
                    self.grow_window(at + 1);
                }
                // A reply inside a conversation has no row of its own: its
                // thread head does, and opening that shows the whole thread,
                // the reply included.
                let key = match self.shown.iter().any(|m| (m.account_id, m.id) == key) {
                    true => key,
                    false => match self.thread_head_for(key) {
                        Some(head) => head,
                        None => key,
                    },
                };
                if let Some(m) = self
                    .shown
                    .iter()
                    .find(|m| (m.account_id, m.id) == key)
                    .map(|m| Message::clone(m))
                {
                    self.selected_id = Some(key);
                    self.selected_ids = vec![key];
                    // Exactly this row: anything already selected (e.g. the
                    // row the last deletion advanced to) would otherwise stay
                    // lit and turn this into a two-row selection the reader
                    // ignores.
                    self.select_current();
                    // These arrive from outside the list (a notification
                    // click, an undo) where the row can be far outside the
                    // viewport — bring it into view, in an idle so a rebuild
                    // queued just before this has run first.
                    if let Some(idx) = self.shown.iter().position(|m| (m.account_id, m.id) == key) {
                        let list = self.list_view.clone();
                        gtk::glib::idle_add_local_once(move || {
                            if idx < list.model().map_or(0, |m| m.n_items() as usize) {
                                list.scroll_to(idx as u32, gtk::ListScrollFlags::FOCUS, None);
                                list.grab_focus();
                            }
                            if let Some(win) =
                                list.root().and_then(|r| r.downcast::<gtk::Window>().ok())
                            {
                                win.set_focus_visible(false);
                            }
                        });
                    }
                    let (thread, solo) = self.conversation_for(&m);
                    self.emitted_thread = thread.iter().map(|t| (t.account_id, t.id)).collect();
                    let _ = sender.output(MessageListOutput::Selected {
                        message: m,
                        thread,
                        solo,
                    });
                } else {
                    // Not listed yet: mail that has only just arrived, whose
                    // notification was clicked before its folder's list was in.
                    // Dropping the request left the reader empty (#332).
                    self.late_select = Some((asked, std::time::Instant::now()));
                }
            }
            MessageListInput::RowMoveTo { message, x, y } => {
                let (messages, offer_whole) = self.move_to_messages(&message);
                let _ = sender.output(MessageListOutput::MoveTo {
                    messages,
                    offer_whole,
                    x,
                    y,
                });
            }
            MessageListInput::ContextMenu { x, y, key } => {
                let Some(pos) = self.shown.iter().position(|m| (m.account_id, m.id) == key) else {
                    return;
                };
                let selected = self.selected_positions();
                if selected.len() > 1 && selected.contains(&pos) {
                    // Right-clicked inside a multi-selection → bulk menu.
                    self.show_bulk_menu(x, y, &sender);
                } else {
                    // Single-row menu acting on the clicked message. Crucially,
                    // don't select it — selecting would load it in the reader,
                    // and the user may just intend to move/archive/delete it.
                    let msg = Message::clone(&self.shown[pos]);
                    self.show_context_menu(&msg, x, y, &sender);
                }
            }
        }
        // Whatever just happened, restate the header count if it moved — every
        // path that changes the visible rows funnels through here.
        let count = self.count_label();
        if count != self.last_count {
            self.last_count = count.clone();
            let _ = sender.output(MessageListOutput::CountChanged(count));
        }
    }
}

#[cfg(test)]
mod characterization_tests;

#[cfg(test)]
mod tests;
