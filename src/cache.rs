//! Local SQLite cache for folders, message summaries, and bodies.
//!
//! The cache lets the app show mail instantly on startup, read offline, and
//! avoid re-fetching message bodies. Each per-account worker opens its own
//! connection (so the `!Send` `rusqlite::Connection` never crosses threads);
//! WAL mode keeps concurrent access from contending. Everything is keyed by
//! account so two accounts can both have an "INBOX". It is strictly
//! best-effort: any error is logged and degrades to "no cache".

use rusqlite::Connection;

mod addresses;
mod bodies;
mod flags;
mod gallery;
mod messages;
mod open;
mod outbox;
mod schema;
mod smart;
mod threads;

pub use gallery::*;
pub use messages::*;

#[cfg(test)]
use schema::*;

#[cfg(test)]
mod gallery_tests;
#[cfg(test)]
mod schema_tests;
#[cfg(test)]
mod test_util;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod thread_tests;

/// A message's keywords as one column: the server's, then any tag kept
/// locally for the same Message-ID (POP3, or an IMAP server that refuses
/// custom keywords). `local_tags` is small and keyed for this lookup, so the
/// correlated subquery is an index seek per row.
const KEYWORDS_COL: &str =
    "messages.keywords || ' ' || COALESCE((SELECT group_concat(lt.keyword, ' ') \
    FROM local_tags lt WHERE lt.account_id = messages.account_id \
    AND messages.message_id <> '' AND lt.message_id = messages.message_id), '')";

/// Split a keywords column back into the list a [`Message`] carries.
fn split_keywords(col: String) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in col.split_whitespace() {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(k)) {
            out.push(k.to_string());
        }
    }
    out
}

/// Gmail labels as one column. A label can hold a space, so they are kept
/// apart by the unit separator, which no mailbox name contains.
const LABEL_SEP: char = '\u{1f}';

fn join_labels(labels: &[String]) -> String {
    labels.join(&LABEL_SEP.to_string())
}

fn split_labels(col: String) -> Vec<String> {
    col.split(LABEL_SEP)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// One connection to the cache database. Each worker thread opens its own;
/// the methods live in the child modules by what they touch.
pub struct Cache {
    conn: Connection,
}
