//! The two one-line settings files read before anything else starts:
//! the interface language and the GTK renderer.

use std::path::PathBuf;

use super::config_base;

/// The interface language the user chose in Settings, as a locale code
/// ("fr", "en"); empty = the system's own (#179). Its own small file
/// rather than a preference in privacy.toml: it is read before anything
/// else starts, when no TOML has been parsed yet.
pub fn load_language() -> String {
    language_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn save_language(lang: &str) {
    let Some(path) = language_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if lang.trim().is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        let _ = std::fs::write(&path, format!("{}\n", lang.trim()));
    }
}

/// How GTK draws the windows (#403): empty for GTK's own choice, else a
/// `GSK_RENDERER` value ("vulkan", "gl", "cairo"). Its own small file, as
/// the language is: it has to be read before GTK starts.
pub fn load_renderer() -> String {
    renderer_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn save_renderer(renderer: &str) {
    let Some(path) = renderer_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if renderer.trim().is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        let _ = std::fs::write(&path, format!("{}\n", renderer.trim()));
    }
}

fn renderer_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("renderer"))
}

fn language_path() -> Option<PathBuf> {
    Some(config_base()?.join("hylki").join("language"))
}
