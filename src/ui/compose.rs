//! Compose window: write a new message, reply, or forward.

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::Suggestion;
use crate::models::DraftOrigin;
use crate::ui::rich_editor::{self, RichEditor, js_escape};
use crate::worker::OutgoingMessage;
use crate::i18n::{i18n, i18n_f};

/// Which recipient field a suggestion is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    To,
    Cc,
    Bcc,
}

/// A signature block as HTML (`-- ` delimiter). The stored signature is HTML
/// (legacy plain-text signatures are converted).
fn sig_html(sig: &str) -> String {
    let body = rich_editor::signature_to_html(sig);
    format!("<div class=\"vireo-sig\"><br>-- <br>{body}</div>")
}

/// Size the pane for its host. Both hosts impose a definite height now — the
/// reader-covering overlay inline (it fills the whole pane), the window itself
/// popped out — so the editor always expands to fill whatever it is given.
/// Inline, the compose header also stands in for the reader's (which it
/// covers), so it takes over the GNOME window decorations.
fn size_for_host(
    root: &adw::ToolbarView,
    header: &adw::HeaderBar,
    editor_holder: &gtk::Box,
    windowed: bool,
) {
    root.set_vexpand(true);
    editor_holder.set_vexpand(true);
    editor_holder.set_height_request(-1);
    header.set_show_end_title_buttons(!windowed);
}

/// Set the inline/window toggle button's icon + tooltip for the current host.
fn set_toggle_icon(btn: &gtk::Button, windowed: bool) {
    if windowed {
        btn.set_icon_name("co.hyprlab.Vireo-view-restore-symbolic");
        btn.set_tooltip_text(Some(i18n("Collapse into reader").as_str()));
    } else {
        btn.set_icon_name("co.hyprlab.Vireo-view-fullscreen-symbolic");
        btn.set_tooltip_text(Some(i18n("Open in window").as_str()));
    }
}

/// Replace the recipient currently being typed (after the last comma) with the
/// chosen suggestion, leaving a trailing ", " ready for the next recipient.
fn complete_field(row: &adw::EntryRow, sug: &Suggestion) {
    let text = row.text().to_string();
    let prefix_len = text.rfind(',').map(|i| i + 1).unwrap_or(0);
    let prefix = &text[..prefix_len];
    row.set_text(&format!("{}{}, ", prefix, sug.display()));
    row.set_position(-1);
}

/// One selectable "from" account in the compose window.
#[derive(Debug, Clone)]
pub struct ComposeAccount {
    pub id: u32,
    pub label: String,
    /// Signature text appended to the body when this account is selected.
    pub signature: String,
    /// The identity's sending address (the account's own, or an alias's).
    pub email: String,
    /// The account's chosen OpenPGP key (fingerprint), if any (#133).
    pub pgp_key: Option<String>,
    /// Set for a send-as alias (#34): the full From to put on the wire
    /// ("Name <alias@host>"). `None` sends as the account itself.
    pub alias_from: Option<String>,
}

/// Initial field contents (empty for a new message; populated for reply/forward).
#[derive(Debug, Default)]
pub struct ComposePrefill {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// HTML prefill placed into the rich editor (e.g. a quoted reply/forward).
    pub body_html: String,
    /// Files to attach on open, already on disk (a queued message's attachments
    /// are written out before its composer opens).
    pub attachments: Vec<std::path::PathBuf>,
    /// Threading headers for a reply: the parent's Message-ID, and the thread's
    /// id chain. Both stored bare (no angle brackets).
    pub in_reply_to: String,
    pub references: String,
    /// When editing an existing draft, its origin (so saving/sending replaces it).
    pub draft_origin: Option<DraftOrigin>,
    /// Start with Encrypt (and so Sign) on: a reply to an encrypted message (#133).
    pub encrypt: bool,
    /// When editing a queued Outbox message, the row this replaces.
    pub outbox_origin: Option<u32>,
    /// For a reply: the original's To+Cc, so the composer can answer from the
    /// alias the mail was addressed to (#34). Empty otherwise.
    pub reply_addressed_to: String,
    /// Send Later (#145): a queued message's scheduled time, kept while it is
    /// edited so Send re-queues it for the same moment.
    pub send_at: Option<i64>,
}

/// Everything the compose pane needs to open.
#[derive(Debug)]
pub struct ComposeInit {
    /// Stable id so the app can track this composer across inline/window moves.
    pub compose_id: u32,
    pub prefill: ComposePrefill,
    pub accounts: Vec<ComposeAccount>,
    /// Index into `accounts` of the account to send from by default.
    pub selected: usize,
    /// Recipient autocomplete suggestions (Contacts + mail history).
    pub suggestions: Vec<Suggestion>,
    /// Whether the pane starts hosted in a standalone window (vs. inline).
    pub windowed: bool,
    /// Whether the inline/window toggle button is offered (reply/forward only).
    pub can_toggle: bool,
    /// Compact reply (#86 follow-up): the split reply hides every address/
    /// subject row and shows just the editor — popping out to a window brings
    /// the full fields back.
    pub compact: bool,
}

pub struct Compose {
    accounts: Vec<ComposeAccount>,
    /// The rich-text (HTML) body editor.
    editor: RichEditor,
    /// Signature currently appended to the body (so it can be swapped out).
    current_sig: String,
    /// Files to attach.
    attachments: Vec<std::path::PathBuf>,
    /// When editing a queued Outbox message, the row this replaces once sent.
    outbox_origin: Option<u32>,
    /// Recipient suggestions, filtered as the user types.
    suggestions: Vec<Suggestion>,
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
    /// When editing an existing draft, its origin (replaced on save/send).
    draft_origin: Option<DraftOrigin>,
    /// Stable id the app uses to track this composer across host moves.
    compose_id: u32,
    /// Currently shown as a standalone window (drives the toggle-button icon).
    windowed: bool,
    /// Whether this composer offers the inline/window toggle at all.
    can_toggle: bool,
    compact: bool,
    /// A compact reply's field rows, revealed by the header button (#154)
    /// or from the start by the preference.
    fields_shown: bool,
    /// A recipient/subject field was edited since open (body edits are tracked
    /// separately by the editor itself). Used for save-if-dirty.
    fields_dirty: bool,
    /// OpenPGP (#133): sign the message; encrypt it to every recipient.
    sign: bool,
    encrypt: bool,
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
}

/// A share link placed in the body (#144), by the id of its paragraph.
#[derive(Clone, Debug)]
struct CloudLink {
    id: String,
    name: String,
    url: String,
}

#[derive(Debug)]
pub enum ComposeInput {
    Send,
    /// Send Later (#145): queue for this unix time, then send as usual.
    SendAt(i64),
    /// Open the date-and-time picker.
    PickSendTime,
    /// Forget the scheduled time: Send goes out at once again.
    ClearSendAt,
    /// A compact reply shows or hides its From/To/Subject rows (#154).
    ShowFields(bool),
    /// Cloud attachments (#144): pick files to upload and share.
    CloudAttach,
    /// Files picked; ask which account and how, then upload.
    CloudPicked(Vec<std::path::PathBuf>),
    /// Upload these files to `account`, whose `expire_days` and `password`
    /// carry the user's choices for this upload; `link_password` is a
    /// password of their own for every link, else one is generated per
    /// file.
    CloudUpload { paths: Vec<std::path::PathBuf>, account: crate::cloud::CloudAccount, link_password: Option<String> },
    /// One upload finished (in a thread): the link, or why not.
    CloudUploaded { name: String, result: Result<crate::cloud::ShareResult, String> },
    /// Take a link back out of the body.
    RemoveCloudLink(usize),
    CopyCloudPasswords,
    /// Move the draft being edited to Trash and close without saving.
    DeleteDraft,
    /// The OpenPGP Sign toggle (#133).
    ToggleSign(bool),
    /// The OpenPGP Encrypt toggle; encrypting turns signing on too.
    ToggleEncrypt(bool),
    /// The editor's HTML + plain text came back asynchronously — finish sending.
    SendBody { html: String, text: String, to: String, cc: String, bcc: String, reply_to: String, subject: String, from_account_id: u32, from_alias: Option<String> },
    /// Save the current message to Drafts.
    SaveDraft,
    /// The editor content came back — finish saving the draft.
    SaveDraftBody { html: String, text: String, to: String, cc: String, bcc: String, reply_to: String, subject: String, from_account_id: u32, from_alias: Option<String> },
    Cancel,
    /// The user clicked the inline/window toggle button.
    ToggleWindowed,
    /// The app moved this pane between inline and window; sync the button icon.
    SetWindowed(bool),
    /// Re-grab keyboard focus into the editor (after a host move).
    FocusEditor,
    /// A recipient/subject field changed — mark dirty.
    MarkFieldsDirty,
    /// Save to Drafts only if edited, then close (used when superseded / on nav).
    SaveDraftIfDirty,
    AccountChanged,
    AttachFiles,
    AddAttachments(Vec<std::path::PathBuf>),
    RemoveAttachment(usize),
    OpenContacts,
    /// The given recipient field changed — refresh autocomplete.
    Suggest(Field),
    /// Addresses just sent to from another composer: into this one's
    /// suggestions at once, without waiting for a reopen.
    AddSuggestions(Vec<Suggestion>),
    /// Arrow-key move of the autocomplete highlight (+1 down, -1 up).
    CompletionMove(i32),
    /// Accept the highlighted suggestion into the active field.
    CompletionAccept,
    /// Dismiss the autocomplete popover.
    CompletionClose,
}

#[derive(Debug)]
pub enum ComposeOutput {
    Send(Box<OutgoingMessage>),
    /// Save the message to the Drafts folder (no send).
    SaveDraft(Box<OutgoingMessage>),
    /// Delete the draft this composer was opened from, and close it. The app
    /// moves the draft to Trash (undoable, like deleting it from the list).
    DeleteDraft { id: u32, origin: DraftOrigin },
    /// Ask the app to promote/demote this pane (inline ↔ window). Carries the id.
    ToggleWindow(u32),
    /// This pane is done (cancelled / sent / draft-saved / superseded). Carries
    /// the id so the app tears down the right host.
    Close(u32),
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
        adw::ToolbarView {
                #[name = "header"]
                add_top_bar = &adw::HeaderBar {
                    set_show_start_title_buttons: false,
                    set_show_end_title_buttons: false,
                    // No "Vireo" branding on the compose bar.
                    #[wrap(Some)]
                    set_title_widget = &gtk::Label {
                        set_label: "",
                    },

                    pack_start = &gtk::Button {
                        set_label: &i18n("Cancel"),
                        connect_clicked => ComposeInput::Cancel,
                    },
                    pack_start = &gtk::Button {
                        set_label: &i18n("Save Draft"),
                        set_tooltip_text: Some(i18n("Save to Drafts").as_str()),
                        connect_clicked => ComposeInput::SaveDraft,
                    },
                    // Only while editing an existing draft: the message is
                    // moved to Trash, not saved, and the editor closes.
                    pack_start = &gtk::Button {
                        set_label: &i18n("Delete Draft"),
                        set_tooltip_text: Some(i18n("Move this draft to Trash").as_str()),
                        #[watch]
                        set_visible: model.draft_origin.is_some(),
                        connect_clicked => ComposeInput::DeleteDraft,
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
                            set_icon_name: "co.hyprlab.Vireo-pan-down-symbolic",
                            add_css_class: "suggested-action",
                            set_tooltip_text: Some(i18n("Send later").as_str()),
                            set_can_focus: false,
                            #[wrap(Some)]
                            set_popover = &gtk::Popover {
                                gtk::Box {
                                    set_orientation: gtk::Orientation::Vertical,
                                    set_spacing: 2,
                                    gtk::Button {
                                        add_css_class: "flat",
                                        set_halign: gtk::Align::Fill,
                                        #[wrap(Some)]
                                        set_child = &gtk::Label { set_label: &i18n("Send now"), set_halign: gtk::Align::Start },
                                        connect_clicked[sender] => move |b| {
                                            b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                            sender.input(ComposeInput::ClearSendAt);
                                            sender.input(ComposeInput::Send);
                                        },
                                    },
                                    gtk::Separator {},
                                    gtk::Button {
                                        add_css_class: "flat",
                                        #[wrap(Some)]
                                        set_child = &gtk::Label { set_label: &i18n("Tomorrow morning (8:00)"), set_halign: gtk::Align::Start },
                                        connect_clicked[sender] => move |b| {
                                            b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                            sender.input(ComposeInput::SendAt(preset_time(1, 8)));
                                        },
                                    },
                                    gtk::Button {
                                        add_css_class: "flat",
                                        #[wrap(Some)]
                                        set_child = &gtk::Label { set_label: &i18n("Tomorrow afternoon (13:00)"), set_halign: gtk::Align::Start },
                                        connect_clicked[sender] => move |b| {
                                            b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                            sender.input(ComposeInput::SendAt(preset_time(1, 13)));
                                        },
                                    },
                                    gtk::Button {
                                        add_css_class: "flat",
                                        #[wrap(Some)]
                                        set_child = &gtk::Label { set_label: &i18n("Monday morning (8:00)"), set_halign: gtk::Align::Start },
                                        connect_clicked[sender] => move |b| {
                                            b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                            sender.input(ComposeInput::SendAt(next_monday(8)));
                                        },
                                    },
                                    gtk::Separator {},
                                    gtk::Button {
                                        add_css_class: "flat",
                                        #[wrap(Some)]
                                        set_child = &gtk::Label { set_label: &i18n("Pick a date and time…"), set_halign: gtk::Align::Start },
                                        connect_clicked[sender] => move |b| {
                                            b.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>().map(|p| p.popdown());
                                            sender.input(ComposeInput::PickSendTime);
                                        },
                                    },
                                },
                            },
                        },
                    },
                    // OpenPGP (#133): only offered where a gpg exists.
                    #[name = "encrypt_btn"]
                    pack_end = &gtk::ToggleButton {
                        set_icon_name: "co.hyprlab.Vireo-channel-secure-symbolic",
                        set_tooltip_text: Some(i18n("Encrypt with OpenPGP to every recipient's key").as_str()),
                        set_visible: crate::pgp::available(),
                        connect_toggled[sender] => move |b| {
                            sender.input(ComposeInput::ToggleEncrypt(b.is_active()));
                        },
                    },
                    #[name = "sign_btn"]
                    pack_end = &gtk::ToggleButton {
                        set_icon_name: "co.hyprlab.Vireo-security-high-symbolic",
                        set_tooltip_text: Some(i18n("Sign with your OpenPGP key").as_str()),
                        set_visible: crate::pgp::available(),
                        connect_toggled[sender] => move |b| {
                            sender.input(ComposeInput::ToggleSign(b.is_active()));
                        },
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Vireo-mail-attachment-symbolic",
                        set_tooltip_text: Some(i18n("Attach files").as_str()),
                        connect_clicked => ComposeInput::AttachFiles,
                    },
                    // Cloud attachments (#144): only with an account set up.
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Vireo-cloud-symbolic",
                        set_tooltip_text: Some(i18n("Upload to cloud storage and share a link").as_str()),
                        #[watch]
                        set_visible: !model.cloud_accounts.is_empty(),
                        #[watch]
                        set_sensitive: model.cloud_busy == 0,
                        connect_clicked => ComposeInput::CloudAttach,
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Vireo-x-office-address-book-symbolic",
                        set_tooltip_text: Some(i18n("Open Contacts").as_str()),
                        connect_clicked => ComposeInput::OpenContacts,
                    },
                    // Promote inline reply → window, or collapse window → inline.
                    // Icon/visibility set in `init` and on SetWindowed.
                    #[name = "toggle_btn"]
                    pack_end = &gtk::Button {
                        set_tooltip_text: Some(i18n("Open in window").as_str()),
                        connect_clicked => ComposeInput::ToggleWindowed,
                    },
                    // The compact reply's From/To/Subject rows (#154): folded
                    // away by default, one press brings them back.
                    #[name = "fields_btn"]
                    pack_end = &gtk::ToggleButton {
                        set_icon_name: "co.hyprlab.Vireo-pan-down-symbolic",
                        add_css_class: "fields-chevron",
                        set_tooltip_text: Some(i18n("Show From, To and Subject").as_str()),
                        set_can_focus: false,
                        #[watch]
                        set_visible: model.compact && !model.windowed,
                        connect_toggled[sender] => move |b| {
                            sender.input(ComposeInput::ShowFields(b.is_active()));
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
                    gtk::Image { set_icon_name: Some("co.hyprlab.Vireo-alarm-symbolic") },
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
                    gtk::Image { set_icon_name: Some("co.hyprlab.Vireo-dialog-password-symbolic") },
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
                        #[name = "to_row"]
                        adw::EntryRow {
                            set_title: &i18n("To"),
                            set_input_purpose: gtk::InputPurpose::Email,
                        },
                        #[name = "cc_row"]
                        adw::EntryRow {
                            set_title: &i18n("Cc"),
                            set_input_purpose: gtk::InputPurpose::Email,
                        },
                        #[name = "bcc_row"]
                        adw::EntryRow {
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

                    #[name = "attach_box"]
                    gtk::FlowBox {
                        set_selection_mode: gtk::SelectionMode::None,
                        set_column_spacing: 6,
                        set_row_spacing: 6,
                        set_max_children_per_line: 4,
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
            suggestions,
            windowed,
            can_toggle,
            compact,
        } = init;
        let in_reply_to = prefill.in_reply_to.clone();
        let references = prefill.references.clone();
        let draft_origin = prefill.draft_origin.clone();
        let outbox_origin = prefill.outbox_origin;
        let prefill_attachments = prefill.attachments.clone();
        let prefill_encrypt = prefill.encrypt;
        let send_at = prefill.send_at;
        let current_sig = accounts.get(selected).map(|a| a.signature.clone()).unwrap_or_default();

        let completion = gtk::Popover::new();
        completion.set_autohide(false); // don't steal focus from the entry
        completion.set_can_focus(false);
        completion.set_position(gtk::PositionType::Bottom);
        completion.add_css_class("menu");

        // Initial editor content: a blank line to type on, the quoted
        // reply/forward (if any), then the signature.
        let mut content = String::from("<div><br></div>");
        if !prefill.body_html.is_empty() {
            content.push_str(&prefill.body_html);
        }
        // A draft already contains its signature; don't add another.
        if draft_origin.is_none() && !current_sig.is_empty() {
            content.push_str(&sig_html(&current_sig));
        }
        let editor = RichEditor::new(&content);
        // "Send as Attachment Instead" on an inline image: the editor lifts
        // it to a temp file and it joins the attachment chips here.
        {
            let s = sender.input_sender().clone();
            editor.connect_send_as_attachment(move |path| {
                let _ = s.send(ComposeInput::AddAttachments(vec![path]));
            });
        }

        let model = Compose {
            accounts,
            editor,
            current_sig,
            attachments: prefill_attachments,
            suggestions,
            completion,
            completion_field: None,
            completion_list: None,
            completion_selected: 0,
            completion_count: 0,
            completion_open: std::rc::Rc::new(std::cell::Cell::new(false)),
            in_reply_to,
            references,
            draft_origin,
            outbox_origin,
            compose_id,
            windowed,
            can_toggle,
            // A compact (fields-hidden) pane only makes sense once it is
            // addressed: replies arrive with To filled, forwards do not.
            compact: compact && !prefill.to.trim().is_empty(),
            fields_shown: crate::config::load_reply_fields(),
            fields_dirty: false,
            sign: false,
            encrypt: false,
            send_at,
            cloud_accounts: crate::cloud::load_accounts(),
            cloud_links: Vec::new(),
            cloud_busy: 0,
            cloud_passwords: Vec::new(),
        };
        let widgets = view_output!();
        if prefill_encrypt && crate::pgp::available() {
            // Through the buttons, so the toggles and the model agree.
            widgets.encrypt_btn.set_active(true);
        }
        widgets.editor_holder.append(&model.editor.widget);

        // The inline/window toggle: only reply/forward panes can toggle. Its icon
        // reflects the current host (fullscreen = "expand to window", restore =
        // "collapse back inline").
        widgets.toggle_btn.set_visible(model.can_toggle);
        set_toggle_icon(&widgets.toggle_btn, model.windowed);
        size_for_host(&root, &widgets.header, &widgets.editor_holder, model.windowed);

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
        widgets.fields_list.set_visible(!model.compact || model.fields_shown);
        widgets.fields_btn.set_active(model.fields_shown);
        {
            let more = gtk::Button::with_label(&i18n("More"));
            more.add_css_class("flat");
            more.set_valign(gtk::Align::Center);
            more.set_tooltip_text(Some(i18n("Show Cc, Bcc and Reply-To").as_str()));
            let cc = widgets.cc_row.clone();
            let bcc = widgets.bcc_row.clone();
            let reply_to = widgets.reply_to_row.clone();
            let btn = more.clone();
            more.connect_clicked(move |_| {
                cc.set_visible(true);
                bcc.set_visible(true);
                reply_to.set_visible(true);
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

        widgets.to_row.set_text(&prefill.to);
        widgets.cc_row.set_text(&prefill.cc);
        widgets.bcc_row.set_text(&prefill.bcc);
        widgets.subject_row.set_text(&prefill.subject);
        if !model.attachments.is_empty() {
            model.rebuild_attachments(&widgets.attach_box, &sender);
        }

        // VIREO_SHOWCASE_CLOUD_DIALOG opens the cloud upload dialog on a
        // stand-in file two seconds after the composer is up (demo only),
        // for a capture of this upload's terms.
        if std::env::var_os("VIREO_SHOWCASE_CLOUD_DIALOG").is_some() && std::env::var_os("VIREO_DEMO").is_some() {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(2, move || {
                s.input(ComposeInput::CloudPicked(vec![std::path::PathBuf::from("/tmp/Q3 report.pdf")]));
            });
        }

        // Wire autocomplete *after* prefilling, so the initial text doesn't pop it.
        for (row, field) in [
            (&widgets.to_row, Field::To),
            (&widgets.cc_row, Field::Cc),
            (&widgets.bcc_row, Field::Bcc),
        ] {
            let s = sender.clone();
            row.connect_changed(move |_| {
                s.input(ComposeInput::Suggest(field));
                s.input(ComposeInput::MarkFieldsDirty);
            });

            // Close the popover when the field loses focus.
            let focus = gtk::EventControllerFocus::new();
            let s = sender.clone();
            focus.connect_leave(move |_| s.input(ComposeInput::CompletionClose));
            row.add_controller(focus);
        }
        // Subject edits also count as dirtying the draft.
        let s = sender.clone();
        widgets
            .subject_row
            .connect_changed(move |_| s.input(ComposeInput::MarkFieldsDirty));
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
        key.connect_key_pressed(move |_, keyval, _, state| {
            use gtk::glib::Propagation;
            // Ctrl+V pastes per the "Paste as plain text" preference, read
            // here so a settings change applies to composers already open.
            // Only over the body: the address and subject entries are plain
            // text by nature, and the focus guard leaves their Ctrl+V alone.
            if state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                && keyval == gtk::gdk::Key::v
                && editor.has_focus()
            {
                editor.paste(!crate::config::load_paste_plain());
                return Propagation::Stop;
            }
            if !open.get() {
                // Escape backs out of the whole composer — the same as Cancel,
                // so an accidental reply is one key away from being undone. Only
                // once the suggestion list is closed, which Escape dismisses
                // first (below), so one press never does both.
                if keyval == gtk::gdk::Key::Escape {
                    s.input(ComposeInput::Cancel);
                    return Propagation::Stop;
                }
                return Propagation::Proceed;
            }
            // Compare by value (the const-as-pattern match wasn't matching).
            if keyval == gtk::gdk::Key::Down {
                s.input(ComposeInput::CompletionMove(1));
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Up {
                s.input(ComposeInput::CompletionMove(-1));
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Return || keyval == gtk::gdk::Key::KP_Enter {
                s.input(ComposeInput::CompletionAccept);
                Propagation::Stop
            } else if keyval == gtk::gdk::Key::Escape {
                s.input(ComposeInput::CompletionClose);
                Propagation::Stop
            } else {
                Propagation::Proceed
            }
        });
        root.add_controller(key);

        if prefill.to.is_empty() {
            widgets.to_row.grab_focus();
        } else {
            model.editor.grab_focus();
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
        match message {
            ComposeInput::Cancel => {
                let _ = sender.output(ComposeOutput::Close(self.compose_id));
            }

            ComposeInput::SendAt(at) => {
                self.send_at = Some(at);
                sender.input(ComposeInput::Send);
            }

            ComposeInput::ClearSendAt => {
                self.send_at = None;
            }

            ComposeInput::ShowFields(on) => {
                self.fields_shown = on;
                widgets.fields_list.set_visible(!(self.compact && !self.windowed) || on);
            }

            ComposeInput::CloudAttach => {
                let dialog = gtk::FileDialog::new();
                dialog.set_title(&i18n("Upload to Cloud Storage"));
                let parent = root.root().and_downcast::<gtk::Window>();
                let s = sender.input_sender().clone();
                dialog.open_multiple(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                    if let Ok(model) = res {
                        let paths: Vec<_> = (0..model.n_items())
                            .filter_map(|i| model.item(i).and_downcast::<gtk::gio::File>()?.path())
                            .collect();
                        if !paths.is_empty() {
                            let _ = s.send(ComposeInput::CloudPicked(paths));
                        }
                    }
                });
            }

            ComposeInput::CloudPicked(paths) => {
                let parent = root.root().and_downcast::<gtk::Window>();
                cloud_upload_dialog(parent.as_ref(), &self.cloud_accounts, paths, sender.input_sender().clone());
            }

            ComposeInput::CloudUpload { paths, account, link_password } => {
                let secret = if account.has_secret() {
                    crate::config::load_cloud_password(&account.key())
                } else {
                    Some(String::new())
                };
                let Some(password) = secret else {
                    let parent = root.root().and_downcast::<gtk::Window>();
                    let d = adw::MessageDialog::new(
                        parent.as_ref(),
                        Some(i18n("Not signed in").as_str()),
                        Some(i18n_f("The sign-in for {name} is not in the keyring. Open Settings, Cloud Storage, and enter it again.", &[("name", &account.name)]).as_str()),
                    );
                    d.add_response("ok", &i18n("OK"));
                    d.present();
                    return;
                };
                for path in paths {
                    let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    self.cloud_busy += 1;
                    self.rebuild_attachments(&widgets.attach_box, &sender);
                    let s = sender.input_sender().clone();
                    let (a, pw, lp) = (account.clone(), password.clone(), link_password.clone());
                    std::thread::spawn(move || {
                        let result = crate::cloud::upload_and_share(&a, &pw, &path, lp.as_deref());
                        let _ = s.send(ComposeInput::CloudUploaded { name, result });
                    });
                }
            }

            ComposeInput::CloudUploaded { name, result } => {
                self.cloud_busy = self.cloud_busy.saturating_sub(1);
                match result {
                    Ok(share) => {
                        let id = format!("vireo-cloud-{}", crate::rng::token(8).unwrap_or_else(|_| share.size.to_string()));
                        let mut caption = crate::cloud::human_size(share.size);
                        if let Some(d) = &share.expires {
                            caption.push_str(&format!(", {}", i18n_f("link expires {date}", &[("date", d)])));
                        }
                        if share.password.is_some() {
                            caption.push_str(&format!(", {}", i18n("password-protected")));
                        }
                        let html = format!(
                            "<p id=\"{id}\" data-vireo-cloud=\"1\">\u{1F4CE} <a href=\"{url}\">{name}</a> ({caption})</p>",
                            url = html_escape(&share.url),
                            name = html_escape(&share.name),
                            caption = html_escape(&caption),
                        );
                        // Into the body where the user's own text ends: above
                        // the signature, and above a quoted original in a
                        // reply, so the link reads as part of the message.
                        self.editor.run_js(&format!(
                            "(function(){{var d=document.createElement('div');d.innerHTML='{}';\
                             var p=d.firstChild;var b=document.body;\
                             var first=null;var cands=b.querySelectorAll('.vireo-sig,.vireo-quote-attr,blockquote');\
                             for(var i=0;i<cands.length;i++){{var t=cands[i];while(t.parentNode&&t.parentNode!==b)t=t.parentNode;\
                             if(t.parentNode===b&&(!first||(t.compareDocumentPosition(first)&Node.DOCUMENT_POSITION_FOLLOWING)))first=t;}}\
                             if(first)b.insertBefore(p,first);else b.appendChild(p);\
                             document.dispatchEvent(new Event('input'));}})()",
                            js_escape(&html)
                        ));
                        if let Some(p) = share.password.clone() {
                            self.cloud_passwords.push((share.name.clone(), p));
                        }
                        self.cloud_links.push(CloudLink { id, name: share.name, url: share.url });
                    }
                    Err(e) => {
                        let parent = root.root().and_downcast::<gtk::Window>();
                        let d = adw::MessageDialog::new(
                            parent.as_ref(),
                            Some(i18n_f("Could not upload {name}", &[("name", &name)]).as_str()),
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
                    let _ = sender
                        .output(ComposeOutput::DeleteDraft { id: self.compose_id, origin });
                }
            }

            ComposeInput::ToggleWindowed => {
                let _ = sender.output(ComposeOutput::ToggleWindow(self.compose_id));
            }

            ComposeInput::SetWindowed(windowed) => {
                self.windowed = windowed;
                set_toggle_icon(&widgets.toggle_btn, windowed);
                size_for_host(root, &widgets.header, &widgets.editor_holder, windowed);
                // A compact reply grows its field rows back in a window (and
                // sheds them again if it returns inline).
                widgets.fields_list.set_visible(!(self.compact && !windowed) || self.fields_shown);
            }

            ComposeInput::FocusEditor => self.editor.grab_focus(),

            ComposeInput::MarkFieldsDirty => self.fields_dirty = true,

            ComposeInput::SaveDraftIfDirty => {
                // Save only if the user actually edited something, so navigating
                // away from a pristine quote-only reply doesn't litter Drafts.
                if self.fields_dirty {
                    sender.input(ComposeInput::SaveDraft);
                } else {
                    let s = sender.clone();
                    let id = self.compose_id;
                    self.editor.is_dirty(move |body_dirty| {
                        if body_dirty {
                            s.input(ComposeInput::SaveDraft);
                        } else {
                            let _ = s.output(ComposeOutput::Close(id));
                        }
                    });
                }
            }

            ComposeInput::OpenContacts => {
                // Browse contacts; the chosen one is appended to the To field.
                let Some(win) = root.root().and_downcast::<gtk::Window>() else {
                    return;
                };
                let to_row = widgets.to_row.clone();
                crate::ui::contacts_browser::present(&win, move |contact| {
                    let display = if contact.name.trim().is_empty()
                        || contact.name == contact.email
                    {
                        contact.email.clone()
                    } else {
                        format!("{} <{}>", contact.name, contact.email)
                    };
                    let cur = to_row.text().to_string();
                    let trimmed = cur.trim_end();
                    let sep = if trimmed.is_empty() {
                        ""
                    } else if trimmed.ends_with(',') {
                        " "
                    } else {
                        ", "
                    };
                    to_row.set_text(&format!("{cur}{sep}{display}, "));
                    to_row.set_position(-1);
                });
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
                self.attachments.extend(paths);
                self.rebuild_attachments(&widgets.attach_box, &sender);
            }

            ComposeInput::RemoveAttachment(i) => {
                if i < self.attachments.len() {
                    self.attachments.remove(i);
                    self.rebuild_attachments(&widgets.attach_box, &sender);
                }
            }

            ComposeInput::AccountChanged => {
                // Swap the editor's signature block for the new account's.
                let idx = widgets.from_row.selected() as usize;
                let new_sig = self.accounts.get(idx).map(|a| a.signature.clone()).unwrap_or_default();
                if new_sig != self.current_sig {
                    let replacement = if new_sig.is_empty() {
                        String::new()
                    } else {
                        sig_html(&new_sig)
                    };
                    let js = format!(
                        "(function(){{var s=document.querySelector('.vireo-sig');\
                         var h='{}';\
                         if(s){{if(h){{s.outerHTML=h;}}else{{s.remove();}}}}\
                         else if(h){{document.body.insertAdjacentHTML('beforeend',h);}}}})()",
                        rich_editor::js_escape(&replacement)
                    );
                    self.editor.run_js(&js);
                    self.current_sig = new_sig;
                }
            }

            ComposeInput::Suggest(field) => {
                let row = match field {
                    Field::To => &widgets.to_row,
                    Field::Cc => &widgets.cc_row,
                    Field::Bcc => &widgets.bcc_row,
                };
                self.show_completion(field, row);
            }

            ComposeInput::AddSuggestions(new) => {
                for n in new {
                    let key = n.email.to_lowercase();
                    match self.suggestions.iter_mut().find(|s| s.email.to_lowercase() == key) {
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
                    return;
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
                    None => return,
                };
                let text = row.text().to_string();
                let token = text.rsplit(',').next().unwrap_or("").trim().to_string();
                if !token.is_empty() {
                    let chosen = self.ranked_matches(&token).into_iter().nth(self.completion_selected);
                    if let Some(sug) = chosen {
                        complete_field(row, &sug);
                    }
                }
                self.completion_open.set(false);
                self.completion.popdown();
            }

            ComposeInput::CompletionClose => {
                self.completion_open.set(false);
                self.completion.popdown();
            }

            ComposeInput::ToggleSign(on) => self.sign = on,
            ComposeInput::ToggleEncrypt(on) => {
                self.encrypt = on;
                if on && !self.sign {
                    self.sign = true;
                    widgets.sign_btn.set_active(true);
                }
            }
            ComposeInput::Send => {
                let to = widgets.to_row.text().trim().to_string();
                if to.is_empty() {
                    widgets.to_row.add_css_class("error");
                    return;
                }
                let cc = widgets.cc_row.text().trim().to_string();
                let bcc = widgets.bcc_row.text().trim().to_string();
                let reply_to = widgets.reply_to_row.text().trim().to_string();
                let subject = widgets.subject_row.text().to_string();
                let idx = widgets.from_row.selected() as usize;
                // OpenPGP (#133): say what is missing before anything leaves.
                if self.sign || self.encrypt {
                    let from = self.accounts.get(idx);
                    let problem = pgp_send_check(
                        from.map(|a| a.email.as_str()).unwrap_or(""),
                        from.and_then(|a| a.pgp_key.as_deref()),
                        &[to.as_str(), cc.as_str(), bcc.as_str()],
                        self.encrypt,
                    );
                    if let Err(problem) = problem {
                        let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
                        let dialog = adw::MessageDialog::new(
                            parent.as_ref(),
                            Some(&i18n("Cannot send with OpenPGP")),
                            Some(&problem),
                        );
                        dialog.add_response("ok", &i18n("OK"));
                        dialog.present();
                        return;
                    }
                }
                let from_account_id = self.accounts.get(idx).map(|a| a.id).unwrap_or(1);
                let from_alias = self.accounts.get(idx).and_then(|a| a.alias_from.clone());

                // Pull the HTML and a plain-text version out of the editor (async),
                // then finish sending via SendBody. The send-time reader also
                // recuts any picture armed for it; a draft save below does not.
                let s = sender.clone();
                self.editor.extract_for_send(move |html, text| {
                    s.input(ComposeInput::SendBody {
                        html,
                        text,
                        to: to.clone(),
                        cc: cc.clone(),
                        bcc: bcc.clone(),
                        reply_to: reply_to.clone(),
                        subject: subject.clone(),
                        from_account_id,
                        from_alias: from_alias.clone(),
                    });
                });
            }

            ComposeInput::SendBody { html, text, to, cc, bcc, reply_to, subject, from_account_id, from_alias } => {
                let out = self
                    .build_outgoing(from_account_id, from_alias, to, cc, bcc, reply_to, subject, text, html);
                let _ = sender.output(ComposeOutput::Send(Box::new(out)));
                let _ = sender.output(ComposeOutput::Close(self.compose_id));
            }

            ComposeInput::SaveDraft => {
                // A draft can be saved without recipients; just capture the fields.
                let to = widgets.to_row.text().trim().to_string();
                let cc = widgets.cc_row.text().trim().to_string();
                let bcc = widgets.bcc_row.text().trim().to_string();
                let reply_to = widgets.reply_to_row.text().trim().to_string();
                let subject = widgets.subject_row.text().to_string();
                let idx = widgets.from_row.selected() as usize;
                let from_account_id = self.accounts.get(idx).map(|a| a.id).unwrap_or(1);
                let from_alias = self.accounts.get(idx).and_then(|a| a.alias_from.clone());
                let s = sender.clone();
                self.editor.extract(move |html, text| {
                    s.input(ComposeInput::SaveDraftBody {
                        html,
                        text,
                        to: to.clone(),
                        cc: cc.clone(),
                        bcc: bcc.clone(),
                        reply_to: reply_to.clone(),
                        subject: subject.clone(),
                        from_account_id,
                        from_alias: from_alias.clone(),
                    });
                });
            }

            ComposeInput::SaveDraftBody { html, text, to, cc, bcc, reply_to, subject, from_account_id, from_alias } => {
                let out = self
                    .build_outgoing(from_account_id, from_alias, to, cc, bcc, reply_to, subject, text, html);
                let _ = sender.output(ComposeOutput::SaveDraft(Box::new(out)));
                let _ = sender.output(ComposeOutput::Close(self.compose_id));
            }
        }
    }
}

impl Compose {
    /// Assemble an [`OutgoingMessage`] from the composed fields + attachments,
    /// carrying the draft origin so a saved/sent draft replaces its predecessor.
    #[allow(clippy::too_many_arguments)]
    fn build_outgoing(
        &self,
        from_account_id: u32,
        from_alias: Option<String>,
        to: String,
        cc: String,
        bcc: String,
        reply_to: String,
        subject: String,
        text: String,
        html: String,
    ) -> OutgoingMessage {
        OutgoingMessage {
            from_account_id,
            from_alias,
            to,
            cc,
            bcc,
            reply_to,
            subject,
            body: text,
            html,
            attachments: self
                .attachments
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            draft_origin: self.draft_origin.clone(),
            outbox_origin: self.outbox_origin,
            sign: self.sign,
            encrypt: self.encrypt,
            send_at: self.send_at,
        }
    }

    /// Suggestions matching `token`, ranked best-first (prefix match, then most
    /// frequently used), capped to a handful.
    fn ranked_matches(&self, token: &str) -> Vec<Suggestion> {
        let q = token.to_lowercase();
        let mut matches: Vec<Suggestion> =
            self.suggestions.iter().filter(|s| s.matches(token)).cloned().collect();
        matches.sort_by(|a, b| {
            let pa = a.email.to_lowercase().starts_with(&q) || a.name.to_lowercase().starts_with(&q);
            let pb = b.email.to_lowercase().starts_with(&q) || b.name.to_lowercase().starts_with(&q);
            // Own addresses come after everyone else's, prefix match or not.
            a.own.cmp(&b.own)
                .then(pb.cmp(&pa))
                .then(b.score.cmp(&a.score))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        matches.truncate(8);
        matches
    }

    /// Filter suggestions by the recipient fragment being typed and show them in
    /// a popover under the field; clicking one completes that recipient.
    fn show_completion(&mut self, field: Field, row: &adw::EntryRow) {
        let text = row.text().to_string();
        // The recipient currently being typed is the part after the last comma.
        let token = text.rsplit(',').next().unwrap_or("").trim().to_string();
        if token.is_empty() {
            self.completion_open.set(false);
            self.completion.popdown();
            return;
        }
        let matches = self.ranked_matches(&token);
        if matches.is_empty() {
            self.completion_open.set(false);
            self.completion.popdown();
            return;
        }

        // Attach the popover to the active field (only re-parent when it moves).
        if self.completion_field != Some(field) {
            if self.completion.parent().is_some() {
                self.completion.unparent();
            }
            self.completion.set_parent(row);
            self.completion_field = Some(field);
        }

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        // Keep focus in the entry so its key controller drives navigation.
        list.set_can_focus(false);
        list.add_css_class("autocomplete");
        let count = matches.len();
        for sug in matches {
            let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            item.set_margin_start(6);
            item.set_margin_end(6);
            item.set_margin_top(3);
            item.set_margin_bottom(3);
            // Mark where the suggestion came from: address book vs. mail history.
            let icon = gtk::Image::from_icon_name(if sug.from_contacts {
                "co.hyprlab.Vireo-avatar-default-symbolic"
            } else {
                "co.hyprlab.Vireo-document-open-recent-symbolic"
            });
            icon.set_valign(gtk::Align::Center);
            icon.add_css_class("dim-label");
            item.append(&icon);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let title = gtk::Label::new(Some(&sug.name));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            text.append(&title);
            if sug.name != sug.email {
                let sub = gtk::Label::new(Some(&sug.email));
                sub.set_halign(gtk::Align::Start);
                sub.set_xalign(0.0);
                sub.add_css_class("dim-label");
                sub.add_css_class("caption");
                text.append(&sub);
            }
            item.append(&text);
            let lbr = gtk::ListBoxRow::new();
            lbr.set_can_focus(false);
            lbr.set_child(Some(&item));
            list.append(&lbr);

            // Complete this recipient on click.
            let row2 = row.clone();
            let pop = self.completion.downgrade();
            let sug = sug.clone();
            let gesture = gtk::GestureClick::new();
            gesture.connect_released(move |_, _, _, _| {
                complete_field(&row2, &sug);
                if let Some(p) = pop.upgrade() {
                    p.popdown();
                }
            });
            lbr.add_controller(gesture);
        }

        // Highlight the first suggestion so Enter accepts it immediately.
        if let Some(first) = list.row_at_index(0) {
            list.select_row(Some(&first));
        }
        self.completion_selected = 0;
        self.completion_count = count;
        self.completion_list = Some(list.clone());

        self.completion.set_child(Some(&list));
        self.completion.popup();
        self.completion_open.set(true);
    }

    fn rebuild_attachments(&self, flow: &gtk::FlowBox, sender: &ComponentSender<Self>) {
        while let Some(child) = flow.first_child() {
            flow.remove(&child);
        }
        for (i, path) in self.attachments.iter().enumerate() {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string());

            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            chip.add_css_class("attach-chip");
            // FlowBoxChild defaults to halign: Fill, which would otherwise
            // stretch this box the full width of its cell — leaving the pill's
            // background trailing well past the remove button. Hug the content.
            chip.set_halign(gtk::Align::Start);
            chip.append(&gtk::Image::from_icon_name("co.hyprlab.Vireo-mail-attachment-symbolic"));
            let lbl = gtk::Label::new(Some(&name));
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            lbl.set_max_width_chars(22);
            chip.append(&lbl);
            let rm = gtk::Button::from_icon_name("co.hyprlab.Vireo-window-close-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            let s = sender.input_sender().clone();
            rm.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::RemoveAttachment(i));
            });
            chip.append(&rm);

            flow.append(&chip);
            // GtkFlowBox auto-wraps `chip` in a FlowBoxChild that, unlike
            // `chip` itself, has no halign we can set beforehand — it still
            // fills (and hover-highlights) the full cell. Shrink it to the
            // pill's own size and drop its own row interactivity, since the
            // remove button inside is the only real click target.
            if let Some(cell) = chip.parent().and_downcast::<gtk::FlowBoxChild>() {
                cell.set_halign(gtk::Align::Start);
                cell.set_can_focus(false);
                cell.set_focusable(false);
            }
        }
        // Cloud links (#144) sit with the attachments but read as links: a
        // cloud icon, the name, and a remove that also takes the paragraph
        // out of the body. Uploads in flight show a spinner chip.
        for (i, link) in self.cloud_links.iter().enumerate() {
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            chip.add_css_class("attach-chip");
            chip.add_css_class("cloud-chip");
            chip.set_halign(gtk::Align::Start);
            chip.set_tooltip_text(Some(&link.url));
            chip.append(&gtk::Image::from_icon_name("co.hyprlab.Vireo-cloud-symbolic"));
            let lbl = gtk::Label::new(Some(&i18n_f("{name} (link)", &[("name", &link.name)])));
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            lbl.set_max_width_chars(26);
            chip.append(&lbl);
            let rm = gtk::Button::from_icon_name("co.hyprlab.Vireo-window-close-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            let s = sender.input_sender().clone();
            rm.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::RemoveCloudLink(i));
            });
            chip.append(&rm);
            flow.append(&chip);
            if let Some(cell) = chip.parent().and_downcast::<gtk::FlowBoxChild>() {
                cell.set_halign(gtk::Align::Start);
                cell.set_can_focus(false);
                cell.set_focusable(false);
            }
        }
        for _ in 0..self.cloud_busy {
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            chip.add_css_class("attach-chip");
            chip.set_halign(gtk::Align::Start);
            let spin = gtk::Spinner::new();
            spin.start();
            chip.append(&spin);
            chip.append(&gtk::Label::new(Some(&i18n("Uploading…"))));
            flow.append(&chip);
            if let Some(cell) = chip.parent().and_downcast::<gtk::FlowBoxChild>() {
                cell.set_halign(gtk::Align::Start);
                cell.set_can_focus(false);
            }
        }
        flow.set_visible(!self.attachments.is_empty() || !self.cloud_links.is_empty() || self.cloud_busy > 0);
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Which account to upload to, and how the links are made this time:
/// the expiry and the password, seeded from the account's settings and
/// changeable for this upload alone.
fn cloud_upload_dialog(
    parent: Option<&gtk::Window>,
    accounts: &[crate::cloud::CloudAccount],
    paths: Vec<std::path::PathBuf>,
    sender: relm4::Sender<ComposeInput>,
) {
    if accounts.is_empty() {
        return;
    }
    let n = paths.len();
    let dialog = adw::MessageDialog::new(
        parent,
        Some(i18n("Upload to Cloud Storage").as_str()),
        Some(crate::i18n::ni18n_f("Upload {n} file and put its share link in the message.", "Upload {n} files and put their share links in the message.", n as u32, &[("n", &n.to_string())]).as_str()),
    );
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("upload", &i18n("Upload"));
    dialog.set_default_response(Some("upload"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("upload", adw::ResponseAppearance::Suggested);

    let names: Vec<String> = accounts.iter().map(|a| if a.name.trim().is_empty() { a.where_shown() } else { a.name.clone() }).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let combo = adw::ComboRow::new();
    combo.set_title(&i18n("Account"));
    combo.set_model(Some(&gtk::StringList::new(&name_refs)));
    combo.set_visible(accounts.len() > 1);

    // This upload's terms, starting from the account's own.
    let expire = adw::SpinRow::with_range(0.0, 365.0, 1.0);
    expire.set_title(&i18n("Links expire after"));
    expire.set_subtitle(&i18n("Days; 0 keeps the link"));
    let protect = adw::SwitchRow::new();
    protect.set_title(&i18n("Protect with a password"));
    let password = adw::EntryRow::new();
    password.set_title(&i18n("Download password (empty: generated)"));
    let apply_defaults = {
        let (expire, protect, password) = (expire.clone(), protect.clone(), password.clone());
        let accounts = accounts.to_vec();
        move |i: usize| {
            if let Some(a) = accounts.get(i) {
                expire.set_value(a.expire_days as f64);
                protect.set_active(a.password);
                password.set_text("");
            }
        }
    };
    apply_defaults(0);
    password.set_visible(protect.is_active());
    {
        let password = password.clone();
        protect.connect_active_notify(move |p| password.set_visible(p.is_active()));
    }
    {
        let apply_defaults = apply_defaults.clone();
        combo.connect_selected_notify(move |c| apply_defaults(c.selected() as usize));
    }

    let group = adw::PreferencesGroup::new();
    group.add(&combo);
    group.add(&expire);
    group.add(&protect);
    group.add(&password);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bx.set_width_request(400);
    bx.append(&group);
    dialog.set_extra_child(Some(&bx));

    let accounts = accounts.to_vec();
    let paths = std::cell::RefCell::new(Some(paths));
    dialog.connect_response(None, move |_, resp| {
        if resp != "upload" {
            return;
        }
        let (Some(paths), Some(account)) = (paths.borrow_mut().take(), accounts.get(combo.selected() as usize)) else {
            return;
        };
        let mut account = account.clone();
        account.expire_days = expire.value() as u32;
        account.password = protect.is_active();
        let typed = password.text().trim().to_string();
        let link_password = (account.password && !typed.is_empty()).then_some(typed);
        let _ = sender.send(ComposeInput::CloudUpload { paths, account, link_password });
    });
    dialog.present();
}

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

/// Whether an OpenPGP send can go ahead (#133): a key of the user's own
/// for the From address (or the account's chosen key), and, to encrypt, a
/// public key for every recipient. The message names the first gap.
fn pgp_send_check(from: &str, chosen_key: Option<&str>, fields: &[&str], encrypt: bool) -> Result<(), String> {
    let gpg = crate::pgp::Gpg::system();
    let own = chosen_key
        .and_then(|f| crate::pgp::secret_key_by_fingerprint(&gpg, f))
        .or_else(|| crate::pgp::secret_key_for(&gpg, from));
    if own.is_none() {
        return Err(crate::i18n::i18n_f(
            "There is no OpenPGP key of your own for {addr}. Generate one under Settings, OpenPGP, \
             or choose a key in the account's settings.",
            &[("addr", from)],
        ));
    }
    if encrypt {
        for field in fields {
            for part in field.split(',') {
                let (_, addr) = crate::config::split_identity(part.trim());
                let addr = addr.trim();
                if addr.is_empty() {
                    continue;
                }
                if crate::pgp::public_key_for(&gpg, addr).is_none() {
                    return Err(crate::i18n::i18n_f(
                        "There is no OpenPGP key for {addr}. Ask them for their public key, or fetch it \
                         under Settings, OpenPGP.",
                        &[("addr", addr)],
                    ));
                }
            }
        }
    }
    Ok(())
}


/// Send Later presets (#145): `days` from today at `hour`:00, local time.
fn preset_time(days: i64, hour: u32) -> i64 {
    use chrono::{Duration, Local, TimeZone};
    let day = (Local::now() + Duration::days(days)).date_naive();
    let ndt = day.and_hms_opt(hour, 0, 0).unwrap_or_default();
    Local.from_local_datetime(&ndt).single().map(|t| t.timestamp()).unwrap_or_else(crate::datefmt::now)
}

/// The coming Monday at `hour`:00 local time (a Monday today means next week's).
fn next_monday(hour: u32) -> i64 {
    use chrono::Datelike;
    let today = chrono::Local::now().weekday().num_days_from_monday() as i64;
    let ahead = (7 - today) % 7;
    preset_time(if ahead == 0 { 7 } else { ahead }, hour)
}

/// The Send Later picker (#145): a calendar and an hour/minute pair, starting
/// from the scheduled time if there is one, else the next full hour. A time
/// already past is refused rather than queued to go at once by surprise.
fn pick_send_time(parent: Option<&gtk::Window>, current: Option<i64>, sender: relm4::Sender<ComposeInput>) {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    let start = match current {
        Some(t) => Local.timestamp_opt(t, 0).single().unwrap_or_else(Local::now),
        None => {
            let n = Local::now() + chrono::Duration::hours(1);
            n.with_minute(0).and_then(|n| n.with_second(0)).unwrap_or(n)
        }
    };
    let dialog = adw::MessageDialog::new(parent, Some(i18n("Send later").as_str()), None);
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("ok", &i18n("Schedule"));
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let calendar = gtk::Calendar::new();
    calendar.set_year(start.year());
    calendar.set_month(start.month0() as i32);
    calendar.set_day(start.day() as i32);
    bx.append(&calendar);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_halign(gtk::Align::Center);
    let hour = gtk::SpinButton::with_range(0.0, 23.0, 1.0);
    hour.set_value(start.hour() as f64);
    hour.set_orientation(gtk::Orientation::Vertical);
    hour.set_wrap(true);
    let minute = gtk::SpinButton::with_range(0.0, 55.0, 5.0);
    minute.set_value((start.minute() / 5 * 5) as f64);
    minute.set_orientation(gtk::Orientation::Vertical);
    minute.set_wrap(true);
    row.append(&hour);
    row.append(&gtk::Label::new(Some(":")));
    row.append(&minute);
    bx.append(&row);
    dialog.set_extra_child(Some(&bx));
    let chosen = move || -> Option<i64> {
        let d = calendar.date();
        let ndt = chrono::NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32)?
            .and_hms_opt(hour.value_as_int() as u32, minute.value_as_int() as u32, 0)?;
        Local.from_local_datetime(&ndt).single().map(|t| t.timestamp())
    };
    dialog.connect_response(None, move |dlg, resp| {
        if resp != "ok" {
            return;
        }
        match chosen() {
            Some(t) if t > crate::datefmt::now() => {
                let _ = sender.send(ComposeInput::SendAt(t));
            }
            _ => {
                // An explicit time in the past is a slip, not a request to
                // send at once.
                let d = adw::MessageDialog::new(
                    dlg.transient_for().as_ref(),
                    Some(i18n("That time has passed").as_str()),
                    Some(i18n("Choose a time later than now.").as_str()),
                );
                d.add_response("ok", &i18n("OK"));
                d.present();
            }
        }
    });
    dialog.present();
}
