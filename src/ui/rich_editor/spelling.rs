//! Spell-check languages for the editor's WebKit context, and the Spelling
//! submenu of its context menu.

use super::SpellLangsCallback;
use crate::i18n::{i18n, i18n_f};

/// Push the spell-checking preference onto the shared web context (#114).
/// Checking only happens in editable views, so the reader — which shares the
/// context — is unaffected.
///
/// The languages are ALWAYS set when checking is on: enchant is given no
/// language at all until told, so "enabled" without this call underlines
/// nothing. The configured languages win; blank follows the session locale;
/// and each is swapped for the closest installed dictionary when its own
/// is missing, since a language without a dictionary silently checks
/// nothing. WebKit accepts a word any of the languages knows (#365).
pub fn apply_spellcheck() {
    let ctx = crate::ui::message_view::shared_web_context();
    let on = crate::config::load_privacy().spellcheck;
    ctx.set_spell_checking_enabled(on);
    if !on {
        return;
    }
    let langs = resolved_spell_languages();
    let langs: Vec<&str> = langs.iter().map(String::as_str).collect();
    ctx.set_spell_checking_languages(&langs);
}

/// The dictionaries checking actually runs with: the configured languages,
/// else the session locale, mapped onto the installed dictionaries.
pub fn resolved_spell_languages() -> Vec<String> {
    let configured = crate::config::load_privacy().spellcheck_langs;
    crate::spell::resolve_languages(&configured, locale_language(), &installed_dictionaries())
}

/// The dictionaries "System language" stands for: the session locale's.
pub fn system_spell_languages() -> Vec<String> {
    crate::spell::resolve_languages("", locale_language(), &installed_dictionaries())
}

/// The session locale as a dictionary-style code ("en_US.UTF-8" → "en_US").
fn locale_language() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .map(|l| l.split('.').next().unwrap_or_default().to_string())
        .find(|l| !l.is_empty() && l != "C" && l != "POSIX")
}

/// The hunspell dictionaries that resolve to real files. Inside the Flatpak
/// most of `/usr/share/hunspell` is dangling symlinks until the locale
/// extension carries the language; `metadata()` follows symlinks, so a
/// dangling one errs out and is rightly skipped.
pub fn installed_dictionaries() -> Vec<String> {
    let mut codes: std::collections::BTreeSet<String> = Default::default();
    // Exactly the places enchant's hunspell provider reads (verified
    // empirically — a listed dictionary the checker can't use is the silent
    // failure this feature avoids): the classic locations, every SYSTEM XDG
    // data dir (which is how the Flatpak's bundled /app/share/hunspell set
    // is found), and enchant's per-user drop-in under the config dir. The
    // user DATA dir is deliberately absent: enchant does not search it.
    let mut dirs = vec![
        std::path::PathBuf::from("/usr/share/hunspell"),
        std::path::PathBuf::from("/usr/share/myspell"),
    ];
    for d in gtk::glib::system_data_dirs() {
        dirs.push(d.join("hunspell"));
    }
    dirs.push(
        gtk::glib::user_config_dir()
            .join("enchant")
            .join("hunspell"),
    );
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let is_dic = p.extension().is_some_and(|x| x == "dic");
            if is_dic && p.metadata().map(|m| m.is_file()).unwrap_or(false) {
                if let Some(s) = p.file_stem().and_then(|s| s.to_str()) {
                    codes.insert(s.to_string());
                }
            }
        }
    }
    // The runtime ships English many times over (eighteen region variants,
    // all hardlinks to the same data); the list keeps the five anyone looks
    // for. Other languages are shown as installed.
    const KEPT_ENGLISH: &[&str] = &["en_US", "en_GB", "en_CA", "en_AU", "en_NZ"];
    codes
        .into_iter()
        .filter(|c| c.split('_').next() != Some("en") || KEPT_ENGLISH.contains(&c.as_str()))
        .collect()
}

/// The right-click menu's Spelling Language submenu (#365): the system
/// language, then a check item per installed dictionary, ticked as the
/// setting stands. Choosing one switches it as the matching switch in
/// Settings does, and `cb` takes the setting that results.
pub(super) fn append_spelling_menu(menu: &webkit6::ContextMenu, cb: &SpellLangsCallback) {
    use gtk::prelude::ToVariant;
    if !crate::config::load_privacy().spellcheck {
        return;
    }
    let dicts = installed_dictionaries();
    if dicts.is_empty() {
        return;
    }
    let setting = crate::config::load_privacy().spellcheck_langs;
    let chosen = crate::spell::parse_languages(&setting);
    let system = system_spell_languages();
    let system_label = i18n_f(
        "System language — {language}",
        &[("language", &spell_language_names(&system))],
    );
    let entries = std::iter::once((String::new(), system_label, chosen.is_empty())).chain(
        dicts.iter().map(|d| {
            (
                d.clone(),
                crate::spell::language_display_name(d),
                chosen.contains(d),
            )
        }),
    );
    let sub = webkit6::ContextMenu::new();
    for (i, (code, label, on)) in entries.enumerate() {
        // A boolean state is what makes WebKit draw a check item.
        let action = gtk::gio::SimpleAction::new_stateful(
            &format!("hylki-spell-lang-{i}"),
            None,
            &on.to_variant(),
        );
        let (cb, setting, system) = (cb.clone(), setting.clone(), system.clone());
        action.connect_activate(move |_, _| {
            let next = crate::spell::toggle_language(&setting, &code, !on, &system);
            if let Some(f) = cb.borrow().as_ref() {
                f(next);
            }
        });
        sub.append(&webkit6::ContextMenuItem::from_gaction(
            &action, &label, None,
        ));
        if i == 0 {
            sub.append(&webkit6::ContextMenuItem::new_separator());
        }
    }
    menu.append(&webkit6::ContextMenuItem::new_separator());
    menu.append(&webkit6::ContextMenuItem::with_submenu(
        &i18n("Spelling Language"),
        &sub,
    ));
}

/// Dictionary codes as the names people read, joined: "Deutsch
/// (Deutschland), Français (France)".
pub fn spell_language_names(codes: &[String]) -> String {
    codes
        .iter()
        .map(|c| crate::spell::language_display_name(c))
        .collect::<Vec<_>>()
        .join(", ")
}
