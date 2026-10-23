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

impl MessageView {
    /// Reader View on or off, re-rendering what is on screen if it changed.
    fn set_reader_mode(&mut self, on: bool) {
        if self.reader_mode == on {
            return;
        }
        self.reader_mode = on;
        // A frame measures differently in each view; the heights remembered
        // for the other one would only make the cards lurch on the way to
        // their real size.
        self.frame_heights.clear();
        if self.current.is_some() && !self.loading {
            self.render();
        }
    }

    /// Run one of the in-message find engine's calls (#103), logging failures.
    fn eval_find(&self, js: &str) {
        self.webview
            .evaluate_javascript(js, None, None, None::<&gtk::gio::Cancellable>, |r| {
                if let Err(e) = r {
                    tracing::warn!("find js failed: {e}");
                }
            });
    }
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
            } => {
                let shown = primary.map(|p| *p).or_else(|| thread.first().cloned());
                // A new message: the previous message's verdict must not linger
                // on screen while this one's is still being fetched.
                let same_message = self
                    .current
                    .as_ref()
                    .zip(shown.as_ref())
                    .is_some_and(|(a, b)| a.id == b.id && a.account_id == b.account_id);
                if !same_message {
                    self.sender_check = None;
                    self.member_checks.clear();
                    self.unsub_state.clear();
                }
                // A card taken out of the conversation on screen, deleted or
                // moved, can leave another message heading it. What remains
                // is the same document, read on from where it was.
                let card_left = !same_message
                    && thread.len() < self.thread.len()
                    && thread.iter().all(|m| {
                        self.thread
                            .iter()
                            .any(|t| (t.account_id, t.id) == (m.account_id, m.id))
                    });
                // What was done to a card (translated, folded) holds while it
                // stays in the conversation on screen: stepping to another of
                // its messages must not undo it. The message stepped to opens.
                let stays = |k: &(u32, u32)| thread.iter().any(|m| (m.account_id, m.id) == *k);
                self.translations.retain(|k, _| stays(k));
                self.folds.retain(|k, _| stays(k));
                if !same_message && !card_left {
                    if let Some(key) = shown.as_ref().map(|s| (s.account_id, s.id)) {
                        if self.thread.iter().any(|m| (m.account_id, m.id) == key) {
                            self.folds.insert(key, false);
                        }
                    }
                }
                self.link_preview.set_visible(false);
                self.current = shown;
                // A fresh open: anything the user marked unread earlier can be
                // read by scrolling again — and the first render may auto-scroll
                // to the unread mark again.
                if !same_message && !card_left {
                    self.no_autoread.clear();
                    self.did_autoscroll = false;
                    self.saved_anchor = None;
                    self.scroll_owed = false;
                    // A per-message Reader View default (Settings) resets
                    // the view for each conversation opened; the switch can
                    // still flip this one.
                    if let Some(on) = self.reader_default.starts_on() {
                        self.set_reader_mode(on);
                    }
                }
                self.thread = thread;
                self.folder_labels = folder_labels;
                self.sent = sent;
                self.account_name = account_name;
                self.loading = loading;
                self.instant = instant;
                if let Some(color) = &account_color {
                    let class = format!(
                        "chip-{}",
                        color
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric())
                            .collect::<String>()
                    );
                    self.chip_rules.set(vec![(
                        format!(".hylki-account-chip.{class}"),
                        format!(
                            "background-color: {}; color: {color};",
                            crate::color::pale(color, 0.18)
                        ),
                    )]);
                    if let Some(chip) = &self.account_chip {
                        wear_class(chip, "chip-", &class);
                    }
                }
                self.remote_allowed = allow_remote;
                let has_remote = self.thread.iter().any(|m| has_remote_resources(&m.body));
                self.blocked = has_remote && !allow_remote;
                // Repaint the cover for what is arriving, even when the spinner
                // is about to be shown instead of a document: the whole point is
                // that the spinner already sits on the right color.
                self.apply_webview_bg(self.effective_dark());
                // While loading, the spinner page is shown; rendering the (empty)
                // body would just flash blank, so wait for the real body.
                if !self.loading {
                    self.render();
                    // Stepping to another message of the same conversation
                    // repaints nothing, but that message must open (#326).
                    self.patch_folds();
                }
            }
            MessageViewInput::LoadRemoteOnce => {
                // Tell the app as well, so the choice survives the next repaint
                // and the reader menu stops offering to do what is already done.
                if let Some(m) = &self.current {
                    let _ = sender.output(MessageViewOutput::SetRemote {
                        account_id: m.account_id,
                        id: m.id,
                        show: true,
                    });
                }
                self.remote_allowed = true;
                self.blocked = false;
                self.render();
            }
            MessageViewInput::ToggleRemote { account_id, id } => {
                let show = !self.remote_allowed;
                self.remote_allowed = show;
                self.blocked = !show && self.thread.iter().any(|m| has_remote_resources(&m.body));
                let _ = sender.output(MessageViewOutput::SetRemote {
                    account_id,
                    id,
                    show,
                });
                // The app records it and re-shows the message; that render is
                // free (the fingerprint already matches), so paint now.
                self.render();
            }
            MessageViewInput::AllowSenderAlways => {
                if let Some(m) = &self.current {
                    let _ = sender.output(MessageViewOutput::AllowSender(m.from_addr.clone()));
                }
                self.remote_allowed = true;
                self.blocked = false;
                self.render();
            }
            MessageViewInput::Print => {
                crate::ui::print_preview::print_html(
                    &self.print_document_html(),
                    &sanitize_filename(&self.job_name()),
                    self.webview.root().and_downcast::<gtk::Window>(),
                );
            }

            MessageViewInput::PrintPreview => {
                // Shown inside Hylki rather than exported to a PDF and handed to
                // whatever the desktop opens PDFs with: that route is a temporary
                // file, a URI, the document portal and an external viewer, each
                // able to fail without saying anything — and it did.
                // A pop-out reader's window is not an application window,
                // and asking for one there showed nothing (#359).
                let Some(parent) = self.webview.root().and_downcast::<gtk::Window>() else {
                    tracing::warn!("no window to attach the preview to");
                    return;
                };
                crate::ui::print_preview::open(
                    &parent,
                    &self.print_document_html(),
                    &sanitize_filename(&self.job_name()),
                );
            }

            MessageViewInput::FacesChanged => self.render(),

            MessageViewInput::ThemeChanged => {
                let dark = self.effective_dark();
                self.apply_webview_bg(dark);
                if self.current.is_some() && !self.loading {
                    self.render();
                }
            }
            MessageViewInput::SetAlwaysShowRecipients(on) => {
                self.always_show_recipients = on;
            }
            MessageViewInput::SetFoldMessages(fold) => {
                if self.fold_messages != fold {
                    self.fold_messages = fold;
                    self.folds.clear();
                    self.render();
                }
            }
            MessageViewInput::SetFoldLook { preview_lines } => {
                let preview_lines = preview_lines.min(3);
                if self.fold_preview_lines != preview_lines {
                    self.fold_preview_lines = preview_lines;
                    self.render();
                }
            }
            MessageViewInput::Folded {
                account_id,
                id,
                folded,
            } => {
                self.folds.insert((account_id, id), folded);
            }
            MessageViewInput::Mapped => {
                if std::mem::take(&mut self.painted_hidden)
                    && self.current.is_some()
                    && !self.loading
                {
                    self.shown_fingerprint = None;
                    self.render();
                }
            }
            MessageViewInput::SetChecked {
                count,
                conversations,
            } => self.checked = (count, conversations),
            MessageViewInput::FoldAll(folded) => {
                for m in &self.thread {
                    self.folds.insert((m.account_id, m.id), folded);
                }
                self.patch_folds();
            }
            MessageViewInput::SetSingleMessageCard(on) => {
                self.single_message_card = on;
            }
            MessageViewInput::SetPgpLabels(on) => {
                self.pgp_labels = on;
                let js = format!(
                    "document.body&&document.body.toggleAttribute('data-hylki-pgpicons',{})",
                    !on
                );
                self.webview.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |_| {},
                );
            }
            MessageViewInput::SetCardAttachments(map) => {
                self.card_atts = map;
                let keys: Vec<(u32, u32)> =
                    self.thread.iter().map(|m| (m.account_id, m.id)).collect();
                self.patch_card_atts(&keys);
            }
            MessageViewInput::SetCardAttachmentsShown(on) => {
                if self.card_atts_shown != on {
                    self.card_atts_shown = on;
                    if self.current.is_some() && !self.loading {
                        self.render();
                    }
                }
            }
            MessageViewInput::SetAttachmentDrawer(on) => {
                if self.drawer_on != on {
                    self.drawer_on = on;
                    if self.current.is_some() && !self.loading {
                        self.render();
                    }
                }
            }
            MessageViewInput::ScrollToAttachments { account_id, id } => {
                // The row when it has chips, else the card itself; centred,
                // with the row flashed so the eye lands on it.
                let js = format!(
                    "(function(){{\
                     var d=document.querySelector('.hylki-atts[data-key=\"{account_id}:{id}\"]');\
                     var s=document.querySelector('.hylki-msg[data-key=\"{account_id}:{id}\"]');\
                     var t=(d&&d.children.length)?d:s;if(!t)return;\
                     t.scrollIntoView({{behavior:'smooth',block:'center'}});\
                     if(d){{d.classList.remove('flash');void d.offsetWidth;d.classList.add('flash');\
                     setTimeout(function(){{d.classList.remove('flash');}},1300);}}}})()"
                );
                self.webview.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |_| {},
                );
            }
            MessageViewInput::AttachmentAction {
                account_id,
                id,
                index,
                save,
            } => {
                let _ = sender.output(MessageViewOutput::AttachmentAction {
                    account_id,
                    id,
                    index,
                    save,
                });
            }
            MessageViewInput::SetBannerShown(show) => {
                self.show_banner = show;
            }
            MessageViewInput::SetSpoofBannerShown(show) => {
                self.show_spoof_banner = show;
            }
            MessageViewInput::SetUnderSplit(under) => {
                self.under_split = under;
            }
            MessageViewInput::SetContentTheme(o) => {
                if self.content_dark != o {
                    self.content_dark = o;
                    let dark = self.effective_dark();
                    self.apply_webview_bg(dark);
                    if self.current.is_some() && !self.loading {
                        self.render();
                    }
                }
            }
            MessageViewInput::SetReaderStyle(style) => {
                if self.reader_style != style {
                    self.reader_style = style;
                    if self.current.is_some() && !self.loading {
                        self.render();
                    }
                }
            }
            MessageViewInput::SetReaderMode(on) => {
                self.set_reader_mode(on);
            }
            MessageViewInput::SetMessageFlipped(on) => {
                if self.message_flipped() == on {
                    return;
                }
                let Some(key) = self.current.as_ref().map(|m| (m.account_id, m.id)) else {
                    return;
                };
                // Off forgets the choice: the message follows the theme again.
                if on {
                    self.theme_picks.insert(key, !self.theme_dark());
                } else {
                    self.theme_picks.remove(&key);
                }
                let dark = self.effective_dark();
                self.apply_webview_bg(dark);
                if !self.loading {
                    self.render();
                }
            }
            MessageViewInput::SetZoomDefault(percent) => {
                self.zoom_default = percent;
            }
            MessageViewInput::SetZoom(percent) => {
                if self.zoom != percent {
                    self.zoom = percent;
                    // The frames remeasure at the new size; the heights
                    // remembered for the old one would only make them lurch.
                    self.frame_heights.clear();
                    if self.current.is_some() && !self.loading {
                        let js = format!(
                            "if(window.vfZoom)window.vfZoom({});",
                            percent as f64 / 100.0
                        );
                        self.webview.evaluate_javascript(
                            &js,
                            None,
                            None,
                            None::<&gtk::gio::Cancellable>,
                            |_| {},
                        );
                    }
                }
            }
            MessageViewInput::SetReaderSwitchShown(on) => {
                self.reader_switch_shown = on;
            }
            MessageViewInput::SetThemeSwitchShown(on) => {
                self.theme_switch_shown = on;
            }
            MessageViewInput::SetReaderDefault(policy) => {
                self.reader_default = policy;
                if let Some(on) = policy.starts_on() {
                    self.set_reader_mode(on);
                }
            }
            MessageViewInput::ToggleSenderStyle { account_id, id } => {
                let key = (account_id, id);
                if !self.sender_style.remove(&key) {
                    self.sender_style.insert(key);
                }
                if self.current.is_some() && !self.loading {
                    self.render();
                }
            }
            MessageViewInput::SetSenderCheck(check) => {
                self.sender_check = Some(*check);
            }

            MessageViewInput::SenderCheckFor {
                account_id,
                id,
                check,
            } => {
                self.member_checks.insert((account_id, id), *check);
                if self.webview_ready {
                    self.patch_verify_badge(account_id, id);
                    self.patch_unsub(account_id, id);
                    self.patch_invite(account_id, id);
                }
            }
            MessageViewInput::SetUnsubscribed(lists) => {
                self.unsubscribed = lists;
                let keys: Vec<(u32, u32)> =
                    self.thread.iter().map(|m| (m.account_id, m.id)).collect();
                for (account_id, id) in keys {
                    self.patch_unsub(account_id, id);
                }
            }
            MessageViewInput::Translate { account_id, id } => {
                let key = (account_id, id);
                match self.translations.get(&key).cloned() {
                    Some(TrState::Working) => {}
                    Some(TrState::Shown(t)) => {
                        self.translations.insert(key, TrState::Original(t));
                        self.render();
                    }
                    Some(TrState::Original(t)) => {
                        self.translations.insert(key, TrState::Shown(t));
                        self.render();
                    }
                    None | Some(TrState::Failed(_)) => {
                        let Some(m) = self
                            .thread
                            .iter()
                            .find(|m| (m.account_id, m.id) == key)
                            .cloned()
                        else {
                            return;
                        };
                        let settings = crate::translate::load();
                        let refused = if self.encrypted(key) {
                            Some(i18n("Encrypted messages are not sent for translation."))
                        } else if !settings.enabled() {
                            Some(i18n(
                                "Choose a translation service in Settings → Translation first.",
                            ))
                        } else if m.body.trim().is_empty() {
                            Some(i18n("The message has not loaded yet."))
                        } else {
                            None
                        };
                        if let Some(why) = refused {
                            self.translations.insert(key, TrState::Failed(why));
                            self.patch_translate(account_id, id);
                            return;
                        }
                        let cache = crate::translate::cache_key(
                            account_id,
                            if m.message_id.is_empty() {
                                &m.body
                            } else {
                                &m.message_id
                            },
                            &settings,
                        );
                        // Already translated this session: no second request.
                        if let Some(t) = crate::translate::cached(&cache) {
                            self.translations.insert(key, TrState::Shown(t));
                            self.render();
                            return;
                        }
                        self.translations.insert(key, TrState::Working);
                        self.patch_translate(account_id, id);
                        let input = sender.input_sender().clone();
                        std::thread::spawn(move || {
                            let api_key =
                                crate::translate::load_key(settings.service).unwrap_or_default();
                            let result = crate::translate::translate(
                                &settings, &api_key, &m.body, &m.subject, &cache,
                            );
                            input.emit(MessageViewInput::Translated {
                                account_id,
                                id,
                                result,
                            });
                        });
                    }
                }
            }
            MessageViewInput::Translated {
                account_id,
                id,
                result,
            } => {
                let key = (account_id, id);
                // The reader moved on to another message meanwhile.
                if self.translations.get(&key) != Some(&TrState::Working) {
                    return;
                }
                match result {
                    Ok(t) => {
                        self.translations.insert(key, TrState::Shown(t));
                        self.render();
                    }
                    Err(why) => {
                        tracing::warn!("translation failed: {why}");
                        self.translations.insert(key, TrState::Failed(why));
                        self.patch_translate(account_id, id);
                    }
                }
            }
            MessageViewInput::Unsubscribe { account_id, id } => {
                let info = self
                    .member_checks
                    .get(&(account_id, id))
                    .and_then(|c| c.unsubscribe.clone());
                let message = self
                    .thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                    .cloned();
                if let (Some(info), Some(message)) = (info, message) {
                    let _ = sender.output(MessageViewOutput::Unsubscribe {
                        message: Box::new(message),
                        info: Box::new(info),
                    });
                }
            }
            MessageViewInput::UnsubscribeState {
                account_id,
                id,
                state,
            } => {
                match state {
                    Some(s) => {
                        self.unsub_state.insert((account_id, id), s);
                    }
                    None => {
                        self.unsub_state.remove(&(account_id, id));
                    }
                }
                self.patch_unsub(account_id, id);
            }
            MessageViewInput::SetIdentities(identities) => {
                if self.identities == identities {
                    return;
                }
                self.identities = identities;
                let keys: Vec<(u32, u32)> =
                    self.thread.iter().map(|m| (m.account_id, m.id)).collect();
                for (account_id, id) in keys {
                    self.patch_invite(account_id, id);
                }
            }
            MessageViewInput::SetInviteAnswers(answers) => {
                self.invite_answers = answers;
                let keys: Vec<(u32, u32)> =
                    self.thread.iter().map(|m| (m.account_id, m.id)).collect();
                for (account_id, id) in keys {
                    self.patch_invite(account_id, id);
                }
            }
            MessageViewInput::InviteAction {
                account_id,
                id,
                action,
            } => {
                let invite = self
                    .member_checks
                    .get(&(account_id, id))
                    .and_then(|c| c.invite.clone());
                let message = self
                    .thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                    .cloned();
                if let (Some(invite), Some(message)) = (invite, message) {
                    let _ = sender.output(MessageViewOutput::InviteAction {
                        message: Box::new(message),
                        invite,
                        action,
                    });
                }
            }

            MessageViewInput::SenderInfoAt {
                account_id,
                id,
                rect,
                page_width,
            } => {
                self.show_sender_popover(account_id, id, rect, page_width, &sender);
            }
            MessageViewInput::PgpFetchKey { account_id, id } => {
                let Some(pgp) = self
                    .member_checks
                    .get(&(account_id, id))
                    .and_then(|c| c.pgp.clone())
                else {
                    return;
                };
                let input = sender.input_sender().clone();
                sender.oneshot_command(async move {
                    let result = tokio::task::spawn_blocking(move || fetch_sender_key(&pgp))
                        .await
                        .unwrap_or_else(|_| Err("task failed".into()));
                    let _ = input.send(MessageViewInput::PgpDone {
                        account_id,
                        id,
                        result,
                    });
                });
            }
            MessageViewInput::PgpTrustKey { account_id, id } => {
                let Some(pgp) = self
                    .member_checks
                    .get(&(account_id, id))
                    .and_then(|c| c.pgp.clone())
                else {
                    return;
                };
                let crate::models::PgpSignature::Good { signer, key_id, .. } = &pgp.signature
                else {
                    return;
                };
                // gpg on a worker thread: a stale keyring lock makes it
                // wait about ten seconds (#316).
                let signer = signer.clone();
                let key_id = key_id.clone();
                let parent = self.webview.root().and_downcast::<gtk::Window>();
                let s = sender.clone();
                gtk::glib::spawn_future_local(async move {
                    let key = gtk::gio::spawn_blocking(move || {
                        crate::pgp::key_by_fingerprint(&crate::pgp::Gpg::system(), &key_id)
                    })
                    .await
                    .ok()
                    .flatten();
                    let Some(key) = key else {
                        let _ = s.output(MessageViewOutput::Notice(i18n(
                            "That key is no longer in your keyring.",
                        )));
                        return;
                    };
                    confirm_trust(parent.as_ref(), &signer, &key, s, account_id, id);
                });
            }
            MessageViewInput::PgpTrustConfirmed {
                account_id,
                id,
                fingerprint,
            } => {
                let input = sender.input_sender().clone();
                sender.oneshot_command(async move {
                    let result = tokio::task::spawn_blocking(move || crate::pgp::trust_key(&crate::pgp::Gpg::system(), &fingerprint, None))
                        .await
                        .unwrap_or_else(|_| Err("task failed".into()))
                        .map(|()| i18n("Key trusted."))
                        .map_err(|e| {
                            if e.contains("secret key") || e.contains("default") {
                                i18n("Generate a key of your own first (Settings, OpenPGP): trusting a key means signing it with yours.")
                            } else {
                                e
                            }
                        });
                    let _ = input.send(MessageViewInput::PgpDone { account_id, id, result });
                });
            }
            MessageViewInput::PgpDone {
                account_id,
                id,
                result,
            } => match result {
                Ok(text) => {
                    let _ = sender.output(MessageViewOutput::Notice(text));
                    if let Some(m) = self
                        .thread
                        .iter()
                        .find(|m| m.account_id == account_id && m.id == id)
                    {
                        let _ = sender.output(MessageViewOutput::ReloadBody(Box::new(m.clone())));
                    }
                }
                Err(e) => {
                    let _ = sender.output(MessageViewOutput::Notice(e));
                }
            },

            MessageViewInput::Rendered => {
                self.webview_ready = true;
                // Verdicts that arrived while the document was still loading.
                for (aid, id) in self.member_checks.keys().copied().collect::<Vec<_>>() {
                    self.patch_verify_badge(aid, id);
                    self.patch_unsub(aid, id);
                    self.patch_invite(aid, id);
                }
                // Likewise attachments (#213): the document was built from
                // what was known when the render was queued.
                let keys: Vec<(u32, u32)> =
                    self.thread.iter().map(|m| (m.account_id, m.id)).collect();
                self.patch_card_atts(&keys);
            }
            MessageViewInput::SuppressAutoRead { account_id, id } => {
                self.no_autoread.insert((account_id, id));
                for m in self.thread.iter_mut() {
                    if m.account_id == account_id && m.id == id {
                        m.unread = true;
                    }
                }
                // Put the card's dot back in place — reloading the document
                // for one mark made every card visibly resettle (the same
                // blip ClearDot avoids in the other direction).
                let js = format!(
                    "(function(){{\
                     var s=document.querySelector('.hylki-msg[data-key=\"{account_id}:{id}\"]');\
                     if(s)s.classList.add('unread');\
                     var h=document.querySelector('.hylki-msg-hdr[data-key=\"{account_id}:{id}\"]');\
                     if(!h||h.querySelector('.hylki-dot'))return;\
                     var d=document.createElement('span');d.className='hylki-dot';\
                     d.setAttribute('data-key','{account_id}:{id}');\
                     var f=h.querySelector('.hylki-from');\
                     if(f)f.parentNode.insertBefore(d,f);else h.appendChild(d);}})()"
                );
                self.webview.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |r| {
                        if let Err(e) = r {
                            tracing::warn!("unread-dot patch failed: {e}");
                        }
                    },
                );
            }
            MessageViewInput::ClearCards => {
                if !self.selected_cards.is_empty() {
                    self.selected_cards.clear();
                    self.apply_card_selection();
                    let _ = sender.output(MessageViewOutput::SelectCards(Vec::new()));
                }
            }
            MessageViewInput::CardClicked {
                account_id,
                id,
                mode,
            } => {
                // Only meaningful in a conversation: a lone message is already
                // the selection.
                if self.thread.len() <= 1 {
                    return;
                }
                let key = (account_id, id);
                let order: Vec<(u32, u32)> =
                    self.thread.iter().map(|m| (m.account_id, m.id)).collect();
                let mut keys = match mode {
                    SelectMode::Plain => vec![key],
                    SelectMode::Toggle => {
                        let mut k = self.selected_cards.clone();
                        if let Some(pos) = k.iter().position(|x| *x == key) {
                            k.remove(pos);
                        } else {
                            k.push(key);
                        }
                        k
                    }
                    SelectMode::Range => {
                        // From the first thing already selected to this one, in
                        // the order the conversation is shown.
                        let anchor = self
                            .selected_cards
                            .first()
                            .and_then(|a| order.iter().position(|x| x == a))
                            .unwrap_or_else(|| order.iter().position(|x| *x == key).unwrap_or(0));
                        let here = order.iter().position(|x| *x == key).unwrap_or(anchor);
                        let (lo, hi) = (anchor.min(here), anchor.max(here));
                        order[lo..=hi].to_vec()
                    }
                };
                keys.retain(|k| order.contains(k));
                self.selected_cards = keys.clone();
                self.apply_card_selection();
                let _ = sender.output(MessageViewOutput::SelectCards(keys));
            }
            MessageViewInput::CopySelection => {
                // Copying from a body frame focuses the frame and then the
                // page, and a page focusing itself takes GTK focus: Ctrl+C
                // pressed in the message list left the keyboard in the
                // reader (#274). The script has run by the time its result
                // comes back, so hand focus back to where it was then.
                let before = self.webview.root().and_then(|r| r.focus());
                let webview = self.webview.clone();
                self.webview.evaluate_javascript(
                    "window.__hylkiCopySel && window.__hylkiCopySel()",
                    None,
                    None,
                    gtk::gio::Cancellable::NONE,
                    move |_| {
                        let Some(before) = before else { return };
                        let now = webview.root().and_then(|r| r.focus());
                        let taken = now.is_some_and(|f| {
                            f == *webview.upcast_ref::<gtk::Widget>() || f.is_ancestor(&webview)
                        });
                        if taken && before != *webview.upcast_ref::<gtk::Widget>() {
                            before.grab_focus();
                        }
                    },
                );
            }
            MessageViewInput::SelectAllCards => {
                if self.thread.len() <= 1 {
                    return;
                }
                let keys: Vec<(u32, u32)> =
                    self.thread.iter().map(|m| (m.account_id, m.id)).collect();
                if self.selected_cards != keys {
                    self.selected_cards = keys.clone();
                    self.apply_card_selection();
                    let _ = sender.output(MessageViewOutput::SelectCards(keys));
                }
            }
            MessageViewInput::StepCard(delta) => {
                // A lone message has nothing to step through: the app moves
                // along the list instead.
                if self.thread.len() <= 1 {
                    return;
                }
                let order: Vec<(u32, u32)> =
                    self.thread.iter().map(|m| (m.account_id, m.id)).collect();
                let from = match self.selected_cards.as_slice() {
                    [key] => order.iter().position(|k| k == key),
                    _ => None,
                };
                let last = order.len() as i64 - 1;
                let next = match from {
                    Some(i) => (i as i64 + i64::from(delta)).clamp(0, last) as usize,
                    // No card picked yet: the first press lands where reading
                    // starts, the first message unread or open, not one past
                    // the conversation's first (#392).
                    None => {
                        let folded = self.folded_cards();
                        self.thread
                            .iter()
                            .position(|m| m.unread || !folded.contains(&(m.account_id, m.id)))
                            .unwrap_or(if delta > 0 { 0 } else { last as usize })
                    }
                };
                let key = order[next];
                if self.folded_cards().contains(&key) {
                    self.folds.insert(key, false);
                    self.patch_folds();
                }
                self.selected_cards = vec![key];
                self.apply_card_selection();
                let _ = sender.output(MessageViewOutput::SelectCards(vec![key]));
                let js = format!(
                    "(function(){{var s=document.querySelector('.hylki-msg[data-key=\"{}:{}\"]');\
                     if(s)s.scrollIntoView({{behavior:'smooth',block:'start'}});}})()",
                    key.0, key.1
                );
                self.webview.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |_| {},
                );
            }
            MessageViewInput::FocusBody => {
                if self.current.is_some() {
                    match &self.loading_page {
                        Some(page) if self.body_page() == "loading" => {
                            page.grab_focus();
                            self.focus_held.set(true);
                        }
                        _ => {
                            self.webview.grab_focus();
                        }
                    }
                }
            }
            MessageViewInput::ComposeTo(addr) => {
                let _ = sender.output(MessageViewOutput::ComposeTo(addr));
            }
            MessageViewInput::AddContact(addr) => {
                let _ = sender.output(MessageViewOutput::AddContactAddr(addr));
            }
            MessageViewInput::SetSelectedCards(keys) => {
                // The list mirrors every selection change here — including the
                // plain single "this row is open" state that comes with merely
                // opening a message or thread, which must not outline anything.
                // A lone mirrored key is therefore dropped: the accent border
                // appears only for a deliberate selection — a card header
                // clicked in the reader (CardClicked, which never routes
                // through here), or a multi-selection made in the list.
                let keys = if keys.len() == 1 { Vec::new() } else { keys };
                if self.selected_cards != keys {
                    self.selected_cards = keys;
                    self.apply_card_selection();
                }
            }
            MessageViewInput::FrameSized {
                account_id,
                id,
                height,
            } => {
                // A few hundred numbers at most, and only for messages actually
                // opened; the store is trimmed rather than allowed to creep.
                if self.frame_heights.len() > 512 {
                    self.frame_heights.clear();
                }
                self.frame_heights.insert((account_id, id), height);
            }
            MessageViewInput::MarkSeen { account_id, id } => {
                // A deliberate unread mark stands until the conversation is
                // reopened — the viewport observer must not undo it.
                if self.no_autoread.contains(&(account_id, id)) {
                    return;
                }
                // A card read while open stays open: folding the read ones
                // is for when the conversation is drawn, not under the
                // reader's eyes the next time the folds are patched.
                let key = (account_id, id);
                if !self.folded_cards().contains(&key) {
                    self.folds.entry(key).or_insert(false);
                }
                // Keep the local copy in step so a later re-render doesn't put
                // the mark back on a message already read.
                let mut found = false;
                for m in self.thread.iter_mut() {
                    if m.account_id == account_id && m.id == id && m.unread {
                        m.unread = false;
                        found = true;
                    }
                }
                if found {
                    let _ = sender.output(MessageViewOutput::MarkSeen { account_id, id });
                }
            }
            MessageViewInput::CardAction {
                action,
                account_id,
                id,
            } => {
                if let Some(m) = self
                    .thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                {
                    let _ = sender.output(MessageViewOutput::CardAction {
                        action,
                        message: Box::new(m.clone()),
                    });
                }
            }
            MessageViewInput::CardMenuAt { key, x, y, hit } => {
                // Unnamed (the page did not answer): the message on screen
                // when there is one, else the conversation's first, so a
                // right-click never ends without a menu.
                let m = key
                    .and_then(|(a, i)| self.thread.iter().find(|m| m.account_id == a && m.id == i))
                    .or_else(|| self.thread.first());
                let Some(m) = m.cloned() else { return };
                let mut hit = hit;
                let key = (m.account_id, m.id);
                if crate::translate::load().enabled() && !self.encrypted(key) {
                    let label = match self.translations.get(&key) {
                        Some(TrState::Shown(_)) => i18n("Show Original"),
                        Some(TrState::Original(_)) => i18n("Show Translation"),
                        _ => i18n("Translate"),
                    };
                    hit.translate = Some(TranslateEntry {
                        label,
                        reader: sender.input_sender().clone(),
                    });
                }
                if self.thread.len() > 1 {
                    let folded = self.folded_cards();
                    hit.folds = Some((
                        !folded.is_empty(),
                        folded.len() < self.thread.len(),
                        sender.input_sender().clone(),
                    ));
                }
                let point = self.webview.root().and_then(|root| {
                    let root: gtk::Widget = root.upcast();
                    self.webview
                        .compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))
                });
                let (wx, wy) = point.map_or((x, y), |p| (p.x() as f64, p.y() as f64));
                let _ = sender.output(MessageViewOutput::CardMenu {
                    message: Box::new(m),
                    x: wx,
                    y: wy,
                    hit,
                });
            }
            MessageViewInput::CardMenuFrom {
                account_id,
                id,
                x,
                y,
                page_width,
            } => {
                // CSS pixels → widget pixels (the page may be zoomed); from
                // there it is the same menu the right-click path opens.
                let ratio = self.webview.width() as f64 / page_width.max(1.0);
                sender.input_sender().emit(MessageViewInput::CardMenuAt {
                    key: Some((account_id, id)),
                    x: x * ratio,
                    y: y * ratio,
                    hit: MenuHit::default(),
                });
            }
            MessageViewInput::CardMoveAt {
                account_id,
                id,
                x,
                y,
                page_width,
            } => {
                let Some(m) = self
                    .thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                    .cloned()
                else {
                    return;
                };
                // CSS pixels → widget pixels (the page may be zoomed), then
                // → window.
                let ratio = self.webview.width() as f64 / page_width.max(1.0);
                let (vx, vy) = (x * ratio, y * ratio);
                let point = self.webview.root().and_then(|root| {
                    let root: gtk::Widget = root.upcast();
                    self.webview
                        .compute_point(&root, &gtk::graphene::Point::new(vx as f32, vy as f32))
                });
                let (wx, wy) = point.map_or((vx, vy), |p| (p.x() as f64, p.y() as f64));
                let _ = sender.output(MessageViewOutput::CardMoveTo {
                    message: Box::new(m),
                    x: wx,
                    y: wy,
                });
            }
            MessageViewInput::SetCardActionsMode {
                hover_toggle,
                hover_auto,
            } => {
                if self.card_actions_hover != hover_toggle || self.card_actions_auto != hover_auto {
                    self.card_actions_hover = hover_toggle;
                    self.card_actions_auto = hover_auto;
                    if self.current.is_some() && !self.loading {
                        self.render();
                    }
                }
            }
            MessageViewInput::SetPaletteCollapse(secs) => {
                if self.palette_collapse_secs != secs {
                    self.palette_collapse_secs = secs;
                    if self.current.is_some() && !self.loading {
                        self.render();
                    }
                }
            }
            MessageViewInput::SetCardPaletteMenu(on) => {
                if self.card_palette_menu != on {
                    self.card_palette_menu = on;
                    if self.current.is_some() && !self.loading {
                        self.render();
                    }
                }
            }
            MessageViewInput::ClearDot { account_id, id } => {
                for m in self.thread.iter_mut() {
                    if m.account_id == account_id && m.id == id {
                        m.unread = false;
                    }
                }
                let js = format!(
                    "(function(){{\
                     var d=document.querySelector('.hylki-dot[data-key=\"{account_id}:{id}\"]');\
                     if(d)d.remove();\
                     var s=document.querySelector('.hylki-msg[data-key=\"{account_id}:{id}\"]');\
                     if(s)s.classList.remove('unread');}})()"
                );
                self.webview.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |r| {
                        if let Err(e) = r {
                            tracing::warn!("clear-dot patch failed: {e}");
                        }
                    },
                );
            }
            MessageViewInput::SetReadMark(policy) => {
                if self.read_mark != policy {
                    self.read_mark = policy;
                    if self.current.is_some() && !self.loading {
                        self.render();
                    }
                }
            }
            MessageViewInput::OpenFind => {
                // The toolbar button toggles: a second press collapses the bar.
                if self.find_open {
                    sender.input_sender().emit(MessageViewInput::CloseFind);
                    return;
                }
                if self
                    .find_closed_at
                    .take()
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(300))
                {
                    return;
                }
                self.find_open = true;
                if let Some(entry) = self.find_entry.clone() {
                    gtk::glib::idle_add_local_once(move || {
                        entry.grab_focus();
                    });
                }
            }
            MessageViewInput::CloseFind => {
                self.find_open = false;
                self.find_closed_at = Some(std::time::Instant::now());
                self.find_matches = None;
                if let Some(entry) = &self.find_entry {
                    entry.set_text("");
                }
                self.eval_find("hylkiFindClear()");
            }
            MessageViewInput::FindChanged(text) => {
                let text = text.trim();
                if text.is_empty() {
                    self.find_matches = None;
                    self.eval_find("hylkiFindClear()");
                    return;
                }
                // Into a JS single-quoted string: backslashes and quotes only.
                let quoted = text.replace('\\', "\\\\").replace('\'', "\\'");
                self.eval_find(&format!("hylkiFind('{quoted}')"));
            }
            MessageViewInput::FindNext => self.eval_find("hylkiFindStep(1)"),
            MessageViewInput::FindPrev => self.eval_find("hylkiFindStep(-1)"),
            MessageViewInput::FindCounted { current, total } => {
                self.find_matches = Some((current, total));
            }
            MessageViewInput::SetTags(tags) => {
                if self.tags != tags {
                    self.tags = tags;
                    *self.header_tags_rendered.borrow_mut() = None;
                    if !self.thread.is_empty() {
                        // The chips are baked into the document: draw it again.
                        self.shown_fingerprint = None;
                        self.render();
                    }
                }
            }
            MessageViewInput::SetCardKeywords {
                account_id,
                id,
                keywords,
                labels,
            } => {
                for m in self.thread.iter_mut() {
                    if m.account_id == account_id && m.id == id {
                        m.keywords = keywords.clone();
                        m.labels = labels.clone();
                    }
                }
                if let Some(c) = self.current.as_mut() {
                    if c.account_id == account_id && c.id == id {
                        c.keywords = keywords.clone();
                        c.labels = labels.clone();
                    }
                }
                let chips = tag_chips_html(&self.tags, &keywords, &labels);
                let js = format!(
                    "(function(){{\
                     var t=document.querySelector('.hylki-tags[data-key=\"{account_id}:{id}\"]');\
                     if(t)t.innerHTML={chips};}})()",
                    chips = serde_json::to_string(&chips).unwrap_or_else(|_| "''".into()),
                );
                self.webview.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |r| {
                        if let Err(e) = r {
                            tracing::warn!("card tag patch failed: {e}");
                        }
                    },
                );
            }
            MessageViewInput::SetCardStar {
                account_id,
                id,
                starred,
            } => {
                for m in self.thread.iter_mut() {
                    if m.account_id == account_id && m.id == id {
                        m.starred = starred;
                    }
                }
                let js = format!(
                    "(function(){{\
                     var b=document.querySelector('.hylki-act[data-act=\"star\"][data-key=\"{account_id}:{id}\"]');\
                     if(b)b.classList.{}('on');}})()",
                    if starred { "add" } else { "remove" },
                );
                self.webview.evaluate_javascript(
                    &js,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |r| {
                        if let Err(e) = r {
                            tracing::warn!("card star patch failed: {e}");
                        }
                    },
                );
            }
            MessageViewInput::FocusCard { account_id, id } => {
                let webview = self.webview.clone();
                let gen = self.focus_gen.clone();
                gen.set(gen.get() + 1);
                let my_gen = gen.get();
                // Let the split's 300ms slide finish first — scrolling while
                // the viewport is still shrinking lands somewhere else.
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(380),
                    move || {
                        if gen.get() != my_gen {
                            // A BlurCard beat us here: the reply was cancelled
                            // before the slide finished. Adding the outline
                            // now would strand it.
                            return;
                        }
                        let js = format!(
                            "(function(){{\
                             if(!document.body.classList.contains('hylki-conv'))return;\
                             var s=document.querySelector('.hylki-msg[data-key=\"{account_id}:{id}\"]');\
                             if(!s)return;\
                             document.querySelectorAll('.hylki-msg.selected').forEach(function(e){{e.classList.remove('selected');}});\
                             s.classList.add('selected');\
                             s.style.scrollMarginTop='14px';\
                             s.scrollIntoView({{behavior:'smooth',block:'start'}});}})()"
                        );
                        webview.evaluate_javascript(
                            &js,
                            None,
                            None,
                            None::<&gtk::gio::Cancellable>,
                            |r| {
                                if let Err(e) = r {
                                    tracing::warn!("focus-card scroll failed: {e}");
                                }
                            },
                        );
                    },
                );
            }
            MessageViewInput::BlurCard => {
                self.focus_gen.set(self.focus_gen.get() + 1);
                self.webview.evaluate_javascript(
                    "(function(){document.querySelectorAll('.hylki-msg.selected')\
                     .forEach(function(e){e.classList.remove('selected');});})()",
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |r| {
                        if let Err(e) = r {
                            tracing::warn!("card blur failed: {e}");
                        }
                    },
                );
                // Cards the list still has selected keep their outline.
                self.apply_card_selection();
            }
            MessageViewInput::ScrollAnchor {
                account_id,
                id,
                offset,
            } => {
                self.saved_anchor = Some((account_id, id, offset));
                self.anchor_gutter = false;
            }
            MessageViewInput::PinArrival { account_id, id } => {
                self.saved_anchor = Some((account_id, id, 0));
                self.anchor_gutter = true;
                // The anchor rides only on a document that keeps its place.
                self.did_autoscroll = true;
                self.skip_fade = true;
            }
            MessageViewInput::SetDraftsView(on) => {
                self.drafts_view = on;
            }
            MessageViewInput::SetTemplateFolders(folders) => {
                if self.template_folders != folders {
                    let shown = |set: &std::collections::HashSet<(u32, u32)>| {
                        self.thread
                            .iter()
                            .any(|m| set.contains(&(m.account_id, m.folder_id)))
                    };
                    let redraw = shown(&self.template_folders) || shown(&folders);
                    self.template_folders = folders;
                    if redraw {
                        self.render();
                    }
                }
            }
            MessageViewInput::TemplateAction {
                account_id,
                id,
                edit,
            } => {
                let message = self
                    .thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                    .cloned();
                if let Some(message) = message {
                    use crate::ui::message_list::RowAction;
                    let action = if edit {
                        RowAction::EditTemplate
                    } else {
                        RowAction::EditAsNew
                    };
                    let _ = sender.output(MessageViewOutput::CardAction {
                        action,
                        message: Box::new(message),
                    });
                }
            }
            MessageViewInput::HoldPlace { account_id, id } => {
                if self.saved_anchor.is_none() {
                    self.saved_anchor = Some((account_id, id, 0));
                    self.anchor_gutter = true;
                }
            }
            MessageViewInput::CardContact { account_id, id } => {
                if let Some(m) = self
                    .thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                {
                    let _ = sender.output(MessageViewOutput::ContactSender(Box::new(m.clone())));
                }
            }
            MessageViewInput::OpenHeader { account_id, id } => {
                if let Some(m) = self
                    .thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                {
                    let _ = sender.output(MessageViewOutput::OpenWindow(Box::new(m.clone())));
                }
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
