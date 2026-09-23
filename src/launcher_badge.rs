//! The unread count on the app's launcher icon (#271).
//!
//! There is no portal for this. The de facto interface is Unity's
//! `com.canonical.Unity.LauncherEntry`: a broadcast `Update` signal naming
//! the desktop file, which KDE Plasma's task manager, Dash to Dock and Dash
//! to Panel read. A broadcast needs no bus permission in the Flatpak sandbox.
//! Plain GNOME Shell ignores it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use zbus::zvariant::Value;

const IFACE: &str = "com.canonical.Unity.LauncherEntry";
const PATH: &str = "/co/hyprlab/Hylki/LauncherEntry";

/// One connection for the life of the process: Plasma and Dash to Dock
/// drop a badge when the connection that set it goes away, so a connection
/// per update would clear the count as soon as it was shown.
fn connection() -> Option<&'static zbus::blocking::Connection> {
    static CONN: OnceLock<Option<zbus::blocking::Connection>> = OnceLock::new();
    CONN.get_or_init(|| match zbus::blocking::Connection::session() {
        Ok(conn) => Some(conn),
        Err(e) => {
            tracing::debug!("launcher badge: no session bus: {e}");
            None
        }
    })
    .as_ref()
}

/// Show `count` on the launcher icon; 0 hides the badge. Repeats of the
/// value last sent are skipped, since this runs on every unread change.
pub fn set_count(count: u32) {
    static LAST: Mutex<Option<u32>> = Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if *last == Some(count) {
            return;
        }
        *last = Some(count);
    }
    let Some(conn) = connection() else { return };
    let uri = format!("application://{}.desktop", crate::APP_ID);
    let mut props: HashMap<&str, Value> = HashMap::new();
    props.insert("count", Value::I64(i64::from(count)));
    props.insert("count-visible", Value::Bool(count > 0));
    if let Err(e) = conn.emit_signal(None::<&str>, PATH, IFACE, "Update", &(uri, props)) {
        tracing::debug!("launcher badge not set: {e}");
    }
}
