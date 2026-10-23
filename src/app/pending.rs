//! Changes made on screen before the server confirms them (read marks, tags,
//! moves), and how long each is trusted over what a sync reports.

/// How long a read/unread change sent to a worker keeps overriding what the
/// server reports for its message and folder, should the worker's
/// confirmation never come (a dropped connection mid-request). A worker
/// busy with a large sync can take well over 20 seconds to reach a mark,
/// and read mail used to turn unread again in the meantime (#255).
pub(super) const PENDING_SEEN_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// The same for a tag, which the worker confirms: a confirmed tag still
/// wins for a few seconds over a list another connection fetched before
/// the STORE, and an unconfirmed one for as long as a busy queue can take.
pub(super) const PENDING_TAG_MAX: std::time::Duration = std::time::Duration::from_secs(300);

pub(super) const PENDING_TAG_SETTLED: std::time::Duration = std::time::Duration::from_secs(10);

/// The same for mail taken out of a folder (moved, deleted, marked as
/// spam): how long it stays off that folder's list without the worker's
/// word that the move has run.
pub(super) const PENDING_MOVE_MAX: std::time::Duration = std::time::Duration::from_secs(120);
