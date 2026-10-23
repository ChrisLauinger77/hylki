//! Answers given from the reader that are remembered: the lists
//! unsubscribed from (unsubscribed.toml) and the invitations answered
//! (invites.toml).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::config_base;

/// A mailing list the user has unsubscribed from through the reader's
/// button, so a later message from the same list can say so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsubscribedList {
    /// [`crate::models::Unsubscribe::key`]: the List-Id, or the From address.
    pub key: String,
    /// Who the mail came from, for the record.
    #[serde(default)]
    pub name: String,
    /// Unix seconds of the request.
    #[serde(default)]
    pub at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct UnsubscribedFile {
    #[serde(default)]
    pub(super) lists: Vec<UnsubscribedList>,
}

fn unsubscribed_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("unsubscribed.toml"))
}

/// The lists left through the reader's Unsubscribe button, newest last.
pub fn load_unsubscribed() -> Vec<UnsubscribedList> {
    let Some(text) = unsubscribed_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    toml::from_str::<UnsubscribedFile>(&text)
        .unwrap_or_default()
        .lists
}

pub fn save_unsubscribed(lists: &[UnsubscribedList]) {
    let Some(path) = unsubscribed_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = UnsubscribedFile {
        lists: lists.to_vec(),
    };
    if let Ok(toml) = toml::to_string_pretty(&file) {
        let _ = std::fs::write(&path, toml);
    }
}

/// How many answered invitations are remembered. An invitation is
/// answered far more often than a mailing list is left, and the file is
/// read at every start — the oldest fall off rather than growing it
/// without end. A meeting whose answer has been forgotten simply offers
/// the buttons again.
const INVITE_ANSWER_LIMIT: usize = 500;

/// A meeting invitation answered through the reader's buttons (#223), so
/// the card says so when the message is opened again. The organizer's
/// calendar is the record that counts; this is only what the reader shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteAnswer {
    /// [`crate::models::Invite::key`]: the event's UID, plus the occurrence
    /// when the answer was to one of a series.
    pub key: String,
    /// The `PARTSTAT` sent: ACCEPTED, TENTATIVE or DECLINED.
    pub status: String,
    /// Unix seconds of the answer.
    #[serde(default)]
    pub at: i64,
    /// What the meeting was called, for the record.
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct InviteAnswerFile {
    #[serde(default)]
    pub(super) answers: Vec<InviteAnswer>,
}

fn invites_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("invites.toml"))
}

/// The invitations answered through the reader, newest last.
pub fn load_invite_answers() -> Vec<InviteAnswer> {
    let Some(text) = invites_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    toml::from_str::<InviteAnswerFile>(&text)
        .unwrap_or_default()
        .answers
}

pub fn save_invite_answers(answers: &[InviteAnswer]) {
    let Some(path) = invites_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let kept = answers.len().saturating_sub(INVITE_ANSWER_LIMIT);
    let file = InviteAnswerFile {
        answers: answers[kept..].to_vec(),
    };
    if let Ok(toml) = toml::to_string_pretty(&file) {
        let _ = std::fs::write(&path, toml);
    }
}
