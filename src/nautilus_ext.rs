//! The GNOME Files (Nautilus) right-click extension (#188): "Send with
//! Vireo" on selected files opens a composer with them attached.
//!
//! Nautilus loads its extensions on the host, outside any sandbox, so the
//! copy bundled here is installed into the user's own extension directory
//! on request (Settings → System → GNOME Files) and removed the same way.
//! The extension itself launches Vireo by desktop id, so it does not care
//! whether the app is the Flatpak or a native package.

use std::path::PathBuf;

/// The extension, verbatim: `data/nautilus/vireo-nautilus.py`.
pub const SOURCE: &str = include_str!("../data/nautilus/vireo-nautilus.py");
const FILE_NAME: &str = "vireo-nautilus.py";

/// Whether (and which) copy of the extension the user's directory holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    NotInstalled,
    /// The installed file is this build's copy.
    Installed,
    /// A file is installed but differs from this build's copy (an older
    /// release's, or edited by hand).
    Outdated,
}

/// The user's home — the host's, from inside the sandbox too.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).or_else(dirs::home_dir)
}

/// The per-user extension directory as Nautilus sees it. Inside Flatpak
/// XDG_DATA_HOME is the sandbox's private dir, so the host's default is
/// used (where the manifest mounts `xdg-data/nautilus-python/extensions`).
pub fn dir() -> Option<PathBuf> {
    let base = if crate::platform::is_flatpak() {
        home()?.join(".local/share")
    } else {
        dirs::data_dir()?
    };
    Some(base.join("nautilus-python/extensions"))
}

/// Where the extension is (or would be) installed.
pub fn path() -> Option<PathBuf> {
    dir().map(|d| d.join(FILE_NAME))
}

pub fn status() -> Status {
    path().map(|p| status_at(&p)).unwrap_or(Status::NotInstalled)
}

fn status_at(path: &std::path::Path) -> Status {
    match std::fs::read_to_string(path) {
        Err(_) => Status::NotInstalled,
        Ok(text) if text == SOURCE => Status::Installed,
        Ok(_) => Status::Outdated,
    }
}

/// Write (or overwrite) this build's copy of the extension.
pub fn install() -> Result<PathBuf, String> {
    let path = path().ok_or_else(|| "no home directory".to_string())?;
    install_at(&path)?;
    tracing::info!("nautilus extension installed at {}", path.display());
    Ok(path)
}

fn install_at(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, SOURCE).map_err(|e| format!("{}: {e}", path.display()))
}

/// Delete the installed copy, if any.
pub fn remove() -> Result<(), String> {
    let Some(path) = path() else { return Ok(()) };
    remove_at(&path)
}

fn remove_at(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::info!("nautilus extension removed from {}", path.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Ask GNOME Files to quit (what `nautilus -q` does), so that its next
/// window opens with the extension loaded — or without it, once removed.
/// Nothing is started if Files is not running.
pub fn quit_files() -> Result<(), String> {
    use gtk::glib::prelude::ToVariant;
    let conn = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    let params = (
        "quit",
        Vec::<gtk::glib::Variant>::new(),
        std::collections::HashMap::<String, gtk::glib::Variant>::new(),
    )
        .to_variant();
    match conn.call_sync(
        Some("org.gnome.Nautilus"),
        "/org/gnome/Nautilus",
        "org.gtk.Actions",
        "Activate",
        Some(&params),
        None,
        gtk::gio::DBusCallFlags::NO_AUTO_START,
        5000,
        gtk::gio::Cancellable::NONE,
    ) {
        Ok(_) => Ok(()),
        // Not running: nothing to restart, and nothing to report.
        Err(e) if e.matches(gtk::gio::DBusError::ServiceUnknown)
            || e.matches(gtk::gio::DBusError::NameHasNoOwner) =>
        {
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_status_remove_round_trip() {
        let dir = std::env::temp_dir().join(format!("vireo-nautilus-ext-{}", std::process::id()));
        let path = dir.join("extensions").join(FILE_NAME);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(status_at(&path), Status::NotInstalled);
        install_at(&path).unwrap();
        assert_eq!(status_at(&path), Status::Installed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SOURCE);
        // An older release's copy (anything that differs) reads as outdated.
        std::fs::write(&path, "# something else\n").unwrap();
        assert_eq!(status_at(&path), Status::Outdated);
        remove_at(&path).unwrap();
        assert_eq!(status_at(&path), Status::NotInstalled);
        // Removing what is not there is not an error.
        remove_at(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_extension_names_both_builds() {
        assert!(SOURCE.contains("\"co.hyprlab.Vireo\""));
        assert!(SOURCE.contains("\"co.hyprlab.Vireo.Beta\""));
        assert!(SOURCE.contains("Nautilus.MenuProvider"));
    }
}
