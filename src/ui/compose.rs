//! Compose window: write a new message, reply, or forward.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::{ComposeFormat, SignaturePosition};
use crate::contacts::Suggestion;
use crate::i18n::{i18n, i18n_f, i18n_noop};
use crate::models::DraftOrigin;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::drop_zones::{DropChoice, DropContext, DropZones};
use crate::ui::recipient_row::RecipientRow;
use crate::ui::rich_editor::{self, js_escape, RichEditor, SourceKind};

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

use attachments::{attachment_label, AttachThumbs};
use cloud::{cloud_upload_dialog, html_escape};
use format::{format_icon, format_label, markdown_body, SHOWCASE_PLAIN_LOG, ZOOM};
use host::{
    confirm_discard_dialog, header_rows_width, set_toggle_icon, size_for_host,
    COMPOSE_ACTIONS_BREAKPOINT, COMPOSE_MIN_WIDTH,
};
pub(crate) use keys::composer_key;
use keys::{focus_is_entry, zoom_key};
use messages::OutgoingFields;
pub use messages::{ComposeInput, ComposeOutput};
use send::{
    identity_display, next_monday, pgp_send_check, pick_send_time, preset_time, same_from,
    AUTOSAVE_SECS,
};
use signature::sig_html;
use suggest::{complete_field, merge_loaded_suggestions, recipient_focused, recipient_token};
pub use translate::TranslateHow;
use translate::{
    TR_CANCEL, TR_QUOTE_TEXT, TR_RICH_BEGIN, TR_RICH_END, TR_SOURCE_BEGIN, TR_SOURCE_END,
};
use types::{CloudLink, ComposeStep, ComposeUndoEntry};
pub use types::{ComposeAccount, ComposeInit, ComposePrefill, Field, OwnMail, OwnOnly, SigPlace};

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
                                                b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
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
                                                b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
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
                                                b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
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
                                                b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                                sender.input_sender().emit(ComposeInput::SendAt(preset_time(1, 8)));
                                            },
                                        },
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n_f("Tomorrow afternoon ({time})", &[("time", &crate::datefmt::clock_label(13))]), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                                sender.input_sender().emit(ComposeInput::SendAt(preset_time(1, 13)));
                                            },
                                        },
                                        gtk::Button {
                                            add_css_class: "flat",
                                            add_css_class: "context-menu-item",
                                            #[wrap(Some)]
                                            set_child = &gtk::Label { set_label: &i18n_f("Monday morning ({time})", &[("time", &crate::datefmt::clock_label(8))]), set_halign: gtk::Align::Start },
                                            connect_clicked[sender] => move |b| {
                                                b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
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
                                                b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
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
        'handle: {
            match message {
                ComposeInput::Cancel => {
                    if self.asking_discard {
                        break 'handle;
                    }
                    if self.fields_dirty {
                        sender.input_sender().emit(ComposeInput::ConfirmDiscard);
                    } else {
                        let s = sender.clone();
                        self.editor.is_dirty(move |body_dirty| {
                            s.input_sender().emit(if body_dirty {
                                ComposeInput::ConfirmDiscard
                            } else {
                                ComposeInput::Discard
                            });
                        });
                    }
                }

                ComposeInput::ConfirmDiscard => {
                    if self.asking_discard {
                        break 'handle;
                    }
                    self.asking_discard = true;
                    let parent = root.root().and_downcast::<gtk::Window>();
                    confirm_discard_dialog(
                        parent.as_ref(),
                        self.template,
                        sender.input_sender().clone(),
                    );
                }

                ComposeInput::KeepEditing => self.asking_discard = false,

                ComposeInput::Discard => {
                    // A message nobody saved leaves nothing behind, though the
                    // composer saved it on its own meanwhile (#340).
                    match self.draft_origin.clone().filter(|_| self.autosave_made) {
                        Some(origin) => {
                            let _ = sender.output(ComposeOutput::DiscardAutosave {
                                id: self.compose_id,
                                origin,
                            });
                        }
                        None => {
                            let _ = sender.output(ComposeOutput::Close(self.compose_id));
                        }
                    }
                }

                ComposeInput::SendAt(at) => {
                    self.send_at = Some(at);
                    sender.input_sender().emit(ComposeInput::Send);
                }

                ComposeInput::ClearSendAt => {
                    self.send_at = None;
                }

                ComposeInput::FocusField(field) => {
                    if !widgets.fields_list.is_visible() {
                        // The chevron's toggle sends ShowFields, which records it.
                        widgets.fields_btn.set_active(true);
                        widgets.fields_list.set_visible(true);
                    }
                    let row = match field {
                        Field::To => &widgets.to_row,
                        Field::Cc => &widgets.cc_row,
                        Field::Bcc => &widgets.bcc_row,
                    };
                    row.set_visible(true);
                    row.grab_focus();
                }

                ComposeInput::ShowFields(on) => {
                    self.fields_shown = on;
                    widgets
                        .fields_list
                        .set_visible(!(self.compact && !self.windowed) || on);
                }

                ComposeInput::SetNarrow(narrow) => {
                    self.narrow = narrow;
                    self.dress_format_buttons();
                    // Folded, the header is as narrow as it gets: the pane goes
                    // no narrower, so a small window stops shrinking rather than
                    // cutting Send off at its edge. Unfolded, the floor drops
                    // back, or the pane could never get narrow enough to fold.
                    let (root, header) = (root.clone(), widgets.header.clone());
                    gtk::glib::idle_add_local_once(move || {
                        let floor = if narrow {
                            header
                                .measure(gtk::Orientation::Horizontal, -1)
                                .0
                                .max(COMPOSE_MIN_WIDTH)
                        } else {
                            COMPOSE_MIN_WIDTH
                        };
                        root.set_size_request(floor, 200);
                    });
                }

                ComposeInput::OverflowMenu => {
                    let entry = |label: String, icon: &str, msg: fn() -> ComposeInput| {
                        let s = sender.clone();
                        MenuEntry::new(&label, move || s.input_sender().emit(msg()))
                            .icon(&format!("{icon}-symbolic"))
                    };
                    // Save Draft, Save as Template and Delete Draft are not here:
                    // the Save Draft button and its arrow stay in the header at
                    // any width.
                    let mut attach = vec![entry(i18n("Attach files"), "mail-attachment", || {
                        ComposeInput::AttachFiles
                    })];
                    if !self.cloud_accounts.is_empty() {
                        attach.push(
                            entry(
                                i18n("Upload to cloud storage and share a link"),
                                "cloud",
                                || ComposeInput::CloudAttach,
                            )
                            .enabled(self.cloud_busy == 0),
                        );
                    }
                    attach.push(entry(
                        i18n("Open Contacts"),
                        "x-office-address-book",
                        || ComposeInput::OpenContacts,
                    ));
                    // The OpenPGP toggles go through their buttons so the
                    // toggled handlers keep the model and the buttons agreeing.
                    let mut pgp = Vec::new();
                    if crate::pgp::available() {
                        let check = |on: bool, icon: &str| {
                            if on { "verified-checkmark" } else { icon }.to_string()
                        };
                        let sign = widgets.sign_btn.clone();
                        pgp.push(
                            MenuEntry::new(&i18n("Sign with your OpenPGP key"), move || {
                                sign.set_active(!sign.is_active())
                            })
                            .icon(&format!("{}-symbolic", check(self.sign, "security-high"))),
                        );
                        let encrypt = widgets.encrypt_btn.clone();
                        pgp.push(
                            MenuEntry::new(
                                &i18n("Encrypt with OpenPGP to every recipient's key"),
                                move || encrypt.set_active(!encrypt.is_active()),
                            )
                            .icon(&format!(
                                "{}-symbolic",
                                check(self.encrypt, "channel-secure")
                            )),
                        );
                    }
                    // The format chooser, the preview toggle and Insert Signature
                    // are not here: they sit at the end of the formatting row,
                    // which a narrow pane never folds away.
                    let mut host = Vec::new();
                    if self.can_toggle {
                        host.push(if self.windowed {
                            entry(i18n("Collapse into reader"), "view-restore", || {
                                ComposeInput::ToggleWindowed
                            })
                        } else {
                            entry(i18n("Open in window"), "view-fullscreen", || {
                                ComposeInput::ToggleWindowed
                            })
                        });
                    }
                    let btn = &widgets.overflow_btn;
                    show_context_menu(
                        btn,
                        (btn.width() / 2) as f64,
                        btn.height() as f64,
                        vec![attach, pgp, host],
                    );
                }

                ComposeInput::CloudAttach => {
                    let dialog = gtk::FileDialog::new();
                    dialog.set_title(&i18n("Upload to Cloud Storage"));
                    let parent = root.root().and_downcast::<gtk::Window>();
                    let s = sender.input_sender().clone();
                    dialog.open_multiple(
                        parent.as_ref(),
                        gtk::gio::Cancellable::NONE,
                        move |res| {
                            if let Ok(model) = res {
                                let paths: Vec<_> = (0..model.n_items())
                                    .filter_map(|i| {
                                        model.item(i).and_downcast::<gtk::gio::File>()?.path()
                                    })
                                    .collect();
                                if !paths.is_empty() {
                                    let _ = s.send(ComposeInput::CloudPicked(paths));
                                }
                            }
                        },
                    );
                }

                ComposeInput::DroppedFiles(choice, paths) => match choice {
                    DropChoice::Attach => sender
                        .input_sender()
                        .emit(ComposeInput::AddAttachments(paths)),
                    DropChoice::Inline => self.editor.insert_files(&paths),
                    DropChoice::Cloud => {
                        sender.input_sender().emit(ComposeInput::CloudPicked(paths))
                    }
                },

                ComposeInput::CloudPicked(paths) => {
                    let parent = root.root().and_downcast::<gtk::Window>();
                    cloud_upload_dialog(
                        parent.as_ref(),
                        &self.cloud_accounts,
                        paths,
                        sender.input_sender().clone(),
                    );
                }

                ComposeInput::CloudUpload {
                    paths,
                    account,
                    link_password,
                } => {
                    if !account.has_secret() {
                        sender
                            .input_sender()
                            .emit(ComposeInput::CloudUploadSignedIn {
                                paths,
                                account,
                                link_password,
                                password: Some(String::new()),
                            });
                        break 'handle;
                    }
                    // Off the main thread: the keyring may wait on its user.
                    let key = account.key();
                    let s = sender.input_sender().clone();
                    crate::config::keyring_then(
                        move || crate::config::load_cloud_password(&key),
                        move |password| {
                            s.emit(ComposeInput::CloudUploadSignedIn {
                                paths,
                                account,
                                link_password,
                                password,
                            })
                        },
                    );
                }

                ComposeInput::CloudUploadSignedIn {
                    paths,
                    account,
                    link_password,
                    password,
                } => {
                    let Some(password) = password else {
                        let parent = root.root().and_downcast::<gtk::Window>();
                        let d = adw::MessageDialog::new(
                        parent.as_ref(),
                        Some(i18n("Not signed in").as_str()),
                        Some(i18n_f("The sign-in for {name} is not in the keyring. Open Settings, Cloud Storage, and enter it again.", &[("name", &account.name)]).as_str()),
                    );
                        d.add_response("ok", &i18n("OK"));
                        d.present();
                        break 'handle;
                    };
                    for path in paths {
                        let name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        self.cloud_busy += 1;
                        self.rebuild_attachments(&widgets.attach_box, &sender);
                        let s = sender.input_sender().clone();
                        let (a, pw, lp) =
                            (account.clone(), password.clone(), link_password.clone());
                        std::thread::spawn(move || {
                            let result =
                                crate::cloud::upload_and_share(&a, &pw, &path, lp.as_deref());
                            let _ = s.send(ComposeInput::CloudUploaded { name, result });
                        });
                    }
                }

                ComposeInput::CloudUploaded { name, result } => {
                    self.cloud_busy = self.cloud_busy.saturating_sub(1);
                    match result {
                        Ok(share) => {
                            let id = format!(
                                "hylki-cloud-{}",
                                crate::rng::token(8).unwrap_or_else(|_| share.size.to_string())
                            );
                            let mut caption = crate::models::human_size(share.size);
                            if let Some(d) = &share.expires {
                                caption.push_str(&format!(
                                    ", {}",
                                    i18n_f("link expires {date}", &[("date", d)])
                                ));
                            }
                            if share.password.is_some() {
                                caption.push_str(&format!(", {}", i18n("password-protected")));
                            }
                            let html = format!(
                            "<p id=\"{id}\" data-hylki-cloud=\"1\">\u{1F4CE} <a href=\"{url}\">{name}</a> ({caption})</p>",
                            url = html_escape(&share.url),
                            name = html_escape(&share.name),
                            caption = html_escape(&caption),
                        );
                            // Into the body where the user's own text ends: above
                            // the signature, and above a quoted original in a
                            // reply, so the link reads as part of the message.
                            if let Some(kind) = self.editor.source_kind() {
                                let line = match kind {
                                    SourceKind::Markdown => format!(
                                        "\u{1F4CE} [{name}]({url}) ({caption})",
                                        name = share.name,
                                        url = share.url,
                                        caption = caption,
                                    ),
                                    SourceKind::Html => html.clone(),
                                };
                                // The same place, said in text: above the `-- `
                                // signature line if there is one, else at the end.
                                self.editor.run_js(&format!(
                                "(function(){{var t=document.getElementById('src');if(!t)return;\
                                 var l='{}';var v=t.value;var i=v.lastIndexOf('\\n-- \\n');\
                                 if(i>=0){{v=v.slice(0,i)+'\\n'+l+'\\n'+v.slice(i);}}\
                                 else{{v=v.replace(/\\s*$/,'')+'\\n\\n'+l+'\\n';}}\
                                 t.value=v;window.__hylkiDirty=true;}})()",
                                js_escape(&line)
                            ));
                                if let Some(p) = share.password.clone() {
                                    self.cloud_passwords.push((share.name.clone(), p));
                                }
                                self.cloud_links.push(CloudLink {
                                    id,
                                    name: share.name,
                                    url: share.url,
                                });
                                self.rebuild_attachments(&widgets.attach_box, &sender);
                                break 'handle;
                            }
                            self.editor.run_js(&format!(
                            "(function(){{var d=document.createElement('div');d.innerHTML='{}';\
                             var p=d.firstChild;var b=document.body;\
                             var first=null;var cands=b.querySelectorAll('.hylki-sig,.hylki-quote-attr,blockquote');\
                             for(var i=0;i<cands.length;i++){{var t=cands[i];while(t.parentNode&&t.parentNode!==b)t=t.parentNode;\
                             if(t.parentNode===b&&(!first||(t.compareDocumentPosition(first)&Node.DOCUMENT_POSITION_FOLLOWING)))first=t;}}\
                             if(first)b.insertBefore(p,first);else b.appendChild(p);\
                             window.__hylkiPlainFix&&window.__hylkiPlainFix();\
                             document.dispatchEvent(new Event('input'));}})()",
                            js_escape(&html)
                        ));
                            if let Some(p) = share.password.clone() {
                                self.cloud_passwords.push((share.name.clone(), p));
                            }
                            self.cloud_links.push(CloudLink {
                                id,
                                name: share.name,
                                url: share.url,
                            });
                        }
                        Err(e) => {
                            let parent = root.root().and_downcast::<gtk::Window>();
                            let d = adw::MessageDialog::new(
                                parent.as_ref(),
                                Some(
                                    i18n_f("Could not upload {name}", &[("name", &name)]).as_str(),
                                ),
                                Some(&e),
                            );
                            d.add_response("ok", &i18n("OK"));
                            d.present();
                        }
                    }
                    self.rebuild_attachments(&widgets.attach_box, &sender);
                }

                ComposeInput::RemoveCloudLink(i) => {
                    if i < self.cloud_links.len() {
                        let link = self.cloud_links.remove(i);
                        self.cloud_passwords.retain(|(n, _)| *n != link.name);
                        // Source mode has no element to remove: the line that
                        // names the link is what goes.
                        if self.editor.source_kind().is_some() {
                            self.editor.run_js(&format!(
                            "(function(){{var t=document.getElementById('src');if(!t)return;\
                             var u='{}';\
                             t.value=t.value.split('\\n').filter(function(l){{return l.indexOf(u)<0;}})\
                               .join('\\n');window.__hylkiDirty=true;}})()",
                            js_escape(&link.url)
                        ));
                            self.rebuild_attachments(&widgets.attach_box, &sender);
                            break 'handle;
                        }
                        self.editor.run_js(&format!(
                        "(function(){{var p=document.getElementById('{}');if(p)p.remove();document.dispatchEvent(new Event('input'));}})()",
                        js_escape(&link.id)
                    ));
                        self.rebuild_attachments(&widgets.attach_box, &sender);
                    }
                }

                ComposeInput::CopyCloudPasswords => {
                    let text = self
                        .cloud_passwords
                        .iter()
                        .map(|(n, p)| format!("{n}: {p}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if let Some(display) = gtk::gdk::Display::default() {
                        display.clipboard().set_text(&text);
                    }
                }

                ComposeInput::PickSendTime => {
                    let parent = root.root().and_downcast::<gtk::Window>();
                    pick_send_time(parent.as_ref(), self.send_at, sender.input_sender().clone());
                }

                ComposeInput::DeleteDraft => {
                    if let Some(origin) = self.draft_origin.clone() {
                        let _ = sender.output(ComposeOutput::DeleteDraft {
                            id: self.compose_id,
                            origin,
                        });
                    }
                }

                ComposeInput::ToggleWindowed => {
                    let _ = sender.output(ComposeOutput::ToggleWindow(self.compose_id));
                }

                ComposeInput::SetDecorations(on) => {
                    self.decorations = on;
                    size_for_host(
                        &widgets.toolbar_root,
                        &widgets.header,
                        &widgets.editor_holder,
                        self.windowed,
                        on,
                    );
                }

                ComposeInput::SetWindowed(windowed) => {
                    self.windowed = windowed;
                    set_toggle_icon(&widgets.toggle_btn, windowed);
                    size_for_host(
                        &widgets.toolbar_root,
                        &widgets.header,
                        &widgets.editor_holder,
                        windowed,
                        self.decorations,
                    );
                    // A compact reply grows its field rows back in a window (and
                    // sheds them again if it returns inline).
                    widgets
                        .fields_list
                        .set_visible(!(self.compact && !windowed) || self.fields_shown);
                }

                ComposeInput::FocusInitial => self.focus_initial(widgets),

                ComposeInput::MarkFieldsDirty => self.fields_dirty = true,

                ComposeInput::SaveDraftIfDirty => {
                    // Save only if the user actually edited something, so navigating
                    // away from a pristine quote-only reply doesn't litter Drafts.
                    if self.fields_dirty {
                        sender.input_sender().emit(ComposeInput::SaveDraft);
                    } else {
                        let s = sender.clone();
                        let id = self.compose_id;
                        self.editor.is_dirty(move |body_dirty| {
                            if body_dirty {
                                s.input_sender().emit(ComposeInput::SaveDraft);
                            } else {
                                let _ = s.output(ComposeOutput::Close(id));
                            }
                        });
                    }
                }

                ComposeInput::OpenContacts => {
                    // Browse contacts; the chosen one is appended to the To field.
                    let Some(win) = root.root().and_downcast::<gtk::Window>() else {
                        break 'handle;
                    };
                    let to_row = widgets.to_row.clone();
                    let s = sender.clone();
                    crate::ui::contacts_browser::present(
                        &win,
                        move |contact| {
                            to_row.add_recipient(&crate::worker::format_recipient(
                                &contact.name,
                                &contact.email,
                            ));
                        },
                        move || {
                            let _ = s.output(ComposeOutput::ShowContacts);
                        },
                    );
                }

                ComposeInput::AttachFiles => {
                    let dialog = gtk::FileDialog::new();
                    dialog.set_title(&i18n("Attach Files"));
                    let parent = root.root().and_downcast::<gtk::Window>();
                    let s = sender.input_sender().clone();
                    dialog.open_multiple(
                        parent.as_ref(),
                        gtk::gio::Cancellable::NONE,
                        move |res| {
                            if let Ok(model) = res {
                                let mut paths = Vec::new();
                                for i in 0..model.n_items() {
                                    if let Some(file) =
                                        model.item(i).and_downcast::<gtk::gio::File>()
                                    {
                                        if let Some(p) = file.path() {
                                            paths.push(p);
                                        }
                                    }
                                }
                                if !paths.is_empty() {
                                    let _ = s.send(ComposeInput::AddAttachments(paths));
                                }
                            }
                        },
                    );
                }

                ComposeInput::AddAttachments(paths) => {
                    if paths.is_empty() {
                        break 'handle;
                    }
                    self.fields_dirty = true;
                    let at = self.attachments.len();
                    let what = attachment_label(i18n_noop("Attach {name}"), &paths[0], paths.len());
                    self.attachments.extend(paths.iter().cloned());
                    self.push_history(
                        what,
                        ComposeStep::Remove {
                            at,
                            count: paths.len(),
                        },
                    );
                    self.rebuild_attachments(&widgets.attach_box, &sender);
                    self.report_history(&sender);
                }

                ComposeInput::RemoveAttachment(i) => {
                    if i < self.attachments.len() {
                        self.fields_dirty = true;
                        let path = self.attachments.remove(i);
                        let what = attachment_label(i18n_noop("Remove {name}"), &path, 1);
                        self.push_history(
                            what,
                            ComposeStep::Insert {
                                at: i,
                                paths: vec![path],
                            },
                        );
                        self.rebuild_attachments(&widgets.attach_box, &sender);
                        self.report_history(&sender);
                    }
                }

                ComposeInput::Zoom(step) => {
                    if !self.zoomable.get() {
                        break 'handle;
                    }
                    use crate::config::READER_ZOOM_STEPS as STEPS;
                    let now = ZOOM.load(std::sync::atomic::Ordering::Relaxed);
                    let at = STEPS.iter().position(|&z| z == now).unwrap_or(5) as i32;
                    let zoom = match step {
                        0 => 100,
                        s => {
                            STEPS[(at + i32::from(s.signum())).clamp(0, STEPS.len() as i32 - 1)
                                as usize]
                        }
                    };
                    ZOOM.store(zoom, std::sync::atomic::Ordering::Relaxed);
                    self.editor.set_zoom(zoom);
                }

                ComposeInput::History { redo } => {
                    self.history_step(redo, widgets, &sender);
                }

                ComposeInput::ShowcaseHistory(step) => {
                    self.showcase_history(step, widgets, &sender);
                }

                ComposeInput::ShowcaseFrom(n) => {
                    widgets.from_row.set_selected(n);
                }

                ComposeInput::BodyHistory(can_undo) => {
                    // A fresh edit takes its place in the order — one marker
                    // covers a whole run of typing, since the editor coalesces
                    // its own steps and this only has to say where they sit.
                    if can_undo
                        && !matches!(
                            self.undo_stack.last().map(|e| &e.step),
                            Some(ComposeStep::Body)
                        )
                    {
                        self.push_history(i18n("Typing"), ComposeStep::Body);
                    }
                    self.report_history(&sender);
                }

                ComposeInput::CustomFrom(on) => {
                    self.custom_from = on;
                    if on {
                        // Start from the address that was picked, to edit.
                        let idx = widgets.from_row.selected() as usize;
                        let start = self
                            .accounts
                            .get(idx)
                            .map(identity_display)
                            .unwrap_or_default();
                        if widgets.custom_from_row.text().trim().is_empty() {
                            widgets.custom_from_row.set_text(&start);
                        }
                        widgets.from_row.set_visible(false);
                        widgets.custom_from_row.set_visible(true);
                        widgets.custom_from_row.grab_focus();
                    } else {
                        widgets.custom_from_row.set_visible(false);
                        widgets.from_row.set_visible(true);
                    }
                }

                ComposeInput::AccountChanged => {
                    self.follow_account_signing(widgets);
                    let idx = widgets.from_row.selected() as usize;
                    // Another account may suggest from elsewhere: what a
                    // directory answered for the last one is not kept.
                    let account_changed = self
                        .accounts
                        .get(idx)
                        .is_some_and(|a| a.id != self.from_account);
                    if let Some(id) = self
                        .accounts
                        .get(idx)
                        .map(|a| a.id)
                        .filter(|id| *id != self.from_account)
                    {
                        self.from_account = id;
                        self.directory_generation = self.directory_generation.wrapping_add(1);
                        self.directory_matches.clear();
                    }
                    // Swap the editor's signature block for the new account's.
                    // An alias or another identity of the same account shares
                    // its signatures, so the one picked from them stays.
                    let Some(account) = self.accounts.get(idx) else {
                        break 'handle;
                    };
                    self.signature_btn
                        .set_visible(!account.signatures.is_empty());
                    let new_sig = account.signature.clone();
                    if !account_changed || new_sig == self.current_sig {
                        break 'handle;
                    }
                    self.put_signature(new_sig);
                }

                ComposeInput::InsertSignature(place) => {
                    let idx = widgets.from_row.selected() as usize;
                    let sig = self
                        .accounts
                        .get(idx)
                        .map(|a| self.chosen_signature(a))
                        .unwrap_or_default();
                    self.insert_signature(sig, place);
                }

                ComposeInput::SignatureClicked => {
                    let idx = widgets.from_row.selected() as usize;
                    let Some(account) = self.accounts.get(idx) else {
                        break 'handle;
                    };
                    if account.signatures.len() < 2 {
                        sender
                            .input_sender()
                            .emit(ComposeInput::InsertSignature(SigPlace::Cursor));
                        break 'handle;
                    }
                    // The one in the message is drawn as chosen; a body that
                    // started without one (Edit as New, a draft) may still
                    // carry the default's text, which is no harm to show.
                    let list: Vec<MenuEntry> = account
                        .signatures
                        .iter()
                        .enumerate()
                        .map(|(i, sig)| {
                            let s = sender.clone();
                            MenuEntry::new(&sig.name, move || {
                                s.input_sender().emit(ComposeInput::UseSignature(Some(i)))
                            })
                            .selected(!self.current_sig.is_empty() && sig.html == self.current_sig)
                        })
                        .collect();
                    let s = sender.clone();
                    let none = MenuEntry::new(i18n("No Signature"), move || {
                        s.input_sender().emit(ComposeInput::UseSignature(None))
                    })
                    .selected(self.current_sig.is_empty());
                    let btn = &self.signature_btn;
                    show_context_menu(
                        btn,
                        (btn.width() / 2) as f64,
                        btn.height() as f64,
                        vec![list, vec![none]],
                    );
                }

                ComposeInput::UseSignature(choice) => {
                    let idx = widgets.from_row.selected() as usize;
                    let sig = match choice {
                        Some(i) => match self.accounts.get(idx).and_then(|a| a.signatures.get(i)) {
                            Some(sig) => sig.html.clone(),
                            None => break 'handle,
                        },
                        None => String::new(),
                    };
                    if sig != self.current_sig {
                        self.put_signature(sig);
                    }
                }

                ComposeInput::SignatureMenu => {
                    let entry = |label: String, place: SigPlace| {
                        let s = sender.clone();
                        MenuEntry::new(&label, move || {
                            s.input_sender().emit(ComposeInput::InsertSignature(place))
                        })
                    };
                    let btn = &self.signature_btn;
                    show_context_menu(
                        btn,
                        (btn.width() / 2) as f64,
                        btn.height() as f64,
                        vec![vec![
                            entry(i18n("Insert Before the Text"), SigPlace::Start),
                            entry(i18n("Insert After the Text"), SigPlace::End),
                        ]],
                    );
                }

                ComposeInput::Suggest(field) => {
                    let row = match field {
                        Field::To => &widgets.to_row,
                        Field::Cc => &widgets.cc_row,
                        Field::Bcc => &widgets.bcc_row,
                    };
                    self.directory_generation = self.directory_generation.wrapping_add(1);
                    self.directory_matches.clear();
                    let token = recipient_token(row);
                    if token.chars().count() >= crate::directory::MIN_QUERY {
                        // A directory is asked once the typing pauses, not per key.
                        let generation = self.directory_generation;
                        let s = sender.input_sender().clone();
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(300),
                            move || {
                                let _ = s.send(ComposeInput::DirectoryLookup {
                                    field,
                                    token,
                                    generation,
                                });
                            },
                        );
                    }
                    self.show_completion(field, row, &widgets.bcc_row);
                }

                ComposeInput::DirectoriesKnown { graph, own_mail } => {
                    self.graph_directories = std::sync::Arc::new(graph);
                    self.own_mail = own_mail;
                }

                ComposeInput::SuggestionsLoaded(loaded) => {
                    tracing::debug!("composer suggestions loaded: {}", loaded.len());
                    merge_loaded_suggestions(&mut self.suggestions, loaded);
                    // The user may already be typing. Update that field's
                    // matches without touching its text, caret or dirty state.
                    for (field, row) in [
                        (Field::To, &widgets.to_row),
                        (Field::Cc, &widgets.cc_row),
                        (Field::Bcc, &widgets.bcc_row),
                    ] {
                        if recipient_focused(row) {
                            self.show_completion(field, row, &widgets.bcc_row);
                            break;
                        }
                    }
                }

                ComposeInput::DirectoryLookup {
                    field,
                    token,
                    generation,
                } => {
                    if generation == self.directory_generation {
                        let s = sender.input_sender().clone();
                        // An account that suggests only its own asks its
                        // directory alone, no LDAP and no other tenant.
                        let own = self.own_mail.get(&self.from_account);
                        let narrowed = own.is_some();
                        if let Some(own) = own {
                            tracing::debug!(
                                target: "hylki::graph",
                                "suggestions narrowed to {}: its directory and {} addresses from its mail",
                                own.account.email,
                                own.addresses.len()
                            );
                        }
                        let graph = match own {
                            Some(own) => std::sync::Arc::new(vec![own.account.clone()]),
                            None => self.graph_directories.clone(),
                        };
                        std::thread::spawn(move || {
                            // Both at once: the slower decides when the list
                            // grows, not the two one after the other.
                            let (matches, failed) = std::thread::scope(|scope| {
                                let org = (!graph.is_empty()).then(|| {
                                    scope.spawn(|| crate::graph_directory::search(&token, &graph))
                                });
                                let (mut matches, failed) = if narrowed {
                                    Default::default()
                                } else {
                                    crate::directory::search(&token)
                                };
                                if let Some(found) = org.and_then(|h| h.join().ok()) {
                                    matches.extend(found);
                                }
                                (matches, failed)
                            });
                            let _ = s.send(ComposeInput::DirectoryResults {
                                field,
                                token,
                                generation,
                                matches,
                                failed,
                            });
                        });
                    }
                }

                ComposeInput::DirectoryResults {
                    field,
                    token,
                    generation,
                    matches,
                    failed,
                } => {
                    // A directory that cannot be asked says so once, whatever
                    // came of the typing meanwhile.
                    for f in failed {
                        let text = f.text();
                        if self.directory_errors_shown.insert(text.clone()) {
                            let _ = sender.output(ComposeOutput::DirectoryError(text));
                        }
                    }
                    let row = match field {
                        Field::To => &widgets.to_row,
                        Field::Cc => &widgets.cc_row,
                        Field::Bcc => &widgets.bcc_row,
                    };
                    if generation == self.directory_generation
                        && recipient_token(row) == token
                        && !matches.is_empty()
                    {
                        self.directory_matches = matches;
                        if recipient_focused(row) {
                            self.show_completion(field, row, &widgets.bcc_row);
                        }
                    }
                }

                ComposeInput::AddSuggestions { account_id, new } => {
                    for n in new {
                        let key = n.email.to_lowercase();
                        if let Some(own) = self.own_mail.get_mut(&account_id) {
                            own.addresses.insert(key.clone());
                        }
                        match self
                            .suggestions
                            .iter_mut()
                            .find(|s| s.email.to_lowercase() == key)
                        {
                            Some(s) => {
                                s.score += 1;
                                if s.name.trim().is_empty() || s.name == s.email {
                                    s.name = n.name;
                                }
                            }
                            None => self.suggestions.push(n),
                        }
                    }
                }

                ComposeInput::CompletionMove(delta) => {
                    if self.completion_count == 0 {
                        break 'handle;
                    }
                    let max = self.completion_count as i32 - 1;
                    let new = (self.completion_selected as i32 + delta).clamp(0, max) as usize;
                    self.completion_selected = new;
                    if let Some(list) = &self.completion_list {
                        if let Some(row) = list.row_at_index(new as i32) {
                            list.select_row(Some(&row));
                        }
                    }
                }

                ComposeInput::CompletionAccept => {
                    let row = match self.completion_field {
                        Some(Field::To) => &widgets.to_row,
                        Some(Field::Cc) => &widgets.cc_row,
                        Some(Field::Bcc) => &widgets.bcc_row,
                        None => break 'handle,
                    };
                    let token = row.typed();
                    if !token.is_empty() {
                        let chosen = self
                            .ranked_matches(&token)
                            .into_iter()
                            .nth(self.completion_selected);
                        if let Some(sug) = chosen {
                            complete_field(row, &widgets.bcc_row, &sug);
                        }
                    }
                    self.completion_open.set(false);
                    self.completion.popdown();
                }

                ComposeInput::CompletionClose => {
                    self.completion_open.set(false);
                    self.completion.popdown();
                }

                ComposeInput::ToggleSign(on) => {
                    self.sign = on;
                    // Anything but the composer's own setting is a choice for
                    // this message, which a later From change must not undo.
                    if self.sign_expected.take() != Some(on) {
                        self.sign_touched = true;
                    }
                }

                ComposeInput::FormatMenu => {
                    let btn = &self.format_btn;
                    show_context_menu(
                        btn,
                        (btn.width() / 2) as f64,
                        btn.height() as f64,
                        vec![self.format_entries(&sender)],
                    );
                }

                ComposeInput::TranslateMenu => {
                    if self.translating {
                        break 'handle;
                    }
                    let s = sender.input_sender().clone();
                    let js = format!(
                        "JSON.stringify({{q:{TR_QUOTE_TEXT},t:window.__hylkiTrState||''}})"
                    );
                    self.editor.eval(&js, move |text| {
                        let _ = s.send(ComposeInput::ShowTranslateMenu(text));
                    });
                }

                ComposeInput::ShowTranslateMenu(found) => {
                    let v: serde_json::Value = serde_json::from_str(&found).unwrap_or_default();
                    let quoted = v["q"].as_str().unwrap_or_default().to_string();
                    let entry = |code: String, label: String| {
                        let s = sender.clone();
                        MenuEntry::new(label, move || {
                            s.input_sender()
                                .emit(ComposeInput::TranslateTo(code.clone()))
                        })
                    };
                    // Back to what was written, or to the translation again,
                    // while the translation is still the last thing done to the
                    // text (#327).
                    let toggle = match v["t"].as_str() {
                        Some("translated") => Some((i18n("Show Original"), true)),
                        Some("original") => Some((i18n("Show Translation"), false)),
                        _ => None,
                    };
                    let toggle = toggle.map(|(label, original)| {
                        let s = sender.clone();
                        vec![MenuEntry::new(label, move || {
                            s.input_sender()
                                .emit(ComposeInput::TranslateToggle { original })
                        })]
                    });
                    // First what the conversation suggests: the language of the
                    // message being answered, then the one last used.
                    let mut first = Vec::new();
                    let quoted = crate::translate::detect(&quoted);
                    if let Some(lang) = &quoted {
                        first.push(
                            entry(
                                lang.clone(),
                                i18n_f(
                                    "{language} (the quoted message)",
                                    &[("language", &crate::translate::language_name(lang))],
                                ),
                            )
                            .icon("mail-reply-sender-symbolic"),
                        );
                    }
                    if let Some(last) = crate::translate::last_compose_target() {
                        if quoted.as_deref().map(crate::translate::base)
                            != Some(crate::translate::base(&last))
                        {
                            first.push(
                                entry(last.clone(), crate::translate::language_name(&last))
                                    .icon("translate-symbolic"),
                            );
                        }
                    }
                    let all: Vec<MenuEntry> = crate::translate::LANGUAGES
                        .iter()
                        .map(|code| entry(code.to_string(), crate::translate::language_name(code)))
                        .collect();
                    let mut sections = if first.is_empty() {
                        vec![all]
                    } else {
                        vec![
                            first,
                            vec![MenuEntry::submenu(i18n("Other Languages"), vec![all])],
                        ]
                    };
                    if let Some(toggle) = toggle {
                        sections.insert(0, toggle);
                    }
                    let btn = &self.translate_btn;
                    crate::ui::context_menu::show_context_menu_with_header(
                        btn,
                        (btn.width() / 2) as f64,
                        btn.height() as f64,
                        Some(&i18n("Translate into")),
                        sections,
                    );
                }

                ComposeInput::TranslateTo(target) => {
                    if self.translating {
                        break 'handle;
                    }
                    // A message going out encrypted must not have its text
                    // handed to a third party on the way.
                    if self.encrypt {
                        let parent = root.root().and_downcast::<gtk::Window>();
                        let d = adw::MessageDialog::new(
                        parent.as_ref(),
                        Some(&i18n("Not translated")),
                        Some(&i18n("This message is set to be encrypted, so its text is not sent for translation.")),
                    );
                        d.add_response("ok", &i18n("OK"));
                        d.present();
                        break 'handle;
                    }
                    self.translating = true;
                    self.translate_btn.set_sensitive(false);
                    let js = match self.format {
                        ComposeFormat::Html => TR_SOURCE_BEGIN.replace("HTMLFLAG", "true"),
                        ComposeFormat::Markdown => TR_SOURCE_BEGIN.replace("HTMLFLAG", "false"),
                        _ => TR_RICH_BEGIN.to_string(),
                    };
                    let s = sender.input_sender().clone();
                    self.editor.eval(&js, move |found| {
                        let _ = s.send(ComposeInput::TranslateBegun { found, target });
                    });
                }

                ComposeInput::TranslateBegun { found, target } => {
                    let v: serde_json::Value = serde_json::from_str(&found).unwrap_or_default();
                    let how = match self.format {
                        ComposeFormat::Html => TranslateHow::Html,
                        ComposeFormat::Markdown => TranslateHow::Markdown,
                        _ => TranslateHow::Rich,
                    };
                    let parts: Vec<String> = match (&how, v["text"].as_str()) {
                        (TranslateHow::Html, Some(text)) => vec![text.to_string()],
                        (TranslateHow::Markdown, Some(text)) => {
                            crate::translate::text_to_parts(text)
                        }
                        _ => v["parts"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|p| p.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    };
                    if parts.is_empty() {
                        self.translating = false;
                        self.translate_btn.set_sensitive(true);
                        self.editor.run_js(TR_CANCEL);
                        if v["empty"].as_bool() == Some(true) {
                            let parent = root.root().and_downcast::<gtk::Window>();
                            let d = adw::MessageDialog::new(
                                parent.as_ref(),
                                Some(&i18n("Nothing to translate yet")),
                                Some(&i18n(
                                    "Write something first, or select the text to translate.",
                                )),
                            );
                            d.add_response("ok", &i18n("OK"));
                            d.present();
                        }
                        break 'handle;
                    }
                    // The subject goes too when the whole message is translated,
                    // unless it is a reply's or a forward's: that one is already
                    // in the conversation's language.
                    let subject = widgets.subject_row.text().trim().to_string();
                    let subject = (v["sel"].as_bool() != Some(true)
                        && !subject.is_empty()
                        && self.in_reply_to.trim().is_empty()
                        && !crate::models::is_reply_or_forward_subject(&subject))
                    .then_some(subject);
                    let mut parts = parts;
                    if let Some(subject) = &subject {
                        parts.push(gtk::glib::markup_escape_text(subject).to_string());
                    }
                    let s = sender.input_sender().clone();
                    std::thread::spawn(move || {
                        let settings = crate::translate::load();
                        let key = crate::translate::load_key(settings.service).unwrap_or_default();
                        let result =
                            crate::translate::translate_parts(&settings, &key, &parts, &target);
                        let _ = s.send(ComposeInput::Translated {
                            result,
                            how,
                            subject,
                        });
                    });
                }

                ComposeInput::TranslateToggle { original } => {
                    self.history_step(!original, widgets, &sender);
                    if let Some((written, translated)) = &self.tr_subject {
                        let (from, to) = if original {
                            (translated, written)
                        } else {
                            (written, translated)
                        };
                        if widgets.subject_row.text().as_str() == from.as_str() {
                            widgets.subject_row.set_text(to);
                        }
                    }
                }

                ComposeInput::Translated {
                    result,
                    how,
                    subject,
                } => {
                    self.translating = false;
                    self.translate_btn.set_sensitive(true);
                    match result {
                        // One edit, so Ctrl+Z gives back what was written.
                        Ok(mut parts) => {
                            self.tr_subject = None;
                            if let Some(original) = subject {
                                let translated = parts.pop().map(|t| {
                                    crate::markdown::plain_text(&t)
                                        .split_whitespace()
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                });
                                if let Some(translated) = translated.filter(|t| !t.is_empty()) {
                                    widgets.subject_row.set_text(&translated);
                                    self.tr_subject = Some((original, translated));
                                }
                            }
                            let (end, text) = match how {
                                TranslateHow::Rich => (TR_RICH_END, parts.concat()),
                                TranslateHow::Html => (TR_SOURCE_END, parts.concat()),
                                TranslateHow::Markdown => {
                                    (TR_SOURCE_END, crate::translate::parts_to_text(&parts))
                                }
                            };
                            let arg = serde_json::to_string(&text).unwrap_or_else(|_| "''".into());
                            self.editor.run_js(&end.replace("ARG", &arg));
                        }
                        Err(why) => {
                            self.editor.run_js(TR_CANCEL);
                            let parent = root.root().and_downcast::<gtk::Window>();
                            let d = adw::MessageDialog::new(
                                parent.as_ref(),
                                Some(&i18n("Could not translate")),
                                Some(&why),
                            );
                            d.add_response("ok", &i18n("OK"));
                            d.present();
                        }
                    }
                }

                ComposeInput::SetFormat(to) => {
                    if to == self.format {
                        break 'handle;
                    }
                    // Plain text shows only what is sent (#424), so moving a
                    // formatted body there drops what it cannot show, and the
                    // way back does not bring it back.
                    if self.format == ComposeFormat::Rich && to == ComposeFormat::Plain {
                        let s = sender.clone();
                        self.editor.eval(
                            "String(!!(window.__hylkiPlainLoses&&window.__hylkiPlainLoses()))",
                            move |loses| {
                                s.input_sender().emit(if loses == "true" {
                                    ComposeInput::AskPlain
                                } else {
                                    ComposeInput::ApplyFormat(ComposeFormat::Plain)
                                });
                            },
                        );
                        break 'handle;
                    }
                    sender.input_sender().emit(ComposeInput::ApplyFormat(to));
                }

                ComposeInput::AskPlain => {
                    let dialog = adw::AlertDialog::new(
                        Some(&i18n("Switch to plain text?")),
                        Some(&i18n("Formatting, images and links are removed.")),
                    );
                    dialog.add_response("cancel", &i18n("Cancel"));
                    dialog.add_response("switch", &i18n("Switch"));
                    dialog.set_response_appearance("switch", adw::ResponseAppearance::Destructive);
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");
                    let s = sender.input_sender().clone();
                    dialog.connect_response(None, move |_, resp| {
                        if resp == "switch" {
                            let _ = s.send(ComposeInput::ApplyFormat(ComposeFormat::Plain));
                        }
                    });
                    dialog.present(Some(root));
                    // HYLKI_SHOWCASE_PLAIN_ANSWER=switch|cancel answers it two
                    // seconds on (demo only), for a check without a pointer.
                    if let (Ok(answer), Some(_)) = (
                        std::env::var("HYLKI_SHOWCASE_PLAIN_ANSWER"),
                        std::env::var_os("HYLKI_DEMO"),
                    ) {
                        tracing::info!("showcase plain: asked before switching");
                        let (d, s) = (dialog.clone(), sender.input_sender().clone());
                        gtk::glib::timeout_add_seconds_local_once(2, move || {
                            d.force_close();
                            if answer == "switch" {
                                let _ = s.send(ComposeInput::ApplyFormat(ComposeFormat::Plain));
                            }
                        });
                    }
                }

                ComposeInput::ApplyFormat(to) => {
                    let from = self.format;
                    if to == from {
                        break 'handle;
                    }
                    self.format = to;
                    self.editor
                        .set_formatting_visible(to == ComposeFormat::Rich);
                    self.zoomable.set(to != ComposeFormat::Rich);
                    self.editor.set_zoom(if self.zoomable.get() {
                        ZOOM.load(std::sync::atomic::Ordering::Relaxed)
                    } else {
                        100
                    });
                    if let Some(zones) = &self.drop_zones {
                        zones.set_allow_inline(to == ComposeFormat::Rich);
                    }
                    self.dress_format_buttons();
                    self.preview_btn.set_visible(to.is_source());
                    // A preview of the old format's render would be a lie about
                    // the new one.
                    self.preview_btn.set_active(false);
                    // Every change reloads the body, plain text included: it
                    // shows the message flattened to what is sent (#424), so it
                    // cannot share the rich document's undo history.
                    let s = sender.clone();
                    self.read_body_as(from, false, move |body, _| {
                        s.input_sender()
                            .emit(ComposeInput::LoadAs { from, to, body });
                    });
                }

                ComposeInput::LoadAs { from, to, body } => {
                    // Every conversion goes through HTML: it is the one form
                    // all four formats can be written to and read back from.
                    let html = match from {
                        ComposeFormat::Markdown => crate::markdown::to_html(&body),
                        _ => body,
                    };
                    self.editor.set_plain(to == ComposeFormat::Plain);
                    match to {
                        ComposeFormat::Markdown => self
                            .editor
                            .set_source(SourceKind::Markdown, &markdown_body(&html)),
                        ComposeFormat::Html => self
                            .editor
                            .set_source(SourceKind::Html, &crate::markdown::pretty_html(&html)),
                        ComposeFormat::Rich | ComposeFormat::Plain => self.editor.set_html(&html),
                    }
                    self.editor.grab_focus();
                    // HYLKI_SHOWCASE_PLAIN_LOG logs the reloaded body, its font
                    // and the text it sends (demo only).
                    if std::env::var_os("HYLKI_SHOWCASE_PLAIN_LOG").is_some()
                        && std::env::var_os("HYLKI_DEMO").is_some()
                    {
                        let ed = self.editor.clone();
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(1500),
                            move || {
                                ed.eval(SHOWCASE_PLAIN_LOG, move |v| {
                                    tracing::info!("showcase plain: {to:?} {v}")
                                });
                            },
                        );
                    }
                }

                ComposeInput::TogglePreview(on) => {
                    // Keep the button with the state, for the times the message
                    // arrives from somewhere other than a click on it.
                    if self.preview_btn.is_active() != on {
                        self.preview_btn.set_active(on);
                    }
                    if !on {
                        self.editor.show_preview(None);
                        break 'handle;
                    }
                    let s = sender.clone();
                    self.read_body_as(self.format, false, move |body, _| {
                        s.input_sender().emit(ComposeInput::ShowPreview(body));
                    });
                }

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
                    self.pgp_checking = false;
                    match result {
                        Ok(()) => {
                            self.pgp_cleared = Some(checked);
                            sender.input_sender().emit(ComposeInput::Send);
                        }
                        Err(problem) => {
                            let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
                            let dialog = adw::MessageDialog::new(
                                parent.as_ref(),
                                Some(&i18n("Cannot send with OpenPGP")),
                                Some(&problem),
                            );
                            dialog.add_response("ok", &i18n("OK"));
                            dialog.present();
                        }
                    }
                }
                ComposeInput::Send => {
                    let to = widgets.to_row.text().trim().to_string();
                    if to.is_empty() {
                        widgets.to_row.add_css_class("error");
                        break 'handle;
                    }
                    // The composer closes on Send, so an address the message
                    // cannot go to is caught here, while it can still be fixed
                    // (#368). Invisible characters pasted along with an address
                    // are taken out first, where the user can see the result.
                    for row in [&widgets.to_row, &widgets.cc_row, &widgets.bcc_row] {
                        if let Some(clean) = crate::worker::clean_recipient_field(&row.text()) {
                            row.set_text(&clean);
                        }
                    }
                    if let Some(clean) =
                        crate::worker::clean_recipient_field(&widgets.reply_to_row.text())
                    {
                        widgets.reply_to_row.set_text(&clean);
                    }
                    let rows: [(&gtk::Widget, String); 4] = [
                        (widgets.to_row.upcast_ref(), widgets.to_row.text()),
                        (widgets.cc_row.upcast_ref(), widgets.cc_row.text()),
                        (widgets.bcc_row.upcast_ref(), widgets.bcc_row.text()),
                        (
                            widgets.reply_to_row.upcast_ref(),
                            widgets.reply_to_row.text().to_string(),
                        ),
                    ];
                    if let Some((row, addr)) = rows.iter().find_map(|(row, text)| {
                        crate::worker::invalid_recipient(text).map(|addr| (row, addr))
                    }) {
                        row.add_css_class("error");
                        let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
                        let dialog = adw::MessageDialog::new(
                            parent.as_ref(),
                            Some(&i18n("Check the address")),
                            Some(&i18n_f(
                                "“{addr}” is not an email address.",
                                &[("addr", &addr)],
                            )),
                        );
                        dialog.add_response("ok", &i18n("OK"));
                        dialog.present();
                        break 'handle;
                    }
                    let to = widgets.to_row.text().trim().to_string();
                    // A file moved or deleted since it was attached (#340): the
                    // composer closes on Send, so this is the last moment the
                    // message can still be fixed rather than lost.
                    let missing: Vec<String> = self
                        .attachments
                        .iter()
                        .filter(|p| std::fs::File::open(p).is_err())
                        .map(|p| {
                            p.file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default()
                        })
                        .collect();
                    if !missing.is_empty() {
                        self.rebuild_attachments(&widgets.attach_box, &sender);
                        let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
                        let dialog = adw::MessageDialog::new(
                        parent.as_ref(),
                        Some(&i18n("An attachment is missing")),
                        Some(&i18n_f(
                            "Hylki cannot read {names}. It was moved or deleted after it was attached. Remove it, or attach it again from where it is now.",
                            &[("names", &missing.join(", "))],
                        )),
                    );
                        dialog.add_response("ok", &i18n("OK"));
                        dialog.present();
                        break 'handle;
                    }
                    let cc = widgets.cc_row.text().trim().to_string();
                    let bcc = widgets.bcc_row.text().trim().to_string();
                    let reply_to = widgets.reply_to_row.text().trim().to_string();
                    let subject = widgets.subject_row.text().to_string();
                    let idx = widgets.from_row.selected() as usize;
                    // OpenPGP (#133): say what is missing before anything leaves.
                    // gpg answers on a worker thread, since a stale keyring lock
                    // makes it wait about ten seconds (#316); its answer sends
                    // again, and passes here when nothing has changed since.
                    if self.sign || self.encrypt {
                        let from = self.accounts.get(idx);
                        let from_email = from.map(|a| a.email.clone()).unwrap_or_default();
                        let key = from.and_then(|a| a.pgp_key.clone());
                        let encrypt = self.encrypt;
                        let fields = vec![to.clone(), cc.clone(), bcc.clone()];
                        let mut checked = vec![
                            from_email.clone(),
                            key.clone().unwrap_or_default(),
                            encrypt.to_string(),
                        ];
                        checked.extend(fields.iter().cloned());
                        if self.pgp_cleared.take().as_ref() != Some(&checked) {
                            if !self.pgp_checking {
                                self.pgp_checking = true;
                                let s = sender.clone();
                                gtk::glib::spawn_future_local(async move {
                                    let result = gtk::gio::spawn_blocking(move || {
                                        let fields: Vec<&str> =
                                            fields.iter().map(String::as_str).collect();
                                        pgp_send_check(
                                            &from_email,
                                            key.as_deref(),
                                            &fields,
                                            encrypt,
                                        )
                                    })
                                    .await
                                    .unwrap_or_else(|_| Err("task failed".into()));
                                    s.input_sender()
                                        .emit(ComposeInput::PgpChecked { checked, result });
                                });
                            }
                            break 'handle;
                        }
                    }
                    let from_account_id = self.accounts.get(idx).map(|a| a.id).unwrap_or(1);
                    let from_alias = match self.chosen_from(widgets) {
                        Ok(from) => from,
                        Err(why) => {
                            let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
                            let dialog = adw::MessageDialog::new(
                                parent.as_ref(),
                                Some(&i18n("Check the From address")),
                                Some(&why),
                            );
                            dialog.add_response("ok", &i18n("OK"));
                            dialog.present();
                            break 'handle;
                        }
                    };

                    // Pull the body out of the editor (async), then finish
                    // sending via SendBody. The send-time reader also recuts any
                    // picture armed for it; a draft save below does not. In a
                    // source format `html` carries the source itself, which
                    // `outgoing_body` converts.
                    let s = sender.clone();
                    self.read_body_as(self.format, true, move |html, text| {
                        s.input_sender()
                            .emit(ComposeInput::SendBody(OutgoingFields {
                                html,
                                text,
                                to: to.clone(),
                                cc: cc.clone(),
                                bcc: bcc.clone(),
                                reply_to: reply_to.clone(),
                                subject: subject.clone(),
                                from_account_id,
                                from_alias: from_alias.clone(),
                            }));
                    });
                }

                ComposeInput::SendBody(fields) => {
                    let mut out = self.outgoing_message(fields);
                    // A draft goes once it is sent; a template stays to be used
                    // again.
                    if self.template {
                        out.draft_origin = None;
                    }
                    let _ = sender.output(ComposeOutput::Send {
                        id: self.compose_id,
                        message: Box::new(out),
                    });
                    let _ = sender.output(ComposeOutput::Close(self.compose_id));
                }

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
                } => {
                    let mut out = self.outgoing_message(fields);
                    if as_template {
                        // A copy, cut loose from wherever this message came
                        // from: saving it replaces no draft and no queued
                        // message, and a message started from it answers
                        // nothing and waits for no time.
                        out.draft_origin = None;
                        out.outbox_origin = None;
                        out.in_reply_to.clear();
                        out.references.clear();
                        out.forwards.clear();
                        out.send_at = None;
                        let _ = sender.output(ComposeOutput::SaveTemplate(Box::new(out)));
                        break 'handle;
                    }
                    if self.template {
                        let _ = sender.output(ComposeOutput::SaveTemplate(Box::new(out)));
                    } else {
                        let _ = sender.output(ComposeOutput::SaveDraft(Box::new(out)));
                    }
                    let _ = sender.output(ComposeOutput::Close(self.compose_id));
                }

                ComposeInput::AutoSave => {
                    if self.autosave_off
                        || self.autosaving
                        || self.asking_discard
                        || self.translating
                    {
                        break 'handle;
                    }
                    // Not before anything was written: an untouched reply is
                    // no draft.
                    if self.fields_dirty {
                        sender.input_sender().emit(ComposeInput::AutoSaveRead);
                    } else {
                        let s = sender.clone();
                        self.editor.is_dirty(move |dirty| {
                            if dirty {
                                s.input_sender().emit(ComposeInput::AutoSaveRead);
                            }
                        });
                    }
                }

                ComposeInput::AutoSaveRead => {
                    if self.autosaving {
                        break 'handle;
                    }
                    let fields = self.draft_fields(widgets);
                    let s = sender.clone();
                    self.read_body_as(self.format, false, move |html, text| {
                        s.input_sender()
                            .emit(ComposeInput::AutoSaveBody(OutgoingFields {
                                html,
                                text,
                                ..fields.clone()
                            }));
                    });
                }

                ComposeInput::AutoSaveBody(fields) => {
                    let OutgoingFields {
                        html,
                        text,
                        to,
                        cc,
                        bcc,
                        reply_to,
                        subject,
                        from_account_id,
                        from_alias,
                    } = fields;
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    (
                        &html,
                        &text,
                        &to,
                        &cc,
                        &bcc,
                        &reply_to,
                        &subject,
                        from_account_id,
                        &from_alias,
                        &self.attachments,
                    )
                        .hash(&mut h);
                    let hash = h.finish();
                    // Unchanged since the last save, or nothing in it at all.
                    let empty = [&to, &cc, &bcc, &subject]
                        .iter()
                        .all(|f| f.trim().is_empty())
                        && text.trim().is_empty()
                        && self.attachments.is_empty();
                    if self.autosaved == Some(hash) || empty || self.autosaving {
                        break 'handle;
                    }
                    let out = self.outgoing_message(OutgoingFields {
                        html,
                        text,
                        to,
                        cc,
                        bcc,
                        reply_to,
                        subject,
                        from_account_id,
                        from_alias,
                    });
                    self.autosaving = true;
                    self.autosave_pending = Some(hash);
                    let _ = sender.output(ComposeOutput::AutoSave {
                        id: self.compose_id,
                        message: Box::new(out),
                    });
                }

                ComposeInput::Autosaved { saved, origin } => {
                    self.autosaving = false;
                    let pending = self.autosave_pending.take();
                    if !saved {
                        // Offline, say: the next tick tries again.
                        break 'handle;
                    }
                    match origin {
                        Some(origin) => {
                            if self.draft_origin.is_none() {
                                self.autosave_made = true;
                            }
                            self.draft_origin = Some(origin);
                            self.autosaved = pending;
                        }
                        // Saved, but not found again to be replaced next time:
                        // one more save would leave a second copy behind.
                        None => {
                            tracing::info!(
                                "automatic draft save stopped: the saved copy was not found"
                            );
                            self.autosave_off = true;
                        }
                    }
                }
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
