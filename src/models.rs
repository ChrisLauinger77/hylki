//! Core domain types shared across the UI and backend layers. Each kind of
//! thing has its own child module; every item is re-exported here, so
//! callers name them as `models::Item`.

mod attachments;
mod folders;
mod message;
mod outbox;
mod verdicts;

pub use attachments::*;
pub use folders::*;
pub use message::*;
pub use outbox::*;
pub use verdicts::*;

#[cfg(test)]
mod tests;

/// A configured mail account (one IMAP/SMTP identity).
#[derive(Debug, Clone)]
pub struct Account {
    pub id: u32,
    pub name: String,
    pub email: String,
    /// How the account is labelled in the UI (All Inboxes, reader chip). Defaults
    /// to the email address.
    pub label: String,
    /// Accent color used for the account dot, as a CSS color string.
    pub accent: String,
}
