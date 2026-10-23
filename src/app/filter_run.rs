//! A manual Apply Filters run (#198) over chosen folders, its progress
//! dialog and its report.

use super::*;

/// How long a manual "Apply Filters" run (#198) waits for the folders it asked
/// for before reporting on whatever came back. A folder that is offline, or
/// that errors, would otherwise leave the run without an answer for good.
pub(super) const FILTER_RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// The one response of the manual run's progress dialog: "Run in Background"
/// while it works, "Close" once it holds the report.
pub(super) const FILTER_RUN_RESPONSE: &str = "close";

/// What the rules did in one folder of a manual run.
#[derive(Default, Clone, Copy)]
pub(super) struct FolderTally {
    /// The most messages one pass over the folder was held up against. A
    /// folder answers twice (cache, then server) over the same mail, so the
    /// larger listing is the count — adding them would count it twice.
    pub(super) checked: usize,
    /// Messages at least one rule matched, whether or not anything was
    /// done about it (#201): mail tagged on an earlier sync matches again
    /// and needs nothing, and a report that only counted actions called
    /// that "none of them matched". Held the same way as `checked`.
    pub(super) matched: usize,
    /// Tags put on, and messages filed away. Both only ever happen once per
    /// message (a second pass sees the tag, or the pending move), so these
    /// add up across passes.
    pub(super) tagged: usize,
    pub(super) filed: usize,
}

/// A manual "Apply Filters" run (#198): the folders it covers, what the rules
/// have done in each so far, and the dialog watching it.
pub(super) struct FilterRun {
    /// Every folder the run covers, with its tally. A folder load answers
    /// twice (what the cache holds, then what the server sends), so the scope
    /// has to outlive the cache's answer or the server's would go unfiltered
    /// outside the Inbox. Cleared by [`FILTER_RUN_TIMEOUT`].
    pub(super) folders: std::collections::HashMap<(u32, u32), FolderTally>,
    /// The folders that have not been to the server and back yet.
    pub(super) pending: std::collections::HashSet<(u32, u32)>,
    /// Whether the run has already said what it did.
    pub(super) reported: bool,
    /// The progress dialog, while it is up. Gone once the run was sent to the
    /// background, in which case the report goes to the status bar instead.
    pub(super) dialog: Option<FilterRunDialog>,
}

impl FilterRun {
    /// The run's totals so far.
    pub(super) fn totals(&self) -> FolderTally {
        self.folders
            .values()
            .fold(FolderTally::default(), |mut t, f| {
                t.checked += f.checked;
                t.matched += f.matched;
                t.tagged += f.tagged;
                t.filed += f.filed;
                t
            })
    }

    /// How many of the run's folders are done.
    pub(super) fn done_count(&self) -> usize {
        self.folders.len() - self.pending.len()
    }
}

/// The widgets of a manual run's progress dialog, kept so its live text can be
/// rewritten as folders report and so the finish can turn it into the report.
pub(super) struct FilterRunDialog {
    pub(super) dialog: adw::MessageDialog,
    pub(super) spinner: gtk::Spinner,
    pub(super) body: gtk::Label,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_run_totals_add_up_the_folders() {
        let mut run = FilterRun {
            folders: Default::default(),
            pending: Default::default(),
            reported: false,
            dialog: None,
        };
        run.folders.insert(
            (1, 1),
            FolderTally {
                checked: 3,
                matched: 2,
                tagged: 1,
                filed: 0,
            },
        );
        run.folders.insert(
            (1, 2),
            FolderTally {
                checked: 4,
                matched: 1,
                tagged: 0,
                filed: 1,
            },
        );
        run.pending.insert((1, 2));
        let t = run.totals();
        assert_eq!((t.checked, t.matched, t.tagged, t.filed), (7, 3, 1, 1));
        assert_eq!(run.done_count(), 1);
    }
}
