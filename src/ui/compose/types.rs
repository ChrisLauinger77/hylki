//! The data a composer is opened with, and the small types its model is made
//! of.

use crate::config::{ComposeFormat, SignaturePosition};
use crate::models::DraftOrigin;

/// Which recipient field a suggestion is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    To,
    Cc,
    Bcc,
}

/// One selectable "from" account in the compose window.
#[derive(Debug, Clone)]
pub struct ComposeAccount {
    pub id: u32,
    pub label: String,
    /// Signature text appended to the body when this account is selected:
    /// its default one, or empty when new messages start without.
    pub signature: String,
    /// Every signature of the account (#445), offered by the signature
    /// button.
    pub signatures: Vec<crate::config::NamedSignature>,
    /// The identity's sending address (the account's own, or an alias's).
    pub email: String,
    /// The account's chosen OpenPGP key (fingerprint), if any (#133).
    pub pgp_key: Option<String>,
    /// The account signs its mail by default (#267): Sign starts on.
    pub sign_default: bool,
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
    /// For a forward, the forwarded message's Message-ID (#454).
    pub forwards: String,
    /// When editing an existing draft, its origin (so saving/sending replaces it).
    pub draft_origin: Option<DraftOrigin>,
    /// Start with Encrypt (and so Sign) on: a reply to an encrypted message (#133).
    pub encrypt: bool,
    /// When editing a queued Outbox message, the row this replaces.
    pub outbox_origin: Option<u32>,
    /// For a reply: the original's To+Cc, so the composer can answer from the
    /// alias the mail was addressed to (#34). Empty otherwise.
    pub reply_addressed_to: String,
    /// A From of the user's own making (#347): a draft saved from an address
    /// that is no identity, or a reply to a `+tag` address. "Name <addr>"
    /// or a bare address; the composer opens its From as text when this is
    /// not the identity it selects.
    pub custom_from: String,
    /// For a new message: the address to send from when the user has chosen
    /// a default identity in Settings (#157). Empty = the account's own
    /// address. Ignored when `reply_addressed_to` names an identity.
    pub from_address: String,
    /// Send Later (#145): a queued message's scheduled time, kept while it is
    /// edited so Send re-queues it for the same moment.
    pub send_at: Option<i64>,
    /// Files handed in from GNOME Files that were too big to attach: the
    /// composer opens its cloud upload dialog on them as soon as it is up,
    /// so the links land in the body instead.
    pub cloud_uploads: Vec<std::path::PathBuf>,
    /// Files dropped on the main window's Insert in New Message card: the
    /// pictures go in the text once the editor is up, the rest attached.
    pub inline_files: Vec<std::path::PathBuf>,
    /// The quoted original's remote content was not loaded in the reader,
    /// so the editor does not load its pictures either (#295).
    pub block_remote_images: bool,
    /// The body is a message picked up again as it was written (one the
    /// worker handed back unsent): it already has its blank line and its
    /// signature. Drafts and queued messages count as this without it.
    pub resumed: bool,
    /// The body is a copy of a whole message (Edit as New), which carries
    /// its own signature: a second one is not put in (#385). Insert
    /// Signature in the menu adds one by hand.
    pub no_signature: bool,
    /// Editing a template (#360): `draft_origin` is where it is kept, and
    /// saving puts it back there rather than in Drafts.
    pub template: bool,
    /// The message replied to or forwarded had no HTML part (#436): with
    /// the setting on, the composer opens in plain text.
    pub original_plain: bool,
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
    /// Own addresses to include in the suggestions loaded after opening.
    pub own_addresses: Vec<(String, String)>,
    /// Whether the pane starts hosted in a standalone window (vs. inline).
    pub windowed: bool,
    /// Whether the inline/window toggle button is offered (reply/forward only).
    pub can_toggle: bool,
    /// Compact reply (#86 follow-up): the split reply hides every address/
    /// subject row and shows just the editor — popping out to a window brings
    /// the full fields back.
    pub compact: bool,
    /// Whether the inline header carries the window decorations. Off for a
    /// split reply placed beneath the reader (#212), whose header stays.
    pub decorations: bool,
    /// What the message is written in: rich text, Markdown, HTML
    /// source, or plain text.
    pub format: ComposeFormat,
    /// Where the signature goes against a quoted original (#237).
    pub signature_position: SignaturePosition,
}

/// An account that suggests only from its directory and its own mail
/// (#410): what its directory is asked with, and the addresses in its mail.
#[derive(Debug)]
pub struct OwnOnly {
    pub(super) account: crate::config::AccountConfig,
    pub(super) addresses: std::collections::HashSet<String>,
}

/// Those accounts by id, as the From row knows them.
pub type OwnMail = std::collections::HashMap<u32, OwnOnly>;

/// One entry of the composer's history: a thing to *do*, and what the user
/// called the change it takes back, so the menu can say "Undo Typing".
#[derive(Debug)]
pub(super) struct ComposeUndoEntry {
    pub(super) step: ComposeStep,
    pub(super) what: String,
}

/// A reversible change inside the composer.
#[derive(Debug)]
pub(super) enum ComposeStep {
    /// The body's own text history. WebKit owns the text — every edit,
    /// every formatting command, every paste and every dropped image is in
    /// *its* stack, which cannot be read, counted or spliced. This marker
    /// stands in for all of it: while the marker is on top and the editor
    /// still has something to take back, each step asks the editor for one
    /// more; once the editor has run out the marker crosses to the other
    /// stack and the change underneath comes up.
    ///
    /// There is never more than one on a stack, and it is kept on top.
    Body,
    /// Put these files back among the attachments at `at`.
    Insert {
        at: usize,
        paths: Vec<std::path::PathBuf>,
    },
    /// Take `count` attachments out again, starting at `at`.
    Remove { at: usize, count: usize },
}

/// A share link placed in the body (#144), by the id of its paragraph.
#[derive(Clone, Debug)]
pub(super) struct CloudLink {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) url: String,
}

/// Where Insert Signature puts the signature (#385).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigPlace {
    /// Where the caret is; where the setting puts it when the body has no
    /// caret.
    Cursor,
    /// The top of the message.
    Start,
    /// The end of the message.
    End,
}
