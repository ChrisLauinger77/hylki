//! The screenshot showcase: environment-driven hooks that stage the window
//! for the site and store screenshots and capture it. Demo and debug use only.

use super::*;

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

/// Showcase helper: scroll every scrolled window under `w` `frac` of the
/// way down (1.0 = its end).
pub(super) fn scroll_all_to(w: &gtk::Widget, frac: f64) {
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
pub(super) fn showcase_pgp(which: &str) -> Option<crate::models::PgpStatus> {
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

impl AppModel {
    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_showcase(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ShowcasePopout => {
                if let Some(m) = self.current.clone() {
                    let thread = self.current_thread.clone();
                    sender.input(AppMsg::OpenMessageWindow { message: m, thread });
                }
            }

            AppMsg::ShowcaseFolder { kind, account } => {
                let account = account.unwrap_or_else(|| self.active_account());
                let found = self
                    .folders
                    .get(&account)
                    .and_then(|fs| fs.iter().find(|f| f.kind == kind))
                    .map(|f| (f.id, f.name.clone(), f.path.clone()));
                if let Some((id, name, path)) = found {
                    self.select_folder(account, id, name, path);
                }
            }

            AppMsg::ShowcaseNotify => {
                let account = self.active_account();
                let inbox = self
                    .folders
                    .get(&account)
                    .and_then(|fs| fs.iter().find(|f| f.kind == FolderKind::Inbox))
                    .map(|f| f.id);
                let newest = inbox.and_then(|f| {
                    self.message_cache
                        .get(&(account, f))
                        .and_then(|ms| ms.iter().max_by_key(|m| m.timestamp))
                        .map(|m| (f, m.id, m.from_name.clone(), m.subject.clone()))
                });
                match newest {
                    Some((folder, id, from, subject)) => {
                        self.announce_new_mail(account, folder, id, &from, &subject, 0, true);
                    }
                    None => tracing::warn!(
                        "showcase notify: no Inbox message cached for account {account}"
                    ),
                }
            }
            AppMsg::ShowcaseDeleteAttachment { name, confirmed } => {
                if let Some(name) = name.strip_prefix("gallery:") {
                    self.showcase_confirm_delete = confirmed;
                    self.gallery
                        .emit(GalleryInput::ShowcaseDelete(name.to_string()));
                    return;
                }
                match self.attachment_target(&name, None) {
                    Some(target) if confirmed => sender.input(AppMsg::DeleteAttachment(target)),
                    Some(target) => sender.input(AppMsg::AskDeleteAttachment(target)),
                    None => tracing::warn!("showcase: no attachment called {name:?} on screen"),
                }
            }
            AppMsg::ShowcaseToolbarGap { zone, index } => {
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::ToolbarGapPreview { zone, index });
                }
            }
            AppMsg::ShowcaseComposeMenu { format } => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(if format {
                        ComposeInput::FormatMenu
                    } else {
                        ComposeInput::OverflowMenu
                    });
                }
            }
            AppMsg::ShowcaseComposePreview => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::TogglePreview(true));
                }
            }
            AppMsg::ShowcaseComposeFrom(n) => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::ShowcaseFrom(n));
                }
            }
            AppMsg::ShowcaseForward { account_id, id } => {
                let found = self
                    .current_thread
                    .iter()
                    .find(|m| m.account_id == account_id && m.id == id)
                    .cloned()
                    .or_else(|| {
                        self.message_cache
                            .values()
                            .flatten()
                            .find(|m| m.account_id == account_id && m.id == id)
                            .cloned()
                    });
                match found {
                    Some(m) => self.forward(m, true, &sender),
                    None => tracing::warn!("showcase forward: {account_id}:{id} is not loaded"),
                }
            }
            AppMsg::ShowcaseComposeUndo => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::ShowcaseHistory(0));
                }
            }
            AppMsg::ShowcaseBurger => {
                if let Some(m) = self.sidebar_menu.as_ref() {
                    // Focus first: a click on a MenuButton focuses it before
                    // the popover opens, and popup() alone skips that — the
                    // difference between the two is a bug's hiding place.
                    m.grab_focus();
                    m.popup();
                    let mut chain = Vec::new();
                    let mut node = gtk::prelude::GtkWindowExt::focus(&self.window);
                    while let Some(w) = node {
                        chain.push(format!("{}", w.type_()));
                        node = w.parent();
                    }
                    tracing::info!(
                        target: "hylki::compose::undo",
                        "burger: opened, focus now [{}]",
                        chain.join(" < ")
                    );
                }
            }
            AppMsg::ShowcaseComposeFormat(format) => {
                if let Some(r) = self.reader_compose.as_ref() {
                    r.controller.emit(ComposeInput::SetFormat(format));
                }
            }

            AppMsg::ShowcaseComposeClose => {
                if let Some(r) = &self.reader_compose {
                    r.controller.emit(ComposeInput::Cancel);
                }
            }

            AppMsg::ShowcaseCopy => {
                let Some(current) = self.current.clone() else {
                    return;
                };
                let Some(p) = self.provisionals.values().next() else {
                    return;
                };
                let mut copy = p.message.clone();
                copy.uid = 9_999;
                copy.message_id = "showcase-copy@hylki.local".into();
                tracing::info!(target: "hylki::morph", "showcase: the copy of {} arrives", p.message.id);
                self.merge_related(current.account_id, current.id, vec![copy]);
                self.show_thread();
                // The window once the copy has painted, beside the morph's
                // own last frame, to hold the two against each other.
                if let Some(dir) = std::env::var_os("HYLKI_MORPH_FRAMES") {
                    let window: gtk::Widget = self.window.clone().upcast();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(1200),
                        move || {
                            crate::ui::send_morph::capture_next_frame(
                                &window.clone(),
                                move |still| {
                                    if let Some((tex, _)) = still {
                                        let _ = tex.save_to_png(
                                            std::path::Path::new(&dir).join("zz-copy.png"),
                                        );
                                    }
                                },
                            );
                        },
                    );
                }
            }

            AppMsg::ShowcaseSend(text) => {
                // Every frame interval for a second and a half, to compare
                // the send morph with the slide it replaces.
                if let Some(clock) = self.window.frame_clock() {
                    let last = std::rc::Rc::new(std::cell::Cell::new(0i64));
                    let id = clock.connect_after_paint(move |c| {
                        let now = c.frame_time();
                        if last.get() > 0 {
                            tracing::debug!(target: "hylki::frames", "frame {} ms", (now - last.get()) / 1000);
                        }
                        last.set(now);
                    });
                    let c2 = clock.clone();
                    let cell = std::rc::Rc::new(std::cell::Cell::new(Some(id)));
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(2500),
                        move || {
                            if let Some(id) = cell.take() {
                                c2.disconnect(id);
                            }
                        },
                    );
                }
                let Some(rc) = self.reader_compose.as_ref().map(|r| &r.controller) else {
                    return;
                };
                let editor = crate::ui::send_morph::find_widget(rc.widget().upcast_ref(), &|w| {
                    w.is::<webkit6::WebView>()
                })
                .and_downcast::<webkit6::WebView>();
                let Some(editor) = editor else { return };
                let js = format!(
                    "document.body.focus();{}",
                    text.split('\n')
                        .map(|l| format!(
                            "document.execCommand('insertText',false,{});",
                            serde_json::to_string(l).unwrap_or_default()
                        ))
                        .collect::<Vec<_>>()
                        .join("document.execCommand('insertParagraph');")
                );
                webkit6::prelude::WebViewExt::evaluate_javascript(
                    &editor,
                    &js,
                    None,
                    None,
                    None::<&gtk::gio::Cancellable>,
                    |_| {},
                );
                let c = rc.sender().clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(600),
                    move || {
                        c.emit(ComposeInput::Send);
                    },
                );
                // The whole window every quarter second for six seconds from
                // Send, to see what the reader does while the message goes.
                if let Some(dir) = std::env::var_os("HYLKI_MORPH_FRAMES") {
                    let window: gtk::Widget = self.window.clone().upcast();
                    for i in 0..24u64 {
                        let (dir, window) = (dir.clone(), window.clone());
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(600 + i * 250),
                            move || {
                                crate::ui::send_morph::capture_next_frame(
                                    &window.clone(),
                                    move |still| {
                                        if let Some((tex, _)) = still {
                                            let _ = tex.save_to_png(
                                                std::path::Path::new(&dir)
                                                    .join(format!("win-{:05}.png", i * 250)),
                                            );
                                        }
                                    },
                                );
                            },
                        );
                    }
                }
            }

            AppMsg::ShowcaseNewAccount(provider) => {
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::AddAccount);
                    if let Some(label) = provider {
                        acc.emit(crate::ui::accounts::AccountsInput::DebugProvider(label));
                    }
                }
            }
            AppMsg::ShowcaseDirtyEditor => {
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::DebugEditLabel(
                        "Edited in the capture".into(),
                    ));
                }
            }

            AppMsg::ShowcaseEditFilter(i) => {
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::EditFilter(i));
                }
            }
            _ => unreachable!("not routed to on_showcase"),
        }
    }
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
