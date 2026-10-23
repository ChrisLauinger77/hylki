//! The About window and the people it thanks.

/// The contributors and translators shown in the About window's thanks lists.
/// Both come from the repository's metafiles, read in at build time: one
/// person per line, `Display Name <github-handle>`, with a translator's
/// languages after a dash. What each person contributed is credited in
/// `docs/CREDITS.md` and the changelog, not in the files.
pub(super) const CONTRIBUTORS: &str = include_str!("../../data/CONTRIBUTORS");
pub(super) const TRANSLATORS: &str = include_str!("../../data/TRANSLATORS");

/// One of those files as (display name, GitHub handle, note) rows. Comments,
/// blank lines and any line without a handle are skipped, so a typo in the
/// file costs that one row rather than the list.
pub(super) fn credits(file: &'static str) -> Vec<(&'static str, &'static str, &'static str)> {
    file.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (name, rest) = line.split_once('<')?;
            let (handle, note) = rest.split_once('>')?;
            Some((
                name.trim(),
                handle.trim(),
                note.trim().trim_start_matches('-').trim(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The credits metafiles are read at build time and shown in About, so a
    /// line that stops parsing loses a real person from the list silently.
    #[test]
    fn credits_files_parse() {
        for file in [CONTRIBUTORS, TRANSLATORS] {
            let rows = credits(file);
            let lines = file
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .count();
            assert_eq!(rows.len(), lines, "every non-comment line must parse");
            for (name, handle, _) in rows {
                assert!(!name.is_empty() && !handle.is_empty(), "{name}/{handle}");
                assert!(
                    !handle.contains(char::is_whitespace),
                    "{handle} is not a GitHub handle"
                );
            }
        }
    }

    /// Translators carry their languages after the dash; contributors carry
    /// nothing after the handle.
    #[test]
    fn credits_notes_are_translator_languages() {
        assert!(credits(CONTRIBUTORS)
            .iter()
            .all(|(_, _, note)| note.is_empty()));
        assert!(credits(TRANSLATORS)
            .iter()
            .all(|(_, _, note)| !note.is_empty()));
    }
}
