//! The app's look outside the mail: light or dark, the bundled icon theme,
//! the colors that follow the light/dark flip, and reduced motion.

use adw::prelude::*;

use crate::config;

/// Force (or release) the app-wide color scheme per the appearance
/// preference — the whole chrome, not just message content, which has its own
/// setting.
pub(super) fn apply_app_theme(theme: config::AppTheme) {
    let scheme = if WIZARD_HOLDS_LIGHT.load(std::sync::atomic::Ordering::Relaxed) {
        adw::ColorScheme::ForceLight
    } else {
        match theme {
            config::AppTheme::System => adw::ColorScheme::Default,
            config::AppTheme::Light => adw::ColorScheme::ForceLight,
            config::AppTheme::Dark => adw::ColorScheme::ForceDark,
        }
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

/// Set while the welcome wizard is open. Its blue ground is designed for the
/// light scheme only, and libadwaita has no per-window scheme, so the whole
/// app is held light until the wizard closes.
pub(super) static WIZARD_HOLDS_LIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Styles that branch on the color scheme, which static CSS cannot do: a
/// dedicated provider (above the static stylesheet's priority) carries the
/// scheme-dependent values and reloads whenever the scheme flips.
///
/// - The message list's selection pill: the alpha that reads right on dark is
///   heavy on a light ground, so light mode runs 25% lighter.
/// - The remote-content banner's shield: amber on dark, a deeper orange on
///   light where amber washes out.
/// - The Refresh button while offline (#442): orange, a step deeper on light.
/// - The composer's ground: the reader's deeper page shade, read from the
///   live theme (#148) rather than the stock GNOME values, so a custom theme
///   carries through to the composer like it does to the reader.
///
/// `window` is only where the theme's named colors are read from.
pub(super) fn install_scheme_css(window: &impl IsA<gtk::Widget>) {
    let provider = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
    }
    let window = window.clone().upcast::<gtk::Widget>();
    let apply = move |provider: &gtk::CssProvider, dark: bool| {
        // The selection is the GNOME accent itself at full saturation, with
        // high-contrast white text, while the list holds the keyboard. A
        // click into the reader hands focus straight back to the list, so
        // that is nearly always; when something else really has it (the
        // composer, a search entry, or focus lost in a rebuild) the row
        // turns grey, as a sidebar's does, so it is plain that the list's
        // keys will not reach it (#274). An inactive window keeps the
        // accent: focus is not lost, the window is just behind another.
        // The conversation chip goes back to its grey pill there, and a tag
        // keeps the text color picked for its fill: the white chip and the
        // window's text color left the count white on white.
        let shield = if dark { "#ffca28" } else { "#ff7800" };
        // GNOME's orange: its lighter step on dark, its deeper one on light.
        let offline = if dark { "#ffa348" } else { "#e66100" };
        // The compose surface sits on the reader's deeper page ground — the
        // same shade the threaded cards float on, as the theme defines it.
        let (_, page, _) = crate::ui::message_view::theme_grounds_for(&window, dark);
        provider.load_from_string(&format!(
            ".message-list > row:selected .message-row, \
             .message-list > row:selected:hover .message-row, \
             .message-list > row:selected:active .message-row {{ \
               background-color: @accent_bg_color; color: white; }}\
             .message-list:not(:focus-within):not(:backdrop) > row:selected .message-row, \
             .message-list:not(:focus-within):not(:backdrop) > row:selected:hover .message-row, \
             .message-list:not(:focus-within):not(:backdrop) > row:selected:active .message-row {{ \
               background-color: alpha(@window_fg_color, 0.14); color: @window_fg_color; }}\
             .message-list:not(:focus-within):not(:backdrop) > row:selected .message-row label:not(.tag-chip) {{ \
               color: @window_fg_color; }}\
             .message-list:not(:focus-within):not(:backdrop) > row:selected .message-row .unread-dot {{ \
               background: @accent_bg_color; }}\
             .message-list:not(:focus-within):not(:backdrop) > row:selected .message-row .thread-chip {{ \
               background: alpha(@window_fg_color, 0.1); }}\
             .message-list:not(:focus-within):not(:backdrop) > row:selected .message-row .thread-chip image {{ \
               color: @window_fg_color; }}\
             .remote-alert image {{ color: {shield}; }}\
             .refresh-offline {{ color: {offline}; }}\
             .inline-compose-surface, .compose-pane {{ background-color: {page}; }}\
             .reader-split > separator {{ background-color: {page}; }}"
        ));
    };
    let style = adw::StyleManager::default();
    apply(&provider, style.is_dark());
    // A theme change moves the same colors without any scheme flip, so the
    // provider has to be reloadable on demand as well (AppMsg::Pref(PrefOutput::SetTheme)).
    SCHEME_REFRESH.with(|slot| {
        let provider = provider.clone();
        let apply = apply.clone();
        *slot.borrow_mut() = Some(Box::new(move || {
            apply(&provider, adw::StyleManager::default().is_dark());
        }));
    });
    style.connect_dark_notify(move |sm| {
        // The theme's named colors are re-resolved after this signal, not
        // before it: read them now and the lookup answers for the scheme
        // just left (a light-grey composer in dark mode). Apply once the
        // main loop comes round, as the reader does through its own message.
        let dark = sm.is_dark();
        let provider = provider.clone();
        let apply = apply.clone();
        gtk::glib::idle_add_local_once(move || apply(&provider, dark));
    });
}

/// Re-read the theme's colors into the scheme-dependent CSS, for a change
/// that moves them without flipping the color scheme.
pub(super) fn refresh_scheme_css() {
    SCHEME_REFRESH.with(|slot| {
        if let Some(refresh) = slot.borrow().as_ref() {
            refresh();
        }
    });
}

thread_local! {
    /// Reloads the scheme-dependent provider with the colors the live theme
    /// answers with now (see [`install_scheme_css`]).
    static SCHEME_REFRESH: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Draw every icon from the copies built into the binary, and register the
/// app icon so windows and dialogs can find it by name.
///
/// The bundle (tools/gen-icon-gresource.sh) holds every icon Hylki names,
/// every one GTK and libadwaita draw for their own widgets, as found on the
/// build machine, and the app's own icon. Icons once followed the user's
/// theme (#260); a theme that restyled some of them made the app look
/// different from one distribution to the next, so by default none of them
/// do. `desktop` (Settings → Appearance) gives the desktop's theme back, the
/// bundle filling in what it lacks.
pub(super) fn register_icons(desktop: bool) {
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        DESKTOP_ICON_PATH.get_or_init(|| theme.search_path());
        // Ahead of GTK's and libadwaita's own copies, so their widgets take
        // the bundle's: those copies come with whatever GTK the system has.
        let mut paths = vec![ICON_RESOURCE.to_string()];
        paths.extend(
            theme
                .resource_path()
                .into_iter()
                .map(|p| p.to_string())
                .filter(|p| p.trim_end_matches('/') != ICON_RESOURCE),
        );
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        theme.set_resource_path(&paths);
    }
    apply_icon_theme(desktop);
    gtk::Window::set_default_icon_name(crate::APP_ID);
}

/// Draw icons from Hylki's own theme, or from the desktop's. Applies at once:
/// GTK redraws every icon when the name changes.
///
/// Hylki's own way, the folders on disk leave the search altogether but for
/// the ones only Hylki writes to. Naming a theme that does not exist is not
/// enough by itself: GTK looks through hicolor's folders before the bundle,
/// and an icon of the same name there, from an icon theme installed in the
/// home folder or another program, drew a gear for the main menu on one
/// system. With nothing foreign left to search, the bundle answers for every
/// name.
pub(super) fn apply_icon_theme(desktop: bool) {
    let Some(settings) = gtk::Settings::default() else {
        return;
    };
    if desktop {
        // Back to whatever the desktop says, now and when it changes.
        settings.reset_property("gtk-icon-theme-name");
    } else {
        // Set by the app, the name outranks the desktop's setting, now and
        // whenever it changes: GTK would otherwise switch the theme back to it.
        settings.set_gtk_icon_theme_name(Some(ICON_THEME));
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let theme = gtk::IconTheme::for_display(&display);
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    if desktop {
        paths.extend(DESKTOP_ICON_PATH.get().cloned().unwrap_or_default());
        // The user's icon directory: inside Flatpak XDG_DATA_HOME is the
        // sandbox's, so GTK wouldn't look there on its own.
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(std::path::Path::new(&home).join(".local/share/icons"));
        }
    }
    // The app icon chosen in Settings, for the window's own icon.
    if let Some(own) = crate::app_icon::private_icons_root() {
        paths.push(own);
    }
    // Running from the source tree: the app's icons as the install has them.
    let dev = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    if dev.is_dir() {
        paths.push(dev.to_path_buf());
    }
    let paths: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    theme.set_search_path(&paths);
    if !desktop {
        let theme = theme.clone();
        gtk::glib::idle_add_local_once(move || check_icon_sources(&theme));
    }
}

/// An icon theme over the desktop's own icon folders, for the icons that are
/// the desktop's business rather than Hylki's: a launcher icon set outside
/// the app (#252).
pub(crate) fn desktop_icon_theme() -> gtk::IconTheme {
    let theme = gtk::IconTheme::new();
    let mut paths = DESKTOP_ICON_PATH.get().cloned().unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(std::path::Path::new(&home).join(".local/share/icons"));
    }
    let paths: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    theme.set_search_path(&paths);
    theme
}

/// Whether every bundled icon is drawn from the bundle: any that resolves
/// elsewhere is logged with the file that won, so the exported log names
/// the culprit when an icon looks wrong on someone's system.
fn check_icon_sources(theme: &gtk::IconTheme) {
    let dir = format!("{ICON_RESOURCE}/scalable/actions");
    let names = gtk::gio::resources_enumerate_children(&dir, gtk::gio::ResourceLookupFlags::NONE)
        .unwrap_or_default();
    let mut foreign = 0;
    for file in &names {
        let Some(name) = file.strip_suffix(".svg") else {
            continue;
        };
        let icon = theme.lookup_icon(
            name,
            &[],
            16,
            1,
            gtk::TextDirection::Ltr,
            gtk::IconLookupFlags::empty(),
        );
        let uri = icon.file().map(|f| f.uri().to_string()).unwrap_or_default();
        if !uri.starts_with(&format!("resource://{ICON_RESOURCE}/")) {
            foreign += 1;
            tracing::warn!("icon {name} is drawn from {uri}, not from Hylki's own icons");
        }
    }
    tracing::debug!(
        "icons: {} bundled, {foreign} drawn from elsewhere",
        names.len()
    );
    // The app icon may come from a source checkout's data/icons, which is
    // the same art, so it is only reported.
    let app = theme.lookup_icon(
        crate::APP_ID,
        &[],
        128,
        1,
        gtk::TextDirection::Ltr,
        gtk::IconLookupFlags::empty(),
    );
    tracing::debug!(
        "app icon: {}",
        app.file().map(|f| f.uri().to_string()).unwrap_or_default()
    );
}

/// Hylki's icon theme: a name no theme on disk carries, so the user's icon
/// theme is never consulted and the app looks the same on every desktop.
/// Lookups fall through to hicolor, which holds the bundle below and GTK's
/// and libadwaita's built-in icons.
const ICON_THEME: &str = "Hylki";

/// Where the bundled icons sit in the app's GResource (registered in `main`).
const ICON_RESOURCE: &str = "/co/hyprlab/Hylki/icons";

/// The icon folders GTK searched before Hylki narrowed them: what "Use the
/// desktop's icons" gives back.
static DESKTOP_ICON_PATH: std::sync::OnceLock<Vec<std::path::PathBuf>> = std::sync::OnceLock::new();

/// GNOME Shell turns animations off in every app while a remote desktop
/// session is on, to spare the connection; that is not the person asking
/// for less motion. Where the app can read GNOME's own Reduce Animation
/// switch (through dconf, which the Flatpak has no access to), Hylki's
/// animations follow that switch instead, now and whenever it changes.
pub(super) fn follow_motion_setting() {
    use gtk::gio;
    if gio::SettingsBackend::default().type_().name() != "DConfSettingsBackend" {
        return;
    }
    let readable = gio::SettingsSchemaSource::default()
        .and_then(|s| s.lookup("org.gnome.desktop.interface", true))
        .is_some_and(|s| s.has_key("enable-animations"));
    let Some(gtk_settings) = gtk::Settings::default().filter(|_| readable) else {
        return;
    };
    // The binding holds the settings object for as long as GTK's lives.
    gio::Settings::new("org.gnome.desktop.interface")
        .bind("enable-animations", &gtk_settings, "gtk-enable-animations")
        .flags(gio::SettingsBindFlags::GET)
        .build();
}

/// Whether things on screen should move (see [`follow_motion_setting`]).
pub(super) fn motion_wanted() -> bool {
    gtk::Settings::default().is_some_and(|s| s.is_gtk_enable_animations())
}

#[cfg(test)]
mod tests {
    /// Every icon the code names is in the bundle Hylki draws from: with
    /// the icon folders on disk out of the search, one missing from it shows
    /// as a broken image on every system. tools/gen-icon-gresource.sh adds
    /// any the code has come to name.
    #[test]
    fn every_icon_named_in_the_code_is_bundled() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = std::fs::read_to_string(root.join("resources/hylki.gresource.xml")).unwrap();
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(&root.join("src"), &mut files);
        let mut missing = std::collections::BTreeSet::new();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for (at, _) in text.match_indices("-symbolic\"") {
                let start = text[..at].rfind('"').map_or(at, |q| q + 1);
                let name = &text[start..at + "-symbolic".len()];
                if !name.starts_with('-')
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                {
                    let entry = format!("icons/scalable/actions/{name}.svg");
                    if !manifest.contains(&entry) {
                        missing.insert(name.to_string());
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "not in the icon bundle, run tools/gen-icon-gresource.sh: {missing:?}"
        );
    }
}
