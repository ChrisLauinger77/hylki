//! Undo and redo (#200): the steps recorded for each action and applying
//! them, instantly on screen when the action was recent.

use crate::models::Message;

/// Which flag a recorded change touched (#200).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum FlagKind {
    Read,
    Star,
    Tag(String),
}

/// One message a flag change covers. Folder ids are re-checked when the
/// change is applied — they are index-based and shift as folders come and go.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FlagRef {
    pub(super) folder_id: u32,
    pub(super) uid: u32,
    /// The list row's id, which is how the setters find every copy of it.
    pub(super) id: u32,
}

/// Put `kind` to `value` on each of `items`. Only messages the original
/// action actually changed are listed, so one value covers them all.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FlagChange {
    pub(super) kind: FlagKind,
    pub(super) value: bool,
    pub(super) items: Vec<FlagRef>,
}

/// One step of the undo/redo history (#200). A step is a thing to *do*: the
/// undo stack holds the step that reverses each action, and applying a step
/// pushes its [`UndoStep::inverse`] onto the other stack. Operations with no
/// inverse — a permanent delete, an emptied folder, a sent message — are
/// never recorded.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum UndoStep {
    /// Take `message_ids` out of `from` and put them in `to`. Symmetric, so
    /// redoing a move is the same request with the two folders swapped.
    Move {
        from: String,
        to: String,
        message_ids: Vec<String>,
    },
    /// Set flags back to what they were.
    Flags(Vec<FlagChange>),
    CreateFolder {
        path: String,
    },
    /// Only ever the inverse of a [`UndoStep::CreateFolder`]: deleting a
    /// folder the user made is undoing that, not a delete of its own. A
    /// folder the user deletes is not undoable at all and never recorded.
    DeleteFolder {
        path: String,
    },
    RenameFolder {
        from: String,
        to: String,
    },
}

impl UndoStep {
    /// The step that puts this one back.
    pub(super) fn inverse(&self) -> UndoStep {
        match self {
            UndoStep::Move {
                from,
                to,
                message_ids,
            } => UndoStep::Move {
                from: to.clone(),
                to: from.clone(),
                message_ids: message_ids.clone(),
            },
            UndoStep::Flags(changes) => UndoStep::Flags(
                changes
                    .iter()
                    .map(|c| FlagChange {
                        value: !c.value,
                        ..c.clone()
                    })
                    .collect(),
            ),
            UndoStep::CreateFolder { path } => UndoStep::DeleteFolder { path: path.clone() },
            UndoStep::DeleteFolder { path } => UndoStep::CreateFolder { path: path.clone() },
            UndoStep::RenameFolder { from, to } => UndoStep::RenameFolder {
                from: to.clone(),
                to: from.clone(),
            },
        }
    }
}

/// One entry on the undo (or redo) stack: the step to apply, and what the
/// user called the action it belongs to, so the menu can say "Undo Archive"
/// rather than just "Undo".
pub(super) struct UndoEntry {
    pub(super) account_id: u32,
    pub(super) what: String,
    pub(super) step: UndoStep,
    /// When the action happened, for the [`INSTANT_UNDO`] window.
    pub(super) at: std::time::Instant,
    /// The rows a move took off screen, kept whole (bodies included) so a
    /// quick undo can put them straight back without a round trip.
    pub(super) rows: Vec<Message>,
    /// The conversations those rows were part of, by Message-ID. Taken as
    /// the move is recorded: moving a message forgets its account's
    /// conversations, so by the time the undo runs they are gone.
    pub(super) threads: Vec<(String, Vec<Message>)>,
    /// Whether applying this entry puts `rows` back on screen (undoing a
    /// move) or takes them away again (redoing it).
    pub(super) rows_return: bool,
    /// Steps on other accounts that belong to the same action, applied with
    /// `step`: a move to another account (#265) is undone by bringing the
    /// original back on one account and taking the copy away on the other.
    pub(super) also: Vec<(u32, UndoStep)>,
}

/// How long after an action its undo still repaints the list itself rather
/// than waiting for the server (#200). Long enough to cover "that was a
/// mistake", short enough that the rows it puts back are still what is
/// there — past this, another client or a sync may have moved on.
pub(super) const INSTANT_UNDO: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
mod tests {
    #[test]
    fn every_undo_step_is_its_own_inverse_twice_over() {
        use super::{FlagChange, FlagKind, FlagRef, UndoStep};
        let steps = vec![
            UndoStep::Move {
                from: "Archive".into(),
                to: "INBOX".into(),
                message_ids: vec!["a@b.c".into()],
            },
            UndoStep::Flags(vec![
                FlagChange {
                    kind: FlagKind::Read,
                    value: false,
                    items: vec![FlagRef {
                        folder_id: 1,
                        uid: 7,
                        id: 7,
                    }],
                },
                FlagChange {
                    kind: FlagKind::Tag("Work".into()),
                    value: true,
                    items: vec![FlagRef {
                        folder_id: 1,
                        uid: 8,
                        id: 8,
                    }],
                },
            ]),
            UndoStep::CreateFolder {
                path: "Projects".into(),
            },
            UndoStep::DeleteFolder {
                path: "Projects".into(),
            },
            UndoStep::RenameFolder {
                from: "Old".into(),
                to: "New".into(),
            },
        ];
        for step in &steps {
            assert_eq!(&step.inverse().inverse(), step, "{step:?} round-trips");
            assert_ne!(&step.inverse(), step, "{step:?} actually reverses");
        }
        // A move undoes by swapping the two folders, which is what makes
        // redo the very same request back the other way.
        assert_eq!(
            steps[0].inverse(),
            UndoStep::Move {
                from: "INBOX".into(),
                to: "Archive".into(),
                message_ids: vec!["a@b.c".into()],
            }
        );
        // Undoing a folder someone made deletes it again; there is no step
        // that undoes a folder the user deleted.
        assert_eq!(
            UndoStep::CreateFolder {
                path: "Projects".into()
            }
            .inverse(),
            UndoStep::DeleteFolder {
                path: "Projects".into()
            }
        );
    }
}
