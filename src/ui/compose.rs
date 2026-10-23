//! Compose window: write a new message, reply, or forward.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{ComposeFormat, SignaturePosition};
use crate::contacts::Suggestion;
use crate::i18n::{i18n, i18n_f};
use crate::models::DraftOrigin;
use crate::ui::drop_zones::{DropChoice, DropContext, DropZones};
use crate::ui::recipient_row::RecipientRow;
use crate::ui::rich_editor::{self, RichEditor, SourceKind};

mod attachments;
mod cloud;
mod format;
mod history;
mod host;
mod keys;
mod messages;
mod send;
mod signature;
mod suggest;
#[cfg(test)]
mod tests;
mod translate;
mod types;

use attachments::AttachThumbs;
use cloud::not_signed_in;
use format::{format_icon, format_label, markdown_body, SHOWCASE_PLAIN_LOG, ZOOM};
use host::{
    header_rows_width, set_toggle_icon, size_for_host, COMPOSE_ACTIONS_BREAKPOINT,
    COMPOSE_MIN_WIDTH,
};
pub(crate) use keys::composer_key;
use keys::{focus_is_entry, zoom_key};
use messages::OutgoingFields;
pub use messages::{ComposeInput, ComposeOutput};
use send::{identity_display, next_monday, pick_send_time, preset_time, same_from, AUTOSAVE_SECS};
use signature::sig_html;
use types::{CloudLink, ComposeUndoEntry};
pub use types::{ComposeAccount, ComposeInit, ComposePrefill, Field, OwnMail, OwnOnly};

pub struct Compose {
    accounts: Vec<ComposeAccount>,
    /// The rich-text (HTML) body editor.
    editor: RichEditor,
    /// Signature currently appended to the body (so it can be swapped out).
    current_sig: String,
    /// Whether signatures go in under a `-- ` line (Settings, read when the
    /// composer opens).
    sig_dashes: bool,
    /// Where that signature sits against a quoted original (#237), so a
    /// signature added on an account switch lands in the same place.
    signature_position: SignaturePosition,
    /// Files to attach.
    attachments: Vec<std::path::PathBuf>,
    /// Thumbnails of the attached pictures and PDFs by path (#299), kept
    /// so a chip rebuild does not read and decode the file again. `None`
    /// is a file that would not decode.
    attach_thumbs: AttachThumbs,
    /// The surfaces shown while files are dragged over the composer, set up
    /// once the view exists.
    drop_zones: Option<std::rc::Rc<DropZones>>,
    /// When editing a queued Outbox message, the row this replaces once sent.
    outbox_origin: Option<u32>,
    /// Recipient suggestions, filtered as the user types.
    suggestions: Vec<Suggestion>,
    /// The Microsoft 365 accounts whose organization directory is searched
    /// too (#410), found once the composer opens. The LDAP directories are
    /// not: one switched on in Evolution while the composer is open counts
    /// as well (#307), and [`crate::directory::search`] keeps the list.
    graph_directories: std::sync::Arc<Vec<crate::config::AccountConfig>>,
    /// The accounts that suggest only from their directory and their own
    /// mail (#410), with the addresses in that mail.
    own_mail: OwnMail,
    /// The account the From row has picked, which decides whether
    /// `own_mail` narrows the suggestions.
    from_account: u32,
    /// What the directories answered for the recipient being typed.
    directory_matches: Vec<Suggestion>,
    /// Bumped on every keystroke, so a slow directory's answer to an
    /// earlier fragment is dropped.
    directory_generation: u64,
    /// The directory failures this composer has put in the status bar. A
    /// directory that keeps failing says so once here, not at every pause
    /// in the typing, and still once in the next composer (#307).
    directory_errors_shown: std::collections::HashSet<String>,
    /// Shared autocomplete popover and which field it's currently attached to.
    completion: gtk::Popover,
    completion_field: Option<Field>,
    /// The list inside the popover + the keyboard-highlighted row, for arrow-key nav.
    completion_list: Option<gtk::ListBox>,
    completion_selected: usize,
    completion_count: usize,
    /// Whether the popover is showing (read synchronously by the key handler).
    completion_open: std::rc::Rc<std::cell::Cell<bool>>,
    /// Threading headers carried from the message being replied to.
    in_reply_to: String,
    references: String,
    forwards: String,
    /// When editing an existing draft, its origin (replaced on save/send).
    draft_origin: Option<DraftOrigin>,
    /// Editing a template (#360), kept at `draft_origin`: Save puts it back,
    /// and Send leaves it where it is.
    template: bool,
    /// Stable id the app uses to track this composer across host moves.
    compose_id: u32,
    /// Currently shown as a standalone window (drives the toggle-button icon).
    windowed: bool,
    /// Whether this composer offers the inline/window toggle at all.
    can_toggle: bool,
    compact: bool,
    /// Whether the inline header carries the window decorations (see
    /// `ComposeInit::decorations`).
    decorations: bool,
    /// A compact reply's field rows, revealed by the header button (#154)
    /// or from the start by the preference.
    fields_shown: bool,
    /// The From is typed, in `custom_from_row`, not picked (#347).
    custom_from: bool,
    /// The pane is too narrow for the full toolbar: everything but Cancel,
    /// Send and the fields chevron folds into the ⋯ menu (like the reader's
    /// header), so the window controls at the end never leave the canvas.
    narrow: bool,
    /// A recipient/subject field was edited since open (body edits are tracked
    /// separately by the editor itself). Used for save-if-dirty.
    fields_dirty: bool,
    /// The save-or-discard question is on screen (or about to be), so a
    /// second Escape in the moment before it appears asks nothing twice.
    asking_discard: bool,
    /// OpenPGP (#133): sign the message; encrypt it to every recipient.
    sign: bool,
    encrypt: bool,
    /// The OpenPGP send check is running on a worker thread.
    pgp_checking: bool,
    /// What the last passing OpenPGP check covered (From, key, encrypt,
    /// recipients); a Send with the same passes straight on.
    pgp_cleared: Option<Vec<String>>,
    /// Sign was set by hand, so it no longer follows the From account's
    /// default (#267).
    sign_touched: bool,
    /// The state the composer itself last put the Sign toggle in, until its
    /// `toggled` arrives: that one is not the user's.
    sign_expected: Option<bool>,
    /// What this message is written in. Plain text hides the
    /// formatting toolbar and sends no HTML part (#180); Markdown and HTML
    /// are written as source and converted on the way out.
    format: ComposeFormat,
    /// Whether the body zooms (Ctrl+ / Ctrl- / Ctrl+0, Ctrl + the scroll
    /// wheel): every format but rich text. Shared with the key and scroll
    /// handlers, which have no model to ask.
    zoomable: std::rc::Rc<std::cell::Cell<bool>>,
    /// The format chooser and preview toggle, which live at the end of the
    /// editor's formatting row rather than in the header.
    format_btn: gtk::Button,
    /// Translate what is being written (#327), beside the format chooser;
    /// shown only with a translation service set up.
    translate_btn: gtk::Button,
    /// Insert Signature (#385), beside Translate: Edit as New leaves the
    /// signature out, and this puts it back by hand. With several (#445)
    /// it lists them. Shown only while the From account has one.
    signature_btn: gtk::Button,
    /// A translation is out: the button waits and the body is locked.
    translating: bool,
    /// Saving on its own (#340): a save on its way, what the last one held
    /// (a hash of the fields and body), whether the Drafts copy is one only
    /// these saves made (so Discard removes it), and whether they have
    /// stopped (the copy could not be found again to replace it).
    autosaving: bool,
    autosaved: Option<u64>,
    autosave_pending: Option<u64>,
    autosave_made: bool,
    autosave_off: bool,
    /// The subject as written and as translated with the body, so Show
    /// Original and Show Translation can swap it back and forth (#327).
    tr_subject: Option<(String, String)>,
    preview_btn: gtk::ToggleButton,
    /// The preview toggle's icon-and-label insides, kept only so the label
    /// can be dropped in a pane too narrow to carry it.
    preview_content: adw::ButtonContent,
    /// Send Later (#145): when set, Send queues the message for this time.
    send_at: Option<i64>,
    /// Cloud attachments (#144): the accounts files can be uploaded to, the
    /// links already placed in the body, and how many uploads are running.
    cloud_accounts: Vec<crate::cloud::CloudAccount>,
    cloud_links: Vec<CloudLink>,
    cloud_busy: u32,
    /// Download passwords made for this message's links, to pass on
    /// separately: (file name, password).
    cloud_passwords: Vec<(String, String)>,
    /// The composer's own undo history (#200): the body's typing and
    /// formatting, and the attachments alongside it. Ctrl+Z takes the top
    /// one, applies it, and puts what reverses it onto `redo_stack` — the
    /// same shape as the message list's history. See
    /// [`Compose::push_history`] for the one place the two kinds of change
    /// are ordered against each other.
    undo_stack: Vec<ComposeUndoEntry>,
    redo_stack: Vec<ComposeUndoEntry>,
}

#[relm4::component(pub)]
impl Component for Compose {
    type Init = ComposeInit;
    type Input = ComposeInput;
    type Output = ComposeOutput;
    type CommandOutput = ();

    view! {
        // Host-agnostic root: the same pane is shown inline (in a reader Revealer)
        // or set as the content of an app-owned window. Hosting/close is the app's
        // job (see ComposeOutput::ToggleWindow / Close).
        adw::BreakpointBin {
            // The floor the pane can shrink to (inline in a narrow reader,
            // or a small window): below the measured full-toolbar width the
            // breakpoint set in `init` folds the actions into the ⋯ menu.
            set_size_request: (COMPOSE_MIN_WIDTH, 200),

            // The overlay carries the drop surfaces shown while files are
            // dragged over the composer (drop_zones.rs).
            #[wrap(Some)]
            #[name = "drop_overlay"]
            set_child = &gtk::Overlay {
                #[wrap(Some)]
                #[name = "toolbar_root"]
                set_child = &adw::ToolbarView {
                    #[name = "header"]
                    add_top_bar = &adw::HeaderBar {
                        add_css_class: "compose-toolbar",
                        set_show_start_title_buttons: false,
                        set_show_end_title_buttons: false,
                        // No "Hylki" branding on the compose bar.
                        #[wrap(Some)]
                        set_title_widget = &gtk::Label {
                            set_label: "",
                        },

                        pack_start = &gtk::Button {
                            set_label: &i18n("Cancel"),
                            connect_clicked => ComposeInput::Cancel,
                        },
                        // Save Draft stays, label and all, however narrow the
                        // pane: it is the one action worth a click in a hurry.
                        // Save as Template (#360) beside it, as Send Later
                        // is beside Send; a template being edited has the
                        // one way to be saved.
                        pack_start = &gtk::Box {
                            add_css_class: "linked",
                            gtk::Button {
                                set_label: &if model.template { i18n("Save Template") } else { i18n("Save Draft") },
                                set_tooltip_text: Some(
                                    if model.template { i18n("Save to Templates") } else { i18n("Save to Drafts") }.as_str()
                                ),
                                connect_clicked => ComposeInput::SaveDraft,
                            },
                            gtk::MenuButton {
                                set_icon_name: "pan-down-symbolic",
                                set_tooltip_text: Some(i18n("More ways to save").as_str()),
                                set_can_focus: false,
                                set_visible: !model.template,
                                #[wrap(Some)]
                                set_popover = &gtk::Popover {
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        add_css_class: "context-menu-list",
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            set_halign: gtk::Align::Fill,
                                            set_tooltip_text: Some(i18n("Keep a copy in Templates to start new messages from").as_str()),
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n("Save as Template"), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::SaveAsTemplate);
                                            },
                                        },
                                        // Only while editing an existing draft: the
                                        // message is moved to Trash, not saved, and
                                        // the editor closes.
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            set_halign: gtk::Align::Fill,
                                            set_tooltip_text: Some(i18n("Move this draft to Trash").as_str()),
                                            #[watch]
                                            set_visible: model.draft_origin.is_some(),
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n("Delete Draft"), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::DeleteDraft);
                                            },
                                        },
                                    },
                                },
                            },
                        },
                        // Send, with Send Later beside it (#145): presets, or a
                        // date and time of your own.
                        pack_end = &gtk::Box {
                            add_css_class: "linked",
                            add_css_class: "send-split",
                            gtk::Button {
                                #[watch]
                                set_label: &if model.send_at.is_some() { i18n("Schedule") } else { i18n("Send") },
                                add_css_class: "suggested-action",
                                connect_clicked => ComposeInput::Send,
                            },
                            // A floating divider, not a seam: the box paints the
                            // accent behind it so the two read as one control.
                            gtk::Separator {
                                set_orientation: gtk::Orientation::Vertical,
                            },
                            gtk::MenuButton {
                                set_icon_name: "pan-down-symbolic",
                                add_css_class: "suggested-action",
                                set_tooltip_text: Some(i18n("Send later").as_str()),
                                set_can_focus: false,
                                #[wrap(Some)]
                                set_popover = &gtk::Popover {
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 2,
                                        // Menu rows, not buttons: regular weight
                                        // like every other popover menu (#167).
                                        add_css_class: "context-menu-list",
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            set_halign: gtk::Align::Fill,
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n("Send now"), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::ClearSendAt);
                                                sender.input_sender().emit(ComposeInput::Send);
                                            },
                                        },
                                        gtk::Separator {},
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n_f("Tomorrow morning ({time})", &[("time", &crate::datefmt::clock_label(8))]), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::SendAt(preset_time(1, 8)));
                                            },
                                        },
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n_f("Tomorrow afternoon ({time})", &[("time", &crate::datefmt::clock_label(13))]), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::SendAt(preset_time(1, 13)));
                                            },
                                        },
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n_f("Monday morning ({time})", &[("time", &crate::datefmt::clock_label(8))]), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::SendAt(next_monday(8)));
                                            },
                                        },
                                        gtk::Separator {},
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n("Pick a date and time…"), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                if let Some(p) = b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() { p.popdown(); }
                                                sender.input_sender().emit(ComposeInput::PickSendTime);
                                            },
                                        },
                                    },
                                },
                            },
                        },
                        // The folded toolbar (narrow pane): every action that is
                        // not Cancel or Send lives in this menu.
                        #[name = "overflow_btn"]
                        pack_end = &gtk::Button {
                            set_icon_name: "view-more-horizontal-symbolic",
                            set_tooltip_text: Some(i18n("Actions").as_str()),
                            #[watch]
                            set_visible: model.narrow,
                            connect_clicked => ComposeInput::OverflowMenu,
                        },
                        // OpenPGP (#133): only offered where a gpg exists.
                        #[name = "encrypt_btn"]
                        pack_end = &gtk::ToggleButton {
                            set_icon_name: "channel-secure-symbolic",
                            set_tooltip_text: Some(i18n("Encrypt with OpenPGP to every recipient's key").as_str()),
                            #[watch]
                            set_visible: crate::pgp::available() && !model.narrow,
                            connect_toggled[sender] => move |b| {
                                sender.input_sender().emit(ComposeInput::ToggleEncrypt(b.is_active()));
                            },
                        },
                        #[name = "sign_btn"]
                        pack_end = &gtk::ToggleButton {
                            set_icon_name: "security-high-symbolic",
                            set_tooltip_text: Some(i18n("Sign with your OpenPGP key").as_str()),
                            #[watch]
                            set_visible: crate::pgp::available() && !model.narrow,
                            connect_toggled[sender] => move |b| {
                                sender.input_sender().emit(ComposeInput::ToggleSign(b.is_active()));
                            },
                        },
                        pack_end = &gtk::Button {
                            set_icon_name: "mail-attachment-symbolic",
                            set_tooltip_text: Some(i18n("Attach files").as_str()),
                            #[watch]
                            set_visible: !model.narrow,
                            connect_clicked => ComposeInput::AttachFiles,
                        },
                        // Cloud attachments (#144): only with an account set up.
                        pack_end = &gtk::Button {
                            set_icon_name: "cloud-symbolic",
                            set_tooltip_text: Some(i18n("Upload to cloud storage and share a link").as_str()),
                            #[watch]
                            set_visible: !model.cloud_accounts.is_empty() && !model.narrow,
                            #[watch]
                            set_sensitive: model.cloud_busy == 0,
                            connect_clicked => ComposeInput::CloudAttach,
                        },
                        pack_end = &gtk::Button {
                            set_icon_name: "x-office-address-book-symbolic",
                            set_tooltip_text: Some(i18n("Open Contacts").as_str()),
                            #[watch]
                            set_visible: !model.narrow,
                            connect_clicked => ComposeInput::OpenContacts,
                        },
                        // Promote inline reply → window, or collapse window → inline.
                        // Icon set in `init` and on SetWindowed.
                        #[name = "toggle_btn"]
                        pack_end = &gtk::Button {
                            set_tooltip_text: Some(i18n("Open in window").as_str()),
                            #[watch]
                            set_visible: model.can_toggle && !model.narrow,
                            connect_clicked => ComposeInput::ToggleWindowed,
                        },
                        // The compact reply's From/To/Subject rows (#154): folded
                        // away by default, one press brings them back.
                        #[name = "fields_btn"]
                        pack_end = &gtk::ToggleButton {
                            set_icon_name: "pan-down-symbolic",
                            add_css_class: "fields-chevron",
                            set_tooltip_text: Some(i18n("Show From, To and Subject").as_str()),
                            set_can_focus: false,
                            #[watch]
                            set_visible: model.compact && !model.windowed,
                            connect_toggled[sender] => move |b| {
                                sender.input_sender().emit(ComposeInput::ShowFields(b.is_active()));
                            },
                        },
                    },
                    // Send Later (#145): says when a scheduled message goes, with a
                    // way back to sending at once.
                    add_top_bar = &gtk::Box {
                        add_css_class: "schedule-bar",
                        set_spacing: 8,
                        set_margin_start: 12,
                        set_margin_end: 12,
                        set_margin_top: 4,
                        set_margin_bottom: 4,
                        #[watch]
                        set_visible: model.send_at.is_some(),
                        gtk::Image { set_icon_name: Some("alarm-symbolic") },
                        gtk::Label {
                            set_hexpand: true,
                            set_halign: gtk::Align::Start,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            #[watch]
                            set_label: &model.send_at.map(|t| i18n_f("Scheduled for {when}", &[("when", &crate::datefmt::date_time(t))])).unwrap_or_default(),
                        },
                        gtk::Button {
                            add_css_class: "flat",
                            set_label: &i18n("Send now instead"),
                            connect_clicked => ComposeInput::ClearSendAt,
                        },
                    },
                    // Cloud attachments (#144): the download passwords, which
                    // stay out of the message and go to the recipient some other way.
                    add_top_bar = &gtk::Box {
                        set_spacing: 8,
                        set_margin_start: 12,
                        set_margin_end: 12,
                        set_margin_top: 4,
                        set_margin_bottom: 4,
                        #[watch]
                        set_visible: !model.cloud_passwords.is_empty(),
                        gtk::Image { set_icon_name: Some("dialog-password-symbolic") },
                        gtk::Label {
                            set_hexpand: true,
                            set_halign: gtk::Align::Start,
                            set_wrap: true,
                            set_selectable: true,
                            #[watch]
                            set_label: &model.cloud_passwords.iter().map(|(n, p)| i18n_f("Download password for {name}: {password}", &[("name", n), ("password", p)])).collect::<Vec<_>>().join("\n"),
                        },
                        gtk::Button {
                            add_css_class: "flat",
                            set_label: &i18n("Copy"),
                            connect_clicked => ComposeInput::CopyCloudPasswords,
                        },
                    },

                    #[wrap(Some)]
                    set_content = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 12,
                        add_css_class: "compose-pane",

                        // From and To are always offered, inline included — a
                        // forward is unaddressable without To (#25, #52). Cc, Bcc,
                        // and (for replies/forwards) the prefilled Subject wait
                        // behind the To row's "More" button; per-row visibility is
                        // set in `init`.
                        #[name = "fields_list"]
                        gtk::ListBox {
                            add_css_class: "boxed-list",
                            add_css_class: "compose-fields",
                            set_selection_mode: gtk::SelectionMode::None,

                            #[name = "from_row"]
                            adw::ComboRow {
                                set_title: &i18n("From"),
                                connect_selected_notify => ComposeInput::AccountChanged,
                            },
                            // The From as text (#347): any name and address,
                            // sent through the account chosen above.
                            #[name = "custom_from_row"]
                            adw::EntryRow {
                                set_title: &i18n("From"),
                                set_input_purpose: gtk::InputPurpose::Email,
                                set_visible: false,
                            },
                            #[name = "to_row"]
                            RecipientRow {
                                set_title: &i18n("To"),
                                set_input_purpose: gtk::InputPurpose::Email,
                            },
                            #[name = "cc_row"]
                            RecipientRow {
                                set_title: &i18n("Cc"),
                                set_input_purpose: gtk::InputPurpose::Email,
                            },
                            #[name = "bcc_row"]
                            RecipientRow {
                                set_title: &i18n("Bcc"),
                                set_input_purpose: gtk::InputPurpose::Email,
                            },
                            #[name = "reply_to_row"]
                            adw::EntryRow {
                                set_title: &i18n("Reply-To"),
                                set_input_purpose: gtk::InputPurpose::Email,
                            },
                            #[name = "subject_row"]
                            adw::EntryRow {
                                set_title: &i18n("Subject"),
                            },
                        },

                        // Attachments in up to three equal columns (#299),
                        // filling left to right and wrapping.
                        #[name = "attach_box"]
                        gtk::FlowBox {
                            set_selection_mode: gtk::SelectionMode::None,
                            set_column_spacing: 6,
                            set_row_spacing: 6,
                            set_homogeneous: true,
                            set_max_children_per_line: 3,
                            add_css_class: "attach-flow",
                            set_visible: false,
                        },

                        // Holder for the shared rich-text editor (toolbar + body),
                        // appended in `init`.
                        #[name = "editor_holder"]
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_vexpand: true,
                        },
                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let ComposeInit {
            compose_id,
            prefill,
            accounts,
            selected,
            own_addresses,
            windowed,
            can_toggle,
            compact,
            decorations,
            format,
            signature_position,
        } = init;
        let in_reply_to = prefill.in_reply_to.clone();
        let references = prefill.references.clone();
        let forwards = prefill.forwards.clone();
        let draft_origin = prefill.draft_origin.clone();
        let template = prefill.template && draft_origin.is_some();
        let outbox_origin = prefill.outbox_origin;
        let prefill_attachments = prefill.attachments.clone();
        let handed_files =
            !prefill_attachments.is_empty() && draft_origin.is_none() && outbox_origin.is_none();
        let prefill_encrypt = prefill.encrypt;
        let send_at = prefill.send_at;
        let current_sig = accounts
            .get(selected)
            .map(|a| a.signature.clone())
            .unwrap_or_default();
        let sign_default = accounts.get(selected).is_some_and(|a| a.sign_default);

        let completion = gtk::Popover::new();
        completion.set_autohide(false); // don't steal focus from the entry
        completion.set_can_focus(false);
        completion.set_position(gtk::PositionType::Bottom);
        completion.add_css_class("menu");

        // Initial editor content: a blank line to type on, then the
        // signature and the quoted reply/forward (if any), in the order
        // the setting says (#237). A draft or a queued message already has
        // both and gets neither again: each reopening of a draft used to add
        // another blank line (#349). With Return set to start paragraphs the
        // line is a paragraph too, so the first Return splits it into two.
        let resumed = prefill.resumed || draft_origin.is_some() || outbox_origin.is_some();
        let mut content = String::from(
            match (resumed, crate::config::load_privacy().return_paragraph) {
                (true, _) => "",
                (false, true) => "<p><br></p>",
                (false, false) => "<div><br></div>",
            },
        );
        let sig_dashes = crate::config::load_privacy().signature_dashes;
        let sig = if !resumed && !prefill.no_signature && !current_sig.is_empty() {
            sig_html(&current_sig, sig_dashes)
        } else {
            String::new()
        };
        match signature_position {
            SignaturePosition::AboveQuote => {
                content.push_str(&sig);
                content.push_str(&prefill.body_html);
            }
            SignaturePosition::BelowQuote => {
                content.push_str(&prefill.body_html);
                content.push_str(&sig);
            }
        }
        // Blocking the quote's pictures must not blank the user's own
        // signature, whichever identity it ends up sent from.
        let remote_images = prefill.block_remote_images.then(|| {
            accounts
                .iter()
                .flat_map(|a| a.signatures.iter())
                .flat_map(|sig| super::rich_editor::remote_image_urls(&sig.html))
                .collect::<Vec<_>>()
        });
        let editor = RichEditor::new(
            &content,
            remote_images.as_deref(),
            format == ComposeFormat::Plain,
        );
        editor.set_formatting_visible(format == ComposeFormat::Rich);
        let zoomable = std::rc::Rc::new(std::cell::Cell::new(format != ComposeFormat::Rich));
        if zoomable.get() {
            editor.set_zoom(ZOOM.load(std::sync::atomic::Ordering::Relaxed));
        }
        {
            let z = zoomable.clone();
            let s = sender.input_sender().clone();
            editor.widget.add_controller(super::ctrl_scroll_zoom(
                move || z.get(),
                move |step| {
                    let _ = s.send(ComposeInput::Zoom(step));
                },
            ));
        }
        // A source format starts from the same content, written out as
        // source: a reply's quoted original becomes `> ` lines in Markdown,
        // or the HTML it already was.
        match format {
            ComposeFormat::Markdown => {
                editor.set_source(SourceKind::Markdown, &markdown_body(&content))
            }
            ComposeFormat::Html => {
                editor.set_source(SourceKind::Html, &crate::markdown::pretty_html(&content))
            }
            ComposeFormat::Rich | ComposeFormat::Plain => {}
        }
        // HYLKI_SHOWCASE_PLAIN_LOG also logs the body a composer opens with
        // (demo only), to check a reply started in plain text (#436).
        if std::env::var_os("HYLKI_SHOWCASE_PLAIN_LOG").is_some()
            && std::env::var_os("HYLKI_DEMO").is_some()
        {
            let ed = editor.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(2500), move || {
                ed.eval(SHOWCASE_PLAIN_LOG, move |v| {
                    tracing::info!("showcase plain: opened {format:?} {v}")
                });
            });
        }
        // The format chooser and the preview toggle belong with the other
        // formatting controls, at the far end of the same row: a header
        // button for them folded away exactly when the pane was narrow, and
        // the format is a property of the body, not of the window.
        // Icon alone: it sits at the end of a row of icons, and the label
        // it wore for a while read as a second toolbar rather than as one
        // more control on the same one. The tooltip still names the format.
        let format_btn = gtk::Button::from_icon_name(format_icon(format));
        format_btn.set_tooltip_text(Some(
            i18n_f("Writing in {format}", &[("format", &format_label(format))]).as_str(),
        ));
        format_btn.add_css_class("flat");
        format_btn.set_can_focus(false);
        // The preview keeps its word. It is a state rather than an action,
        // and an eye on its own leaves which state to guesswork.
        let preview_content = adw::ButtonContent::builder()
            .icon_name("eye-open-negative-filled-symbolic")
            .label(i18n("Preview"))
            .build();
        let preview_btn = gtk::ToggleButton::builder().child(&preview_content).build();
        preview_btn.set_tooltip_text(Some(i18n("Show the message as it will be sent").as_str()));
        preview_btn.add_css_class("flat");
        preview_btn.set_can_focus(false);
        preview_btn.set_can_shrink(true);
        preview_btn.set_visible(format.is_source());
        {
            let s = sender.input_sender().clone();
            format_btn.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::FormatMenu);
            });
            let s = sender.input_sender().clone();
            preview_btn.connect_toggled(move |b| {
                let _ = s.send(ComposeInput::TogglePreview(b.is_active()));
            });
        }
        // The chooser goes last, which in a right-aligned group is the far
        // end of the row: it is there in every format, so it is the one
        // that must not move. Preview comes and goes beside it, to its
        // left, where an appearing button pushes nothing around.
        // Translate (#327) goes before both: it is there in every format, but
        // only once a translation service is set up.
        let translate_btn = gtk::Button::from_icon_name("translate-symbolic");
        translate_btn.set_tooltip_text(Some(i18n("Translate what you wrote").as_str()));
        translate_btn.add_css_class("flat");
        translate_btn.set_can_focus(false);
        translate_btn.set_visible(crate::translate::load().enabled());
        {
            let s = sender.input_sender().clone();
            translate_btn.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::TranslateMenu);
            });
        }
        let signature_btn = gtk::Button::from_icon_name("signature-symbolic");
        signature_btn.set_tooltip_text(Some(i18n("Insert Signature").as_str()));
        signature_btn.add_css_class("flat");
        signature_btn.set_can_focus(false);
        signature_btn.set_visible(
            accounts
                .get(selected)
                .is_some_and(|a| !a.signatures.is_empty()),
        );
        {
            let s = sender.input_sender().clone();
            signature_btn.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::SignatureClicked);
            });
            let s = sender.input_sender().clone();
            let right = gtk::GestureClick::new();
            right.set_button(gtk::gdk::BUTTON_SECONDARY);
            right.connect_pressed(move |g, _, _, _| {
                g.set_state(gtk::EventSequenceState::Claimed);
                let _ = s.send(ComposeInput::SignatureMenu);
            });
            signature_btn.add_controller(right);
        }
        editor.toolbar_end().append(&signature_btn);
        editor.toolbar_end().append(&translate_btn);
        editor.toolbar_end().append(&preview_btn);
        editor.toolbar_end().append(&format_btn);

        // "Send as Attachment Instead" on an inline image: the editor lifts
        // it to a temp file and it joins the attachment chips here.
        {
            let s = sender.input_sender().clone();
            editor.connect_send_as_attachment(move |path| {
                let _ = s.send(ComposeInput::AddAttachments(vec![path]));
            });
        }

        // The body's text history joins the composer's order (#200): every
        // edit in there is one step, wherever the attachments' steps fall.
        {
            let s = sender.input_sender().clone();
            editor.connect_history_changed(move |can_undo, _| {
                let _ = s.send(ComposeInput::BodyHistory(can_undo));
            });
        }

        {
            let s = sender.output_sender().clone();
            editor.connect_spell_languages(move |langs| {
                s.emit(ComposeOutput::SetSpellcheckLangs(langs));
            });
        }

        let mut model = Compose {
            accounts,
            editor,
            current_sig,
            sig_dashes,
            signature_position,
            attachments: prefill_attachments,
            attach_thumbs: Default::default(),
            suggestions: Vec::new(),
            graph_directories: Default::default(),
            own_mail: Default::default(),
            from_account: 0,
            directory_matches: Vec::new(),
            directory_generation: 0,
            directory_errors_shown: std::collections::HashSet::new(),
            completion,
            completion_field: None,
            completion_list: None,
            completion_selected: 0,
            completion_count: 0,
            completion_open: std::rc::Rc::new(std::cell::Cell::new(false)),
            in_reply_to,
            references,
            forwards,
            draft_origin,
            template,
            outbox_origin,
            compose_id,
            windowed,
            can_toggle,
            // A compact (fields-hidden) pane only makes sense once it is
            // addressed: replies arrive with To filled, forwards do not.
            compact: compact && !prefill.to.trim().is_empty(),
            decorations,
            fields_shown: crate::config::load_privacy().reply_fields,
            custom_from: false,
            narrow: false,
            // Files handed in from outside are something to lose: closing
            // asks first, as it would once the user had attached them.
            fields_dirty: handed_files,
            asking_discard: false,
            sign: false,
            sign_touched: false,
            sign_expected: None,
            format,
            zoomable: zoomable.clone(),
            format_btn,
            translate_btn,
            signature_btn,
            translating: false,
            autosaving: false,
            autosaved: None,
            autosave_pending: None,
            autosave_made: false,
            // A template is saved when its editor says so: an automatic save
            // would put the half-edited one in its place.
            autosave_off: template,
            tr_subject: None,
            preview_btn,
            preview_content,
            encrypt: false,
            pgp_checking: false,
            pgp_cleared: None,
            send_at,
            cloud_accounts: crate::cloud::load_enabled_accounts(),
            drop_zones: None,
            cloud_links: Vec::new(),
            cloud_busy: 0,
            cloud_passwords: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        };
        model.from_account = model
            .accounts
            .get(selected)
            .map(|a| a.id)
            .unwrap_or_default();
        let widgets = view_output!();
        if prefill_encrypt && crate::pgp::available() {
            // Through the buttons, so the toggles and the model agree.
            widgets.encrypt_btn.set_active(true);
        } else if sign_default && crate::pgp::available() {
            // A draft does not record whether it was signed, so a reopened
            // one starts as its account says, like any new message.
            model.sign_expected = Some(true);
            widgets.sign_btn.set_active(true);
        }
        widgets.editor_holder.append(&model.editor.widget);

        // The inline/window toggle: only reply/forward panes can toggle. Its icon
        // reflects the current host (fullscreen = "expand to window", restore =
        // "collapse back inline").
        set_toggle_icon(&widgets.toggle_btn, model.windowed);
        size_for_host(
            &widgets.toolbar_root,
            &widgets.header,
            &widgets.editor_holder,
            model.windowed,
            model.decorations,
        );

        // Fold the toolbar once the pane is narrower than the full row (as
        // the reader's header does). Measured on the first map — before
        // anything is hidden, and once the header has a window, since the
        // window controls it carries inline only exist (and measure) with
        // one. A later host move changes that by a few dozen pixels; the
        // first host's threshold is kept, which only ever folds early.
        {
            let s = sender.clone();
            let header: gtk::Widget = widgets.header.clone().upcast();
            let measured = std::rc::Rc::new(std::cell::Cell::new(false));
            root.connect_map(move |root| {
                if measured.replace(true) {
                    return;
                }
                // Not the bar's own natural width: a header bar keeps its
                // title centred, so it asks for twice its wider side, and
                // this bar's end row is much wider than its start — every
                // button added there counted double and the fold came far
                // too soon. The centre box's rows, summed, are what has to
                // fit.
                let full = header.measure(gtk::Orientation::Horizontal, -1).1;
                let rows = header_rows_width(&header);
                tracing::debug!("compose fold: header natural {full}, rows {rows:?}");
                let need = rows.unwrap_or(full);
                let threshold = if need <= 0 {
                    COMPOSE_ACTIONS_BREAKPOINT
                } else {
                    need as f64 + 24.0
                };
                let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                    adw::BreakpointConditionLengthType::MaxWidth,
                    threshold,
                    adw::LengthUnit::Px,
                ));
                let s1 = s.clone();
                bp.connect_apply(move |_| s1.input_sender().emit(ComposeInput::SetNarrow(true)));
                let s2 = s.clone();
                bp.connect_unapply(move |_| s2.input_sender().emit(ComposeInput::SetNarrow(false)));
                root.add_breakpoint(bp);
            });
        }

        // Per-row visibility (#25): To always; Cc/Bcc only when prefilled (a
        // reply-all carries Cc). The Subject is always shown — replies and
        // forwards arrive with it prefilled, but it stays the user's to see
        // and change (2026-08-31).
        let cc_shown = !prefill.cc.trim().is_empty();
        let bcc_shown = !prefill.bcc.trim().is_empty();
        widgets.cc_row.set_visible(cc_shown);
        widgets.bcc_row.set_visible(bcc_shown);
        // Reply-To (#58) is rare enough to always start hidden behind "More".
        widgets.reply_to_row.set_visible(false);
        widgets.subject_row.set_visible(true);
        // Compact split reply: only the editor shows; the full field rows
        // return when the composer pops out to a window. Never for a pane
        // that arrives unaddressed — a forward — which needs its To row
        // (#139).
        widgets
            .fields_list
            .set_visible(!model.compact || model.fields_shown);
        widgets.fields_btn.set_active(model.fields_shown);
        {
            let more = gtk::Button::with_label(&i18n("More"));
            more.add_css_class("flat");
            more.set_valign(gtk::Align::Center);
            more.set_tooltip_text(Some(i18n("Show Cc, Bcc and Reply-To").as_str()));
            let cc = widgets.cc_row.clone();
            let bcc = widgets.bcc_row.clone();
            let reply_to = widgets.reply_to_row.clone();
            let from = widgets.from_row.clone();
            let custom = widgets.custom_from_row.clone();
            let btn = more.clone();
            more.connect_clicked(move |_| {
                cc.set_visible(true);
                bcc.set_visible(true);
                reply_to.set_visible(true);
                // With one address the From row is hidden; More brings it,
                // with its pencil for typing another (#347).
                if !custom.is_visible() {
                    from.set_visible(true);
                }
                btn.set_visible(false);
            });
            widgets.to_row.add_suffix(&more);
        }

        // Populate the From dropdown.
        let labels: Vec<&str> = model.accounts.iter().map(|a| a.label.as_str()).collect();
        let strings = gtk::StringList::new(&labels);
        widgets.from_row.set_model(Some(&strings));
        // Custom factory so the selected account isn't needlessly ellipsized.
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                let label = gtk::Label::new(None);
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::None);
                item.set_child(Some(&label));
            }
        });
        factory.connect_bind(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                let text = item
                    .item()
                    .and_downcast::<gtk::StringObject>()
                    .map(|s| s.string().to_string())
                    .unwrap_or_default();
                if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                    label.set_label(&text);
                }
            }
        });
        widgets.from_row.set_factory(Some(&factory));
        widgets.from_row.set_selected(selected as u32);
        widgets.from_row.set_visible(model.accounts.len() > 1);
        {
            // The pencil types a From of your own (#347); the custom row's
            // button goes back to the list.
            let edit = gtk::Button::from_icon_name("document-edit-symbolic");
            edit.add_css_class("flat");
            edit.set_valign(gtk::Align::Center);
            edit.set_tooltip_text(Some(i18n("Type a From address").as_str()));
            let s = sender.input_sender().clone();
            edit.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::CustomFrom(true));
            });
            widgets.from_row.add_suffix(&edit);
            let back = gtk::Button::from_icon_name("pan-down-symbolic");
            back.add_css_class("flat");
            back.set_valign(gtk::Align::Center);
            back.set_tooltip_text(Some(i18n("Choose from your addresses").as_str()));
            let s = sender.input_sender().clone();
            back.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::CustomFrom(false));
            });
            widgets.custom_from_row.add_suffix(&back);
        }
        // A From that is none of the identities opens as text (#347).
        let custom = prefill.custom_from.trim();
        if !custom.is_empty() {
            let addr = crate::worker::parse_recipients(custom)
                .into_iter()
                .next()
                .map(|(_, a)| a)
                .unwrap_or_default();
            let same = model
                .accounts
                .get(selected)
                .is_some_and(|a| same_from(&identity_display(a), custom));
            // A bare address that is the identity's counts as the identity.
            let bare = !custom.contains('<')
                && model
                    .accounts
                    .get(selected)
                    .is_some_and(|a| a.email.eq_ignore_ascii_case(&addr));
            if !same && !bare {
                model.custom_from = true;
                widgets.from_row.set_visible(false);
                widgets.custom_from_row.set_text(custom);
                widgets.custom_from_row.set_visible(true);
            }
        }

        widgets.to_row.set_text(&prefill.to);
        widgets.cc_row.set_text(&prefill.cc);
        widgets.bcc_row.set_text(&prefill.bcc);
        widgets.subject_row.set_text(&prefill.subject);
        if !model.attachments.is_empty() {
            model.rebuild_attachments(&widgets.attach_box, &sender);
        }

        // Files dragged over the composer bring up cards for where they go:
        // attached, in the text, or uploaded to the cloud (#293).
        let s = sender.input_sender().clone();
        let zones = DropZones::install(
            DropContext::Composer,
            &root,
            &widgets.drop_overlay,
            move |choice, paths| {
                s.emit(ComposeInput::DroppedFiles(choice, paths));
            },
        );
        zones.set_allow_inline(model.format == ComposeFormat::Rich);
        zones.set_cloud_names(
            model
                .cloud_accounts
                .iter()
                .map(|a| a.name.clone())
                .collect(),
        );
        // HYLKI_SHOWCASE_DROP_ZONES=<attach|inline|cloud|none>:<file>[:<file>…]
        // raises the surfaces two seconds after the composer opens.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_DROP_ZONES") {
            let mut parts = std::env::split_paths(&v);
            let hover = match parts.next().as_deref().and_then(|p| p.to_str()) {
                Some("attach") => Some(DropChoice::Attach),
                Some("inline") => Some(DropChoice::Inline),
                Some("cloud") => Some(DropChoice::Cloud),
                _ => None,
            };
            let paths: Vec<_> = parts.collect();
            let zones = zones.clone();
            let host = root.clone().upcast::<gtk::Widget>();
            gtk::glib::timeout_add_seconds_local_once(2, move || {
                zones.showcase(&paths, &host, hover)
            });
        }
        model.drop_zones = Some(zones);

        if !prefill.inline_files.is_empty() {
            model.editor.insert_files(&prefill.inline_files);
        }
        // Files handed in over the size limit (Settings → System → GNOME
        // Files): straight into the upload dialog, once the composer has a
        // window for it to be transient for.
        if !prefill.cloud_uploads.is_empty() {
            let s = sender.clone();
            let paths = prefill.cloud_uploads.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
                s.input_sender().emit(ComposeInput::CloudPicked(paths));
            });
        }

        // HYLKI_SHOWCASE_CLOUD_DIALOG opens the cloud upload dialog on a
        // stand-in file two seconds after the composer is up (demo only),
        // for a capture of this upload's terms.
        if std::env::var_os("HYLKI_SHOWCASE_CLOUD_DIALOG").is_some()
            && std::env::var_os("HYLKI_DEMO").is_some()
        {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(2, move || {
                s.input_sender()
                    .emit(ComposeInput::CloudPicked(vec![std::path::PathBuf::from(
                        "/tmp/Q3 report.pdf",
                    )]));
            });
        }

        // HYLKI_SHOWCASE_EDITOR_JS=<file> runs that script in the body three
        // seconds after the composer opens, and logs the body's HTML a
        // second later (demo only), to drive the format bar's commands
        // without a pointer (#358).
        if let (Some(path), Some(_)) = (
            std::env::var_os("HYLKI_SHOWCASE_EDITOR_JS"),
            std::env::var_os("HYLKI_DEMO"),
        ) {
            if let Ok(js) = std::fs::read_to_string(&path) {
                let ed = model.editor.clone();
                gtk::glib::timeout_add_seconds_local_once(3, move || {
                    ed.run_js(&js);
                    gtk::glib::timeout_add_seconds_local_once(1, move || {
                        ed.eval("document.body.innerHTML", |html| {
                            tracing::info!("showcase editor body: {html}");
                        });
                    });
                });
            }
        }

        // HYLKI_SHOWCASE_ATTACH=<file>[:<file>…] attaches those files a
        // second after the composer opens (demo only), for a capture of the
        // chips (#299); HYLKI_SHOWCASE_ATTACH_SHOT=<png>[:<seconds>]
        // captures the composer that long after it opens.
        if let (Some(v), Some(_)) = (
            std::env::var_os("HYLKI_SHOWCASE_ATTACH"),
            std::env::var_os("HYLKI_DEMO"),
        ) {
            let paths: Vec<_> = std::env::split_paths(&v).collect();
            // A composer made ahead of time and dropped unused is gone by then.
            let s = sender.input_sender().clone();
            gtk::glib::timeout_add_seconds_local_once(1, move || {
                let _ = s.send(ComposeInput::AddAttachments(paths));
            });
            if let Ok(v) = std::env::var("HYLKI_SHOWCASE_ATTACH_SHOT") {
                // <png>[:<seconds>], three seconds unless given.
                let (shot, at) = match v.rsplit_once(':') {
                    Some((p, n)) if n.parse::<u32>().is_ok() => (p.to_string(), n.parse().unwrap()),
                    _ => (v, 3),
                };
                let host = root.clone().upcast::<gtk::Widget>();
                gtk::glib::timeout_add_seconds_local_once(at, move || {
                    if host.is_mapped() {
                        crate::app::showcase_capture(&host, &shot);
                    }
                });
            }
        }

        {
            let s = sender.input_sender().clone();
            std::thread::spawn(move || {
                let graph = crate::graph_directory::accounts();
                // An account's id is its place in the file, from 1.
                let narrowed: Vec<(u32, crate::config::AccountConfig)> = crate::config::load()
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                    .filter(|(_, a)| a.graph_directory_only && crate::graph_directory::usable(a))
                    .map(|(i, a)| (i as u32 + 1, a))
                    .collect();
                let own_mail = match crate::cache::Cache::open() {
                    Ok(cache) if !narrowed.is_empty() => narrowed
                        .into_iter()
                        .map(|(id, account)| {
                            (
                                id,
                                OwnOnly {
                                    addresses: cache.account_addresses(id),
                                    account,
                                },
                            )
                        })
                        .collect(),
                    _ => OwnMail::new(),
                };
                let _ = s.send(ComposeInput::DirectoriesKnown { graph, own_mail });
            });
        }

        {
            // EDS can stop answering, and a large mail cache takes time to
            // scan. Neither is a prerequisite for writing a message, and a
            // slow keyring behind the directories above must not hold up
            // autocomplete.
            let s = sender.input_sender().clone();
            std::thread::spawn(move || {
                let suggestions = crate::contacts::suggestions(&own_addresses);
                let _ = s.send(ComposeInput::SuggestionsLoaded(suggestions));
            });
        }

        // Wire autocomplete *after* prefilling, so the initial text doesn't pop it.
        for (row, field) in [
            (&widgets.to_row, Field::To),
            (&widgets.cc_row, Field::Cc),
            (&widgets.bcc_row, Field::Bcc),
        ] {
            let s = sender.clone();
            row.connect_changed(move |row| {
                row.remove_css_class("error");
                s.input_sender().emit(ComposeInput::Suggest(field));
                s.input_sender().emit(ComposeInput::MarkFieldsDirty);
            });

            // Close the popover when the field loses focus.
            let focus = gtk::EventControllerFocus::new();
            let s = sender.clone();
            focus.connect_leave(move |_| s.input_sender().emit(ComposeInput::CompletionClose));
            row.add_controller(focus);
        }
        // Pills are selected across these and move between them by drag
        // or by their menu (#492). While some are dragged, the hidden Cc and
        // Bcc show, to be dropped on; those still empty after hide again.
        {
            let rows = [
                widgets.to_row.clone(),
                widgets.cc_row.clone(),
                widgets.bcc_row.clone(),
            ];
            let shown: std::rc::Rc<std::cell::RefCell<Vec<RecipientRow>>> = Default::default();
            for row in &rows {
                row.set_fields(&[&rows[0], &rows[1], &rows[2]]);
                let (cc, bcc, shown) = (rows[1].clone(), rows[2].clone(), shown.clone());
                row.connect_pill_drag(move |_, on| {
                    if on {
                        for r in [&cc, &bcc].into_iter().filter(|r| !r.is_visible()) {
                            r.set_visible(true);
                            shown.borrow_mut().push(r.clone());
                        }
                    } else {
                        for r in shown.take() {
                            if r.text().is_empty() {
                                r.set_visible(false);
                            }
                        }
                    }
                });
            }
        }
        widgets
            .reply_to_row
            .connect_changed(|row| row.remove_css_class("error"));
        // Subject edits also count as dirtying the draft.
        let s = sender.clone();
        widgets
            .subject_row
            .connect_changed(move |_| s.input_sender().emit(ComposeInput::MarkFieldsDirty));
        // The subject gets the body's red underlines too (#114): every edit
        // re-checks the line through enchant directly and paints error
        // underlines onto the row's inner GtkText — the row itself exposes
        // no Pango attributes. The word the cursor sits in is exempt while
        // typing and joins the check after a 400ms pause, so mistakes show
        // before the space without half-words flashing red mid-keystroke.
        // Addresses stay uncheckable on purpose: only the subject is prose.
        if let Some(text) = inner_text(widgets.subject_row.upcast_ref()) {
            let t = text.clone();
            let pending: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>> =
                std::rc::Rc::new(std::cell::RefCell::new(None));
            widgets.subject_row.connect_changed(move |row| {
                if let Some(prev) = pending.borrow_mut().take() {
                    prev.remove();
                }
                let content = row.text().to_string();
                // Editable positions count characters; attribute ranges
                // count bytes.
                let cursor = content
                    .char_indices()
                    .nth(row.position().max(0) as usize)
                    .map(|(b, _)| b)
                    .unwrap_or(content.len());
                t.set_attributes(crate::spell::error_attrs(&content, Some(cursor)).as_ref());
                let t = t.clone();
                let row = row.clone();
                let slot = pending.clone();
                let id = gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(400),
                    move || {
                        slot.borrow_mut().take();
                        t.set_attributes(crate::spell::error_attrs(&row.text(), None).as_ref());
                    },
                );
                *pending.borrow_mut() = Some(id);
            });
            // Prefilled subjects (replies, drafts) get checked on open too.
            text.set_attributes(
                crate::spell::error_attrs(&widgets.subject_row.text(), None).as_ref(),
            );
        }

        // Drive the suggestion list from a single capture-phase key handler on
        // the window — the toplevel sees every key first, regardless of focus.
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let s = sender.clone();
        let open = model.completion_open.clone();
        let editor = model.editor.clone();
        let zoomable = model.zoomable.clone();
        // Weak: the root owns this controller, so a strong root here would
        // be a cycle that kept a closed composer's whole widget tree alive,
        // its editor's WebKitWebProcess included (#221).
        let key_root = root.downgrade();
        key.connect_key_pressed(move |_, keyval, _, state| {
            use gtk::glib::Propagation;
            let Some(key_root) = key_root.upgrade() else {
                return Propagation::Proceed;
            };
            // Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y run the composer's own history
            // (#200): the body's typing and formatting with the attachments
            // in the same order. Caught here, in the capture phase above the
            // editor, so the editor's own handler for the bare body never
            // sees them and the attachments stay in the order.
            //
            // The address and subject rows are the exception: GTK gives every
            // GtkText an undo history of its own, and for a single line that
            // is the one the user means.
            if let Some(redo) = rich_editor::history_key(keyval, state) {
                if !focus_is_entry(&key_root) {
                    s.input_sender().emit(ComposeInput::History { redo });
                    return Propagation::Stop;
                }
                return Propagation::Proceed;
            }
            // Ctrl+V pastes per the "Paste as plain text" preference, read
            // here so a settings change applies to composers already open.
            // Only over the body: the address and subject entries are plain
            // text by nature, and the focus guard leaves their Ctrl+V alone.
            if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                && keyval == gtk::gdk::Key::v
                && editor.has_focus()
            {
                editor.paste(!crate::config::load_privacy().paste_plain);
                return Propagation::Stop;
            }
            // Ctrl+ / Ctrl- / Ctrl+0 zoom the body, as they zoom the reader,
            // in the formats whose size is not part of the message. This
            // is a composer window's; inline, the main window's own keys
            // get there first and pass them on (app.rs).
            if let Some(step) = zoom_key(keyval, state) {
                if zoomable.get() {
                    s.input_sender().emit(ComposeInput::Zoom(step));
                    return Propagation::Stop;
                }
                return Propagation::Proceed;
            }
            // Ctrl+Enter sends (#238), as it does in Gmail, Apple Mail and
            // Thunderbird. Ahead of the suggestion list's own Enter, so a
            // press with the list open never both accepts a name and sends.
            // Send itself refuses an unaddressed message, so the shortcut
            // cannot post what the button would not.
            if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                && (keyval == gtk::gdk::Key::Return || keyval == gtk::gdk::Key::KP_Enter)
            {
                s.input_sender().emit(ComposeInput::Send);
                return Propagation::Stop;
            }
            // Thunderbird's keys (#490, #491): Ctrl+Shift+A attaches files,
            // Ctrl+Shift+T, C and B go to To, Cc and Bcc. A composer window
            // gets them here; inline, the same keys are the main window's
            // first, and app.rs passes them on.
            if let Some(input) = composer_key(keyval, state) {
                s.input_sender().emit(input);
                return Propagation::Stop;
            }
            if !open.get() {
                // Escape backs out of the whole composer, the same as Cancel:
                // an untouched reply goes at once, an edited one asks first
                // (#290). Only once the suggestion list is closed, which
                // Escape dismisses first (below), so one press never does both.
                if keyval == gtk::gdk::Key::Escape {
                    s.input_sender().emit(ComposeInput::Cancel);
                    return Propagation::Stop;
                }
                return Propagation::Proceed;
            }
            // Compare by value (the const-as-pattern match wasn't matching).
            if keyval == gtk::gdk::Key::Down {
                s.input_sender().emit(ComposeInput::CompletionMove(1));
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Up {
                s.input_sender().emit(ComposeInput::CompletionMove(-1));
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Return || keyval == gtk::gdk::Key::KP_Enter {
                s.input_sender().emit(ComposeInput::CompletionAccept);
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Escape {
                s.input_sender().emit(ComposeInput::CompletionClose);
                Propagation::Stop
            } else {
                Propagation::Proceed
            }
        });
        root.add_controller(key);

        // The message is saved to Drafts on its own every so often while it
        // changes (#340), so a crash, a lost connection or a closed laptop
        // costs at most that much. The timer stops with the composer.
        {
            let s = sender.input_sender().clone();
            gtk::glib::timeout_add_seconds_local(AUTOSAVE_SECS, move || {
                if s.send(ComposeInput::AutoSave).is_err() {
                    return gtk::glib::ControlFlow::Break;
                }
                gtk::glib::ControlFlow::Continue
            });
        }

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        // Each arm handles its message in full, or hands it to an `on_*`
        // method that returns early where it has to; every message ends at
        // `update_view` below.
        {
            match message {
                ComposeInput::Cancel => self.on_cancel(&sender),

                ComposeInput::ConfirmDiscard => self.on_confirm_discard(&sender, root),

                ComposeInput::KeepEditing => self.asking_discard = false,

                ComposeInput::Discard => self.on_discard(&sender),

                ComposeInput::SendAt(at) => {
                    self.send_at = Some(at);
                    sender.input_sender().emit(ComposeInput::Send);
                }

                ComposeInput::ClearSendAt => {
                    self.send_at = None;
                }

                ComposeInput::FocusField(field) => self.on_focus_field(field, widgets),

                ComposeInput::ShowFields(on) => {
                    self.fields_shown = on;
                    widgets
                        .fields_list
                        .set_visible(!(self.compact && !self.windowed) || on);
                }

                ComposeInput::SetNarrow(narrow) => self.on_set_narrow(narrow, widgets, root),

                ComposeInput::OverflowMenu => self.on_overflow_menu(widgets, &sender),

                ComposeInput::CloudAttach => self.on_cloud_attach(&sender, root),

                ComposeInput::DroppedFiles(choice, paths) => match choice {
                    DropChoice::Attach => sender
                        .input_sender()
                        .emit(ComposeInput::AddAttachments(paths)),
                    DropChoice::Inline => self.editor.insert_files(&paths),
                    DropChoice::Cloud => {
                        sender.input_sender().emit(ComposeInput::CloudPicked(paths))
                    }
                },

                ComposeInput::CloudPicked(paths) => self.on_cloud_picked(paths, &sender, root),

                ComposeInput::CloudUpload {
                    paths,
                    account,
                    link_password,
                } => self.on_cloud_upload(paths, account, link_password, &sender),

                ComposeInput::CloudUploadSignedIn {
                    paths,
                    account,
                    link_password,
                    password,
                } => match password {
                    Some(password) => self.on_cloud_upload_signed_in(
                        paths,
                        account,
                        link_password,
                        password,
                        widgets,
                        &sender,
                    ),
                    None => not_signed_in(root, &account.name),
                },

                ComposeInput::CloudUploaded { name, result } => {
                    self.on_cloud_uploaded(name, result, widgets, &sender, root)
                }

                ComposeInput::RemoveCloudLink(i) => self.on_remove_cloud_link(i, widgets, &sender),

                ComposeInput::CopyCloudPasswords => self.on_copy_cloud_passwords(),

                ComposeInput::PickSendTime => {
                    let parent = root.root().and_downcast::<gtk::Window>();
                    pick_send_time(parent.as_ref(), self.send_at, sender.input_sender().clone());
                }

                ComposeInput::DeleteDraft => self.on_delete_draft(&sender),

                ComposeInput::ToggleWindowed => {
                    let _ = sender.output(ComposeOutput::ToggleWindow(self.compose_id));
                }

                ComposeInput::SetDecorations(on) => self.on_set_decorations(on, widgets),

                ComposeInput::SetWindowed(windowed) => self.on_set_windowed(windowed, widgets),

                ComposeInput::FocusInitial => self.focus_initial(widgets),

                ComposeInput::MarkFieldsDirty => self.fields_dirty = true,

                ComposeInput::SaveDraftIfDirty => self.on_save_draft_if_dirty(&sender),

                ComposeInput::OpenContacts => self.on_open_contacts(widgets, &sender, root),

                ComposeInput::AttachFiles => self.on_attach_files(&sender, root),

                ComposeInput::AddAttachments(paths) => {
                    self.on_add_attachments(paths, widgets, &sender)
                }

                ComposeInput::RemoveAttachment(i) => self.on_remove_attachment(i, widgets, &sender),

                ComposeInput::Zoom(step) => self.on_zoom(step),

                ComposeInput::History { redo } => {
                    self.history_step(redo, widgets, &sender);
                }

                ComposeInput::ShowcaseHistory(step) => {
                    self.showcase_history(step, widgets, &sender);
                }

                ComposeInput::ShowcaseFrom(n) => {
                    widgets.from_row.set_selected(n);
                }

                ComposeInput::BodyHistory(can_undo) => self.on_body_history(can_undo, &sender),

                ComposeInput::CustomFrom(on) => self.on_custom_from(on, widgets),

                ComposeInput::AccountChanged => self.on_account_changed(widgets),

                ComposeInput::InsertSignature(place) => self.on_insert_signature(place, widgets),

                ComposeInput::SignatureClicked => self.on_signature_clicked(widgets, &sender),

                ComposeInput::UseSignature(choice) => self.on_use_signature(choice, widgets),

                ComposeInput::SignatureMenu => self.on_signature_menu(&sender),

                ComposeInput::Suggest(field) => self.on_suggest(field, widgets, &sender),

                ComposeInput::DirectoriesKnown { graph, own_mail } => {
                    self.graph_directories = std::sync::Arc::new(graph);
                    self.own_mail = own_mail;
                }

                ComposeInput::SuggestionsLoaded(loaded) => {
                    self.on_suggestions_loaded(loaded, widgets)
                }

                ComposeInput::DirectoryLookup {
                    field,
                    token,
                    generation,
                } => self.on_directory_lookup(field, token, generation, &sender),

                ComposeInput::DirectoryResults {
                    field,
                    token,
                    generation,
                    matches,
                    failed,
                } => {
                    self.report_directory_failures(failed, &sender);
                    self.on_directory_results(field, token, generation, matches, widgets);
                }

                ComposeInput::AddSuggestions { account_id, new } => {
                    self.on_add_suggestions(account_id, new)
                }

                ComposeInput::CompletionMove(delta) => self.on_completion_move(delta),

                ComposeInput::CompletionAccept => self.on_completion_accept(widgets),

                ComposeInput::CompletionClose => {
                    self.completion_open.set(false);
                    self.completion.popdown();
                }

                ComposeInput::ToggleSign(on) => self.on_toggle_sign(on),

                ComposeInput::FormatMenu => self.on_format_menu(&sender),

                ComposeInput::TranslateMenu => self.on_translate_menu(&sender),

                ComposeInput::ShowTranslateMenu(found) => {
                    self.on_show_translate_menu(found, &sender)
                }

                ComposeInput::TranslateTo(target) => self.on_translate_to(target, &sender, root),

                ComposeInput::TranslateBegun { found, target } => {
                    self.on_translate_begun(found, target, widgets, &sender, root)
                }

                ComposeInput::TranslateToggle { original } => {
                    self.on_translate_toggle(original, widgets, &sender)
                }

                ComposeInput::Translated {
                    result,
                    how,
                    subject,
                } => self.on_translated(result, how, subject, widgets, root),

                ComposeInput::SetFormat(to) => self.on_set_format(to, &sender),

                ComposeInput::AskPlain => self.on_ask_plain(&sender, root),

                ComposeInput::ApplyFormat(to) => self.on_apply_format(to, &sender),

                ComposeInput::LoadAs { from, to, body } => self.on_load_as(from, to, body),

                ComposeInput::TogglePreview(on) => self.on_toggle_preview(on, &sender),

                ComposeInput::ShowPreview(body) => {
                    let html = match self.format {
                        ComposeFormat::Markdown => crate::markdown::to_html(&body),
                        _ => body,
                    };
                    self.editor.show_preview(Some(&html));
                }
                ComposeInput::ToggleEncrypt(on) => {
                    self.encrypt = on;
                    if on && !self.sign {
                        self.sign = true;
                        widgets.sign_btn.set_active(true);
                    }
                }
                ComposeInput::PgpChecked { checked, result } => {
                    self.on_pgp_checked(checked, result, widgets, &sender)
                }
                ComposeInput::Send => self.on_send(widgets, &sender),

                ComposeInput::SendBody(fields) => self.on_send_body(fields, &sender),

                msg @ (ComposeInput::SaveDraft | ComposeInput::SaveAsTemplate) => {
                    let as_template = matches!(msg, ComposeInput::SaveAsTemplate);
                    // A draft can be saved without recipients; just capture the fields.
                    let fields = self.draft_fields(widgets);
                    let s = sender.clone();
                    self.read_body_as(self.format, false, move |html, text| {
                        s.input_sender().emit(ComposeInput::SaveDraftBody {
                            fields: OutgoingFields {
                                html,
                                text,
                                ..fields.clone()
                            },
                            as_template,
                        });
                    });
                }

                ComposeInput::SaveDraftBody {
                    fields,
                    as_template,
                } => self.on_save_draft_body(fields, as_template, &sender),

                ComposeInput::AutoSave => self.on_auto_save(&sender),

                ComposeInput::AutoSaveRead => self.on_auto_save_read(widgets, &sender),

                ComposeInput::AutoSaveBody(fields) => self.on_auto_save_body(fields, &sender),

                ComposeInput::Autosaved { saved, origin } => self.on_autosaved(saved, origin),
            }
        }
        // Overriding `update_with_view` takes over relm4's default, which
        // ran `update_view` after every message; without this the `#[watch]`
        // bindings above only ever hold their `init` values.
        self.update_view(widgets, sender);
    }
}

impl Compose {}

/// The GtkText embedded somewhere inside a composite row — where Pango
/// attributes (the spell-check underlines) actually live.
fn inner_text(widget: &gtk::Widget) -> Option<gtk::Text> {
    if let Some(t) = widget.downcast_ref::<gtk::Text>() {
        return Some(t.clone());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(t) = inner_text(&c) {
            return Some(t);
        }
        child = c.next_sibling();
    }
    None
}
