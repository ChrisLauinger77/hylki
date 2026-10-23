//! Hylki's settings: the files under `~/.config/hylki/` (accounts, the
//! preferences in privacy.toml, filters, tags, sidebar and window state, and
//! smaller ones), the secrets kept in the system keyring, and where the
//! config, cache and data directories are.
//!
//! Each file has its own child module; every public item is re-exported
//! here, so callers name them as `config::Item`.

use std::path::PathBuf;

mod accounts;
mod bundle;
mod filters;
mod focus;
mod launch_files;
mod look;
mod prefs;
mod privacy;
mod remembered;
mod secrets;
mod sections;
mod smart_folders;
mod sound;
mod state;
mod tags;
mod toolbar;
mod window;

pub use accounts::*;
pub use bundle::*;
pub use filters::*;
pub use focus::*;
pub use launch_files::*;
pub use look::*;
pub use prefs::*;
pub use privacy::*;
pub use remembered::*;
pub use secrets::*;
pub use sections::*;
pub use smart_folders::*;
pub use sound::*;
pub use state::*;
pub use tags::*;
pub use toolbar::*;
pub use window::*;

#[cfg(test)]
mod filter_tests;
#[cfg(test)]
pub(crate) mod golden;
#[cfg(test)]
mod snapshot_tests;
#[cfg(test)]
mod tests;

/// Write a config file that only its owner can read, without ever having
/// existed as anything else.
///
/// `fs::write` then `set_permissions` leaves a window in which the file sits on
/// disk with whatever the umask allowed — brief, but these files carry
/// hostnames, usernames, OAuth client secrets and correspondent lists. Creating
/// with the mode already set closes it.
pub(crate) fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents.as_bytes())?;
    // An existing file keeps its old mode through `OpenOptions::mode`, which
    // only applies at creation — so tighten regardless.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// XDG base directories, with one twist: the beta channel running inside its
/// own Flatpak sandbox (app ID co.hyprlab.Hylki.Beta) would get an empty
/// `~/.var/app/co.hyprlab.Hylki.Beta` tree of its own — so it redirects to the
/// STABLE app's tree instead, sharing accounts, settings and the mail cache
/// with the stable install. The keyring service name is identical across
/// channels, so credentials are shared through the same redirection-free path.
/// Outside Flatpak (host builds) both channels already share `~/.config/hylki`
/// et al., so no redirection is needed or done.
fn shared_base(own: fn() -> Option<PathBuf>, sub: &str) -> Option<PathBuf> {
    if cfg!(feature = "beta")
        && std::env::var("FLATPAK_ID").is_ok_and(|id| id == "co.hyprlab.Hylki.Beta")
        && stable_data_present()
    {
        return Some(
            dirs::home_dir()?
                .join(".var/app/co.hyprlab.Hylki")
                .join(sub),
        );
    }
    own()
}

/// Whether the shared flatpak directory is actually reachable. Flatpak
/// silently SKIPS a `--filesystem` grant whose host path doesn't exist, which
/// left `~/.var/app/co.hyprlab.Hylki` pointing at the sandbox's throwaway
/// tmpfs on beta-only installs — accounts "saved" there and vanished on quit
/// (issue #83). The real fix is the manifest's `:create` suffix, which makes
/// flatpak create the host directory itself — a beta-first install thereby
/// establishes the standard persistent home a later stable install picks up,
/// in either install order. This check remains as defence in depth: should
/// the mount ever be missing anyway (an old manifest, a stripped-down
/// installation), the beta falls back to its own persistent home rather than
/// writing into the tmpfs. Decided once at first use, so our own writes
/// creating the path on the tmpfs mid-session can't flip it.
fn stable_data_present() -> bool {
    use std::sync::OnceLock;
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        dirs::home_dir()
            .map(|h| h.join(".var/app/co.hyprlab.Hylki").is_dir())
            .unwrap_or(false)
    })
}

pub fn config_base() -> Option<PathBuf> {
    shared_base(dirs::config_dir, "config")
}

pub fn cache_base() -> Option<PathBuf> {
    shared_base(dirs::cache_dir, "cache")
}

pub fn data_base() -> Option<PathBuf> {
    shared_base(dirs::data_dir, "data")
}

/// Write a settings file readable by the owner only (for modules outside
/// this one that keep their own file, like `cloud.toml`).
pub fn write_private_file(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    write_private(path, contents)
}

// serde helpers shared by the files' structs.

fn is_false(b: &bool) -> bool {
    !*b
}

/// The default of the switches that start on.
fn default_on() -> bool {
    true
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn is_true(b: &bool) -> bool {
    *b
}
