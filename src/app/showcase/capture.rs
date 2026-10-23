//! Capturing the window to a PNG, and the small helpers the showcase hooks
//! share.

use adw::prelude::*;
use relm4::prelude::*;

use crate::app::{AppModel, AppMsg};

/// Render the window's widget tree to a PNG at 2x (crisp text for marketing
/// shots). Content only — the compositor's shadow/frame is not part of the
/// tree, which is exactly what the site and store screenshots want.
pub(crate) fn showcase_capture(win: &gtk::Widget, path: &str) {
    let (w, h) = (win.width(), win.height());
    if w == 0 || h == 0 {
        tracing::error!("showcase: window not realized");
        return;
    }
    let paintable = gtk::WidgetPaintable::new(Some(win));
    let snapshot = gtk::Snapshot::new();
    snapshot.scale(2.0, 2.0);
    gtk::prelude::PaintableExt::snapshot(&paintable, &snapshot, w as f64, h as f64);
    let Some(node) = snapshot.to_node() else {
        // A fully occluded window is suspended by the compositor and stops
        // producing frames, so the snapshot comes back empty. Raise it and
        // try again shortly.
        tracing::warn!("showcase: nothing to render (window suspended?) — presenting + retrying");
        if let Some(w) = win.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
            w.present();
        }
        let win = win.clone();
        let path = path.to_string();
        gtk::glib::timeout_add_seconds_local_once(2, move || {
            showcase_capture(&win, &path);
        });
        return;
    };
    let Some(renderer) = win.native().and_then(|n| n.renderer()) else {
        tracing::error!("showcase: no renderer");
        return;
    };
    let rect = gtk::graphene::Rect::new(0.0, 0.0, (w * 2) as f32, (h * 2) as f32);
    let texture = renderer.render_texture(&node, Some(&rect));
    match texture.save_to_png(path) {
        Ok(()) => tracing::info!("showcase: saved {path}"),
        Err(e) => tracing::error!("showcase: could not save {path}: {e}"),
    }
}

/// Sends `msg` to the app `secs` seconds from now.
pub(super) fn input_after(sender: &ComponentSender<AppModel>, secs: u32, msg: AppMsg) {
    let s = sender.clone();
    gtk::glib::timeout_add_seconds_local_once(secs, move || s.input(msg));
}

/// Showcase helper: scroll every scrolled window under `w` `frac` of the
/// way down (1.0 = its end).
fn scroll_all_to(w: &gtk::Widget, frac: f64) {
    if let Some(sw) = w.downcast_ref::<gtk::ScrolledWindow>() {
        let adj = sw.vadjustment();
        adj.set_value((adj.upper() - adj.page_size()) * frac.clamp(0.0, 1.0));
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        scroll_all_to(&c, frac);
        child = c.next_sibling();
    }
}

/// A showcase value with an optional `@<seconds>` after it: the value, and
/// the seconds or `default`.
pub(super) fn showcase_at(v: &str, default: u32) -> (&str, u32) {
    match v.rsplit_once('@') {
        Some((value, at)) => (value, at.parse().unwrap_or(default)),
        None => (v, default),
    }
}

/// A made-up OpenPGP verdict for HYLKI_SHOWCASE_PGP (demo only).
pub(in crate::app) fn showcase_pgp(which: &str) -> Option<crate::models::PgpStatus> {
    use crate::models::{PgpSignature as S, PgpStatus, PgpTrust};
    let good = |trust| S::Good {
        signer: "Priya Nair <priya@studio.dev>".into(),
        key_id: "8F2A6C1D9B3E4F70".into(),
        trust,
    };
    let (encrypted, signature) = match which {
        "good" => (false, good(PgpTrust::Full)),
        "untrusted" => (false, good(PgpTrust::Unknown)),
        "bad" => (
            false,
            S::Bad {
                signer: "Priya Nair <priya@studio.dev>".into(),
            },
        ),
        "nokey" => (
            false,
            S::NoKey {
                key_id: "8F2A6C1D9B3E4F70".into(),
            },
        ),
        "enc" => (true, good(PgpTrust::Full)),
        _ => return None,
    };
    Some(PgpStatus {
        encrypted,
        decrypted: encrypted,
        signature,
        ..Default::default()
    })
}

/// Showcase: log where keyboard focus is (#266), in the newest visible
/// window, the main one only when nothing else is up: the field row's
/// title when it is in one, else the focused widget's type and the classes
/// of the views around it, and for a WebView where its document's own
/// focus and caret are.
pub(super) fn showcase_log_focus(main: &adw::ApplicationWindow) {
    let tops = gtk::Window::toplevels();
    let main: gtk::Window = main.clone().upcast();
    let focus = (0..tops.n_items())
        .filter_map(|i| tops.item(i).and_downcast::<gtk::Window>())
        .filter(|w| w.is_visible())
        .max_by_key(|w| *w != main)
        .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w));
    let row = focus.as_ref().and_then(|f| {
        f.ancestor(adw::EntryRow::static_type())
            .and_downcast::<adw::EntryRow>()
    });
    match (row, focus) {
        (Some(row), _) => tracing::info!("showcase: focus in the {:?} row", row.title()),
        (None, Some(f)) => {
            // A few ancestors' classes say which view a WebView belongs to.
            let mut path = vec![f.type_().name().to_string()];
            let mut w = f.parent();
            while let Some(p) = w {
                let classes = p.css_classes();
                if !classes.is_empty() {
                    path.push(
                        classes
                            .iter()
                            .map(|c| c.to_string())
                            .collect::<Vec<_>>()
                            .join("."),
                    );
                }
                if path.len() > 5 {
                    break;
                }
                w = p.parent();
            }
            tracing::info!("showcase: focus on {}", path.join(" < "));
            // In a WebView, where the document's own focus and caret are.
            if let Some(view) = f.downcast_ref::<webkit6::WebView>() {
                use webkit6::prelude::WebViewExt;
                view.evaluate_javascript(
                    "(document.activeElement ? document.activeElement.tagName : 'none') + \
                     ' has focus, selection ranges ' + getSelection().rangeCount + \
                     ', collapsed ' + (getSelection().rangeCount ? getSelection().isCollapsed : '-')",
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |r| {
                        if let Ok(v) = r {
                            tracing::info!("showcase: document: {}", v.to_str());
                        }
                    },
                );
            }
        }
        (None, None) => tracing::info!("showcase: no focus"),
    }
}

/// HYLKI_SHOWCASE_SETTINGS=accounts|prefs opens the Settings window on
/// that panel at 3 s (about: the About window; any other value names a
/// sidebar category), so its pages can be checked in stills too.
pub(super) fn stage_settings_window(sender: &ComponentSender<AppModel>, settings: Option<String>) {
    if let Some(panel) = settings {
        let s = sender.clone();
        gtk::glib::timeout_add_seconds_local_once(3, move || {
            s.input(if panel == "accounts" {
                AppMsg::OpenAccounts
            } else if panel == "about" {
                AppMsg::OpenAbout
            } else {
                AppMsg::OpenPreferences
            });
            // Any other value names a sidebar category (#141).
            if panel != "accounts" && panel != "prefs" && panel != "about" {
                let s = s.clone();
                gtk::glib::timeout_add_seconds_local_once(1, move || {
                    s.input(AppMsg::ShowSettingsPage(panel));
                });
            }
        });
        // HYLKI_SHOWCASE_SETTINGS_REOPEN=1 closes the window at
        // 5s and opens it again at 7s, to time a reopen.
        if std::env::var("HYLKI_SHOWCASE_SETTINGS_REOPEN").is_ok() {
            input_after(sender, 5, AppMsg::DebugCloseSettings);
            input_after(sender, 7, AppMsg::OpenAccounts);
        }
    }
}

/// Captures the window to `shot` at `delay` seconds (or
/// HYLKI_SHOWCASE_DELAY_MS): the Settings window when `settings` named a
/// panel, the newest window with HYLKI_SHOWCASE_TOP, else the main one.
pub(super) fn capture_window(
    root: &adw::ApplicationWindow,
    delay: u32,
    shot: String,
    settings: Option<String>,
) {
    let main: gtk::Window = root.clone().upcast();
    let settings_window = move || {
        let tops = gtk::Window::toplevels();
        (0..tops.n_items())
            .filter_map(|i| tops.item(i))
            .filter_map(|o| o.downcast::<gtk::Window>().ok())
            .filter(|w| *w != main && w.is_visible())
            // The newest: a dialog over Settings, when one is up.
            .last()
    };
    // With HYLKI_SHOWCASE_SCROLL set, the Settings window is
    // made tall a beat after opening and every scroller in it
    // is run to its end just before the capture, so the lower
    // groups of a panel can be checked.
    // The value is the fraction of the way down (default 1).
    let scroll: Option<f64> = std::env::var("HYLKI_SHOWCASE_SCROLL")
        .ok()
        .map(|v| v.parse().unwrap_or(1.0));
    if let (Some(_), Some(frac)) = (&settings, scroll) {
        // HYLKI_SHOWCASE_SCROLL_WIDTH overrides the 720px width,
        // for checking a panel at the window's real width.
        let width: i32 = std::env::var("HYLKI_SHOWCASE_SCROLL_WIDTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(720);
        let find = settings_window.clone();
        gtk::glib::timeout_add_seconds_local_once(4, move || {
            if let Some(w) = find() {
                w.set_default_size(width, 1300);
            }
        });
        let find = settings_window.clone();
        gtk::glib::timeout_add_seconds_local_once(delay.saturating_sub(1), move || {
            if let Some(w) = find() {
                scroll_all_to(w.upcast_ref::<gtk::Widget>(), frac);
            }
        });
    }
    let win = root.clone();
    let top = std::env::var_os("HYLKI_SHOWCASE_TOP").is_some();
    // HYLKI_SHOWCASE_DELAY_MS names the moment to the
    // millisecond instead (a frame inside an animation).
    let delay_ms: u64 = std::env::var("HYLKI_SHOWCASE_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(u64::from(delay) * 1000);
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(delay_ms), move || {
        let target = settings
            .as_ref()
            .map(|_| ())
            .or(top.then_some(()))
            .and_then(|_| settings_window())
            .map(|w| w.upcast::<gtk::Widget>())
            .unwrap_or_else(|| win.clone().upcast::<gtk::Widget>());
        showcase_capture(&target, &shot);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn showcase_values_take_an_optional_delay() {
        assert_eq!(showcase_at("inbox@3", 1), ("inbox", 3));
        assert_eq!(showcase_at("inbox", 1), ("inbox", 1));
        assert_eq!(showcase_at("a@b@x", 1), ("a@b", 1));
    }
}
