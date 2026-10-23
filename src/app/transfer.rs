//! Moving and copying mail between accounts.

use super::*;

/// One message being moved to another account (#265): copied out of its
/// folder, into the destination, and only then taken out of the source.
pub(super) struct Transfer {
    pub(super) src_account: u32,
    pub(super) src_folder: u32,
    pub(super) src_path: String,
    pub(super) uid: u32,
    pub(super) dest_account: u32,
    pub(super) dest_path: String,
    pub(super) seen: bool,
    pub(super) flagged: bool,
    /// The message as the list had it, for the undo step.
    pub(super) row: Option<Message>,
}

/// A message that has landed in another account, and where its original
/// went: what undoing the move needs (#265).
pub(super) struct Landed {
    pub(super) src_account: u32,
    pub(super) src_path: String,
    /// The source account's Trash, where the original now is; `None` when
    /// it was erased, which cannot be undone.
    pub(super) src_trash: Option<String>,
    pub(super) dest_account: u32,
    pub(super) dest_path: String,
    pub(super) row: Message,
}

/// The moves to other accounts under way, all batches together.
#[derive(Default)]
pub(super) struct TransferTally {
    pub(super) total: usize,
    pub(super) done: usize,
    pub(super) failed: usize,
    /// The first failure's reason, for the report.
    pub(super) error: Option<String>,
    /// The receiving accounts, named in the report.
    pub(super) dest_accounts: std::collections::BTreeSet<u32>,
    /// The folders mail went into, synced once everything has landed.
    pub(super) dest_folders: std::collections::BTreeSet<(u32, String)>,
    /// What has landed so far, recorded as one undo step at the end.
    pub(super) landed: Vec<Landed>,
}
