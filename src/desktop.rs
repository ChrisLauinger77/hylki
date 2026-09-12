//! The desktop's own settings (GNOME's `org.gnome.desktop.interface` and
//! kin), which the locale and GTK know nothing about: the clock format
//! (#173), the monospace font (#181). Read through the settings portal,
//! which works inside the Flatpak sandbox and on the host alike, or —
//! outside the sandbox, when no portal answers — straight from GSettings
//! where the schema is installed.

/// The string value of `key` in `namespace`, if the desktop has one.
pub fn setting(namespace: &str, key: &str) -> Option<String> {
    fn from_value(v: zbus::zvariant::Value<'_>) -> Option<String> {
        match v {
            zbus::zvariant::Value::Value(inner) => from_value(*inner),
            zbus::zvariant::Value::Str(s) => Some(s.as_str().to_string()),
            _ => None,
        }
    }
    let portal = (|| -> Option<String> {
        let conn = zbus::blocking::Connection::session().ok()?;
        let reply = conn
            .call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.portal.Settings"),
                "ReadOne",
                &(namespace, key),
            )
            .ok()?;
        let body = reply.body();
        let v: zbus::zvariant::OwnedValue = body.deserialize().ok()?;
        from_value(v.into())
    })();
    if portal.is_some() || std::env::var_os("FLATPAK_ID").is_some() {
        return portal;
    }
    use gtk::gio::prelude::SettingsExt;
    let source = gtk::gio::SettingsSchemaSource::default()?;
    source.lookup(namespace, true)?;
    let settings = gtk::gio::Settings::new(namespace);
    Some(settings.string(key).as_str().to_string())
}

/// The desktop's monospace font as a Pango description, "Monospace 10"
/// where the desktop names none.
pub fn monospace_font() -> String {
    setting("org.gnome.desktop.interface", "monospace-font-name")
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| "Monospace 10".to_string())
}
