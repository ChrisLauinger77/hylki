//! Right pane: the reading view for a single message.
//!
//! Bodies are rendered in a sandboxed WebKit view: JavaScript is disabled and a
//! Content-Security-Policy blocks remote content. When remote content is
//! withheld, its URLs are also stripped from the HTML so nothing is requested;
//! the originals are only used once the user (or a trusted sender) allows them.
//! Link clicks open in the browser.

use self::invite::invite_row_html;
use self::page_fade::{PageFade, PendingLoad};
use self::sender::{confirm_trust, fetch_sender_key};

use self::bridge::{parse_bridge, Bridge};
use self::chrome::{wear_class, ClassRules};
use self::dark::{adapt_colors_for_dark, pin_color_scheme};
use self::frame::{
    ground_rgba, message_frame, CHROME, GROUND, LIVE_ATTS, LIVE_FOLDBAR, LIVE_FOLDED, LIVE_GROUNDS,
    LIVE_INVITE, LIVE_TAGS, LIVE_TEMPLATES, LIVE_TRANSLATE, LIVE_TRANSLATE_ON, LIVE_UNSUB, PAGE,
};
use self::html::{attr_escape, body_html, escape_text};
use self::links::{
    decode_data_uri, default_image_name, is_launchable_uri, link_destination, CLICK_LINK_PROBE,
};
use self::menu::menu_lookup;
use self::print::sanitize_filename;
use self::remote::{has_remote_resources, strip_remote};
use self::size_script::SIZE_SCRIPT;
use self::snippets::{
    att_chips_html, att_row_html, card_action_button, fold_bar_date, inline_icon_svg,
    paints_own_background, recipient_count, recipients_html, tag_chips_html, template_row_html,
    unsub_row_html, FOLD_BAR_FACE,
};
use self::style::{auto_direction, inject_csp, inject_reader_style, reader_style_css};
use self::webview::new_webview;
use adw::prelude::*;
use relm4::prelude::*;
use webkit6::prelude::WebViewExt;

use crate::i18n::{i18n, i18n_f};
use crate::models::Message;

mod bridge;
mod cards;
mod chrome;
mod dark;
mod document;
mod folds;
mod frame;
mod html;
mod invite;
mod links;
mod look;
mod menu;
mod on_card_actions;
mod on_cards;
mod on_find;
mod on_prefs;
mod on_services;
mod on_show;
mod page_fade;
mod print;
mod remote;
mod render;
mod sender;
mod size_script;
mod snippets;
mod style;
mod translation;
mod types;
mod unsub_banner;
mod webview;

pub use frame::theme_grounds_for;
pub(crate) use menu::{hit_menu_entries, strip_navigation_items};
pub use menu::{MenuHit, TranslateEntry};
pub(crate) use page_fade::{on_next_page_fade, PAGE_FADE_MS};
pub use types::{
    CardAttachment, InviteAction, MessageViewInput, MessageViewOutput, SelectMode, TrState,
    UnsubState,
};
pub use webview::{new_preview_webview, shared_web_context};

pub struct MessageView {
    /// Render a lone message as an inset card, same as a conversation's
    /// messages (#57, preference; off keeps the full-bleed view).
    single_message_card: bool,
    /// The OpenPGP chip says its verdict in words (#300); off keeps the
    /// icons alone.
    pgp_labels: bool,
    /// What each message on screen has attached (#213): names and sizes,
    /// handed over by the app as the files arrive. Listed beneath the card.
    card_atts: std::collections::HashMap<(u32, u32), Vec<CardAttachment>>,
    /// Preference: cards list their own attachments (#213).
    card_atts_shown: bool,
    /// Preference: the attachment drawer is in use. A lone message's files
    /// are listed on its card only when the drawer is not there to show them.
    drawer_on: bool,
    /// Always show the recipients line under the sender (preference) — the
    /// chip then only appears for multi-recipient mail, as a collapse toggle.
    always_show_recipients: bool,
    /// The newest message in the thread — drives the header, avatar and actions.
    current: Option<Message>,
    /// The whole conversation, newest first. One entry = a single message (shown
    /// exactly as before); more = a scrollable conversation in the body view.
    thread: Vec<Message>,
    /// Which of the conversation's messages came from another folder, and what to
    /// call that folder.
    folder_labels: std::collections::HashMap<(u32, u32), String>,
    /// The tags (#71): what a message's keywords are drawn as.
    tags: Vec<crate::config::Tag>,
    /// The keywords and Gmail labels the GTK header's chips were last built
    /// for (a lone full-bleed message; cards carry their chips in the
    /// document).
    header_tags_rendered: std::cell::RefCell<Option<(Vec<String>, Vec<String>)>>,
    /// Remote content was detected and is currently withheld — this drives the
    /// "blocked" banner, and nothing else.
    blocked: bool,
    /// Messages the user deliberately marked unread while this conversation was
    /// on screen. They keep their mark but get no scroll sentinel, so simply
    /// having them in view can't undo the thing the user just asked for. Cleared
    /// whenever a conversation is opened afresh.
    no_autoread: std::collections::HashSet<(u32, u32)>,
    /// The reader's own fonts and colors laid over every message (#56);
    /// `NONE` shows each message as its sender formatted it.
    reader_style: crate::config::ReaderStyle,
    /// Messages the user asked to see with the sender's own formatting
    /// despite `reader_style` (the card's "sender's formatting" toggle).
    /// Kept for the session, so a thread reopened keeps the choice.
    sender_style: std::collections::HashSet<(u32, u32)>,
    /// Reader View: every message on screen stripped to its content and set
    /// in the reader's own uniform sheet (see `crate::reader`). The header's
    /// toggle; applies to the whole conversation at once.
    reader_mode: bool,
    /// Whether the header's Reader View switch is shown (Settings).
    reader_switch_shown: bool,
    /// Whether the header's Light / Dark Mode switch is shown (Settings).
    theme_switch_shown: bool,
    /// The scheme the user chose for a message with the header's Light /
    /// Dark Mode switch, dark (`true`) or light. The scheme itself is kept,
    /// not a flip of the theme, so a message turned light stays light when
    /// the theme changes after. Kept for the session, so a message reopened
    /// keeps the choice.
    theme_picks: std::collections::HashMap<(u32, u32), bool>,
    /// Message zoom in percent (Ctrl+ / Ctrl-): the bodies alone are
    /// scaled, in Reader View and out of it; the chrome keeps its size.
    zoom: u32,
    /// The zoom Settings starts every launch at. The header's chip only
    /// speaks up away from it: someone who chose 125% has nothing to be
    /// told at 125%.
    zoom_default: u32,
    /// What Reader View does when a conversation is opened afresh
    /// (Settings): keep the last choice, or start on or off.
    reader_default: crate::config::ReaderDefault,
    /// Read-marking policy (#100), stamped on the document for the
    /// viewport observer.
    read_mark: crate::config::ReadMark,
    /// Rows Gmail's x has checked in the list (#392), and whether they are
    /// conversations: the empty pane counts them.
    checked: (usize, bool),
    /// Whether the blocked-content banner is shown at all. It gates only the notice: `blocked`
    /// still governs what is withheld, so hiding it never loads anything.
    show_banner: bool,
    /// Whether a "Check this sender" verdict gets the red banner. A failed
    /// check gets it regardless: that one says the From: line is forged.
    show_spoof_banner: bool,
    /// Whether the user has actually permitted remote content for what is on
    /// screen (settings, "Load once", or "Always allow sender").
    ///
    /// Kept separate from `blocked` on purpose. `blocked` depends on a detector
    /// guessing whether a message references remote resources; this does not.
    /// Stripping and the CSP key off *this*, so a detector miss costs a banner
    /// rather than the protection itself.
    remote_allowed: bool,
    /// Owning account's display name (header chip).
    account_name: Option<String>,
    /// The header chip's per-account colors.
    chip_rules: ClassRules,
    /// Paints the reader's spinner and its inter-document cover in the *message*
    /// theme rather than the app's. Reading a light message in a dark app used
    /// to mean a dark spinner giving way to a white page.
    cover_rules: ClassRules,
    /// The widgets those rules are applied to, by class.
    account_chip: Option<gtk::Label>,
    cover: Option<gtk::Stack>,
    /// Fingerprint of what the WebView is currently showing. Re-selecting a
    /// message that renders to the same document skips the load entirely —
    /// every load blanks the view for an instant, however briefly.
    shown_fingerprint: Option<u64>,
    /// Conversation card actions hide until the card is hovered (expanded via
    /// their ⋯ toggle). A preference, applied by stamping the document (see
    /// `document_html`).
    card_actions_hover: bool,
    /// With the toggle off: show the actions automatically while the card is
    /// hovered (vs always).
    card_actions_auto: bool,
    /// Seconds an opened card palette lingers after the pointer leaves — the
    /// cards' own "Message card actions palette timeout", not the list's.
    palette_collapse_secs: u64,
    /// The card's ⋯ opens the card menu, the same one a right-click shows,
    /// instead of sliding its actions palette out. A preference, applied by
    /// stamping the document (see `document_html`).
    card_palette_menu: bool,
    /// The open conversation has already auto-scrolled to its first unread
    /// message: later renders of the same thread (bodies streaming in, a theme
    /// flip) carry a no-scroll stamp so the reader's place is kept.
    did_autoscroll: bool,
    /// That auto-scroll was spent on a lone message, before the rest of its
    /// conversation (a sent original, an archived part) was found.
    autoscroll_lone: bool,
    /// The wrapper document's last reported scroll anchor: the topmost card at
    /// the viewport top and the offset into it. A re-render replaces the whole
    /// document (scroll resets to 0) and can reflow everything above — an
    /// element anchor survives that where a raw pixel offset lands short.
    saved_anchor: Option<(u32, u32, u32)>,
    /// The list beside the pane shows Drafts: the empty state invites
    /// editing a draft rather than reading a message.
    drafts_view: bool,
    /// Every account's Templates folder, as (account, folder) (#360): a
    /// message from one wears a banner offering to use it or edit it.
    template_folders: std::collections::HashSet<(u32, u32)>,
    /// The saved anchor was set by the app, not by a scroll: the render
    /// lands it below the page's top gutter (where an unscrolled pane has
    /// its first card), not flush at the top the way a place the user
    /// scrolled to is restored.
    anchor_gutter: bool,
    /// The next document goes in without the page crossfade: a sent reply's
    /// card is arriving, and the app's stage covers the pane while it does.
    skip_fade: bool,
    /// What each message's frame measured last time it was shown, so reopening a
    /// conversation lays out right away instead of settling into place.
    frame_heights: std::collections::HashMap<(u32, u32), u32>,
    /// This conversation came ready-made, so there is nothing to cover.
    instant: bool,
    /// The messages the list has selected, outlined in the reader.
    selected_cards: Vec<(u32, u32)>,
    /// Bumped by FocusCard/BlurCard so a blur cancels a focus whose 380ms
    /// post-slide scroll hasn't fired yet (reply cancelled immediately).
    focus_gen: std::rc::Rc<std::cell::Cell<u64>>,
    /// True while the body is being fetched (show a spinner instead).
    loading: bool,
    /// False from when a render starts until the WebView reports it finished
    /// loading — a themed cover hides the WebView's white inter-document gap.
    webview_ready: bool,
    /// In-message find (#103).
    find_open: bool,
    /// The split reply is open above this reader: its header bar is gone and
    /// the subject block meets the composer's grab strip instead, where the
    /// list-aligned 4px reads as cramped.
    under_split: bool,
    find_matches: Option<(u32, u32)>,
    find_entry: Option<gtk::SearchEntry>,
    /// The page shown while a message loads, and whether it is holding the
    /// keyboard for the web view meanwhile (#392). A web view whose document
    /// is being replaced drops the keys it is given, so a shortcut pressed
    /// then never arrived; this page lets them through.
    loading_page: Option<gtk::Box>,
    focus_held: std::cell::Cell<bool>,
    /// When find last closed itself (empty entry blur) — the toolbar click
    /// that caused the blur must not instantly reopen it.
    find_closed_at: Option<std::time::Instant>,
    /// Which message the document currently on screen was rendered for, so an
    webview: webkit6::WebView,
    /// Bumped per render: each load gets a unique base URI so WebKit treats it
    /// as a fresh document and re-fetches resources (reusing `about:blank` does
    /// not). An https base also lets https images load without mixed-content.
    seq: std::cell::Cell<u64>,
    /// The document waiting to replace the one on screen (`load_after_paint`).
    pending_load: std::rc::Rc<std::cell::RefCell<PendingLoad>>,
    /// The outgoing page's still, faded out over the incoming one.
    page_fade: PageFade,
    /// Forced dark flag for message content, or `None` to follow the system UI.
    /// This themes email content only, not the app chrome.
    content_dark: Option<bool>,
    /// Whether the current message's From: address survived its provider's
    /// authentication checks. `None` until the verdict arrives with the body.
    sender_check: Option<crate::models::SenderCheck>,
    /// Per-member verdicts for the conversation on screen, behind each card's
    /// header seal (#88). Keyed (account_id, id); cleared on Show.
    member_checks: std::collections::HashMap<(u32, u32), crate::models::SenderCheck>,
    /// The mailing lists left through the cards' Unsubscribe button, by
    /// [`crate::models::Unsubscribe::key`] with the time of the request, so a
    /// message from one says so instead of offering the button afresh.
    unsubscribed: std::collections::HashMap<String, i64>,
    /// Cards whose unsubscribe request is under way or has just failed.
    unsub_state: std::collections::HashMap<(u32, u32), UnsubState>,
    /// Which messages of a conversation open folded (#326).
    fold_messages: crate::config::FoldMessages,
    /// How many lines of preview a folded card shows under the sender, as
    /// the message list does (#326).
    fold_preview_lines: u32,
    /// A document was loaded while the view was not on screen (#332).
    painted_hidden: bool,
    /// The open scroll went to a document loaded while the view was hidden,
    /// which never painted (#332): the next document loaded on screen is owed
    /// it, unless the reader has a place to keep by then.
    scroll_owed: bool,
    /// The conversation's messages filed in Sent, for the folded bar.
    sent: std::collections::HashSet<(u32, u32)>,
    /// Cards opened or folded by hand: these win over the default for as
    /// long as they stay in the conversation on screen.
    folds: std::collections::HashMap<(u32, u32), bool>,
    /// Translations (#327), per card: under way, shown, set aside for the
    /// original, or failed.
    translations: std::collections::HashMap<(u32, u32), TrState>,
    /// Which language each card's message is in, for the Translate offer,
    /// worked out once per body (by its length) rather than on every paint.
    detected: std::cell::RefCell<std::collections::HashMap<(u32, u32), (usize, Option<String>)>>,
    /// The addresses each account answers to (its own and its aliases), so
    /// an invitation's attendee list can be searched for the reader (#223).
    identities: std::collections::HashMap<u32, Vec<String>>,
    /// The invitations already answered, by [`crate::models::Invite::key`]:
    /// the PARTSTAT sent and when, so a meeting opened again says so.
    invite_answers: std::collections::HashMap<String, (String, i64)>,
    /// Full URL of the link under the pointer, shown in a corner overlay so a
    /// link's real destination is visible before it is clicked.
    link_preview: gtk::Label,
}

#[relm4::component(pub)]
impl Component for MessageView {
    type Init = ();
    type Input = MessageViewInput;
    type Output = MessageViewOutput;
    type CommandOutput = ();

    view! {
        gtk::Stack {
            set_transition_type: gtk::StackTransitionType::Crossfade,

            add_named[Some("empty")] = &adw::StatusPage {
                // Drafts open in the editor, so the empty pane says so there.
                #[watch]
                set_icon_name: Some(if model.checked.0 > 0 {
                    "object-select-symbolic"
                } else if model.drafts_view {
                    "document-edit-symbolic"
                } else {
                    "hylki-mail-read-symbolic"
                }),
                #[watch]
                set_title: &model.empty_title(),
                #[watch]
                set_description: Some(if model.checked.0 > 0 {
                    i18n("The actions above the list apply to all of them.")
                } else if model.drafts_view {
                    i18n("Choose a draft from the list to edit it here.")
                } else {
                    i18n("Choose a message from the list to read it here.")
                }.as_str()),
            },

            add_named[Some("message")] = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                add_css_class: "reader-pane",

                gtk::Revealer {
                    set_transition_type: gtk::RevealerTransitionType::SlideDown,
                    #[watch]
                    set_reveal_child: model.spoof_alert_shown(),

                    gtk::Box {
                        add_css_class: "spoof-alert",
                        set_spacing: 8,

                        gtk::Image { set_icon_name: Some("dialog-warning-symbolic") },
                        gtk::Label {
                            #[watch]
                            set_label: model
                                .sender_check
                                .as_ref()
                                .map(|c| c.summary.as_str())
                                .unwrap_or_default(),
                            set_hexpand: true,
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_xalign: 0.0,
                        },
                    },
                },

                gtk::Revealer {
                    set_transition_type: gtk::RevealerTransitionType::SlideDown,
                    #[watch]
                    set_reveal_child: model.blocked && model.show_banner,

                    gtk::Box {
                        add_css_class: "remote-alert",
                        set_spacing: 8,

                        gtk::Image { set_icon_name: Some("security-high-symbolic") },
                        gtk::Label {
                            set_label: &i18n("Remote content (images, trackers) is blocked to protect your privacy."),
                            set_hexpand: true,
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            // Ask for the full single-line width and wrap only
                            // when the pane really is too narrow — without this
                            // a wrapping label requests minimal width and folds
                            // to two lines even with room to spare.
                            set_natural_wrap_mode: gtk::NaturalWrapMode::None,
                            set_xalign: 0.0,
                        },
                        gtk::Button {
                            set_label: &i18n("Load"),
                            set_valign: gtk::Align::Center,
                            connect_clicked => MessageViewInput::LoadRemoteOnce,
                        },
                        gtk::Button {
                            set_label: &i18n("Always allow sender"),
                            set_valign: gtk::Align::Center,
                            #[watch]
                            set_tooltip_text: model.current.as_ref().map(|m| m.from_addr.as_str()),
                            connect_clicked => MessageViewInput::AllowSenderAlways,
                        },
                    },
                },

                // In-message find (#103): WebKit's FindController does the
                // matching and highlighting; this bar just drives it.
                gtk::Revealer {
                    set_transition_type: gtk::RevealerTransitionType::SlideDown,
                    #[watch]
                    set_reveal_child: model.find_open,

                    gtk::Box {
                        add_css_class: "reader-find",
                        set_spacing: 6,

                        #[name = "find_entry"]
                        gtk::SearchEntry {
                            set_hexpand: true,
                            set_placeholder_text: Some(i18n("Find in message").as_str()),
                            connect_search_changed[sender] => move |entry| {
                                sender.input_sender().emit(MessageViewInput::FindChanged(entry.text().to_string()));
                            },
                            connect_activate[sender] => move |_| {
                                sender.input_sender().emit(MessageViewInput::FindNext);
                            },
                            connect_stop_search[sender] => move |_| {
                                sender.input_sender().emit(MessageViewInput::CloseFind);
                            },
                            add_controller = gtk::EventControllerFocus {
                                connect_leave[sender] => move |ctl| {
                                    let Some(entry) = ctl.widget().and_downcast::<gtk::SearchEntry>() else { return };
                                    let sender = sender.clone();
                                    crate::ui::close_if_focus_left_row(&entry, move || sender.input_sender().emit(MessageViewInput::CloseFind));
                                },
                            },
                        },

                        gtk::Label {
                            add_css_class: "dim-label",
                            #[watch]
                            set_label: &match model.find_matches {
                                Some((_, 0)) => i18n("No matches"),
                                Some((cur, total)) => i18n_f("{cur} of {total}", &[("cur", &cur.to_string()), ("total", &total.to_string())]),
                                None => String::new(),
                            },
                        },

                        gtk::Button {
                            set_icon_name: "pan-up-symbolic",
                            set_tooltip_text: Some(i18n("Previous match").as_str()),
                            add_css_class: "flat",
                            connect_clicked => MessageViewInput::FindPrev,
                        },
                        gtk::Button {
                            set_icon_name: "pan-down-symbolic",
                            set_tooltip_text: Some(i18n("Next match").as_str()),
                            add_css_class: "flat",
                            connect_clicked => MessageViewInput::FindNext,
                        },
                        gtk::Button {
                            set_icon_name: "window-close-symbolic",
                            set_tooltip_text: Some(i18n("Close find").as_str()),
                            add_css_class: "flat",
                            connect_clicked => MessageViewInput::CloseFind,
                        },
                    },
                },

                gtk::Box {
                    add_css_class: "reader-header",
                    // The header's top padding is cut to the 4px that puts the
                    // account chip level with the message list's first row.
                    // That only reads as alignment when the header is against
                    // the toolbar; under a bar it reads as cramped, so give it
                    // its air back whenever one is showing.
                    #[watch]
                    set_class_active: (
                        "under-bar",
                        model.spoof_alert_shown()
                            || (model.blocked && model.show_banner)
                            || model.find_open,
                    ),
                    // Under the split reply there is no list row to line up
                    // with either, only the composer's grab strip: 14px more
                    // air than the aligned case.
                    #[watch]
                    set_class_active: ("under-split", model.under_split),
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 12,

                    gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 12,
                        #[watch]
                        set_visible: model.account_name.is_some() || model.current.is_some(),

                        gtk::Box {
                            set_halign: gtk::Align::Start,
                            set_hexpand: true,
                            set_valign: gtk::Align::Center,
                            #[watch]
                            set_visible: model.account_name.is_some(),
                            #[name = "account_chip"]
                            gtk::Label {
                                #[watch]
                                set_label: model.account_name.as_deref().unwrap_or_default(),
                                add_css_class: "account-chip",
                                add_css_class: "hylki-account-chip",
                            },
                        },

                        // Message zoom (Ctrl+ / Ctrl-): the percentage, shown
                        // only while it is away from the default Settings
                        // starts at, so a chosen size says nothing about
                        // itself. A click goes back to that default.
                        gtk::Label {
                            #[watch]
                            set_label: &format!("{}%", model.zoom),
                            #[watch]
                            set_visible: model.current.is_some() && model.zoom != model.zoom_default,
                            set_valign: gtk::Align::Center,
                            add_css_class: "caption",
                            add_css_class: "hylki-zoom-chip",
                            set_cursor_from_name: Some("pointer"),
                            set_tooltip_text: Some(i18n("Message zoom. Ctrl+ and Ctrl-, or Ctrl and the scroll wheel, change it; a click here, or Ctrl+0, puts it back to the default from Settings.").as_str()),
                            add_controller = gtk::GestureClick {
                                connect_released[sender] => move |_, _, _, _| {
                                    let _ = sender.output(MessageViewOutput::Zoom(0));
                                },
                            },
                        },

                        // Light / Dark Mode: the message on screen in the
                        // other scheme, for that message alone. Sits just
                        // before the Reader View switch, the highlights icon
                        // for a label.
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 6,
                            set_halign: gtk::Align::End,
                            set_valign: gtk::Align::Center,
                            add_css_class: "reader-toggle",
                            set_tooltip_text: Some(i18n("Show this message in light or dark mode. Other messages are not affected.").as_str()),
                            #[watch]
                            set_visible: model.current.is_some() && model.theme_switch_shown,

                            gtk::Image {
                                set_icon_name: Some("highlights-symbolic"),
                                add_css_class: "dim-label",
                            },
                            gtk::Switch {
                                set_valign: gtk::Align::Center,
                                update_property: &[gtk::accessible::Property::Label(&i18n("Light / Dark Mode"))],
                                #[watch]
                                set_active: model.message_flipped(),
                                // Setting it to what is already shown (a new
                                // message, a theme change) does nothing.
                                connect_active_notify[sender] => move |sw| {
                                    sender.input_sender().emit(MessageViewInput::SetMessageFlipped(sw.is_active()));
                                },
                            },
                        },

                        // Reader View: the message(s) as content alone. A
                        // switch behind a pair of reading glasses on the
                        // account chip's line, at its right end — part of
                        // the message header, not the action row.
                        gtk::Box {
                            set_orientation: gtk::Orientation::Horizontal,
                            set_spacing: 6,
                            set_halign: gtk::Align::End,
                            set_valign: gtk::Align::Center,
                            add_css_class: "reader-toggle",
                            set_tooltip_text: Some(i18n("Reader View shows a message as its text alone, in one plain format, without the sender's layout, colors and fonts.").as_str()),
                            #[watch]
                            set_visible: model.current.is_some() && model.reader_switch_shown,

                            gtk::Image {
                                set_icon_name: Some("glasses-symbolic"),
                                add_css_class: "dim-label",
                            },
                            gtk::Switch {
                                set_valign: gtk::Align::Center,
                                update_property: &[gtk::accessible::Property::Label(&i18n("Reader View"))],
                                #[watch]
                                set_active: model.reader_mode,
                                // The owner decides what a flip does, so it
                                // never renders on its own: the main window
                                // saves the choice and hands it back to every
                                // reader, a message's own window applies it
                                // to itself alone.
                                connect_active_notify[sender] => move |sw| {
                                    let _ = sender.output(MessageViewOutput::ReaderMode(sw.is_active()));
                                },
                            },
                        },
                    },

                    gtk::Label {
                        #[watch]
                        set_label: &model.current.as_ref().map(|m| model.shown_subject(m)).unwrap_or_default(),
                        set_halign: gtk::Align::Start,
                        set_wrap: true,
                        // Break mid-word for unbreakable tokens (e.g. an
                        // undecodable subject or a long URL) so an extreme
                        // subject can never force the pane — and with it the
                        // window controls — wider than the screen.
                        set_wrap_mode: gtk::pango::WrapMode::WordChar,
                        set_xalign: 0.0,
                        set_selectable: true,
                        add_css_class: "reader-subject",
                    },

                    // Tag chips (#71) for a lone full-bleed message, whose
                    // header is these widgets; cards draw theirs in the
                    // document. Filled by post_view.
                    #[local_ref]
                    header_tags -> gtk::Box {
                        set_spacing: 4,
                        set_halign: gtk::Align::Start,
                        #[watch]
                        set_visible: model.header_tags_shown(),
                    },
                },


                #[name = "body_stack"]
                gtk::Stack {
                    // The message's ground, painted on the stack itself so every
                    // page sits on it — the spinner box is centred and paints
                    // only its own few square inches.
                    add_css_class: "reader-cover",
                    // Both pages sit on that same ground, so a short dissolve
                    // between them reads as the message arriving rather than as
                    // the hard cut a stack does by default.
                    set_transition_type: gtk::StackTransitionType::Crossfade,
                    set_transition_duration: 120,
                    set_vexpand: true,
                    #[watch]
                    set_visible_child_name: model.body_page(),

                    #[name = "loading_page"]
                    add_named[Some("loading")] = &gtk::Box {
                        add_css_class: "reader-loading",
                        set_focusable: true,
                        set_orientation: gtk::Orientation::Vertical,
                        set_halign: gtk::Align::Center,
                        set_valign: gtk::Align::Center,
                        set_spacing: 14,

                        gtk::Spinner {
                            set_spinning: true,
                            set_width_request: 36,
                            set_height_request: 36,
                        },
                        gtk::Label {
                            // Dimming comes from the cover's own foreground, which
                            // is picked for the message theme — `dim-label` would
                            // fade it against the app's instead.
                            set_label: &i18n("Loading…"),
                        },
                    },
                },
            },

            #[watch]
            set_visible_child_name: if model.current.is_some() { "message" } else { "empty" },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let webview = new_webview();
        // Browser-style link preview: a small plaque in the bottom-left corner of
        // the body showing exactly where the link under the pointer goes. GTK
        // tooltips are unreliable over a WebView (WebKit handles motion events
        // itself, so GTK's hover timer often never starts), and a phishing check
        // that only sometimes appears is worse than none.
        let link_preview = gtk::Label::new(None);
        link_preview.add_css_class("link-preview");
        link_preview.set_halign(gtk::Align::Start);
        link_preview.set_valign(gtk::Align::End);
        link_preview.set_visible(false);
        link_preview.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        link_preview.set_max_width_chars(90);
        link_preview.set_can_target(false); // never intercept clicks meant for the page
        webview.connect_mouse_target_changed({
            let label = link_preview.clone();
            move |_view, hit, _modifiers| {
                let uri = hit.context_is_link().then(|| hit.link_uri()).flatten();
                match uri {
                    Some(uri) => {
                        label.set_text(&link_destination(&uri, hit.link_label().as_deref()));
                        label.set_visible(true);
                    }
                    None => label.set_visible(false),
                }
            }
        });
        let mut model = MessageView {
            always_show_recipients: false,
            single_message_card: false,
            pgp_labels: false,
            card_atts: std::collections::HashMap::new(),
            card_atts_shown: true,
            drawer_on: true,
            current: None,
            thread: Vec::new(),
            tags: Vec::new(),
            header_tags_rendered: std::cell::RefCell::new(None),
            folder_labels: std::collections::HashMap::new(),
            blocked: false,
            no_autoread: std::collections::HashSet::new(),
            reader_style: crate::config::ReaderStyle::NONE,
            sender_style: std::collections::HashSet::new(),
            reader_mode: false,
            reader_switch_shown: true,
            theme_switch_shown: false,
            theme_picks: std::collections::HashMap::new(),
            zoom: 100,
            zoom_default: 100,
            reader_default: crate::config::ReaderDefault::Remember,
            read_mark: crate::config::ReadMark::default(),
            checked: (0, false),
            show_banner: crate::config::load_privacy().show_remote_banner,
            show_spoof_banner: crate::config::load_privacy().show_spoof_banner,
            card_actions_hover: crate::config::load_privacy().card_actions_hover,
            card_actions_auto: crate::config::load_privacy().card_actions_auto,
            palette_collapse_secs: crate::config::load_privacy().card_palette_collapse_secs,
            card_palette_menu: crate::config::load_privacy().card_palette_menu,
            remote_allowed: false,
            account_name: None,
            chip_rules: ClassRules::new(),
            cover_rules: ClassRules::new(),
            account_chip: None,
            cover: None,
            shown_fingerprint: None,
            did_autoscroll: false,
            autoscroll_lone: false,
            saved_anchor: None,
            anchor_gutter: false,
            skip_fade: false,
            drafts_view: false,
            template_folders: std::collections::HashSet::new(),
            frame_heights: std::collections::HashMap::new(),
            instant: false,
            selected_cards: Vec::new(),
            focus_gen: std::rc::Rc::new(std::cell::Cell::new(0)),
            loading: false,
            webview_ready: false,
            find_open: false,
            under_split: false,
            find_matches: None,
            find_entry: None,
            loading_page: None,
            focus_held: std::cell::Cell::new(false),
            find_closed_at: None,
            webview,
            sender_check: None,
            member_checks: std::collections::HashMap::new(),
            unsubscribed: std::collections::HashMap::new(),
            unsub_state: std::collections::HashMap::new(),
            fold_messages: crate::config::FoldMessages::Never,
            fold_preview_lines: 1,
            painted_hidden: false,
            scroll_owed: false,
            sent: std::collections::HashSet::new(),
            folds: std::collections::HashMap::new(),
            translations: std::collections::HashMap::new(),
            detected: std::cell::RefCell::new(std::collections::HashMap::new()),
            identities: std::collections::HashMap::new(),
            invite_answers: std::collections::HashMap::new(),
            link_preview: link_preview.clone(),
            seq: std::cell::Cell::new(0),
            pending_load: Default::default(),
            page_fade: PageFade::new(),
            content_dark: None,
        };

        // The document being loaded is not the same as it being ready to look at:
        // each message body is a frame that loads afterwards and is then sized
        // from its placeholder height, so revealing here shows every card at the
        // wrong size and then jumping. The page says when its frames have
        // settled; this is only the backstop for a page that never does.
        let ready_sender = sender.clone();
        // Debug hook: HYLKI_SHOWCASE_CLICK_LINK=1 clicks the first web link
        // in the first message that has one, once, a few seconds after a
        // conversation is in, and logs what the click landed on. Exercises
        // the whole link path (frame → policy decision → launcher, #202)
        // without a pointer.
        let click_link = std::env::var("HYLKI_SHOWCASE_CLICK_LINK").is_ok();
        let clicked = std::rc::Rc::new(std::cell::Cell::new(false));
        let page_fade = model.page_fade.clone();
        model.webview.connect_load_changed(move |view, event| {
            if event == webkit6::LoadEvent::Finished {
                page_fade.release(view);
                let s = ready_sender.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(600),
                    move || {
                        s.input_sender().emit(MessageViewInput::Rendered);
                    },
                );
                if click_link && !clicked.get() {
                    let view = view.clone();
                    let clicked = clicked.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_secs(4),
                        move || {
                            view.evaluate_javascript(
                                CLICK_LINK_PROBE,
                                None,
                                None,
                                None::<&gtk::gio::Cancellable>,
                                move |r| match r {
                                    Ok(v) => {
                                        let text = v.to_str().to_string();
                                        if text.starts_with("clicked") {
                                            clicked.set(true);
                                        }
                                        tracing::info!("showcase: click-link probe: {text}");
                                    }
                                    Err(e) => {
                                        tracing::warn!("showcase: click-link probe failed: {e}")
                                    }
                                },
                            );
                        },
                    );
                }
            }
        });

        // A right-click anywhere in the reader (a card's header, its body
        // frame, which never reports events to the page, or the space
        // between cards) shows the message's own menu, the same one the list
        // row has. WebKit says what the click hit but not where the pointer
        // is, so a capture-phase gesture records the point first; the page
        // is then asked which card holds it, or is nearest to it. Links,
        // pictures and selected text used to get WebKit's own short menu
        // instead; their entries now lead the message's.
        {
            let point = std::rc::Rc::new(std::cell::Cell::new((0.0f64, 0.0f64)));
            let click = gtk::GestureClick::new();
            click.set_button(3);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            {
                let point = point.clone();
                click.connect_pressed(move |_, _, x, y| point.set((x, y)));
            }
            model.webview.add_controller(click);
            let menu_sender = sender.clone();
            model.webview.connect_context_menu(move |view, _menu, hit| {
                // Always the message's menu, wherever the click lands (a
                // link, a picture or selected text only adds its own entries
                // at the top), so the menu never depends on what the mail's
                // HTML happens to have under the pointer.
                let mut target = MenuHit {
                    link: hit
                        .context_is_link()
                        .then(|| hit.link_uri())
                        .flatten()
                        .map(|u| u.to_string())
                        .filter(|u| is_launchable_uri(u)),
                    image: hit
                        .context_is_image()
                        .then(|| hit.image_uri())
                        .flatten()
                        .map(|u| u.to_string()),
                    selection: None,
                    translate: None,
                    folds: None,
                };
                let selected = hit.context_is_selection();
                let (x, y) = point.get();
                let width = view.width().max(1);
                let s = menu_sender.clone();
                view.evaluate_javascript(
                    &format!("window.__hylkiMenuAt({x},{y},{width})"),
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    move |r| {
                        let json = r.map(|v| v.to_str().to_string()).unwrap_or_default();
                        let (key, text) = menu_lookup(&json);
                        if key.is_none() {
                            tracing::debug!("reader menu: no card at {x:.0},{y:.0} ({json:?})");
                        }
                        if selected {
                            target.selection = text;
                        }
                        s.input_sender().emit(MessageViewInput::CardMenuAt {
                            key,
                            x,
                            y,
                            hit: target,
                        });
                    },
                );
                true
            });
        }

        // Everything the page posts to the `hylki` handler (see bridge.rs).
        if let Some(ucm) = model.webview.user_content_manager() {
            let open_sender = sender.clone();
            ucm.connect_script_message_received(
                Some("hylki"),
                move |_ucm, value| match parse_bridge(&value.to_str()) {
                    Some(Bridge::Input(input)) => open_sender.input_sender().emit(input),
                    Some(Bridge::Clipboard(text)) => {
                        if let Some(display) = gtk::gdk::Display::default() {
                            display.clipboard().set_text(&text);
                        }
                    }
                    None => {}
                },
            );
        }

        // Re-render the body when the light/dark preference changes so unstyled
        // content tracks the theme live.
        {
            let s = sender.input_sender().clone();
            model.webview.connect_map(move |_| {
                let _ = s.send(MessageViewInput::Mapped);
            });
        }
        // The style manager outlives a message window's reader, so the
        // handler takes itself off once the reader is gone (#488).
        {
            let s = sender.input_sender().clone();
            let handler = std::rc::Rc::new(std::cell::Cell::new(None));
            let id = adw::StyleManager::default().connect_dark_notify({
                let handler = handler.clone();
                move |manager| {
                    if s.send(MessageViewInput::ThemeChanged).is_err() {
                        if let Some(id) = handler.take() {
                            manager.disconnect(id);
                        }
                    }
                }
            });
            handler.set(Some(id));
        }
        // The same for an appearance theme, which moves the colors without
        // touching the light/dark preference. A closed message window's
        // reader drops its watcher the next time the theme changes.
        {
            let s = sender.input_sender().clone();
            crate::theme::connect_changed(move || s.send(MessageViewInput::ThemeChanged).is_ok());
        }

        let header_tags = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let widgets = view_output!();
        model.find_entry = Some(widgets.find_entry.clone());
        model.loading_page = Some(widgets.loading_page.clone());
        model.account_chip = Some(widgets.account_chip.clone());
        model.cover = Some(widgets.body_stack.clone());
        model.apply_webview_bg(model.effective_dark());
        let body_overlay = gtk::Overlay::new();
        body_overlay.set_child(Some(&model.webview));
        body_overlay.add_overlay(&model.page_fade.cover);
        body_overlay.add_overlay(&link_preview);
        let zoom_sender = sender.clone();
        body_overlay.add_controller(super::ctrl_scroll_zoom(
            || true,
            move |step| {
                let _ = zoom_sender.output(MessageViewOutput::Zoom(step));
            },
        ));
        widgets.body_stack.add_named(&body_overlay, Some("body"));
        widgets.body_stack.set_visible_child_name("body");
        ComponentParts { model, widgets }
    }

    fn post_view() {
        self.sync_header_tags(&widgets.header_tags);
        self.hold_focus_while_loading();
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            MessageViewInput::Show {
                thread,
                allow_remote,
                account_name,
                account_color,
                loading,
                primary,
                folder_labels,
                instant,
                sent,
            } => self.on_show(
                thread,
                allow_remote,
                account_name,
                account_color,
                loading,
                primary,
                folder_labels,
                instant,
                sent,
            ),
            MessageViewInput::LoadRemoteOnce => self.on_load_remote_once(&sender),
            MessageViewInput::ToggleRemote { account_id, id } => {
                self.on_toggle_remote(account_id, id, &sender)
            }
            MessageViewInput::AllowSenderAlways => self.on_allow_sender_always(&sender),
            MessageViewInput::Print => self.on_print(),
            MessageViewInput::PrintPreview => self.on_print_preview(),
            MessageViewInput::FacesChanged => self.render(),

            MessageViewInput::ThemeChanged => self.on_theme_changed(),
            MessageViewInput::SetAlwaysShowRecipients(on) => {
                self.always_show_recipients = on;
            }
            MessageViewInput::SetFoldMessages(fold) => self.on_set_fold_messages(fold),
            MessageViewInput::SetFoldLook { preview_lines } => self.on_set_fold_look(preview_lines),
            MessageViewInput::Folded {
                account_id,
                id,
                folded,
            } => self.on_folded(account_id, id, folded),
            MessageViewInput::Mapped => self.on_mapped(),
            MessageViewInput::SetChecked {
                count,
                conversations,
            } => self.checked = (count, conversations),
            MessageViewInput::FoldAll(folded) => self.on_fold_all(folded),
            MessageViewInput::SetSingleMessageCard(on) => {
                self.single_message_card = on;
            }
            MessageViewInput::SetPgpLabels(on) => self.on_set_pgp_labels(on),
            MessageViewInput::SetCardAttachments(map) => self.on_set_card_attachments(map),
            MessageViewInput::SetCardAttachmentsShown(on) => self.on_set_card_attachments_shown(on),
            MessageViewInput::SetAttachmentDrawer(on) => self.on_set_attachment_drawer(on),
            MessageViewInput::ScrollToAttachments { account_id, id } => {
                self.on_scroll_to_attachments(account_id, id)
            }
            MessageViewInput::AttachmentAction {
                account_id,
                id,
                index,
                save,
            } => self.on_attachment_action(account_id, id, index, save, &sender),
            MessageViewInput::SetBannerShown(show) => {
                self.show_banner = show;
            }
            MessageViewInput::SetSpoofBannerShown(show) => {
                self.show_spoof_banner = show;
            }
            MessageViewInput::SetUnderSplit(under) => {
                self.under_split = under;
            }
            MessageViewInput::SetContentTheme(o) => self.on_set_content_theme(o),
            MessageViewInput::SetReaderStyle(style) => self.on_set_reader_style(style),
            MessageViewInput::SetReaderMode(on) => {
                self.set_reader_mode(on);
            }
            MessageViewInput::SetMessageFlipped(on) => self.on_set_message_flipped(on),
            MessageViewInput::SetZoomDefault(percent) => {
                self.zoom_default = percent;
            }
            MessageViewInput::SetZoom(percent) => self.on_set_zoom(percent),
            MessageViewInput::SetReaderSwitchShown(on) => {
                self.reader_switch_shown = on;
            }
            MessageViewInput::SetThemeSwitchShown(on) => {
                self.theme_switch_shown = on;
            }
            MessageViewInput::SetReaderDefault(policy) => self.on_set_reader_default(policy),
            MessageViewInput::ToggleSenderStyle { account_id, id } => {
                self.on_toggle_sender_style(account_id, id)
            }
            MessageViewInput::SetSenderCheck(check) => {
                self.sender_check = Some(*check);
            }

            MessageViewInput::SenderCheckFor {
                account_id,
                id,
                check,
            } => self.on_sender_check_for(account_id, id, check),
            MessageViewInput::SetUnsubscribed(lists) => self.on_set_unsubscribed(lists),
            MessageViewInput::Translate { account_id, id } => {
                self.on_translate(account_id, id, &sender)
            }
            MessageViewInput::Translated {
                account_id,
                id,
                result,
            } => self.on_translated(account_id, id, result),
            MessageViewInput::Unsubscribe { account_id, id } => {
                self.on_unsubscribe(account_id, id, &sender)
            }
            MessageViewInput::UnsubscribeState {
                account_id,
                id,
                state,
            } => self.on_unsubscribe_state(account_id, id, state),
            MessageViewInput::SetIdentities(identities) => self.on_set_identities(identities),
            MessageViewInput::SetInviteAnswers(answers) => self.on_set_invite_answers(answers),
            MessageViewInput::InviteAction {
                account_id,
                id,
                action,
            } => self.on_invite_action(account_id, id, action, &sender),
            MessageViewInput::SenderInfoAt {
                account_id,
                id,
                rect,
                page_width,
            } => self.on_sender_info_at(account_id, id, rect, page_width, &sender),
            MessageViewInput::PgpFetchKey { account_id, id } => {
                self.on_pgp_fetch_key(account_id, id, &sender)
            }
            MessageViewInput::PgpTrustKey { account_id, id } => {
                self.on_pgp_trust_key(account_id, id, &sender)
            }
            MessageViewInput::PgpTrustConfirmed {
                account_id,
                id,
                fingerprint,
            } => self.on_pgp_trust_confirmed(account_id, id, fingerprint, &sender),
            MessageViewInput::PgpDone {
                account_id,
                id,
                result,
            } => self.on_pgp_done(account_id, id, result, &sender),
            MessageViewInput::Rendered => self.on_rendered(),
            MessageViewInput::SuppressAutoRead { account_id, id } => {
                self.on_suppress_auto_read(account_id, id)
            }
            MessageViewInput::ClearCards => self.on_clear_cards(&sender),
            MessageViewInput::CardClicked {
                account_id,
                id,
                mode,
            } => self.on_card_clicked(account_id, id, mode, &sender),
            MessageViewInput::CopySelection => self.on_copy_selection(),
            MessageViewInput::SelectAllCards => self.on_select_all_cards(&sender),
            MessageViewInput::StepCard(delta) => self.on_step_card(delta, &sender),
            MessageViewInput::FocusBody => self.on_focus_body(),
            MessageViewInput::ComposeTo(addr) => {
                let _ = sender.output(MessageViewOutput::ComposeTo(addr));
            }
            MessageViewInput::AddContact(addr) => {
                let _ = sender.output(MessageViewOutput::AddContactAddr(addr));
            }
            MessageViewInput::SetSelectedCards(keys) => self.on_set_selected_cards(keys),
            MessageViewInput::FrameSized {
                account_id,
                id,
                height,
            } => self.on_frame_sized(account_id, id, height),
            MessageViewInput::MarkSeen { account_id, id } => {
                self.on_mark_seen(account_id, id, &sender)
            }
            MessageViewInput::CardAction {
                action,
                account_id,
                id,
            } => self.on_card_action(action, account_id, id, &sender),
            MessageViewInput::CardMenuAt { key, x, y, hit } => {
                self.on_card_menu_at(key, x, y, hit, &sender)
            }
            MessageViewInput::CardMenuFrom {
                account_id,
                id,
                x,
                y,
                page_width,
            } => self.on_card_menu_from(account_id, id, x, y, page_width, &sender),
            MessageViewInput::CardMoveAt {
                account_id,
                id,
                x,
                y,
                page_width,
            } => self.on_card_move_at(account_id, id, x, y, page_width, &sender),
            MessageViewInput::SetCardActionsMode {
                hover_toggle,
                hover_auto,
            } => self.on_set_card_actions_mode(hover_toggle, hover_auto),
            MessageViewInput::SetPaletteCollapse(secs) => self.on_set_palette_collapse(secs),
            MessageViewInput::SetCardPaletteMenu(on) => self.on_set_card_palette_menu(on),
            MessageViewInput::ClearDot { account_id, id } => self.on_clear_dot(account_id, id),
            MessageViewInput::SetReadMark(policy) => self.on_set_read_mark(policy),
            MessageViewInput::OpenFind => self.on_open_find(&sender),
            MessageViewInput::CloseFind => self.on_close_find(),
            MessageViewInput::FindChanged(text) => self.on_find_changed(text),
            MessageViewInput::FindNext => self.eval_find("hylkiFindStep(1)"),
            MessageViewInput::FindPrev => self.eval_find("hylkiFindStep(-1)"),
            MessageViewInput::FindCounted { current, total } => {
                self.find_matches = Some((current, total));
            }
            MessageViewInput::SetTags(tags) => self.on_set_tags(tags),
            MessageViewInput::SetCardKeywords {
                account_id,
                id,
                keywords,
                labels,
            } => self.on_set_card_keywords(account_id, id, keywords, labels),
            MessageViewInput::SetCardStar {
                account_id,
                id,
                starred,
            } => self.on_set_card_star(account_id, id, starred),
            MessageViewInput::FocusCard { account_id, id } => self.on_focus_card(account_id, id),
            MessageViewInput::BlurCard => self.on_blur_card(),
            MessageViewInput::ScrollAnchor {
                account_id,
                id,
                offset,
            } => self.on_scroll_anchor(account_id, id, offset),
            MessageViewInput::PinArrival { account_id, id } => self.on_pin_arrival(account_id, id),
            MessageViewInput::SetDraftsView(on) => {
                self.drafts_view = on;
            }
            MessageViewInput::SetTemplateFolders(folders) => self.on_set_template_folders(folders),
            MessageViewInput::TemplateAction {
                account_id,
                id,
                edit,
            } => self.on_template_action(account_id, id, edit, &sender),
            MessageViewInput::HoldPlace { account_id, id } => self.on_hold_place(account_id, id),
            MessageViewInput::CardContact { account_id, id } => {
                self.on_card_contact(account_id, id, &sender)
            }
            MessageViewInput::OpenHeader { account_id, id } => {
                self.on_open_header(account_id, id, &sender)
            }
        }
    }
}

#[cfg(test)]
mod tests;

impl Drop for MessageView {
    /// A pop-out's view goes with its window (the reader's own only ever
    /// drops at exit): see [`crate::memory_report::release_web_view`].
    fn drop(&mut self) {
        crate::memory_report::release_web_view(&self.webview);
    }
}
