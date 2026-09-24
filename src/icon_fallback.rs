//! Stand-ins for icons the user's theme lists but cannot deliver (#278).
//!
//! Since icons go by their plain names (#260) the user's theme draws every
//! one it has. Inside the Flatpak a theme can name a file it cannot read:
//! a symlink into the host's `/usr/share/icons` lands in the runtime's
//! instead. GTK only finds that out when it draws the icon and paints its
//! "image-missing" placeholder; the bundled copy, further down the lookup,
//! is never reached.
//!
//! So every icon we carry a copy of is looked up in the current theme, and
//! for each file that cannot be read the copy is written to a directory of
//! our own under the same theme and subdirectory names, put first on the
//! search path. GTK keeps the first file it meets for a name in a theme
//! directory, so that one icon is replaced and the theme keeps the rest.

use gtk::gdk;
use gtk::gio;
use gtk::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Sizes checked: the theme can hold a separate file for each.
const SIZES: &[i32] = &[16, 24, 32, 48, 64, 128];

/// Check the current theme, and again whenever the desktop changes it.
/// The check runs once the window is up: a few thousand lookups are no
/// reason to hold up the first frame, and the theme itself loads for that
/// frame anyway.
pub fn install(display: &gdk::Display) {
    schedule(display.clone());
    let display = display.clone();
    gtk::Settings::for_display(&display)
        .connect_gtk_icon_theme_name_notify(move |_| schedule(display.clone()));
}

thread_local! {
    static PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn schedule(display: gdk::Display) {
    // One theme switch can notify more than once.
    if PENDING.replace(true) {
        return;
    }
    // Idle also puts it after GTK's own handler has switched themes.
    gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
        PENDING.set(false);
        repair(&display);
        gtk::glib::ControlFlow::Break
    });
}

fn root() -> Option<PathBuf> {
    Some(crate::config::cache_base()?.join("hylki").join("icon-fallback"))
}

fn repair(display: &gdk::Display) {
    let Some(root) = root() else { return };
    let theme = gtk::IconTheme::for_display(display);

    // Start from the theme alone: stand-ins from an earlier run or an
    // earlier theme would hide whether its own files are readable now.
    let mut paths = theme.search_path();
    if paths.iter().any(|p| p == &root) {
        paths.retain(|p| p != &root);
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        theme.set_search_path(&refs);
    }
    let _ = std::fs::remove_dir_all(&root);

    let copies = bundled_svgs(&theme);
    let scales = scales(display);
    let mut checked = BTreeSet::new();
    let mut broken: BTreeMap<PathBuf, &str> = BTreeMap::new();
    for (name, _) in &copies {
        for &size in SIZES {
            for &scale in &scales {
                let icon = theme.lookup_icon(
                    name,
                    &[],
                    size,
                    scale,
                    gtk::TextDirection::None,
                    gtk::IconLookupFlags::empty(),
                );
                let Some(path) = icon.file().and_then(|f| f.path()) else { continue };
                if checked.insert(path.clone()) && !readable(&path) {
                    broken.insert(path, name.as_str());
                }
            }
        }
    }
    if broken.is_empty() {
        return;
    }

    let mut written = BTreeSet::new();
    for (path, name) in &broken {
        let Some(rel) = theme_relative(&paths, path) else { continue };
        let Some(bytes) = gio::resources_lookup_data(&copies[*name], gio::ResourceLookupFlags::NONE)
            .ok()
        else {
            continue;
        };
        let dest = root.join(rel).join(format!("{name}.svg"));
        let ok = dest
            .parent()
            .is_some_and(|dir| std::fs::create_dir_all(dir).is_ok())
            && std::fs::write(&dest, &bytes).is_ok();
        if ok {
            written.insert(*name);
        } else {
            tracing::warn!("icons: could not write {}", dest.display());
        }
    }
    if written.is_empty() {
        return;
    }
    tracing::info!(
        "icons: the theme's files for {} cannot be read, using Hylki's copies",
        written.into_iter().collect::<Vec<_>>().join(", ")
    );
    let mut refs: Vec<&Path> = vec![root.as_path()];
    refs.extend(paths.iter().map(PathBuf::as_path));
    theme.set_search_path(&refs);
}

/// The scales the monitors draw at, where a theme's `@2x` files come in.
fn scales(display: &gdk::Display) -> BTreeSet<i32> {
    let mut out = BTreeSet::from([1]);
    let monitors = display.monitors();
    for i in 0..monitors.n_items() {
        if let Some(m) = monitors.item(i).and_downcast::<gdk::Monitor>() {
            out.insert(m.scale_factor());
        }
    }
    out
}

/// Every SVG icon in the theme's resource paths (ours, GTK's, libadwaita's),
/// by icon name, with the first found kept.
fn bundled_svgs(theme: &gtk::IconTheme) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for base in theme.resource_path() {
        let mut dirs = vec![format!("{}/", base.trim_end_matches('/'))];
        while let Some(dir) = dirs.pop() {
            let Ok(children) = gio::resources_enumerate_children(&dir, gio::ResourceLookupFlags::NONE)
            else {
                continue;
            };
            for child in children {
                if child.ends_with('/') {
                    dirs.push(format!("{dir}{child}"));
                } else if let Some(name) = child.strip_suffix(".svg") {
                    out.entry(name.to_string()).or_insert_with(|| format!("{dir}{child}"));
                }
            }
        }
    }
    out
}

/// Whether the file behind a theme entry can be opened: a dangling
/// symlink, or one pointing outside the sandbox, cannot.
fn readable(path: &Path) -> bool {
    std::fs::File::open(path)
        .and_then(|f| f.metadata())
        .is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// `<search path>/<theme>/<subdir>/<file>` → `<theme>/<subdir>`.
fn theme_relative(search: &[PathBuf], path: &Path) -> Option<PathBuf> {
    let rel = search.iter().find_map(|s| path.strip_prefix(s).ok())?;
    let dir = rel.parent()?;
    // A file directly in a search path belongs to no theme.
    (dir.components().count() >= 2).then(|| dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_relative_keeps_theme_and_subdir() {
        let search = vec![PathBuf::from("/a/icons"), PathBuf::from("/b/icons")];
        assert_eq!(
            theme_relative(&search, Path::new("/b/icons/Mine/symbolic/places/x-symbolic.svg")),
            Some(PathBuf::from("Mine/symbolic/places"))
        );
        assert_eq!(theme_relative(&search, Path::new("/b/icons/Mine/x.svg")), None);
        assert_eq!(theme_relative(&search, Path::new("/b/icons/x.svg")), None);
        assert_eq!(theme_relative(&search, Path::new("/c/icons/Mine/16/x.svg")), None);
    }
}
