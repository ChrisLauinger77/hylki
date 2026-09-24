# Working in this repository

Conventions for anyone — human or AI — editing Hylki. The code's own rules are
in [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md); this file is what has to be
remembered every session.

## Documentation has a shape, and it is enforced

The README once reached 667 lines because every change added "just one
paragraph" to it (#230). It is now the front door and nothing else. Before
writing a word of documentation, decide where it goes:

| What you have | Where it goes |
| --- | --- |
| A new feature worth pitching | `docs/FEATURES.md`; the README only if it displaces one of the eleven already there |
| How to set something up, or use it | `docs/DOCUMENTATION.md` |
| A keyboard change | `docs/KEYBOARD_SHORTCUTS.md` |
| A package, a distribution, an install caveat | `docs/INSTALLING.md` |
| A build dependency or step | `docs/BUILDING.md` |
| Credit for somebody's work | `data/CONTRIBUTORS` or `data/TRANSLATORS` for the name, `docs/CREDITS.md` for what they did |
| A licence or trademark note | `docs/LICENSE.md` |
| What changed in a release | `CHANGELOG.md` (detail) and `docs/RELEASE_NOTES.md` (the user-facing overview) |
| Artwork for the README or the docs | `data/repo/` — never `docs/`, which holds Markdown only |

A new `docs/*.md` has to be listed in `docs/README.md` and linked from
somewhere, or it is a file nobody will ever find.

**After touching any `.md`, run `python3 tools/check-docs.py`.** It checks all
of the above, plus every relative link and anchor in the repository. It is
fast, it has no dependencies, and a release should not go out while it fails
(see [docs/RELEASING.md](docs/RELEASING.md)).

## Prose style

- Factual, plain language. No marketing, no emoji in the changelog, notes or
  documentation.
- Say what the app does, not what it enables you to do.
- Comments in the code explain *why*, not *what*.

## Commits

The history is the maintainer's. Commits are authored by Hyprlab
<hyprlab@proton.me> and carry no tool attribution: no `Co-Authored-By:` line
for Claude or any other assistant, no "Generated with" footer in a commit
message or a pull request body (#230). The [AI notice](README.md#ai-notice)
declares how the app is built, once, for the whole repository. A
`Co-Authored-By:` trailer is still how an outside contributor is credited, with
the GitHub noreply address that resolves to their profile.

Subjects follow [Conventional Commits](https://www.conventionalcommits.org):
`type(area): summary`, lower case after the colon, imperative, no full stop,
72 characters at most and ideally nearer 50 (#277).

- Types: `feat`, `fix`, `perf`, `refactor`, `docs`, `build`, `ci`, `test`,
  `style`, `chore`, `revert`.
- The area is where the change lives: `reader`, `composer`, `list`,
  `sidebar`, `settings`, `imap`, `jmap`, `graph`, `smtp`, `tray`, `i18n`,
  `flatpak`, `rpm`, `release` and so on. Leave it out only when there is no
  single place.
- An issue number goes at the end: `fix(imap): keep deleted mail deleted (#260)`.
- Releases: `chore(release): 1.42.0`, `build(flatpak): pin the manifest to
  v1.42.0`, `build(rpm): sync the package version to 1.42.0`.

The body is optional and short: why the change exists, in a few lines wrapped
at 72, and nothing the diff already says. Past 100 words the detail belongs in
`docs/` or `CHANGELOG.md`, or the commit wants splitting. The prose rules
above apply too, plus one more: no em dashes, which a colon, a comma or a full
stop replaces.

`tools/git-hooks/commit-msg` refuses the attribution lines, em dashes, a
subject without a type, and an overlong subject or body. A clone is pointed at
it with `git config core.hooksPath tools/git-hooks`.

## Issue replies

A reply that says a request is done is one or two sentences: what changed,
from the user's side, and which version carries it. How it was built is in the
commit and the changelog for anyone who wants it. Only a reply that asks for
something (a log, a test) or explains a decline runs longer.

## Code

- User-facing strings go through the `i18n()` helpers in `src/i18n.rs`, and
  `tools/update-pot.sh` is run when they change.
- Contributor names are data (`data/CONTRIBUTORS`, `data/TRANSLATORS`), not
  constants in `src/app.rs`, which reads those files at build time.
- `cargo test` before committing; `cargo build` and run the app before saying
  something works.
