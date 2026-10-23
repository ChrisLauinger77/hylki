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

    /// HYLKI_SHOWCASE_MEMORY=/path.txt writes the exported log (memory
    /// section included) there after HYLKI_SHOWCASE_MEMORY_AT seconds
    /// (default 20), on a real mailbox as much as the demo: a memory
    /// reading is only worth having once the index has loaded. With
    /// HYLKI_SHOWCASE_MEMORY_EVERY=N it is rewritten every N seconds
    /// after that, for watching a session grow.
    pub(super) fn install_showcase_probes(&mut self, sender: &ComponentSender<Self>) {
        if let Some(path) = std::env::var_os("HYLKI_SHOWCASE_MEMORY") {
            let secs = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u32>().ok());
            let at = secs("HYLKI_SHOWCASE_MEMORY_AT").unwrap_or(20);
            let every = secs("HYLKI_SHOWCASE_MEMORY_EVERY").filter(|n| *n > 0);
            let s = sender.clone();
            let path = std::path::PathBuf::from(path);
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                s.input(AppMsg::ExportLogTo(path.clone()));
                if let Some(every) = every {
                    gtk::glib::timeout_add_seconds_local(every, move || {
                        s.input(AppMsg::ExportLogTo(path.clone()));
                        gtk::glib::ControlFlow::Continue
                    });
                }
            });
        }
        // HYLKI_SHOWCASE_SELECT=<account>:<id> opens that message at 5 s
        // (HYLKI_SHOWCASE_SELECT_AT overrides the seconds) through its row,
        // or the head of its conversation, real accounts included: a way to
        // open a conversation the reader assembles from the cache without a
        // pointer, so its log can be read (#236).
        // HYLKI_SHOWCASE_ZOOM=N presses Ctrl+ N times (Ctrl- for a negative
        // N) at 8 s, so the live zoom path can be captured.
        if let Some(n) = std::env::var("HYLKI_SHOWCASE_ZOOM")
            .ok()
            .and_then(|v| v.parse::<i8>().ok())
        {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(8, move || {
                for _ in 0..n.unsigned_abs() {
                    s.input(AppMsg::ZoomMessage(n.signum()));
                }
            });
        }
        // HYLKI_SHOWCASE_DELETE_ATTACHMENT=<name>[:yes] chooses Delete from
        // Server for that file of the message on screen at 10 s
        // (HYLKI_SHOWCASE_DELETE_AT overrides the seconds); `:yes` answers
        // the question as well (#289).
        // `gallery:<name>` opens the attachments gallery 4 s before and
        // deletes the file from there.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_DELETE_ATTACHMENT") {
            let (name, confirmed) = match v.strip_suffix(":yes") {
                Some(name) => (name.to_string(), true),
                None => (v, false),
            };
            let at: u32 = std::env::var("HYLKI_SHOWCASE_DELETE_AT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10);
            if name.starts_with("gallery:") {
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(at.saturating_sub(4), move || {
                    s.input(AppMsg::ShowAttachments)
                });
            }
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                s.input(AppMsg::ShowcaseDeleteAttachment { name, confirmed });
            });
        }
        // Several may be given, comma-separated, each with its own
        // @<seconds>, to open one message and then another.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_SELECT") {
            let default_at: u32 = std::env::var("HYLKI_SHOWCASE_SELECT_AT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5);
            for item in v.split(',') {
                let (item, at) = showcase_at(item, default_at);
                let Some((a, id)) = item
                    .split_once(':')
                    .and_then(|(a, id)| Some((a.parse::<u32>().ok()?, id.parse::<u32>().ok()?)))
                else {
                    continue;
                };
                let list = self.message_list.sender().clone();
                gtk::glib::timeout_add_seconds_local_once(at, move || {
                    let _ = list.send(MessageListInput::SelectAndLoad((a, id)));
                });
            }
        }
        // HYLKI_SHOWCASE_TRANSLATE=<account>:<id>[@<seconds>] translates
        // that message (open by then) at 8 s, as its A文 button would, so a
        // translation can be captured (#327).
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_TRANSLATE") {
            let (item, at) = showcase_at(&v, 8);
            if let Some((a, id)) = item
                .split_once(':')
                .and_then(|(a, id)| Some((a.parse::<u32>().ok()?, id.parse::<u32>().ok()?)))
            {
                let view = self.message_view.sender().clone();
                gtk::glib::timeout_add_seconds_local_once(at, move || {
                    let _ = view.send(MessageViewInput::Translate { account_id: a, id });
                });
            }
        }
        // HYLKI_SHOWCASE_CARD_DELETE=<account>:<id>[@<seconds>] deletes that
        // card of the open conversation at 8 s, as its Delete would. Several
        // may be given, comma-separated.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_CARD_DELETE") {
            for item in v.split(',') {
                let (item, at) = showcase_at(item, 8);
                let Some((a, id)) = item
                    .split_once(':')
                    .and_then(|(a, id)| Some((a.parse::<u32>().ok()?, id.parse::<u32>().ok()?)))
                else {
                    continue;
                };
                let view = self.message_view.sender().clone();
                gtk::glib::timeout_add_seconds_local_once(at, move || {
                    let _ = view.send(MessageViewInput::CardAction {
                        action: crate::ui::message_list::RowAction::Delete,
                        account_id: a,
                        id,
                    });
                });
            }
        }
        // HYLKI_SHOWCASE_INBOX=<account>[@<seconds>] switches to that
        // account's Inbox at 3 s or the time given (the first account's when
        // it is not a number), real accounts included, for the same probe
        // from a folder view.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_INBOX") {
            let (v, at) = showcase_at(&v, 3);
            let account = v.parse::<u32>().ok().filter(|a| *a > 0);
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                s.input(AppMsg::ShowcaseFolder {
                    kind: FolderKind::Inbox,
                    account,
                });
            });
        }
        // HYLKI_SHOWCASE_LIST_SEARCH=<text>[:<scope>][@<seconds>] searches
        // the list at 4 s or the time given, the scope by its place in the
        // dropdown, real accounts included (#472).
        if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_LIST_SEARCH") {
            let (spec, at) = showcase_at(&spec, 4);
            let (text, scope) = match spec.rsplit_once(':') {
                Some((t, s)) => (t.to_string(), s.parse().unwrap_or(0)),
                None => (spec.to_string(), 0),
            };
            let list = self.message_list.sender().clone();
            gtk::glib::timeout_add_seconds_local_once(at, move || {
                let _ = list.send(MessageListInput::ShowcaseSearch(text, scope));
            });
        }
        // HYLKI_SHOWCASE_NOTIFY[=<seconds>[,<seconds>...]] posts the new-mail
        // notification at 5 s or at each time given (the same second twice
        // exercises the sound's hold-off), real accounts included.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_NOTIFY") {
            let mut times: Vec<u32> = v.split(',').filter_map(|t| t.trim().parse().ok()).collect();
            if times.is_empty() {
                times.push(5);
            }
            for at in times {
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(at, move || {
                    s.input(AppMsg::ShowcaseNotify)
                });
            }
        }
    }

    /// Screenshot showcase (HYLKI_DEMO + HYLKI_SHOWCASE=/path.png): stage
    /// the demo the way the marketing shots want it — first row (the demo
    /// conversation, expanded via the threads_expanded preference) selected,
    /// one mid-thread card highlighted — then render the window to a PNG.
    /// Timers leave room for the WebViews to load and settle between steps.
    pub(super) fn stage_showcase(
        &mut self,
        root: &adw::ApplicationWindow,
        widgets: &AppModelWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if demo_mode() {
            if let Some(shot) = std::env::var("HYLKI_SHOWCASE").ok() {
                // HYLKI_SHOWCASE_STAGE=0 skips the staging (capture-only, for
                // testing arbitrary states); HYLKI_SHOWCASE_DELAY overrides the
                // capture time (seconds, default 9).
                let stage = std::env::var("HYLKI_SHOWCASE_STAGE").as_deref() != Ok("0");
                let delay: u32 = std::env::var("HYLKI_SHOWCASE_DELAY")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(9);
                // HYLKI_SHOWCASE_HEIGHTS=1 logs the allocated height of the
                // sidebar's section headings and their rows just before the
                // capture, for checking pill geometry without a pointer.
                if std::env::var("HYLKI_SHOWCASE_HEIGHTS").is_ok() {
                    let sidebar = self.sidebar.widget().clone().upcast::<gtk::Widget>();
                    gtk::glib::timeout_add_seconds_local_once(
                        delay.saturating_sub(1).max(1),
                        move || {
                            fn walk(w: &gtk::Widget, depth: usize, root: &gtk::Widget) {
                                let classes = w.css_classes();
                                let heading = classes.iter().any(|c| c == "unified-folders-toggle");
                                let child_row = w.is::<gtk::ListBoxRow>()
                                    && w.parent().is_some_and(|p| {
                                        p.css_classes().iter().any(|c| c == "section-child-list")
                                    });
                                if heading || child_row {
                                    let y =
                                        w.compute_bounds(root).map(|b| b.y() as i32).unwrap_or(-1);
                                    tracing::info!(
                                    "showcase heights: {} y={y} h={} margins={}/{} classes={:?}",
                                    if heading { "heading" } else { "row" },
                                    w.height(),
                                    w.margin_top(),
                                    w.margin_bottom(),
                                    classes,
                                );
                                }
                                let mut c = w.first_child();
                                while let Some(ch) = c {
                                    walk(&ch, depth + 1, root);
                                    c = ch.next_sibling();
                                }
                            }
                            walk(&sidebar, 0, &sidebar);
                        },
                    );
                }
                if stage {
                    let list = self.message_list.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = list.send(MessageListInput::MoveSelection(1));
                    });
                    // HYLKI_SHOWCASE_PALETTE=N opens row N's actions palette
                    // (so a capture can verify the floating palette's look).
                    if let Some(Ok(idx)) = std::env::var("HYLKI_SHOWCASE_PALETTE")
                        .ok()
                        .map(|v| v.parse())
                    {
                        let list = self.message_list.sender().clone();
                        let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
                        let win = root.clone();
                        {
                            // An occluded window's frame clock is suspended
                            // and animations cannot progress — raise it well
                            // ahead so the slide actually runs for the burst.
                            let win = win.clone();
                            gtk::glib::timeout_add_seconds_local_once(5, move || {
                                win.present();
                            });
                        }
                        gtk::glib::timeout_add_seconds_local_once(7, move || {
                            let _ = list.send(MessageListInput::DebugOpenPalette(idx));
                            // TEMP: frame burst right after the open, to see
                            // the slide animation (or its absence) in stills.
                            if let Some(base) = burst.clone() {
                                for (i, ms) in [40u64, 90, 140, 240].iter().enumerate() {
                                    let win = win.clone();
                                    let path = format!("{base}.{i}.png");
                                    gtk::glib::timeout_add_local_once(
                                        std::time::Duration::from_millis(*ms),
                                        move || {
                                            showcase_capture(
                                                win.upcast_ref::<gtk::Widget>(),
                                                &path,
                                            );
                                        },
                                    );
                                }
                            }
                        });
                    }
                    let view = self.message_view.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = view.send(crate::ui::message_view::MessageViewInput::CardClicked {
                            account_id: 1,
                            id: 2,
                            mode: crate::ui::message_view::SelectMode::Plain,
                        });
                    });
                }
                // HYLKI_SHOWCASE_SWIPE=N[:left|right] swipes row N fully and
                // releases it at 7s, with HYLKI_SHOWCASE_BURST=<path> writing
                // stills through the commit exit — the only way to see the
                // gesture's animation here (no input injection).
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_SWIPE") {
                    let (idx, side) = spec.split_once(':').unwrap_or((spec.as_str(), "left"));
                    let index: usize = idx.trim().parse().unwrap_or(0);
                    let left = side != "right";
                    let list = self.message_list.sender().clone();
                    let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
                    let win = root.clone();
                    {
                        // An occluded window's frame clock is suspended, so
                        // raise it well before the animation runs.
                        let win = win.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || win.present());
                    }
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        let _ = list.send(MessageListInput::DebugSwipe { index, left });
                        if let Some(base) = burst.clone() {
                            for (i, ms) in [40u64, 120, 200, 300, 500].iter().enumerate() {
                                let win = win.clone();
                                let path = format!("{base}.{i}.png");
                                gtk::glib::timeout_add_local_once(
                                    std::time::Duration::from_millis(*ms),
                                    move || {
                                        showcase_capture(win.upcast_ref::<gtk::Widget>(), &path);
                                    },
                                );
                            }
                        }
                    });
                }
                // HYLKI_SHOWCASE_ROW=N selects the list's row N at 4s (with
                // HYLKI_SHOWCASE_STAGE=0), for capturing or probing one message.
                if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_ROW")
                    .ok()
                    .map(|v| v.parse::<u32>())
                {
                    let list = self.message_list.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        for _ in 0..=n {
                            let _ = list.send(MessageListInput::MoveSelection(1));
                        }
                    });
                }
                // HYLKI_SHOWCASE_UNSUB=ask|go presses the open card's
                // Unsubscribe button at 7s (after HYLKI_SHOWCASE_ROW opened
                // it): `ask` stops at the confirmation, `go` skips it.
                if std::env::var("HYLKI_SHOWCASE_UNSUB").is_ok() {
                    let view = self.message_view.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        let _ = view.send(MessageViewInput::Unsubscribe {
                            account_id: 1,
                            id: 10,
                        });
                    });
                }
                // HYLKI_SHOWCASE_INVITE=accept|maybe|decline|calendar presses
                // that button on the open card's invitation banner at 7s
                // (after HYLKI_SHOWCASE_ROW=8 opened the meeting request).
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_INVITE") {
                    use crate::models::Rsvp;
                    use crate::ui::message_view::InviteAction;
                    let action = match which.as_str() {
                        "maybe" => InviteAction::Answer(Rsvp::Tentative),
                        "decline" => InviteAction::Answer(Rsvp::Declined),
                        "calendar" => InviteAction::AddToCalendar,
                        _ => InviteAction::Answer(Rsvp::Accepted),
                    };
                    let view = self.message_view.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        let _ = view.send(MessageViewInput::InviteAction {
                            account_id: 1,
                            id: 13,
                            action,
                        });
                    });
                }
                // HYLKI_SHOWCASE_UNIFIED=sent|starred|drafts|archive|junk|trash|smart opens that unified
                // row at 3s, All Inboxes at 6s and the row again at 9s, so the
                // timing logs show a cold and a warm open. starred@N picks the
                // Nth account's row under Starred instead.
                // HYLKI_SHOWCASE_UNIFIED_AT=<s> starts that at s seconds rather
                // than 3, once a real profile's accounts have loaded.
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_UNIFIED") {
                    let at: u32 = std::env::var("HYLKI_SHOWCASE_UNIFIED_AT")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(3);
                    let sub = which
                        .strip_prefix("starred@")
                        .and_then(|n| n.parse::<i32>().ok());
                    let pick = move || match which.as_str() {
                        _ if let Some(index) = sub => SidebarInput::KindSubRowSelected {
                            kind: FolderKind::Starred,
                            index,
                        },
                        "starred" => SidebarInput::UnifiedKindRowSelected(FolderKind::Starred),
                        "drafts" => SidebarInput::UnifiedKindRowSelected(FolderKind::Drafts),
                        "archive" => SidebarInput::UnifiedKindRowSelected(FolderKind::Archive),
                        "junk" => SidebarInput::UnifiedKindRowSelected(FolderKind::Junk),
                        "trash" => SidebarInput::UnifiedKindRowSelected(FolderKind::Trash),
                        "filtered" => SidebarInput::UnifiedFilteredSelected,
                        "favorites" => SidebarInput::UnifiedFavoritesSelected,
                        "tags" => SidebarInput::UnifiedTagsSelected,
                        "smart" => SidebarInput::SmartRowSelected(0),
                        _ => SidebarInput::UnifiedKindRowSelected(FolderKind::Sent),
                    };
                    let sb = self.sidebar.sender().clone();
                    let p = pick.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || {
                        let _ = sb.send(p());
                    });
                    let sb = self.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(at + 3, move || {
                        let _ = sb.send(SidebarInput::SelectUnifiedRow);
                    });
                    let sb = self.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(at + 6, move || {
                        let _ = sb.send(pick());
                    });
                }
                // HYLKI_SHOWCASE_ROW_MENU=1 opens the first row's context
                // menu at 5s (pair with HYLKI_SHOWCASE_MENU to capture it).
                if std::env::var("HYLKI_SHOWCASE_ROW_MENU").is_ok() {
                    let ml = self.message_list.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = ml.send(MessageListInput::DebugRowMenu);
                    });
                }
                // HYLKI_SHOWCASE_APPLY_FILTERS=inboxes|<account>:<folder>
                // starts a manual filter run at 5s (#198), the way Apply Now
                // and the folder menu's Apply Filters do, and the run's
                // report lands in the status bar a beat later.
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_APPLY_FILTERS") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let targets = spec
                            .split_once(':')
                            .and_then(|(a, f)| Some(vec![(a.parse().ok()?, f.parse().ok()?)]))
                            .unwrap_or_default();
                        s.input(AppMsg::ApplyFilters(targets));
                    });
                }
                // HYLKI_SHOWCASE_TOOLBAR_GAP=<zone>:<index> opens a drop gap
                // in the Settings toolbar editor at 6s (pair with
                // HYLKI_SHOWCASE_SETTINGS=appearance), as a hovering drag
                // would.
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_TOOLBAR_GAP") {
                    if let Some((z, i)) = spec.split_once(':') {
                        if let (Ok(zone), Ok(index)) = (z.parse::<usize>(), i.parse::<usize>()) {
                            let s = sender.input_sender().clone();
                            gtk::glib::timeout_add_seconds_local_once(6, move || {
                                let _ = s.send(AppMsg::ShowcaseToolbarGap { zone, index });
                            });
                        }
                    }
                }
                // HYLKI_SHOWCASE_TOOLBAR_MENU=1 opens the header's right-click
                // menu (Customize Toolbar…) at 5s, as a click on its empty
                // middle would.
                if std::env::var("HYLKI_SHOWCASE_TOOLBAR_MENU").is_ok() {
                    let s = sender.input_sender().clone();
                    let header: gtk::Widget = widgets.reader_header.clone().upcast();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = s.send(AppMsg::ReaderToolbarMenu {
                            x: header.width() as f64 / 2.0,
                            y: header.height() as f64 / 2.0,
                        });
                    });
                }
                // HYLKI_SHOWCASE_READER_MENU=1 opens the reader header's ⋯
                // overflow menu at 5s (pair with HYLKI_SHOWCASE_MENU to
                // capture it; the window must be narrow enough to collapse).
                if std::env::var("HYLKI_SHOWCASE_READER_MENU").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = s.send(AppMsg::ReaderOverflowMenu);
                    });
                }
                // HYLKI_SHOWCASE_PRINT=1 opens the print preview of the open
                // message at 6s (#359); HYLKI_SHOWCASE_TOP captures it.
                if std::env::var("HYLKI_SHOWCASE_PRINT").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = s.send(AppMsg::PrintPreview);
                    });
                }
                // HYLKI_SHOWCASE_POPOUT=1 opens the open message in a window
                // of its own at 6s; HYLKI_SHOWCASE_TOP captures it.
                if std::env::var("HYLKI_SHOWCASE_POPOUT").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = s.send(AppMsg::ShowcasePopout);
                    });
                }
                // HYLKI_SHOWCASE_EDIT_AS_NEW=1 copies the open message into
                // the composer at 6s (#232), as its menu entry does.
                if std::env::var("HYLKI_SHOWCASE_EDIT_AS_NEW").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = s.send(AppMsg::EditAsNewCurrent);
                    });
                }
                // HYLKI_SHOWCASE_MOVE=1 opens the Move To… picker at 5s
                // (it captures itself a second later).
                if std::env::var("HYLKI_SHOWCASE_MOVE").is_ok() {
                    let s = sender.input_sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = s.send(AppMsg::MoveToMenu);
                    });
                }
                // HYLKI_SHOWCASE_GALLERY=1 opens the attachments gallery at
                // 3s; add HYLKI_SHOWCASE_GALLERY_FOLDERS=1 to drop its folder
                // popover open a second later, or HYLKI_SHOWCASE_GALLERY_ACCOUNT=<row>
                // to pick that row of its account filter (0 = all accounts).
                if std::env::var("HYLKI_SHOWCASE_GALLERY").is_ok() {
                    let s = sender.input_sender().clone();
                    let g = self.gallery.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = s.send(AppMsg::ShowAttachments);
                        let row: Option<u32> = std::env::var("HYLKI_SHOWCASE_GALLERY_ACCOUNT")
                            .ok()
                            .and_then(|v| v.parse().ok());
                        let folders = std::env::var("HYLKI_SHOWCASE_GALLERY_FOLDERS").is_ok();
                        // HYLKI_SHOWCASE_GALLERY_SEARCH=<text> types into the
                        // search box and reports whether it keeps the focus.
                        let search = std::env::var("HYLKI_SHOWCASE_GALLERY_SEARCH").ok();
                        // HYLKI_SHOWCASE_GALLERY_MORE=<n> pages down n times,
                        // as scrolling to the end would.
                        let more: u32 = std::env::var("HYLKI_SHOWCASE_GALLERY_MORE")
                            .ok()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                        gtk::glib::timeout_add_seconds_local_once(1, move || {
                            if let Some(row) = row {
                                let _ = g.send(GalleryInput::SetAccountFilter(row));
                            }
                            for i in 0..more {
                                let g = g.clone();
                                gtk::glib::timeout_add_seconds_local_once(1 + i, move || {
                                    let _ = g.send(GalleryInput::LoadMore);
                                });
                            }
                            if let Some(text) = search {
                                let _ = g.send(GalleryInput::ShowcaseSearch(text));
                            }
                            if folders {
                                let _ = g.send(GalleryInput::ShowcaseFolders);
                            }
                        });
                    });
                }
                // HYLKI_SHOWCASE_RAIL=1 collapses the sidebar to the rail
                // at 3s (the user's own toggle, fold-ups and all).
                if std::env::var("HYLKI_SHOWCASE_RAIL").is_ok() {
                    let sb = self.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = sb.send(SidebarInput::ToggleCollapsed);
                    });
                }
                // HYLKI_SHOWCASE_PEEK=<seconds>[,<seconds>…] works the header's
                // sidebar toggle at each of those moments (1 or anything
                // unparseable means 8 s), with the window raised first so the
                // slide really animates; HYLKI_SHOWCASE_BURST writes stills
                // through the first one. In a narrow window (a window.toml
                // under the 1094px breakpoint) that is the floating peek; at
                // full width, or under Focus Mode's rail, it is the rail
                // expanding and folding again.
                if let Ok(spec) = std::env::var("HYLKI_SHOWCASE_PEEK") {
                    let mut at: Vec<f64> = spec
                        .split(',')
                        .filter_map(|p| p.trim().parse::<f64>().ok())
                        .filter(|s| *s > 1.5)
                        .collect();
                    if at.is_empty() {
                        at.push(8.0);
                    }
                    let burst = std::env::var("HYLKI_SHOWCASE_BURST").ok();
                    let win = root.clone();
                    {
                        let win = win.clone();
                        let first = at[0];
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(((first - 1.5) * 1000.0) as u64),
                            move || win.present(),
                        );
                    }
                    for (n, secs) in at.into_iter().enumerate() {
                        let s = sender.clone();
                        let win = win.clone();
                        let burst = (n == 0).then(|| burst.clone()).flatten();
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis((secs * 1000.0) as u64),
                            move || {
                                s.input(AppMsg::ToggleSidebar);
                                if let Some(base) = burst {
                                    for (i, ms) in [60u64, 120, 200, 400].iter().enumerate() {
                                        let win = win.clone();
                                        let path = format!("{base}.{i}.png");
                                        gtk::glib::timeout_add_local_once(
                                            std::time::Duration::from_millis(*ms),
                                            move || {
                                                showcase_capture(
                                                    win.upcast_ref::<gtk::Widget>(),
                                                    &path,
                                                );
                                            },
                                        );
                                    }
                                }
                            },
                        );
                    }
                }
                // HYLKI_SHOWCASE_TOGGLE=starred|sent|drafts toggles that
                // unified row's list at 5s (what a long-press does).
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_TOGGLE") {
                    let kind = match which.as_str() {
                        "sent" => FolderKind::Sent,
                        "drafts" => FolderKind::Drafts,
                        "archive" => FolderKind::Archive,
                        _ => FolderKind::Starred,
                    };
                    let sb = self.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let _ = sb.send(SidebarInput::ToggleKindExpand(kind));
                    });
                }
                // HYLKI_SHOWCASE_FOLD_FILTERED folds All Inboxes' Filtered
                // Folders section, to check its folded header.
                if std::env::var("HYLKI_SHOWCASE_FOLD_FILTERED").is_ok() {
                    let sb = self.sidebar.sender().clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        let _ = sb.send(SidebarInput::ToggleFilteredExpand(
                            crate::ui::sidebar::Slot::Unified,
                        ));
                    });
                }
                // HYLKI_SHOWCASE_REPLY opens the inline reply composer on the
                // selected message, to check the composer's grounds (#148).
                // HYLKI_SHOWCASE_FLIP=dark|light then switches the app theme
                // at 6 s, to check a live flip re-resolves those grounds.
                // HYLKI_SHOWCASE_FOLDER=drafts|templates|sent|archive|junk|trash|outbox[@<s>]
                // switches to that folder at 2 s, before the staging's 3 s
                // selection moves onto its first row, or at the time given.
                // HYLKI_SHOWCASE_FOCUS=<seconds> switches Focus Mode on at
                // that moment (its parts as saved in focus.toml), so a
                // capture a little later catches the slide, and one later
                // still the settled layout. HYLKI_SHOWCASE_FOCUS_OFF=<seconds>
                // switches it off again.
                for (var, on) in [
                    ("HYLKI_SHOWCASE_FOCUS", true),
                    ("HYLKI_SHOWCASE_FOCUS_OFF", false),
                ] {
                    if let Some(at) = std::env::var(var).ok().and_then(|v| v.parse::<f64>().ok()) {
                        let s = sender.clone();
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis((at * 1000.0) as u64),
                            move || {
                                let mut focus = config::load_focus_mode();
                                focus.enabled = on;
                                s.input(AppMsg::Pref(PrefOutput::SetFocusMode(focus)));
                            },
                        );
                    }
                }
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_FOLDER") {
                    let (kind, at) = showcase_at(&v, 2);
                    if kind == "outbox" {
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(at, move || {
                            s.input(AppMsg::ShowOutbox)
                        });
                    }
                    let kind = match kind {
                        "drafts" => Some(FolderKind::Drafts),
                        "templates" => Some(FolderKind::Templates),
                        "sent" => Some(FolderKind::Sent),
                        "archive" => Some(FolderKind::Archive),
                        "junk" => Some(FolderKind::Junk),
                        "trash" => Some(FolderKind::Trash),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(at, move || {
                            s.input(AppMsg::ShowcaseFolder {
                                kind,
                                account: None,
                            });
                        });
                    }
                }
                // HYLKI_SHOWCASE_SEND=<seconds>[:<text>|:@<file>] writes in the
                // reply's editor and sends it at that moment, for the send
                // morph.
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_SEND") {
                    let (at, text) = v
                        .split_once(':')
                        .unwrap_or((v.as_str(), "Sounds good, ship it.\nJason"));
                    let at = at.parse::<f64>().unwrap_or(9.0);
                    // `@<path>` reads the text from a file.
                    let text = match text.strip_prefix('@') {
                        Some(path) => std::fs::read_to_string(path).unwrap_or_default(),
                        None => text.replace("\\n", "\n"),
                    };
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis((at * 1000.0) as u64),
                        move || {
                            s.input(AppMsg::ShowcaseSend(text));
                        },
                    );
                }
                // HYLKI_SHOWCASE_UNDO=<seconds> presses Undo on the first
                // message sent, to check it comes back where it was (#434).
                if let Some(at) = std::env::var("HYLKI_SHOWCASE_UNDO")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis((at * 1000.0) as u64),
                        move || {
                            s.input(AppMsg::UndoSend(1));
                        },
                    );
                }
                // HYLKI_SHOWCASE_COPY=<seconds> has the sent reply's copy
                // read back then, so its stand-in card can be seen to give
                // way to it.
                if let Some(at) = std::env::var("HYLKI_SHOWCASE_COPY")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis((at * 1000.0) as u64),
                        move || {
                            s.input(AppMsg::ShowcaseCopy);
                        },
                    );
                }
                // Two seconds on, it logs where focus landed (#266).
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_REPLY") {
                    // A number is the moment, in seconds (4 otherwise).
                    let at = v.parse::<u32>().unwrap_or(4);
                    let s = sender.clone();
                    let window = root.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || {
                        s.input(AppMsg::Reply);
                        // After the split's slide, which focus waits for.
                        gtk::glib::timeout_add_seconds_local_once(2, move || {
                            showcase_log_focus(&window)
                        });
                    });
                }
                // HYLKI_SHOWCASE_NEW=<seconds>[:<address>] opens a new message
                // at that moment (4 s unless it parses), addressed when an
                // address follows, and, a second later, logs
                // where keyboard focus landed: the field row's title, or the
                // focused widget's type (#266).
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_NEW") {
                    let (at, to) = v.split_once(':').unwrap_or((v.as_str(), ""));
                    let at = at.parse::<u32>().unwrap_or(4);
                    let to = to.to_string();
                    let s = sender.clone();
                    let window = root.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || {
                        s.input(if to.is_empty() {
                            AppMsg::Compose
                        } else {
                            AppMsg::ComposeTo(to)
                        });
                        gtk::glib::timeout_add_seconds_local_once(1, move || {
                            showcase_log_focus(&window)
                        });
                    });
                }
                // HYLKI_SHOWCASE_FORWARD=<seconds> does the same with
                // Forward at that moment (4 s unless it parses), to check
                // the original's attachments come along (#240). As
                // <seconds>:<account>:<id> it forwards that message of the
                // open conversation, the way its card's button would, so a
                // message that is not the conversation's newest can be
                // picked.
                if let Ok(v) = std::env::var("HYLKI_SHOWCASE_FORWARD") {
                    let mut parts = v.split(':');
                    let at = parts
                        .next()
                        .and_then(|p| p.parse::<u32>().ok())
                        .unwrap_or(4);
                    let target = parts.next().zip(parts.next()).and_then(|(a, id)| {
                        Some((a.parse::<u32>().ok()?, id.parse::<u32>().ok()?))
                    });
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || match target {
                        Some((account_id, id)) => {
                            s.input(AppMsg::ShowcaseForward { account_id, id })
                        }
                        None => s.input(AppMsg::Forward),
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_PREVIEW=1 turns the inline
                // composer's preview on a beat after it opens, so a
                // capture can show the rendered message rather than the
                // source it was written in.
                // HYLKI_SHOWCASE_COMPOSE_MENU=format opens the inline
                // composer's format chooser; any other value opens its
                // overflow menu. Pair either with HYLKI_SHOWCASE_MENU=main.
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_COMPOSE_MENU") {
                    let s = sender.clone();
                    let format = which == "format";
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::ShowcaseComposeMenu { format });
                    });
                }
                if std::env::var("HYLKI_SHOWCASE_COMPOSE_PREVIEW").is_ok() {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::ShowcaseComposePreview);
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_FROM=N picks the inline composer's
                // From entry N at 7 s, to check where the new account's
                // signature lands (#237).
                if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_COMPOSE_FROM")
                    .ok()
                    .map(|v| v.parse::<u32>())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        s.input(AppMsg::ShowcaseComposeFrom(n));
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_CLOSE=N cancels the inline composer
                // N seconds in (pair with HYLKI_SHOWCASE_REPLY), to check
                // that a closed composer's web view, and so its web process,
                // actually goes away (#221).
                if let Some(at) = std::env::var("HYLKI_SHOWCASE_COMPOSE_CLOSE")
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(at, move || {
                        s.input(AppMsg::ShowcaseComposeClose);
                    });
                }
                // HYLKI_SHOWCASE_COMPOSE_UNDO=1 runs the composer's history
                // (#200) through a scripted round of edits and undos on the
                // `hylki::compose::undo` log target: the one way to exercise
                // it without a keyboard, since the keys cannot be injected
                // on this desktop. Pair with HYLKI_SHOWCASE_REPLY.
                // Set it to markdown, html or plain to walk a source-mode
                // body (a textarea) instead of the rich document, to
                // `backspace` to trim text rather than type it, or to
                // `pause` to leave a long gap between two runs of typing.
                // HYLKI_SHOWCASE_COMPOSE_FORMAT=plain@6,rich@10 switches the
                // inline composer's format at those moments (#424). Pair
                // with HYLKI_SHOWCASE_REPLY.
                if let Ok(steps) = std::env::var("HYLKI_SHOWCASE_COMPOSE_FORMAT") {
                    for step in steps.split(',') {
                        let (name, at) = showcase_at(step, 6);
                        let format = match name {
                            "rich" => config::ComposeFormat::Rich,
                            "markdown" => config::ComposeFormat::Markdown,
                            "html" => config::ComposeFormat::Html,
                            "plain" => config::ComposeFormat::Plain,
                            _ => continue,
                        };
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(at, move || {
                            s.input(AppMsg::ShowcaseComposeFormat(format));
                        });
                    }
                }
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_COMPOSE_UNDO") {
                    let format = match which.as_str() {
                        "markdown" => Some(config::ComposeFormat::Markdown),
                        "html" => Some(config::ComposeFormat::Html),
                        "plain" => Some(config::ComposeFormat::Plain),
                        _ => None,
                    };
                    if let Some(format) = format {
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            s.input(AppMsg::ShowcaseComposeFormat(format));
                        });
                    }
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        s.input(AppMsg::ShowcaseComposeUndo);
                    });
                    // …and drop the burger menu open over it near the end,
                    // which is the state the entries are actually read in.
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(16, move || {
                        s.input(AppMsg::ShowcaseBurger);
                    });
                }
                // HYLKI_SHOWCASE_FILES=/a:/b hands those files in at 4 s,
                // as GNOME Files' "Send with Hylki" would; with
                // HYLKI_SHOWCASE_TOP=1 the capture takes the newest window
                // (the dialog asking about them) instead of the main one.
                if let Ok(list) = std::env::var("HYLKI_SHOWCASE_FILES") {
                    let s = sender.clone();
                    let paths: Vec<std::path::PathBuf> = list
                        .split(':')
                        .filter(|p| !p.is_empty())
                        .map(Into::into)
                        .collect();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        s.input(AppMsg::OpenWithFiles(paths));
                    });
                }
                if let Ok(flip) = std::env::var("HYLKI_SHOWCASE_FLIP") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::Pref(PrefOutput::SetAppTheme(if flip == "dark" {
                            config::AppTheme::Dark
                        } else {
                            config::AppTheme::Light
                        })));
                    });
                }
                // HYLKI_SHOWCASE_THEME=<id> picks that appearance theme at
                // 6 s, exactly as the Settings gallery does, so the live
                // repaint (chrome, reader and composer) can be captured.
                if let Ok(id) = std::env::var("HYLKI_SHOWCASE_THEME") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::Pref(PrefOutput::SetTheme(id.clone())));
                    });
                }
                // HYLKI_SHOWCASE_SHORTCUTS=<height> opens the shortcut list at
                // 4 s, that tall, so all of it fits a capture (with
                // HYLKI_SHOWCASE_TOP).
                if let Some(h) = std::env::var("HYLKI_SHOWCASE_SHORTCUTS")
                    .ok()
                    .and_then(|v| v.parse::<i32>().ok())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(4, move || {
                        s.input(AppMsg::ShowShortcuts);
                    });
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        let tops = gtk::Window::toplevels();
                        if let Some(w) = (0..tops.n_items())
                            .filter_map(|i| tops.item(i))
                            .filter_map(|o| o.downcast::<adw::Window>().ok())
                            .find(|w| {
                                w.is_visible()
                                    && w.title().is_some_and(|t| t == i18n("Keyboard Shortcuts"))
                            })
                        {
                            w.set_default_size(w.default_width(), h);
                        }
                    });
                }
                // HYLKI_SHOWCASE_ACCOUNT=N opens account N's editor a beat
                // after the Settings window (with HYLKI_SHOWCASE_SETTINGS),
                // so the editor itself can be captured.
                // `new[:<provider label>]` opens a blank editor instead.
                if let Some(rest) = std::env::var("HYLKI_SHOWCASE_ACCOUNT")
                    .ok()
                    .and_then(|v| v.strip_prefix("new").map(str::to_string))
                {
                    let provider = rest.strip_prefix(':').map(str::to_string);
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        s.input(AppMsg::ShowcaseNewAccount(provider.clone()));
                    });
                }
                if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_ACCOUNT")
                    .ok()
                    .map(|v| v.parse::<u32>())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(5, move || {
                        s.input(AppMsg::SidebarContext(CtxAction::OpenAccountSettings(
                            n + 1,
                        )));
                    });
                }
                // HYLKI_SHOWCASE_EDITOR_DIRTY=1 types into the open account
                // editor's Label field at 7s, so leaving an edited editor can
                // be exercised without a keyboard.
                if std::env::var("HYLKI_SHOWCASE_EDITOR_DIRTY").is_ok() {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(7, move || {
                        s.input(AppMsg::ShowcaseDirtyEditor);
                    });
                }
                // HYLKI_SHOWCASE_SETTINGS_GO=<category> picks that sidebar
                // category at 8s, exactly as a click on its row does.
                if let Ok(page) = std::env::var("HYLKI_SHOWCASE_SETTINGS_GO") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(8, move || {
                        s.input(AppMsg::ShowSettingsPage(page.clone()));
                    });
                }
                // HYLKI_SHOWCASE_ADD_CONTACT=<address> opens Add to Contacts
                // for it at 6s and captures the dialog at 10s, beside the
                // HYLKI_SHOWCASE file, so the books it offers can be checked.
                if let Ok(addr) = std::env::var("HYLKI_SHOWCASE_ADD_CONTACT") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        s.input(AppMsg::AddContactAddr(addr));
                    });
                    gtk::glib::timeout_add_seconds_local_once(10, move || {
                        let tops = gtk::Window::toplevels();
                        let dialog = (0..tops.n_items())
                            .filter_map(|i| tops.item(i))
                            .filter_map(|o| o.downcast::<adw::MessageDialog>().ok())
                            .find(|d| d.is_visible());
                        match (dialog, std::env::var("HYLKI_SHOWCASE")) {
                            (Some(d), Ok(path)) => {
                                showcase_capture(d.upcast_ref(), &format!("{path}.contact.png"))
                            }
                            _ => tracing::warn!("showcase: no Add to Contacts dialog"),
                        }
                    });
                }
                // HYLKI_SHOWCASE_DIALOG=save|discard|cancel answers whatever
                // message dialog is on screen at 10s, captured first beside
                // the HYLKI_SHOWCASE file.
                if let Ok(answer) = std::env::var("HYLKI_SHOWCASE_DIALOG") {
                    gtk::glib::timeout_add_seconds_local_once(10, move || {
                        let tops = gtk::Window::toplevels();
                        let dialog = (0..tops.n_items())
                            .filter_map(|i| tops.item(i))
                            .filter_map(|o| o.downcast::<adw::MessageDialog>().ok())
                            .find(|d| d.is_visible());
                        match dialog {
                            Some(d) => {
                                if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
                                    showcase_capture(d.upcast_ref(), &format!("{path}.dialog.png"));
                                }
                                // Answering is what a button click emits; the
                                // click also takes the dialog off screen, so
                                // hide it (closing would answer a second time).
                                d.response(&answer);
                                d.set_visible(false);
                            }
                            None => tracing::warn!("showcase: no dialog to answer"),
                        }
                    });
                }
                // HYLKI_SHOWCASE_CONTACTS=list|new|group|search:<text>[@<n>]|filter:<n>|multi:<n>
                // opens Contacts at 3s, and with "new" its editor for a new
                // contact at 5s ("group": for a new group), or with "search:"
                // types the text into its search
                // then, so the directories are asked too (#307), and with
                // "@<n>" shows only book n of the filter at 6s; "filter:"
                // turns on its nth "Show only" filter at 5s; "multi:"
                // selects the first n entries at 5s.
                if let Ok(which) = std::env::var("HYLKI_SHOWCASE_CONTACTS") {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        s.input(AppMsg::OpenContacts)
                    });
                    let page = self.contacts_page.sender().clone();
                    if which == "new" || which == "group" {
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            let _ = page.send(if which == "group" {
                                ContactsPageInput::NewGroup
                            } else {
                                ContactsPageInput::NewContact
                            });
                        });
                    } else if let Some(filter) = which
                        .strip_prefix("filter:")
                        .and_then(|n| n.parse::<usize>().ok())
                        .and_then(|n| crate::ui::contacts_page::ContactFilter::ALL.get(n).copied())
                    {
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            let _ = page.send(ContactsPageInput::SetFilter(filter, true));
                        });
                    } else if let Some(n) = which
                        .strip_prefix("multi:")
                        .and_then(|n| n.parse::<u32>().ok())
                    {
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            let _ = page.send(ContactsPageInput::SelectFirst(n));
                        });
                    } else if let Some(spec) = which.strip_prefix("search:") {
                        let (text, book) = match spec.rsplit_once('@') {
                            Some((t, n)) if n.parse::<u32>().is_ok() => {
                                (t.to_string(), n.parse::<u32>().ok())
                            }
                            _ => (spec.to_string(), None),
                        };
                        let p = page.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            let _ = p.send(ContactsPageInput::SetSearch(text));
                        });
                        if let Some(n) = book {
                            gtk::glib::timeout_add_seconds_local_once(6, move || {
                                let _ = page.send(ContactsPageInput::PickBook(n));
                            });
                        }
                    }
                }
                // HYLKI_SHOWCASE_SETTINGS=accounts|prefs opens the Settings
                // window on that panel (about: the About window) and
                // captures it instead of the main window, so its pages can
                // be checked in stills too.
                let settings = std::env::var("HYLKI_SHOWCASE_SETTINGS").ok();
                if let Some(panel) = settings.clone() {
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
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(5, move || {
                            s.input(AppMsg::DebugCloseSettings);
                        });
                        let s = sender.clone();
                        gtk::glib::timeout_add_seconds_local_once(7, move || {
                            s.input(AppMsg::OpenAccounts);
                        });
                    }
                }
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
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(delay_ms),
                    move || {
                        let target = settings
                            .as_ref()
                            .map(|_| ())
                            .or(top.then_some(()))
                            .and_then(|_| settings_window())
                            .map(|w| w.upcast::<gtk::Widget>())
                            .unwrap_or_else(|| win.clone().upcast::<gtk::Widget>());
                        showcase_capture(&target, &shot);
                    },
                );
            }
        }
    }

    /// HYLKI_SHOWCASE_ABOUT=1 opens the About window a beat after
    /// launch; pair it with HYLKI_SHOWCASE_TOP=1 to capture that window
    /// instead of the main one.
    pub(super) fn install_showcase_overlays(&mut self, sender: &ComponentSender<Self>) {
        if std::env::var("HYLKI_SHOWCASE_ABOUT").is_ok() {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                s.input(AppMsg::OpenAbout);
            });
        }
        // Screenshot/dev hook: open the status bar console shortly after
        // launch (pref permitting) so captures can show it.
        if std::env::var("HYLKI_SHOWCASE_CONSOLE").is_ok() {
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || {
                s.input(AppMsg::OpenConsole);
            });
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
