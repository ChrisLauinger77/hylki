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

use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::models::Message;

mod chrome;
mod dark;
mod folds;
mod frame;
mod html;
mod invite;
mod links;
mod menu;
mod page_fade;
mod print;
mod remote;
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

impl MessageView {}

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

        // Double-click on a conversation header → open that message's window.
        if let Some(ucm) = model.webview.user_content_manager() {
            let open_sender = sender.clone();
            ucm.connect_script_message_received(Some("hylki"), move |_ucm, value| {
                // "verb:account:id" — the page only ever posts what this document
                // put there, but it is still parsed strictly.
                use crate::ui::message_list::RowAction;
                let msg = value.to_str().to_string();
                let mut parts = msg.splitn(4, ':');
                let (Some(verb), Some(a), Some(i)) = (parts.next(), parts.next(), parts.next())
                else {
                    return;
                };
                let extra = parts.next();
                let (Ok(account_id), Ok(id)) = (a.parse::<u32>(), i.parse::<u32>()) else {
                    return;
                };
                match verb {
                    // Every message frame has loaded and been sized: the layout
                    // has settled, so there is something worth revealing.
                    "ready" => open_sender.input_sender().emit(MessageViewInput::Rendered),
                    "desel" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::ClearCards),
                    "selall" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::SelectAllCards),
                    // Ctrl+C over a body frame the engine would not copy from
                    // itself: the selected text, for the clipboard.
                    "copy" => {
                        if let Some(text) = extra.filter(|t| !t.is_empty()) {
                            if let Some(display) = gtk::gdk::Display::default() {
                                display.clipboard().set_text(text);
                            }
                        }
                    }
                    // An address was clicked (or "New Message" picked from its
                    // little menu) — compose to it. `extra` is the bare address.
                    "composeto" => {
                        if let Some(addr) = extra.map(str::trim).filter(|a| !a.is_empty()) {
                            open_sender
                                .input_sender()
                                .emit(MessageViewInput::ComposeTo(addr.to_string()));
                        }
                    }
                    // "Add to Contacts" from the address menu.
                    "addcontact" => {
                        if let Some(addr) = extra.map(str::trim).filter(|a| !a.is_empty()) {
                            open_sender
                                .input_sender()
                                .emit(MessageViewInput::AddContact(addr.to_string()));
                        }
                    }
                    // "Copy Address" from the address menu.
                    "copyaddr" => {
                        if let Some(addr) = extra.map(str::trim).filter(|a| !a.is_empty()) {
                            if let Some(display) = gtk::gdk::Display::default() {
                                display.clipboard().set_text(addr);
                            }
                        }
                    }
                    // The header's verification seal was clicked; extra is
                    // the seal's own rect plus the page's innerWidth. The
                    // rect is in CSS pixels — under a GNOME text scaling
                    // factor ≠ 1.0 WebKit zooms the page, so CSS pixels no
                    // longer match widget pixels; innerWidth lets the
                    // receiver recover the real ratio.
                    "senderinfo" => {
                        let nums: Vec<f64> = extra
                            .map(|e| e.split(',').filter_map(|n| n.parse().ok()).collect())
                            .unwrap_or_default();
                        if let [x, y, w, h, vw] = nums[..] {
                            open_sender
                                .input_sender()
                                .emit(MessageViewInput::SenderInfoAt {
                                    account_id,
                                    id,
                                    rect: (x, y, w, h),
                                    page_width: vw,
                                });
                        }
                    }
                    "sel" => {
                        let mode = match extra {
                            Some("t") => SelectMode::Toggle,
                            Some("r") => SelectMode::Range,
                            _ => SelectMode::Plain,
                        };
                        open_sender
                            .input_sender()
                            .emit(MessageViewInput::CardClicked {
                                account_id,
                                id,
                                mode,
                            });
                    }
                    "size" => {
                        if let Some(Ok(height)) = extra.map(|h| h.parse::<u32>()) {
                            open_sender
                                .input_sender()
                                .emit(MessageViewInput::FrameSized {
                                    account_id,
                                    id,
                                    height,
                                });
                        }
                    }
                    "open" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::OpenHeader { account_id, id }),
                    "seen" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::MarkSeen { account_id, id }),
                    "unsub" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::Unsubscribe { account_id, id }),
                    // A card's template banner (#360); `extra` is the button.
                    "tpl" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::TemplateAction {
                            account_id,
                            id,
                            edit: extra.map(str::trim) == Some("edit"),
                        }),
                    // A card's invitation banner (#223); `extra` is which
                    // button was pressed.
                    "invite" => {
                        use crate::models::Rsvp;
                        let action = match extra.map(str::trim) {
                            Some("yes") => Some(InviteAction::Answer(Rsvp::Accepted)),
                            Some("maybe") => Some(InviteAction::Answer(Rsvp::Tentative)),
                            Some("no") => Some(InviteAction::Answer(Rsvp::Declined)),
                            Some("cal") => Some(InviteAction::AddToCalendar),
                            _ => None,
                        };
                        if let Some(action) = action {
                            open_sender
                                .input_sender()
                                .emit(MessageViewInput::InviteAction {
                                    account_id,
                                    id,
                                    action,
                                });
                        }
                    }
                    // A card's attachment chip (#213); `extra` is the index.
                    "attopen" | "attsave" => {
                        let index = extra
                            .and_then(|x| x.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        open_sender
                            .input_sender()
                            .emit(MessageViewInput::AttachmentAction {
                                account_id,
                                id,
                                index,
                                save: verb == "attsave",
                            });
                    }
                    "found" => {
                        let (cur, total) = extra
                            .and_then(|e| e.split_once(','))
                            .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
                            .unwrap_or((0, 0));
                        open_sender
                            .input_sender()
                            .emit(MessageViewInput::FindCounted {
                                current: cur,
                                total,
                            });
                    }
                    "reply" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::CardAction {
                            action: RowAction::Reply,
                            account_id,
                            id,
                        }),
                    "replyall" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::CardAction {
                            action: RowAction::ReplyAll,
                            account_id,
                            id,
                        }),
                    "forward" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::CardAction {
                            action: RowAction::Forward,
                            account_id,
                            id,
                        }),
                    "star" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::CardAction {
                            action: RowAction::ToggleStar,
                            account_id,
                            id,
                        }),
                    "spam" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::CardAction {
                            action: RowAction::Spam,
                            account_id,
                            id,
                        }),
                    "remote" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::ToggleRemote { account_id, id }),
                    "archive" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::CardAction {
                            action: RowAction::Archive,
                            account_id,
                            id,
                        }),
                    // The ⋯ acting as a menu: the button's bottom-left corner
                    // and the page width, the same shape "moveto" sends.
                    "cardmenu" => {
                        let nums: Vec<f64> = extra
                            .map(|e| e.split(',').filter_map(|n| n.parse().ok()).collect())
                            .unwrap_or_default();
                        if let [x, y, vw] = nums[..] {
                            open_sender
                                .input_sender()
                                .emit(MessageViewInput::CardMenuFrom {
                                    account_id,
                                    id,
                                    x,
                                    y,
                                    page_width: vw,
                                });
                        }
                    }
                    // Move to… on a card: extra is the button's bottom-centre
                    // and the page width, in CSS pixels (see "senderinfo").
                    "moveto" => {
                        let nums: Vec<f64> = extra
                            .map(|e| e.split(',').filter_map(|n| n.parse().ok()).collect())
                            .unwrap_or_default();
                        if let [x, y, vw] = nums[..] {
                            open_sender
                                .input_sender()
                                .emit(MessageViewInput::CardMoveAt {
                                    account_id,
                                    id,
                                    x,
                                    y,
                                    page_width: vw,
                                });
                        }
                    }
                    "delete" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::CardAction {
                            action: RowAction::Delete,
                            account_id,
                            id,
                        }),
                    "toggleread" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::CardAction {
                            action: RowAction::ToggleRead,
                            account_id,
                            id,
                        }),
                    // Translate on a card, or its banner's button (#327).
                    "translate" | "tr" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::Translate { account_id, id }),
                    // A header folded or opened its card (#326).
                    "fold" => open_sender.input_sender().emit(MessageViewInput::Folded {
                        account_id,
                        id,
                        folded: extra == Some("1"),
                    }),
                    "viewsource" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::CardAction {
                            action: RowAction::ViewSource,
                            account_id,
                            id,
                        }),
                    "contact" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::CardContact { account_id, id }),
                    "senderfmt" => open_sender
                        .input_sender()
                        .emit(MessageViewInput::ToggleSenderStyle { account_id, id }),
                    // "scrollat:<aid>:<id>:<offset>" — the topmost visible card.
                    "scrollat" => {
                        if let Some(Ok(offset)) = extra.map(|o| o.parse::<u32>()) {
                            open_sender
                                .input_sender()
                                .emit(MessageViewInput::ScrollAnchor {
                                    account_id,
                                    id,
                                    offset,
                                });
                        }
                    }
                    _ => {}
                }
            });
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

impl MessageView {
    /// The desktop accent color, as the document can use it. Read from the
    /// widget's style so it follows the user's choice; GNOME's own blue is the
    /// fallback when the theme doesn't define it.
    fn accent_hex(&self) -> String {
        #[allow(deprecated)]
        self.webview
            .style_context()
            .lookup_color("accent_bg_color")
            .map(|c| crate::color::to_hex(&c))
            .unwrap_or_else(|| "#3584e4".to_string())
    }

    /// Outline the selected cards in the document already on screen. Rendering
    /// it again would reload every frame and lose the reader's place.
    fn apply_card_selection(&self) {
        // A lone card never wears the outline (see `conversation_document`).
        if self.thread.len() <= 1 {
            return;
        }
        let keys: Vec<String> = self
            .selected_cards
            .iter()
            .map(|(a, i)| format!("'{a}:{i}'"))
            .collect();
        let js = format!(
            "(function(){{var s=[{}];             var els=document.querySelectorAll('.hylki-msg');             for(var i=0;i<els.length;i++){{var k=els[i].dataset.key;             els[i].classList.toggle('selected', s.indexOf(k)>=0);}}}})()",
            keys.join(",")
        );
        self.webview
            .evaluate_javascript(&js, None, None, None::<&gtk::gio::Cancellable>, |_| {});
    }

    /// Whether the theme draws messages dark: the message theme, or the
    /// system's when it follows the system.
    fn theme_dark(&self) -> bool {
        self.content_dark
            .unwrap_or_else(|| adw::StyleManager::default().is_dark())
    }

    /// Whether the message on screen is drawn dark: the scheme the user
    /// chose for it, else the theme's.
    fn effective_dark(&self) -> bool {
        self.current
            .as_ref()
            .and_then(|m| self.theme_picks.get(&(m.account_id, m.id)).copied())
            .unwrap_or_else(|| self.theme_dark())
    }

    /// The Light / Dark Mode switch's position: on only while the message is
    /// shown opposite the theme. A message the user turned light reads off
    /// once the theme turns light too, and still stays light if it turns
    /// back.
    fn message_flipped(&self) -> bool {
        self.effective_dark() != self.theme_dark()
    }

    /// Whether cards list their own attachments (#213): by preference, and
    /// only where the drawer is not already that list — a lone message's
    /// drawer holds exactly its files, so its card repeats nothing.
    fn chips_shown(&self) -> bool {
        self.card_atts_shown && (self.thread.len() > 1 || !self.drawer_on)
    }

    /// Fill (or empty) the attachment rows of these cards in the live
    /// document (#213). Files arrive after the paint; reloading the document
    /// for them would resettle every card, so the rows are patched in place.
    fn patch_card_atts(&self, keys: &[(u32, u32)]) {
        if !self.webview_ready || self.current.is_none() || keys.is_empty() {
            return;
        }
        let shown = self.chips_shown();
        let mut js = String::from("(function(){");
        for key in keys {
            let html = if shown {
                att_chips_html(
                    *key,
                    self.card_atts.get(key).map(|v| v.as_slice()).unwrap_or(&[]),
                )
            } else {
                String::new()
            };
            js.push_str(&format!(
                "var d=document.querySelector('.hylki-atts[data-key=\"{}:{}\"]');if(d)d.innerHTML={};",
                key.0,
                key.1,
                serde_json::to_string(&html).unwrap_or_else(|_| "''".into()),
            ));
        }
        js.push_str("})()");
        self.webview
            .evaluate_javascript(&js, None, None, None::<&gtk::gio::Cancellable>, |r| {
                if let Err(e) = r {
                    tracing::warn!("attachment row patch failed: {e}");
                }
            });
    }

    fn render(&mut self) {
        let dark = self.effective_dark();
        self.apply_webview_bg(dark);
        // Already showing exactly this — returning to a conversation, or a
        // re-render nothing changed. Loading it again would only blank the view
        // and paint the same pixels back.
        let fingerprint = self.render_fingerprint(dark);
        if self.webview_ready && self.shown_fingerprint == Some(fingerprint) {
            return;
        }
        self.shown_fingerprint = Some(fingerprint);
        // A page handed over while the window is hidden (running in the
        // background, a notification about to bring it back) may never be
        // painted: it is loaded again once the view is on screen (#332).
        if !self.webview.is_mapped() {
            self.painted_hidden = true;
        }
        // A conversation is covered until its new document has painted: the
        // spinner is already up from the moment the thread was opened, so it
        // simply stays until there is something to replace it — one transition,
        // not three.
        //
        // A single message is not. It is one small frame that paints in a few
        // milliseconds, and covering that is how a plain message ended up
        // showing a spinner every time it was opened; the view keeps what it has
        // until the new document replaces it.
        if self.thread.len() > 1 && !self.instant {
            self.webview_ready = false;
        }
        // Only a page that stays in view dissolves into the next; behind the
        // spinner there is nothing to see.
        let fade = self.webview_ready && !self.loading && !std::mem::take(&mut self.skip_fade);
        let html = self.document_html(dark);
        // A message painted alone that has since grown into a conversation
        // gets the conversation's open scroll, unless the reader has been
        // scrolled meanwhile: otherwise it stays at the top, on the oldest
        // part, often the user's own sent original (#351).
        if self.did_autoscroll
            && self.autoscroll_lone
            && self.thread.len() > 1
            && self.saved_anchor.is_none()
        {
            self.did_autoscroll = false;
        }
        // A conversation opened behind the loading cover spends its open
        // scroll on a document nobody saw; the repaint once it is on screen
        // would otherwise keep that unseen place, the top.
        if self.webview.is_mapped() {
            if std::mem::take(&mut self.scroll_owed) && self.saved_anchor.is_none() {
                self.did_autoscroll = false;
            }
        } else if !self.did_autoscroll {
            self.scroll_owed = true;
        }
        // Only a conversation's first document may auto-scroll to the unread
        // mark; every later render of the same thread (bodies streaming in, a
        // theme change) is stamped no-scroll so the reader's place is kept.
        let noscroll = if self.did_autoscroll {
            let anchor = self
                .saved_anchor
                .map(|(a, i, o)| {
                    let g = if self.anchor_gutter { ":g" } else { "" };
                    format!(" data-hylki-anchor=\"{a}:{i}:{o}{g}\"")
                })
                .unwrap_or_default();
            format!(" data-hylki-noscroll=\"1\"{anchor}")
        } else {
            String::new()
        };
        // How card actions show is a body stamp too, so the static document
        // builder (and its tests) need no extra parameter: "toggle" hides them
        // behind the ⋯, "hover" shows them while the card is hovered, and no
        // stamp means always shown.
        let hover = if self.card_actions_hover {
            " data-hylki-acts=\"toggle\""
        } else if self.card_actions_auto {
            " data-hylki-acts=\"hover\""
        } else {
            ""
        };
        // The palette timeout rides along in ms; the script reads it for the
        // toggle mode's collapse timer and the hover mode's fade-out delay.
        let delay = format!(
            " data-hylki-actsdelay=\"{}\"",
            self.palette_collapse_secs.max(1) * 1000
        );
        // With the ⋯ set to open the card's menu there is no palette to
        // slide, so the script hands the click back to the app instead.
        let acts_menu = if self.card_palette_menu {
            " data-hylki-actsmenu=\"1\""
        } else {
            ""
        };
        // The OpenPGP chip as its icons alone, without the words (#300).
        let pgp_icons = if self.pgp_labels {
            ""
        } else {
            " data-hylki-pgpicons"
        };
        // The newest member, for the open-scroll fallback (#101): with no
        // unread mail the reader lands on the newest message rather than
        // wherever the document happens to start.
        let newest = self
            .thread
            .iter()
            .max_by_key(|m| m.timestamp)
            .filter(|_| self.thread.len() > 1)
            .map(|m| format!(" data-hylki-newest=\"{}:{}\"", m.account_id, m.id))
            .unwrap_or_default();
        // Read-marking policy (#100): the observer marks conversation members
        // as they come into view; "manual" installs no observer at all.
        let readmark = match self.read_mark {
            crate::config::ReadMark::Shown => " data-hylki-readmark=\"250\"",
            crate::config::ReadMark::Delay => " data-hylki-readmark=\"2000\"",
            crate::config::ReadMark::Manual => "",
        };
        let copied = format!(
            " data-hylki-copied=\"{}\"",
            i18n("Copied").replace('"', "&quot;")
        );
        let html = html.replacen(
            "<body",
            &format!(
                "<body{noscroll}{hover}{delay}{acts_menu}{pgp_icons}{newest}{readmark}{copied}"
            ),
            1,
        );
        self.did_autoscroll = true;
        self.autoscroll_lone = self.thread.len() <= 1;
        let n = self.seq.get().wrapping_add(1);
        self.seq.set(n);
        self.load_after_paint(html, format!("https://hylki.localhost/message/{n}"), fade);
    }

    /// Which body-stack page to show: spinner while fetching, themed cover while
    /// the WebView loads, then the rendered message(s).
    /// While a document loads, keep the keyboard on the loading page rather
    /// than in the web view, and give it back once the document is shown.
    fn hold_focus_while_loading(&self) {
        let Some(page) = &self.loading_page else {
            return;
        };
        let web: &gtk::Widget = self.webview.upcast_ref();
        let focus = web.root().and_then(|r| r.focus());
        if self.body_page() == "loading" {
            if focus.is_some_and(|f| f == *web || f.is_ancestor(web)) {
                page.grab_focus();
                self.focus_held.set(true);
            }
        } else if self.focus_held.replace(false)
            && focus.is_some_and(|f| f == *page.upcast_ref::<gtk::Widget>())
        {
            self.webview.grab_focus();
        }
    }

    fn body_page(&self) -> &'static str {
        // A document that has been handed over but hasn't painted yet is still
        // loading as far as the reader is concerned. Showing the previous
        // message in the meantime, or a bare cover between two spinners, is the
        // flash this avoids.
        if self.loading || !self.webview_ready {
            "loading"
        } else {
            "body"
        }
    }

    /// The wrapper document for what is currently on screen.
    /// Everything the rendered document depends on, in one number. Ordered by
    /// the thread so it is stable — the sets it consults are read through it
    /// rather than iterated, whose order is not.
    fn render_fingerprint(&self, dark: bool) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        dark.hash(&mut h);
        // A theme change can move the grounds without flipping `dark` (a
        // different GTK theme variant, say) — the document must follow.
        self.theme_grounds(dark).hash(&mut h);
        self.remote_allowed.hash(&mut h);
        self.reader_style.hash(&mut h);
        self.reader_mode.hash(&mut h);
        // Translations (#327): which cards show one, and whether cards
        // offer Translate at all.
        crate::translate::generation().hash(&mut h);
        self.fold_messages.hash(&mut h);
        self.fold_preview_lines.hash(&mut h);
        let mut shown: Vec<(u32, u32, usize)> = self
            .translations
            .iter()
            .filter_map(|(k, t)| match t {
                TrState::Shown(t) => Some((k.0, k.1, t.html.len())),
                _ => None,
            })
            .collect();
        shown.sort_unstable();
        shown.hash(&mut h);
        self.card_actions_hover.hash(&mut h);
        self.card_actions_auto.hash(&mut h);
        self.palette_collapse_secs.hash(&mut h);
        self.card_palette_menu.hash(&mut h);
        // Whether cards list their attachments (#213); the lists themselves
        // are patched in live rather than rendered for.
        self.chips_shown().hash(&mut h);
        // The picture a mailbox of your own shows can change while a
        // conversation is open (#189) — the cards must be built again.
        crate::avatar::own_faces_generation().hash(&mut h);
        self.thread.len().hash(&mut h);
        for m in &self.thread {
            let key = (m.account_id, m.id);
            key.hash(&mut h);
            m.unread.hash(&mut h);
            m.from_name.hash(&mut h);
            m.from_addr.hash(&mut h);
            m.to.hash(&mut h);
            m.cc.hash(&mut h);
            m.bcc.hash(&mut h);
            m.body.hash(&mut h);
            m.datetime_full().hash(&mut h);
            self.no_autoread.contains(&key).hash(&mut h);
            self.sender_style.contains(&key).hash(&mut h);
            self.folder_labels.get(&key).hash(&mut h);
            self.template_folders
                .contains(&(m.account_id, m.folder_id))
                .hash(&mut h);
            // Tags are patched into a card live; Gmail labels (#374) only
            // change with a sync, and are drawn with the card.
            m.labels.hash(&mut h);
        }
        h.finish()
    }

    fn document_html(&self, dark: bool) -> String {
        // Hand the live theme grounds to the (display-free, testable) document
        // builder without widening its signature — see LIVE_GROUNDS.
        LIVE_GROUNDS.with(|g| *g.borrow_mut() = Some(self.theme_grounds(dark)));
        LIVE_TAGS.with(|t| *t.borrow_mut() = self.tags.clone());
        LIVE_ATTS.with(|a| {
            *a.borrow_mut() = if self.chips_shown() {
                self.card_atts.clone()
            } else {
                Default::default()
            }
        });
        LIVE_UNSUB.with(|u| {
            *u.borrow_mut() = self
                .thread
                .iter()
                .map(|m| ((m.account_id, m.id), self.unsub_inner_html(m)))
                .collect()
        });
        LIVE_INVITE.with(|i| {
            *i.borrow_mut() = self
                .thread
                .iter()
                .map(|m| ((m.account_id, m.id), self.invite_inner_html(m)))
                .collect()
        });
        LIVE_FOLDED.with(|f| *f.borrow_mut() = self.folded_cards());
        LIVE_TEMPLATES.with(|t| {
            *t.borrow_mut() = self
                .thread
                .iter()
                .filter(|m| self.template_folders.contains(&(m.account_id, m.folder_id)))
                .map(|m| (m.account_id, m.id))
                .collect()
        });
        LIVE_FOLDBAR.with(|b| {
            *b.borrow_mut() = if self.thread.len() > 1 {
                self.thread
                    .iter()
                    .map(|m| ((m.account_id, m.id), self.fold_bar_html(m)))
                    .collect()
            } else {
                Default::default()
            }
        });
        let translation = crate::translate::load();
        LIVE_TRANSLATE_ON.with(|on| on.set(translation.enabled()));
        LIVE_TRANSLATE.with(|t| {
            *t.borrow_mut() = self
                .thread
                .iter()
                .map(|m| {
                    let key = (m.account_id, m.id);
                    let shown = match self.translations.get(&key) {
                        Some(TrState::Shown(t)) => Some((t.html.clone(), t.reader)),
                        _ => None,
                    };
                    (key, (self.translate_inner_html(m, &translation), shown))
                })
                .collect()
        });
        Self::conversation_document(
            &self.thread,
            &self.folder_labels,
            &self.no_autoread,
            &self.frame_heights,
            &self.selected_cards,
            &self.accent_hex(),
            !self.remote_allowed,
            dark,
            self.always_show_recipients,
            self.single_message_card,
            &self.reader_style,
            &self.sender_style,
            self.reader_mode,
            self.zoom,
        )
    }

    /// The wrapper document: one sandboxed iframe per message (so each email's
    /// CSS is fully isolated and its scripts can't run), with per-message
    /// headers in conversation mode. A small script sizes each iframe to its
    /// content.
    ///
    /// Takes the thread rather than `&self` so it can be exercised without a GTK
    /// display: what it emits is a security boundary, and one that needs
    /// regression cover.
    fn conversation_document(
        thread: &[Message],
        folder_labels: &std::collections::HashMap<(u32, u32), String>,
        _no_autoread: &std::collections::HashSet<(u32, u32)>,
        heights: &std::collections::HashMap<(u32, u32), u32>,
        selected: &[(u32, u32)],
        accent: &str,
        restrict: bool,
        dark: bool,
        always_show_recipients: bool,
        single_message_card: bool,
        style: &crate::config::ReaderStyle,
        sender_style: &std::collections::HashSet<(u32, u32)>,
        reader: bool,
        zoom: u32,
    ) -> String {
        // Every message renders with the conversation chrome — a thread of one
        // gets the same in-document header. But only a real conversation is
        // *carded*: inset rounded cards on the deeper page. A lone message
        // goes full-bleed — no gutter, no radius, its ground filling the whole
        // view — so it reads as a message rather than a card in a margin.
        let conversation = !thread.is_empty();
        // A lone message cards up too when the preference asks (#57);
        // otherwise it keeps the full-bleed treatment below.
        let carded = thread.len() > 1 || (conversation && single_message_card);
        // Card selection only means something between cards: a lone message is
        // already the selection, so it never wears the accent outline.
        let mark_selection = thread.len() > 1;
        let mut sections = String::new();
        for m in thread {
            // A card showing its translation (#327): the message with its
            // text replaced, or for plain text a Reader View fragment.
            let (translated, translated_reader) = LIVE_TRANSLATE
                .with(|t| {
                    t.borrow()
                        .get(&(m.account_id, m.id))
                        .and_then(|(_, shown)| shown.clone())
                })
                .map_or((None, false), |(html, reader)| (Some(html), reader));
            let body = if m.body.trim().is_empty() && translated.is_none() {
                "<div class=\"hylki-loading\">Loading…</div>".to_string()
            } else {
                message_frame(
                    translated.as_deref().unwrap_or(&m.body),
                    restrict,
                    dark,
                    (m.account_id, m.id),
                    heights.get(&(m.account_id, m.id)).copied(),
                    // The card's toggle wins over the preference.
                    if sender_style.contains(&(m.account_id, m.id)) {
                        &crate::config::ReaderStyle::NONE
                    } else {
                        style
                    },
                    accent,
                    reader || translated_reader,
                    zoom,
                )
            };
            if conversation {
                let ava_html = {
                    // One of your own mailboxes wrote this card (#189):
                    // show what its account chose — the sidebar's picture,
                    // or its emoji on the account color — so your own
                    // replies in a conversation wear the face you gave
                    // that mailbox rather than plain initials.
                    let own = crate::avatar::own_face(&m.from_addr).and_then(|face| {
                        // The account's own Gravatar leads when it asked
                        // for one and the address has one; the picture and
                        // the emoji are what it falls back to.
                        let gravatar = face
                            .gravatar
                            .then(|| crate::avatar::own_gravatar_data_uri(&m.from_addr, 26))
                            .flatten();
                        if let Some(uri) = gravatar {
                            return Some(format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">"));
                        }
                        match (&face.picture, &face.emoji) {
                            (Some(path), _) => crate::ui::initials::picture_data_uri(path, 26)
                                .map(|uri| format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">")),
                            (None, Some(emoji)) => {
                                let png = gtk::gdk::RGBA::parse(&face.color)
                                    .ok()
                                    .and_then(|bg| crate::ui::initials::png_data_uri(emoji, bg, 26));
                                Some(match png {
                                    Some(uri) => {
                                        format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">")
                                    }
                                    None => format!(
                                        "<span class=\"hylki-ava\" style=\"background:{bg}\">{glyph}</span>",
                                        bg = attr_escape(&face.color),
                                        glyph = escape_text(emoji),
                                    ),
                                })
                            }
                            (None, None) => None,
                        }
                    });
                    own.unwrap_or_else(|| {
                        let initial = m
                            .from_name
                            .trim()
                            .chars()
                            .next()
                            .or_else(|| m.from_addr.trim().chars().next())
                            .map(|c| c.to_uppercase().to_string())
                            .unwrap_or_else(|| "?".to_string());
                        let hue = m
                            .from_addr
                            .to_ascii_lowercase()
                            .bytes()
                            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
                            % 360;
                        let l = if dark { 38 } else { 45 };
                        // Drawn ink-centred by ui::initials and embedded as
                        // a PNG (the same tint); the markup span stands in
                        // only before a window exists to render with.
                        let bg = crate::ui::initials::hsl(f64::from(hue), 0.52, f64::from(l) / 100.0);
                        match crate::ui::initials::png_data_uri(&initial, bg, 26) {
                            Some(uri) => format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">"),
                            None => format!(
                                "<span class=\"hylki-ava\" style=\"background:hsl({hue},52%,{l}%)\">{}</span>",
                                escape_text(&initial),
                            ),
                        }
                    })
                };
                sections.push_str(&format!(
                    "<section class=\"hylki-msg{sel}{unread_cls}{folded_cls}\" data-key=\"{aid}:{id}\">{fbar}\
                       <header class=\"hylki-msg-hdr\" data-key=\"{aid}:{id}\" \
                         title=\"{hdr_title}\">\
                         <div class=\"hylki-hdr-line\">\
                           {ava}{dot}<span class=\"hylki-from\">{from}</span>{verify}{addr}\
                           <span class=\"hylki-tags\" data-key=\"{aid}:{id}\">{tags}</span>\
                           <span class=\"hylki-hdr-meta\">{folder}{rcpt_toggle}\
                             <span class=\"hylki-date\">{date}</span></span>\
                           {acts_toggle}{acts}\
                         </div>{rcpt}\
                       </header>{tpl}{invite}{unsub}{tr}{body}{atts}</section>",
                    aid = m.account_id,
                    id = m.id,
                    // The message's own attachments beneath its body (#213).
                    // The row is always emitted, so files arriving after
                    // the paint have somewhere to be patched into.
                    atts = LIVE_ATTS.with(|a| {
                        let key = (m.account_id, m.id);
                        att_row_html(key, a.borrow().get(&key).map(|v| v.as_slice()))
                    }),
                    // A template's banner (#360), first: what the message
                    // is for matters more than anything it says.
                    tpl = if LIVE_TEMPLATES.with(|t| t.borrow().contains(&(m.account_id, m.id))) {
                        template_row_html((m.account_id, m.id))
                    } else {
                        String::new()
                    },
                    // The invitation banner (#223) at the top of the card,
                    // above the Unsubscribe one and on the same terms: the
                    // container is always there, empty (hidden) for a
                    // message about no meeting, so an invitation read after
                    // the paint is patched in without a re-render.
                    invite = LIVE_INVITE.with(|i| {
                        invite_row_html(
                            (m.account_id, m.id),
                            i.borrow().get(&(m.account_id, m.id)).map(String::as_str).unwrap_or(""),
                        )
                    }),
                    // The Unsubscribe banner between header and body: the
                    // container is always there, empty (hidden) for a
                    // message from no list, so a verdict arriving after the
                    // paint is patched in without a re-render.
                    // The translation banner (#327), on the same terms.
                    tr = LIVE_TRANSLATE.with(|t| {
                        format!(
                            "<div class=\"hylki-tr\" data-key=\"{}:{}\">{}</div>",
                            m.account_id,
                            m.id,
                            t.borrow().get(&(m.account_id, m.id)).map(|(b, _)| b.as_str()).unwrap_or("")
                        )
                    }),
                    unsub = LIVE_UNSUB.with(|u| {
                        unsub_row_html(
                            (m.account_id, m.id),
                            u.borrow().get(&(m.account_id, m.id)).map(String::as_str).unwrap_or(""),
                        )
                    }),
                    hdr_title = gtk::glib::markup_escape_text(
                        &i18n("Double-click to open in a new window")
                    ),
                    tags = LIVE_TAGS.with(|t| tag_chips_html(&t.borrow(), &m.keywords, &m.labels)),
                    // The ⋯ toggle that expands/collapses the action row when
                    // the hidden-until-hover preference is on; CSS keeps it
                    // display:none otherwise, so it costs nothing when off.
                    acts_toggle = if !thread.is_empty() {
                        format!(
                            "<button type=\"button\" class=\"hylki-acts-toggle\" \
                             title=\"{acts_title}\" data-key=\"{aid}:{id}\">{svg}</button>",
                            aid = m.account_id,
                            id = m.id,
                            acts_title = gtk::glib::markup_escape_text(&i18n("Actions")),
                            // The same ⋯ the list's actions palette toggle uses.
                            svg = inline_icon_svg("view-more-horizontal-symbolic"),
                        )
                    } else {
                        String::new()
                    },
                    // Per-card actions, on every message — single messages
                    // included: the reader toolbar carries no per-message
                    // icons any more, so the card's own row (and the overflow
                    // menu) is where a message is acted on.
                    acts = if !thread.is_empty() {
                        let key = (m.account_id, m.id);
                        format!(
                            "<span class=\"hylki-acts\">{}{}{}{}{}{}{}{}{}{}{}{}{}</span>",
                            // Same order as the reader toolbar and the list's
                            // actions palette, View Source closing the line.
                            card_action_button(key, "reply", "mail-reply-sender-symbolic", &i18n("Reply to this message")),
                            card_action_button(key, "replyall", "mail-reply-all-symbolic", &i18n("Reply to everyone on this message")),
                            card_action_button(key, "forward", "mail-forward-symbolic", &i18n("Forward this message")),
                            // Action-showing icon (read envelope = "mark as
                            // read"), like the menus and toolbar. Both icons
                            // are baked in; the section's `unread` class picks
                            // one, so marking read/unread flips in place.
                            format!(
                                "<button type=\"button\" class=\"hylki-act\" data-act=\"toggleread\" \
                                 data-key=\"{aid}:{id}\" title=\"{title}\">\
                                 <span class=\"tr-when-unread\">{read_svg}</span>\
                                 <span class=\"tr-when-read\">{unread_svg}</span></button>",
                                aid = key.0,
                                id = key.1,
                                title = gtk::glib::markup_escape_text(&i18n("Mark as read or unread")),
                                read_svg = inline_icon_svg("hylki-mail-read-symbolic"),
                                unread_svg = inline_icon_svg("mail-unread-symbolic"),
                            ),
                            // The star keeps one glyph; the starred state is
                            // color alone (`.on`, toggled optimistically on
                            // click too).
                            format!(
                                "<button type=\"button\" class=\"hylki-act{on}\" data-act=\"star\" \
                                 data-key=\"{aid}:{id}\" title=\"{title}\">{svg}</button>",
                                on = if m.starred { " on" } else { "" },
                                title = gtk::glib::markup_escape_text(&i18n("Star this message")),
                                aid = key.0,
                                id = key.1,
                                svg = inline_icon_svg("hylki-non-starred-symbolic"),
                            ),
                            card_action_button(key, "moveto", "folder-symbolic", &i18n("Move this message to a folder")),
                            card_action_button(key, "archive", "mail-archive-symbolic", &i18n("Archive this message")),
                            card_action_button(key, "delete", "user-trash-symbolic", &i18n("Delete this message")),
                            card_action_button(key, "spam", "mail-mark-junk-symbolic", &i18n("Mark as Spam")),
                            card_action_button(key, "contact", "contact-new-symbolic", &i18n("Add sender to Contacts")),
                            format!(
                                "{}{}",
                                card_action_button(key, "viewsource", "code-symbolic", &i18n("View source")),
                                if LIVE_TRANSLATE_ON.with(|on| on.get()) {
                                    card_action_button(key, "translate", "translate-symbolic", &i18n("Translate this message"))
                                } else {
                                    String::new()
                                },
                            ),
                            // The escape from the reader's own fonts and
                            // colors (#56): only offered while an override
                            // is on, lit while this card shows the sender's.
                            if style.active() && !reader {
                                let on = sender_style.contains(&key);
                                format!(
                                    "<button type=\"button\" class=\"hylki-act{on_cls}\" data-act=\"senderfmt\" \
                                     data-key=\"{aid}:{id}\" title=\"{title}\">{svg}</button>",
                                    on_cls = if on { " on" } else { "" },
                                    aid = key.0,
                                    id = key.1,
                                    title = gtk::glib::markup_escape_text(&if on {
                                        i18n("Back to my fonts and colors")
                                    } else {
                                        i18n("Show the sender's fonts and colors")
                                    }),
                                    svg = inline_icon_svg("format-text-rich-symbolic"),
                                )
                            } else {
                                String::new()
                            },
                            // Remote content, per message. The banner offers
                            // the same thing, but it can be switched off
                            // (Settings → Privacy) and then there is nothing to
                            // click — and it only ever lets content in, where
                            // this puts the block back too. Offered only on a
                            // card that actually has something to load.
                            if has_remote_resources(&m.body) {
                                format!(
                                    "<button type=\"button\" class=\"hylki-act{on_cls}\" data-act=\"remote\" \
                                     data-key=\"{aid}:{id}\" title=\"{title}\">{svg}</button>",
                                    on_cls = if restrict { "" } else { " on" },
                                    aid = key.0,
                                    id = key.1,
                                    title = gtk::glib::markup_escape_text(&if restrict {
                                        i18n("Show remote content")
                                    } else {
                                        i18n("Block remote content")
                                    }),
                                    svg = inline_icon_svg("image-x-generic-symbolic"),
                                )
                            } else {
                                String::new()
                            },
                        )
                    } else {
                        String::new()
                    },
                    sel = if mark_selection && selected.contains(&(m.account_id, m.id)) {
                        " selected"
                    } else {
                        ""
                    },
                    unread_cls = if m.unread { " unread" } else { "" },
                    // Folded (#326): the header and a line of preview.
                    folded_cls = if LIVE_FOLDED.with(|f| f.borrow().contains(&(m.account_id, m.id))) {
                        " hylki-folded"
                    } else {
                        ""
                    },
                    // What a folded card shows instead of all of it (#326).
                    fbar = LIVE_FOLDBAR.with(|b| {
                        b.borrow()
                            .get(&(m.account_id, m.id))
                            .map(|bar| bar.replace(FOLD_BAR_FACE, &ava_html))
                            .unwrap_or_default()
                    }),
                    // `escape_text`, not `attr_escape`: these land in element
                    // text content, where `<` and `>` are structural. A `From:`
                    // display name is attacker-controlled (and RFC 2047-decoded,
                    // so any byte sequence can be delivered), and this document
                    // is the trusted wrapper — not a sandboxed message frame.
                    from = escape_text(&m.from_name),
                    // The sender-authentication seal (#88): hidden until a
                    // verdict arrives (SenderCheckFor patches it live), then
                    // tinted by trust — Bazaar's fixed blue for a pass.
                    verify = format!(
                        "<button type=\"button\" class=\"hylki-verify\" data-key=\"{aid}:{id}\" \
                         title=\"\">{svg}</button>\
                         <button type=\"button\" class=\"hylki-pgp\" data-key=\"{aid}:{id}\" \
                         title=\"\">{lock}{sig}<span class=\"hylki-pgp-text\"></span></button>\
                         <span class=\"hylki-sens\" data-key=\"{aid}:{id}\" title=\"\">{sens}\
                         <span class=\"hylki-sens-text\"></span></span>",
                        aid = m.account_id,
                        id = m.id,
                        svg = inline_icon_svg("verified-checkmark-symbolic"),
                        // The OpenPGP chip (#133): a lock for an encrypted
                        // message, a shield for a signed one, then the
                        // verdict in words (#300); hidden until the verdict
                        // is patched in.
                        lock = inline_icon_svg("channel-secure-symbolic"),
                        sig = inline_icon_svg("security-high-symbolic"),
                        // The sensitivity label (#411), hidden until a
                        // label is patched in.
                        sens = inline_icon_svg("tag-outline-symbolic"),
                    ),
                    // An initials circle, tinted per sender address, so who
                    // wrote each card — and which cards are your own replies —
                    // reads at a glance (#22). Nothing is linked: a picture
                    // arrives as an embedded PNG, never a path the document
                    // could reach for, and the initial is escaped like every
                    // other header field.
                    ava = ava_html.clone(),
                    addr = if m.from_addr.is_empty() {
                        String::new()
                    } else {
                        // `data-addr` feeds the hover reveal's ::after overlay;
                        // `data-mail` (the bare address) drives click-to-compose
                        // and the copy menu. Fully entity-escaped (&<>"), so an
                        // attacker-controlled address can't smuggle markup
                        // through the attributes.
                        format!(
                            "<span class=\"hylki-addr hylki-mail\" data-mail=\"{0}\" \
                             data-addr=\"&lt;{0}&gt;\">&lt;{0}&gt;</span>",
                            escape_text(&m.from_addr).replace('"', "&quot;")
                        )
                    },
                    date = escape_text(&m.datetime_full()),
                    // Everyone the message went to, tucked behind a small chip so
                    // a card's header stays one line tall until asked. Escaped
                    // like the sender: recipient headers are attacker-controlled.
                    rcpt_toggle = match recipient_count(m) {
                        0 => String::new(),
                        // With the recipients line always visible, a
                        // single-recipient chip would only repeat it (#40).
                        1 if always_show_recipients => String::new(),
                        n => format!(
                            "<button type=\"button\" class=\"hylki-rcpt-toggle{open}\" \
                             title=\"{title}\">{label}</button>",
                            label = gtk::glib::markup_escape_text(&ni18n_f(
                                "{n} recipient",
                                "{n} recipients",
                                n as u32,
                                &[("n", &n.to_string())],
                            )),
                            open = if always_show_recipients { " open" } else { "" },
                            title = if always_show_recipients {
                                i18n("Hide recipients")
                            } else {
                                i18n("Show recipients")
                            },
                        ),
                    },
                    rcpt = recipients_html(m, always_show_recipients),
                    // Where this message was read from, when that isn't the
                    // folder on screen — the reply you sent, pulled in from Sent.
                    folder = match folder_labels.get(&(m.account_id, m.id)) {
                        Some(label) => format!(
                            "<span class=\"hylki-folder\">{}</span>",
                            escape_text(label)
                        ),
                        None => String::new(),
                    },
                    body = body,
                    // Unread messages in a conversation are marked; the mark
                    // clears when the user clicks the card to read it —
                    // scrolling past no longer counts as reading.
                    dot = if m.unread {
                        format!("<span class=\"hylki-dot\" data-key=\"{}:{}\"></span>", m.account_id, m.id)
                    } else {
                        String::new()
                    },
                ));
            } else {
                sections.push_str(&body);
            }
        }
        let scheme = if dark { "dark" } else { "light" };
        // The ⋯ actions toggle: plain white in dark mode — the inherited text
        // color reads too pale there.
        let toggle_color = if dark { "#ffffff" } else { "inherit" };
        // Paint the wrapper and the (still-loading) iframes in the theme color so
        // there's no white flash before each message's content renders. The live
        // theme's grounds when the reader set them (issue #62); the stock GNOME
        // values otherwise (tests).
        let (bg, deep, chrome) = LIVE_GROUNDS
            .with(|g| g.borrow().clone())
            .unwrap_or_else(|| {
                let (g, p, c) = if dark {
                    (GROUND.1, PAGE.1, CHROME.1)
                } else {
                    (GROUND.0, PAGE.0, CHROME.0)
                };
                (g.to_string(), p.to_string(), c.to_string())
            });
        // Each message card only reads as a card against a slightly deeper
        // ground than its own; a full-bleed single message sits on its own
        // ground — the chrome ground when it paints no background (see
        // `plain_css` below) — so the whole view is one color.
        let single_ground = if !carded
            && thread.len() == 1
            && (reader || !paints_own_background(&thread[0].body))
        {
            chrome.clone()
        } else {
            bg.clone()
        };
        let page = if carded { deep } else { single_ground };
        let body_class = if carded { " class=\"hylki-conv\"" } else { "" };
        // Defence in depth for the wrapper: the only script allowed to run is the
        // one carrying this render's nonce, which is ours. Anything a message
        // manages to smuggle into this document — an injected `<script>`, an
        // `onerror=` handler — is refused by the engine even if the escaping
        // above is ever wrong again.
        //
        // Deliberately *only* `script-src`/`object-src`/`base-uri`: no
        // `default-src`. A wrapper policy is inherited by the `srcdoc` frames, so
        // restricting images or styles here would silently re-block the remote
        // content the user has chosen to load. Each frame carries its own
        // `default-src 'none'` policy for that.
        let nonce = crate::rng::nonce(24).ok();
        let csp = match &nonce {
            Some(n) => format!(
                "<meta http-equiv=\"Content-Security-Policy\" content=\"\
                 script-src 'nonce-{n}'; object-src 'none'; base-uri 'none'\">"
            ),
            // No entropy, no nonce we can trust — allow no script at all. The
            // iframes render at their default height instead of resizing, which
            // is a visual degradation, not a security one.
            None => "<meta http-equiv=\"Content-Security-Policy\" content=\"\
                     script-src 'none'; object-src 'none'; base-uri 'none'\">"
                .to_string(),
        };
        let sizer = match &nonce {
            Some(n) => {
                // The fold's tooltips, in the user's language.
                let qt =
                    serde_json::to_string(&[i18n("Show quoted text"), i18n("Hide quoted text")])
                        .unwrap_or_default()
                        .replace("</", "<\\/");
                format!("<script nonce=\"{n}\">var vireoQT={qt};{SIZE_SCRIPT}</script>")
            }
            None => String::new(),
        };
        // A single message that paints no background of its own (plain mail,
        // and much styled mail too — Apple Mail styles fonts and wrapping but
        // sets no background) takes the chrome ground for its *whole* frame,
        // matching the GTK subject block and the header above it: one
        // unbroken pane from subject to the end of the message. A message
        // that declares a background keeps the plain ground so its design
        // renders as intended.
        let single_plain =
            !carded && thread.len() == 1 && (reader || !paints_own_background(&thread[0].body));
        let plain_css = if single_plain {
            format!(
                "body:not(.hylki-conv) .hylki-msg,\
                 body:not(.hylki-conv) iframe.hylki-frame{{background:{chrome};}}"
            )
        } else {
            String::new()
        };
        // The headers around the messages are the window's own chrome, so
        // they follow its direction; the messages inside their frames take
        // their own (#366).
        let dir = if crate::i18n::is_rtl() {
            " dir=\"rtl\""
        } else {
            ""
        };
        format!(
            "<!doctype html><html{dir}><head><meta charset=\"utf-8\">{csp}\
             <meta name=\"color-scheme\" content=\"{scheme}\">\
             <style>\
               :root{{color-scheme:{scheme};}}\
               body{{margin:0;padding:0;background:{page};font:14px/1.55 system-ui,sans-serif;}}\
               body.hylki-conv{{padding:14px;}}\
               iframe.hylki-frame{{width:100%;border:0;display:block;background:{bg};\
                 visibility:hidden;}}\
               iframe.hylki-frame.hylki-live{{visibility:visible;}}\
               .hylki-pan{{overflow-x:auto;position:relative;}}\
               iframe.hylki-frame.anim{{transition:height 240ms cubic-bezier(0.4,0,0.2,1);}}\
               @media (prefers-reduced-motion:reduce){{iframe.hylki-frame.anim{{transition:none;}}}}\
               .hylki-msg{{background:{bg};\
                 border-radius:12px;overflow:hidden;margin:0 0 14px;}}\
               .hylki-msg:last-child{{margin-bottom:0;}}\
               .hylki-msg{{user-select:none;}}\
               body:not(.hylki-conv) .hylki-msg{{border-radius:0;margin:0;}}\
               .hylki-msg.selected{{box-shadow:0 0 0 2px {accent};}}\
               .hylki-fbar{{display:none;}}\
               .hylki-msg.hylki-folded>.hylki-fbar{{display:flex;align-items:flex-start;gap:12px;\
                 padding:14px 18px;cursor:pointer;user-select:none;}}\
               .hylki-fbar-main{{flex:1 1 auto;min-width:0;display:flex;flex-direction:column;gap:2px;}}\
               .hylki-fbar-top{{display:flex;align-items:center;gap:12px;min-height:26px;}}\
               .hylki-fbar>.hylki-ava{{align-self:flex-start;}}\
               .hylki-fbar-prev{{display:-webkit-box;-webkit-box-orient:vertical;overflow:hidden;\
                 opacity:0.6;font-size:0.92em;line-height:1.35;overflow-wrap:anywhere;}}\
               .hylki-fbar-from{{flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;\
                 white-space:nowrap;opacity:0.85;}}\
               .hylki-msg.unread .hylki-fbar-from{{font-weight:700;opacity:1;}}\
               .hylki-fbar-ico{{flex:none;display:inline-flex;opacity:0.55;line-height:0;}}\
               .hylki-fbar-ico svg{{width:16px;height:16px;}}\
               .hylki-fbar-none{{width:16px;height:16px;}}\
               .hylki-fbar-ico svg,.hylki-fbar-ico svg *{{fill:currentColor;}}\
               .hylki-fbar-date{{flex:none;min-width:6.5em;text-align:end;opacity:0.7;\
                 font-size:0.92em;white-space:nowrap;}}\
               .hylki-msg.hylki-folded>:not(.hylki-fbar){{display:none;}}\
               body.hylki-folds .hylki-msg-hdr{{cursor:pointer;}}\
               @media print{{.hylki-msg.hylki-folded>:not(.hylki-fbar){{display:revert !important;}}\
                 .hylki-msg.hylki-folded>.hylki-fbar{{display:none !important;}}}}\
               .hylki-msg-hdr{{cursor:pointer;}}\
               .hylki-msg-hdr{{padding:12px 16px;cursor:default;user-select:none;\
                 position:sticky;top:0;z-index:1;background-color:{bg};}}\
               /* 8px between items on a line, but only 2px between wrapped\
                  lines — the flex gap otherwise pads rows apart too. */\
               .hylki-hdr-line{{display:flex;column-gap:8px;row-gap:2px;\
                 align-items:baseline;flex-wrap:nowrap;\
                 min-width:0;position:relative;}}\
               .hylki-hdr-meta{{display:flex;gap:8px;align-items:baseline;flex:none;\
                 margin-inline-start:auto;}}\
               /* Narrow pane: the meta group (folder chip, recipients chip, date)\
                  drops to its own line as one unit — no mid-text wrapping — while\
                  the sender and the action palette hold the first line. */\
               @media (max-width:620px){{\
                 /* Tighten the right-hand insets (card gutter + header padding)\
                    so the palette toggle sits nearer the pane's edge — but the\
                    combined inset never drops below 20px. */\
                 body.hylki-conv{{padding:10px;}}\
                 body.hylki-conv .hylki-msg-hdr{{padding:12px 10px;}}\
                 .hylki-hdr-line{{flex-wrap:wrap;}}\
                 .hylki-hdr-meta{{order:10;flex-basis:100%;margin-inline-start:34px;\
                   justify-content:flex-start;}}\
                 /* Below the wrapped meta line the recipients need a touch\
                    more air than the wide layout's 2px. */\
                 .hylki-rcpt{{margin-top:6px;}}\
                 /* With the meta group gone from the first line, the palette\
                    pins itself to the corner (the \u{22ef} is already absolute). */\
                 body:not([data-hylki-acts=\"toggle\"]) .hylki-acts{{margin-inline-start:auto;}}\
               }}\
               body:not(.hylki-conv) .hylki-msg-hdr{{background-color:{chrome};}}\
               {plain_css}\
               .hylki-ava{{width:26px;height:26px;border-radius:50%;flex:none;align-self:center;\
                 display:flex;align-items:center;justify-content:center;color:#fff;\
                 font-size:0.8em;font-weight:700;}}\
               /* Zero flex-basis: the name and address only absorb free space\
                  (up to their natural width), so their length can never force\
                  the palette onto another line. Under pressure the address\
                  gives way first (tiny grow share), fading out at its right\
                  edge; the name ellipsizes only after that. */\
               .hylki-from{{font-weight:700;flex:99 1 0;max-width:max-content;min-width:0;\
                 white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}}\
               .hylki-tags{{display:inline-flex;gap:4px;align-items:center;flex:0 0 auto;}}\
               .hylki-tags:empty{{display:none;}}\
               /* A card's own attachments (#213): chips beneath the body. */\
               .hylki-atts{{display:flex;flex-wrap:wrap;gap:6px;padding:2px 14px 12px;}}\
               .hylki-atts:empty{{display:none;}}\
               /* The Unsubscribe banner between a list message's header and\
                  body: a line of text and one button. */\
               .hylki-tr{{display:flex;align-items:center;gap:10px;padding:8px 14px 9px;\
                 font-size:0.9em;border-bottom:1px solid rgba(128,128,128,0.25);}}\
               .hylki-tr:empty{{display:none;}}\
               .hylki-tr-text{{flex:1 1 auto;min-width:0;opacity:0.8;}}\
               .hylki-tr-btn{{flex:none;border:none;border-radius:6px;\
                 padding:3px 12px;background:color-mix(in srgb,currentColor 10%,transparent);\
                 color:inherit;font:inherit;font-size:0.95em;font-weight:600;cursor:pointer;}}\
               .hylki-tr-btn:hover{{background:color-mix(in srgb,currentColor 15%,transparent);}}\
               .hylki-tr-btn:active{{background:color-mix(in srgb,currentColor 30%,transparent);}}\
               .hylki-unsub{{display:flex;align-items:center;gap:10px;padding:8px 14px 9px;\
                 font-size:0.9em;border-bottom:1px solid rgba(128,128,128,0.25);}}\
               .hylki-unsub:empty{{display:none;}}\
               .hylki-unsub-text{{flex:1 1 auto;min-width:0;opacity:0.8;}}\
               .hylki-unsub-btn{{flex:none;border:none;border-radius:6px;\
                 padding:3px 12px;background:color-mix(in srgb,currentColor 10%,transparent);\
                 color:inherit;font:inherit;font-size:0.95em;font-weight:600;cursor:pointer;}}\
               .hylki-unsub-btn:hover{{background:color-mix(in srgb,currentColor 15%,transparent);}}\
               .hylki-unsub-btn:active{{background:color-mix(in srgb,currentColor 30%,transparent);}}\
               /* The invitation banner (#223): the meeting at the top of\
                  the card, and the answer buttons under it. */\
               .hylki-inv{{display:flex;gap:10px;padding:10px 14px 11px;font-size:0.9em;\
                 border-bottom:1px solid rgba(128,128,128,0.25);}}\
               .hylki-inv:empty{{display:none;}}\
               .hylki-inv-icon{{flex:none;padding-top:1px;opacity:0.7;}}\
               .hylki-inv-icon svg{{width:20px;height:20px;display:block;}}\
               .hylki-inv-icon svg,.hylki-inv-icon svg *{{fill:currentColor;}}\
               .hylki-inv-body{{flex:1 1 auto;min-width:0;}}\
               .hylki-inv-head{{display:flex;align-items:baseline;gap:8px;flex-wrap:wrap;}}\
               .hylki-inv-title{{font-weight:700;font-size:1.05em;}}\
               .hylki-inv-title.off{{text-decoration:line-through;opacity:0.7;}}\
               .hylki-inv-kind{{font-size:0.85em;text-transform:uppercase;\
                 letter-spacing:0.04em;opacity:0.6;}}\
               .hylki-inv-row{{opacity:0.8;margin-top:2px;}}\
               .hylki-inv-note{{margin-top:4px;opacity:0.8;font-style:italic;}}\
               .hylki-inv-acts{{display:flex;flex-wrap:wrap;gap:6px;margin-top:8px;}}\
               .hylki-inv-btn{{border:1px solid rgba(128,128,128,0.4);border-radius:6px;\
                 padding:3px 12px;background:rgba(128,128,128,0.1);color:inherit;font:inherit;\
                 font-size:0.95em;font-weight:600;cursor:pointer;}}\
               .hylki-inv-btn:hover{{background:rgba(128,128,128,0.22);}}\
               /* The answer already given is marked, so a meeting opened\
                  again shows where it stands at a glance. */\
               .hylki-inv-btn.on{{border-color:{accent};color:{accent};\
                 background:color-mix(in srgb, {accent} 16%, transparent);}}\
               .hylki-attw{{display:inline-flex;align-items:stretch;max-width:100%;\
                 border:1px solid rgba(128,128,128,0.35);border-radius:8px;overflow:hidden;\
                 background:rgba(128,128,128,0.08);}}\
               .hylki-attc{{display:inline-flex;align-items:center;gap:6px;padding:4px 8px;\
                 border:none;background:none;color:inherit;font:inherit;font-size:0.85em;\
                 cursor:pointer;min-width:0;max-width:320px;}}\
               .hylki-attc:hover{{background:rgba(128,128,128,0.18);}}\
               .hylki-attc svg{{width:16px;height:16px;flex:none;display:block;}}\
               .hylki-attc svg,.hylki-attc svg *{{fill:currentColor;}}\
               .hylki-attc.ftype-pdf svg{{color:#e01b24;}}\
               .hylki-attc.ftype-doc svg{{color:#3584e4;}}\
               .hylki-attc.ftype-sheet svg{{color:#2ec27e;}}\
               .hylki-attc.ftype-slides svg{{color:#ff7800;}}\
               .hylki-attc.ftype-archive svg{{color:#e5a50a;}}\
               .hylki-attc.ftype-audio svg{{color:#9141ac;}}\
               .hylki-attc.ftype-video svg{{color:#e6689e;}}\
               .hylki-attc.ftype-calendar svg{{color:#009e8f;}}\
               .hylki-attc.ftype-image svg{{color:#1c9bd8;}}\
               .hylki-attc.ftype-generic svg{{color:#8c8c8c;}}\
               .hylki-attn{{white-space:nowrap;overflow:hidden;text-overflow:ellipsis;min-width:0;}}\
               .hylki-attz{{opacity:0.55;white-space:nowrap;flex:none;}}\
               .hylki-attsave{{display:inline-flex;align-items:center;padding:0 7px;border:none;\
                 border-inline-start:1px solid rgba(128,128,128,0.35);background:none;color:inherit;\
                 cursor:pointer;opacity:0.6;}}\
               .hylki-attsave:hover{{opacity:1;background:rgba(128,128,128,0.18);}}\
               .hylki-attsave svg{{width:14px;height:14px;display:block;}}\
               .hylki-attsave svg,.hylki-attsave svg *{{fill:currentColor;}}\
               .hylki-atts.flash .hylki-attw{{animation:hylki-att-flash 1.2s ease-out;}}\
               @keyframes hylki-att-flash{{0%,40%{{box-shadow:0 0 0 3px rgba(53,132,228,0.7);}}\
                 100%{{box-shadow:0 0 0 3px rgba(53,132,228,0);}}}}\
               .hylki-tag{{font-size:0.72em;font-weight:700;line-height:1.4;padding:1px 7px;\
                 border-radius:9999px;white-space:nowrap;}}\
               .hylki-label{{max-width:16em;\
                 overflow:hidden;text-overflow:ellipsis;}}\
               .hylki-addr{{opacity:0.55;font-size:0.9em;flex:1 1 0;max-width:max-content;\
                 min-width:0;white-space:nowrap;overflow:hidden;text-overflow:clip;\
                 position:relative;}}\
               /* The fade exists only while something actually collides with\
                  the address (`clipped` is set by measurement, below). */\
               .hylki-addr.clipped{{\
                 -webkit-mask-image:linear-gradient(to right,#000 calc(100% - 18px),transparent);\
                 mask-image:linear-gradient(to right,#000 calc(100% - 18px),transparent);}}\
               /* Hovering a clipped address reveals it in full: an overlay\
                  pinned to the span's spot, floating over whatever crowded it\
                  out, gone when the cursor leaves. */\
               .hylki-addr.clipped:hover{{-webkit-mask-image:none;mask-image:none;\
                 z-index:6;}}\
               /* Addresses act as links: click composes to them, right-click\
                  offers Copy / New Message. */\
               /* Sender-authentication seal (#88): invisible until a\
                  verdict arrives; a pass wears Bazaar's fixed blue (never the\
                  system accent), problems the warning/error reds.\
                  Sized in em and anchored to the text baseline, then eased\
                  down to the name's optical centre with an em transform\
                  (margins can't move it: WebKit synthesizes the missing\
                  baseline before they apply) — a fixed centred pixel box sat\
                  visibly low whenever a GNOME text scaling factor shrank the\
                  type around it. */\
               .hylki-verify{{display:none;background:none;border:none;\
                 padding:0 2px;margin-inline-start:2px;cursor:pointer;line-height:0;\
                 align-self:baseline;transform:translateY(0.18em);flex:none;}}\
               .hylki-verify.on{{display:inline-flex;}}\
               .hylki-verify svg{{width:0.95em;height:0.95em;display:block;}}\
               .hylki-verify svg,.hylki-verify svg *{{fill:currentColor;}}\
               .hylki-verify.trust-pass{{color:#3584e4;}}\
               .hylki-verify.trust-unverified{{color:currentColor;opacity:0.4;}}\
               .hylki-verify.trust-suspicious{{color:#cd9309;}}\
               .hylki-verify.trust-fail{{color:#c01c28;}}\
               /* The OpenPGP chip (#133), a labelled pill since #300: the\
                  verdict reads in words on a wash of its color. Text of its\
                  own, so it sits on the name's baseline without the seal's\
                  nudge. */\
               .hylki-pgp{{display:none;font:inherit;font-size:0.78em;font-weight:600;\
                 border:none;border-radius:999px;padding-block:0.1em;padding-inline:0.45em 0.6em;\
                 margin-inline-start:4px;cursor:pointer;align-items:center;gap:0.3em;\
                 align-self:baseline;white-space:nowrap;flex:none;color:inherit;\
                 background:rgba(128,128,128,0.16);}}\
               .hylki-pgp.on{{display:inline-flex;}}\
               .hylki-pgp svg{{width:1.1em;height:1.1em;display:none;flex:none;}}\
               .hylki-pgp svg,.hylki-pgp svg *{{fill:currentColor;}}\
               .hylki-pgp.enc svg:first-child{{display:block;}}\
               .hylki-pgp.sig svg:nth-child(2){{display:block;}}\
               .hylki-pgp.pgp-good{{color:#26a269;background:rgba(38,162,105,0.16);}}\
               .hylki-pgp.pgp-warn{{color:#cd9309;background:rgba(205,147,9,0.16);}}\
               .hylki-pgp.pgp-bad{{color:#c01c28;background:rgba(192,28,40,0.14);}}\
               .hylki-pgp:hover{{filter:brightness(1.1);}}\
               /* The sensitivity label (#411): the OpenPGP pill's shape in a\
                  neutral wash, since a label is a statement, not a verdict.\
                  Zero flex-basis and a small share, like the address: the\
                  name fills first and a long label ellipsizes before it. */\
               .hylki-sens{{display:none;font-size:0.78em;font-weight:600;\
                 border-radius:999px;padding-block:0.1em;padding-inline:0.45em 0.6em;\
                 margin-inline-start:4px;align-items:center;gap:0.3em;\
                 align-self:baseline;white-space:nowrap;flex:1 1 0;min-width:3.5em;max-width:max-content;\
                 background:rgba(128,128,128,0.16);cursor:default;}}\
               .hylki-sens.on{{display:inline-flex;}}\
               .hylki-sens svg{{width:1em;height:1em;flex:none;}}\
               .hylki-sens svg,.hylki-sens svg *{{fill:var(--hylki-sens,currentColor);}}\
               .hylki-sens-text{{overflow:hidden;text-overflow:ellipsis;}}\
               /* The icons alone (Settings, #300): the chip as it was before\
                  it had words, sitting on the name's line like the seal. */\
               body[data-hylki-pgpicons] .hylki-pgp{{background:none;padding:0 2px;\
                 margin-inline-start:2px;gap:1px;font-size:inherit;line-height:0;\
                 transform:translateY(0.18em);}}\
               body[data-hylki-pgpicons] .hylki-pgp svg{{width:0.95em;height:0.95em;}}\
               body[data-hylki-pgpicons] .hylki-pgp-text{{display:none;}}\
               .hylki-mail{{cursor:pointer;}}\
               .hylki-mail:hover{{text-decoration:underline;}}\
               /* Styled after the app's own context menus (context_menu.rs +\
                  .context-menu-list in styles.css): same padding, min width,\
                  row shape and weight, so both menu families read as one. */\
               .hylki-mailmenu-scrim{{position:fixed;left:0;top:0;right:0;bottom:0;\
                 z-index:98;}}\
               .hylki-mailmenu{{position:fixed;z-index:99;background:{bg};\
                 border:1px solid rgba(128,128,128,0.2);border-radius:12px;\
                 box-shadow:0 6px 24px rgba(0,0,0,0.22);padding:6px;\
                 min-width:190px;font-size:0.95em;}}\
               .hylki-mailmenu button{{display:block;width:100%;text-align:start;\
                 background:none;border:none;padding:8px 10px;border-radius:6px;\
                 color:inherit;cursor:pointer;font:inherit;font-weight:normal;}}\
               .hylki-mailmenu button:hover{{background:rgba(128,128,128,0.14);}}\
               .hylki-addr.clipped:hover::after{{content:attr(data-addr);\
                 position:absolute;inset-inline-start:-6px;top:50%;transform:translateY(-50%);\
                 background:{bg};border-radius:6px;padding:1px 6px;\
                 white-space:nowrap;text-decoration:underline;}}\
               body:not(.hylki-conv) .hylki-addr.clipped:hover::after{{\
                 background:{chrome};}}\
               .hylki-date{{opacity:0.55;font-size:0.85em;flex:none;white-space:nowrap;}}\
               .hylki-dot{{width:8px;height:8px;border-radius:50%;background:#3584e4;\
                 flex:none;align-self:center;}}\
               .hylki-end{{height:1px;}}\
               .hylki-quote{{display:block;margin:0 16px 12px;padding:0 7px;\
                 font:inherit;font-size:0.7em;line-height:1.45;letter-spacing:0.06em;\
                 color:inherit;opacity:0.6;background:rgba(128,128,128,0.16);\
                 border:0;border-radius:999px;cursor:pointer;}}\
               .hylki-quote:hover{{opacity:0.95;background:rgba(128,128,128,0.28);}}\
               .hylki-quote.open{{opacity:0.95;position:absolute;inset-inline-start:0;z-index:2;}}\
               .hylki-acts{{display:flex;gap:2px;flex:none;align-self:center;margin-inline-start:4px;}}\
               /* Read-toggle: the icon showing is the ACTION (read envelope\
                  means mark-as-read); the section's unread class decides. */\
               .hylki-act .tr-when-unread,.hylki-act .tr-when-read{{display:none;line-height:0;}}\
               .hylki-msg.unread .hylki-act[data-act=\"toggleread\"] .tr-when-unread{{display:inline-flex;}}\
               .hylki-msg:not(.unread) .hylki-act[data-act=\"toggleread\"] .tr-when-read{{display:inline-flex;}}\
               .hylki-act{{color:inherit;background:none;border:none;border-radius:6px;\
                 padding:5px 8px;cursor:pointer;opacity:0.7;\
                 display:inline-flex;align-items:center;justify-content:center;\
                 vertical-align:middle;line-height:0;\
                 transition:opacity 120ms ease,background 120ms ease;}}\
               .hylki-act:hover{{opacity:1;background:rgba(128,128,128,0.18);}}\
               .hylki-act:active{{background:rgba(128,128,128,0.3);}}\
               .hylki-act svg{{width:14px;height:14px;display:block;}}\
               .hylki-act svg,.hylki-act svg *{{fill:currentColor;}}\
               /* A set star: the one state that carries color. */\
               .hylki-act.on{{color:#e5a50a;opacity:1;}}\
               .hylki-acts-toggle{{display:none;}}\
               /* Behind-the-\u{22ef} mode: the palette overlays the header line as an\
                  absolute strip anchored at the toggle, expanding leftward over\
                  the text beneath (never pushing it), its left edge fading in\
                  from transparent. The \u{22ef} itself never moves. */\
               body[data-hylki-acts=\"toggle\"] .hylki-acts{{position:absolute;\
                 inset-inline-end:34px;top:13px;transform:translateY(-50%) scaleX(0.6);\
                 transform-origin:right center;margin:0;\
                 padding-block:2px;padding-inline:30px 6px;border-radius:8px;\
                 background:linear-gradient(to right,transparent,{bg} 26px);\
                 opacity:0;pointer-events:none;\
                 transition:opacity 140ms ease,transform 180ms ease;}}\
               body[data-hylki-acts=\"toggle\"] .hylki-acts.open{{opacity:1;\
                 pointer-events:auto;transform:translateY(-50%) scaleX(1);}}\
               body:not(.hylki-conv)[data-hylki-acts=\"toggle\"] .hylki-acts{{\
                 background:linear-gradient(to right,transparent,{chrome} 26px);}}\
               /* Always visible (not just on card hover), and OUT OF FLOW:\
                  absolutely pinned to the header's right edge, so no amount of\
                  header content can push or wrap it. The line reserves the\
                  button's footprint as fixed right padding instead. */\
               body[data-hylki-acts=\"toggle\"] .hylki-hdr-line{{padding-inline-end:38px;}}\
               /* Anchored to the FIRST row's centre (the 26px avatar/sender\
                  line), not the line box's — when the meta row wraps beneath,\
                  the box grows downward but the button must not move. */\
               body[data-hylki-acts=\"toggle\"] .hylki-acts-toggle{{display:block;\
                 position:absolute;inset-inline-end:0;top:13px;transform:translateY(-50%);\
                 color:{toggle_color};background:none;border:none;border-radius:6px;\
                 padding:4px 8px;margin:0;cursor:pointer;\
                 opacity:0.65;transition:opacity 120ms ease,background 120ms ease;}}\
               /* `svg *` too: the embedded symbolic paths carry their own fill\
                  attribute, which a rule on the svg element alone can't beat. */\
               .hylki-acts-toggle svg{{width:14px;height:14px;display:block;}}\
               .hylki-acts-toggle svg,.hylki-acts-toggle svg *{{fill:currentColor;}}\
               body[data-hylki-acts=\"toggle\"] .hylki-acts-toggle:hover{{opacity:1;\
                 background:rgba(128,128,128,0.18);}}\
               body[data-hylki-acts=\"toggle\"] .hylki-acts-toggle.open{{opacity:1;}}\
               body[data-hylki-acts=\"hover\"] .hylki-acts{{opacity:0;pointer-events:none;\
                 transition:opacity 300ms ease var(--acts-delay,0ms);}}\
               body[data-hylki-acts=\"hover\"] .hylki-msg:hover .hylki-acts{{opacity:1;\
                 pointer-events:auto;transition:opacity 300ms ease 0ms;}}\
               .hylki-folder{{padding:0.05em 0.45em;border-radius:0.7em;\
                 font-size:0.78em;opacity:0.75;border:1px solid currentColor;\
                 white-space:nowrap;flex:none;}}\
               .hylki-rcpt-toggle{{font:inherit;font-size:0.78em;color:inherit;\
                 background:color-mix(in srgb,currentColor 10%,transparent);\
                 border:none;border-radius:6px;padding:0.15em 0.6em;cursor:pointer;\
                 white-space:nowrap;flex:none;transition:background 120ms ease;}}\
               .hylki-rcpt-toggle:hover{{background:color-mix(in srgb,currentColor 15%,transparent);}}\
               .hylki-rcpt-toggle.open{{background:color-mix(in srgb,currentColor 20%,transparent);}}\
               /* Indented past the avatar (26px + 8px gap), so the recipients\
                  align with the sender's name — as does the wrapped meta line. */\
               .hylki-rcpt{{font-size:0.85em;opacity:0.75;\
                 user-select:text;overflow-wrap:anywhere;margin:2px 0 0;margin-inline-start:34px;}}\
               .hylki-rcpt div{{margin-top:2px;}}\
               .hylki-loading{{opacity:0.5;padding:16px;}}\
               /* The Copied pill after Ctrl+C: a toast the page draws itself,\
                  since the reader has no toast overlay of its own. */\
               .hylki-copied{{position:fixed;left:50%;bottom:22px;transform:translateX(-50%) translateY(8px);\
                 padding:8px 18px;border-radius:999px;background:rgba(40,40,40,0.92);color:#fff;\
                 font:inherit;font-size:0.9em;box-shadow:0 2px 10px rgba(0,0,0,0.25);\
                 opacity:0;pointer-events:none;transition:opacity 180ms ease,transform 180ms ease;z-index:50;}}\
               .hylki-copied.on{{opacity:1;transform:translateX(-50%) translateY(0);}}\
               /* Right to left (#366): what logical properties cannot say. */\
               html[dir=rtl] .hylki-addr.clipped{{\
                 -webkit-mask-image:linear-gradient(to left,#000 calc(100% - 18px),transparent);\
                 mask-image:linear-gradient(to left,#000 calc(100% - 18px),transparent);}}\
               html[dir=rtl] .hylki-addr.clipped:hover{{-webkit-mask-image:none;mask-image:none;}}\
               html[dir=rtl] body[data-hylki-acts=\"toggle\"] .hylki-acts{{\
                 transform-origin:left center;\
                 background:linear-gradient(to left,transparent,{bg} 26px);}}\
               html[dir=rtl] body:not(.hylki-conv)[data-hylki-acts=\"toggle\"] .hylki-acts{{\
                 background:linear-gradient(to left,transparent,{chrome} 26px);}}\
               /* Each chip's words in their own direction, as a GTK label\
                  has them: an English 1 recipient in a right-to-left line\
                  otherwise reads recipient 1. */\
               html[dir=rtl] :is(.hylki-rcpt-toggle,.hylki-date,.hylki-fbar-date,\
                 .hylki-folder,.hylki-rcpt div){{unicode-bidi:plaintext;}}\
               html[dir=rtl] .hylki-fbar-date{{text-align:left;}}\
             </style>{sizer}\
             </head><body{body_class}>{sections}</body></html>"
        )
    }

    /// The reader's two grounds for `dark` (see [`theme_grounds_for`]).
    fn theme_grounds(&self, dark: bool) -> (String, String, String) {
        theme_grounds_for(&self.webview, dark)
    }

    /// Paint the WebView canvas in the theme color so unstyled bodies (and the
    /// gap before a load) match light/dark mode instead of flashing white.
    fn apply_webview_bg(&self, dark: bool) {
        // Whatever is about to be shown: a conversation's cards sit on the
        // deeper page ground, a full-bleed single message on the plain ground
        // — or the chrome ground when it paints no background of its own.
        // The cover matches it so the spinner gives way to the document
        // without a change of color.
        let (ground, page, chrome) = self.theme_grounds(dark);
        let (bg, kind) =
            if self.thread.len() > 1 || (!self.thread.is_empty() && self.single_message_card) {
                (&page, "page")
            } else if self.thread.len() == 1
                && (self.reader_mode || !paints_own_background(&self.thread[0].body))
            {
                (&chrome, "chrome")
            } else {
                (&ground, "ground")
            };
        self.webview.set_background_color(&ground_rgba(bg));
        // The spinner and the cover stand in for the message, so they answer to
        // the message's theme: #1e1e1e matches the document's own dark ground,
        // white its light one. The label and spinner take a dimmed foreground
        // from the same side, so neither disappears into the ground.
        let (side, fg) = if dark {
            ("dark", "rgba(255,255,255,0.55)")
        } else {
            ("light", "rgba(0,0,0,0.45)")
        };
        // All three grounds at once, so moving between kinds of message
        // never loads the rules again.
        let cover = format!(".reader-cover.cover-side-{side}");
        let mut rules: Vec<(String, String)> =
            [("page", &page), ("chrome", &chrome), ("ground", &ground)]
                .iter()
                .map(|(k, c)| {
                    (
                        format!("{cover}.cover-ground-{k}"),
                        format!("background-color: {c};"),
                    )
                })
                .collect();
        rules.push((
            format!("{cover} .reader-loading label, {cover} .reader-loading spinner"),
            format!("color: {fg};"),
        ));
        self.cover_rules.set(rules);
        if let Some(cover) = &self.cover {
            wear_class(cover, "cover-side-", &format!("cover-side-{side}"));
            wear_class(cover, "cover-ground-", &format!("cover-ground-{kind}"));
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
