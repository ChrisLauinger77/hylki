//! A small reusable WYSIWYG rich-text editor: a `contentEditable` WebView with a
//! formatting toolbar. JavaScript runs only in this (our own) document, to drive
//! editing commands. Used for the compose body and the account signature.

use super::tool_flow::ToolFlow;
use crate::i18n::i18n;
use adw::prelude::*;
use webkit6::prelude::WebViewExt;

mod scripts;

mod colors;
mod documents;
mod files;
mod fonts;
mod signature;
mod spelling;
mod toolbar;

pub use documents::remote_image_urls;
use documents::{document, image_policy_meta, preview_document, reground, source_document};
pub use files::is_inline_image;
use files::{attach_all, deliver_files, detach_ctx_image, paste_into};
pub(crate) use fonts::{choose_family, compose_font_choices, preview_font, recent_font_css, SIZES};
pub use signature::{signature_from_source, signature_to_html};
use spelling::append_spelling_menu;
pub use spelling::{
    apply_spellcheck, installed_dictionaries, resolved_spell_languages, spell_language_names,
    system_spell_languages,
};
use toolbar::build_toolbar;

/// A rich-text editor widget. Add `widget` to a container; read the content back
/// with [`RichEditor::extract_html`] (asynchronous, since it queries the WebView).
/// Clones share the same underlying widgets (GObject references).
#[derive(Clone)]
pub struct RichEditor {
    /// The toolbar + editor, ready to be placed in a container.
    pub widget: gtk::Box,
    webview: webkit6::WebView,
    /// The formatting commands, hidden while a message is composed as plain
    /// text (#180) or as source — the row they sit in stays either way.
    toolbar_commands: ToolFlow,
    /// The far end of that row, where a host puts controls of its own (the
    /// composer's format chooser and preview toggle).
    toolbar_end: gtk::Box,
    /// Swaps the editor for the rendered preview of a source message.
    stack: gtk::Stack,
    /// The preview view, built the first time a preview is asked for: a
    /// second WebView per composer is not something to open speculatively
    /// (issue #106).
    preview: std::rc::Rc<std::cell::RefCell<Option<webkit6::WebView>>>,
    /// Which source language the editor currently holds, if it is in source
    /// mode rather than rich mode.
    source: std::rc::Rc<std::cell::Cell<Option<SourceKind>>>,
    /// Whether the rich document is showing the message as plain text
    /// (#424): flattened to what is sent, in the monospace font, taking
    /// only text. Read by every load of the document and by whatever puts
    /// files or the clipboard into it.
    plain: std::rc::Rc<std::cell::Cell<bool>>,
    /// Where "Send as Attachment Instead" delivers the lifted image, as a
    /// temp-file path the host adds to its attachment list. Set by the host
    /// via [`RichEditor::connect_send_as_attachment`].
    attach_cb: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(std::path::PathBuf)>>>>,
    /// Whether the document is there to take files, and the ones waiting
    /// for it ([`RichEditor::insert_files`]).
    loaded: std::rc::Rc<std::cell::Cell<bool>>,
    pending_files: std::rc::Rc<std::cell::RefCell<Vec<std::path::PathBuf>>>,
    /// What the document's own text history can do right now, mirrored from
    /// WebKit's editor state so a host can read it without a round trip
    /// (#200 in the composer). `(undo, redo)`.
    text_history: std::rc::Rc<std::cell::Cell<(bool, bool)>>,
    /// Told whenever that pair changes, so a host can fold the body's text
    /// history into an undo stack of its own.
    history_cb: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(bool, bool)>>>>,
    /// Where the right-click menu's Spelling Language choice goes, as the
    /// whole new setting (#365). The submenu is only offered once a host
    /// has connected it ([`RichEditor::connect_spell_languages`]).
    spell_langs_cb: SpellLangsCallback,
    /// The style manager's dark-notify handler that re-grounds the document
    /// on a live theme flip; disconnected when the last clone of the editor
    /// goes (the editor is a cloneable handle, so the guard is shared).
    _theme_handler: std::rc::Rc<ThemeHandlerGuard>,
    /// The Content-Security-Policy `<meta>` every document this editor loads
    /// carries, or empty when any picture may load. See [`RichEditor::new`].
    image_policy: std::rc::Rc<str>,
    /// The page zoom set by [`RichEditor::set_zoom`], kept for a preview
    /// built after it.
    zoom: std::rc::Rc<std::cell::Cell<f64>>,
}

/// What a source-mode editor is holding. Rich text is the absence of
/// both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Markdown,
    Html,
}

type SpellLangsCallback = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String)>>>>;

/// Disconnects a style-manager handler on drop.
struct ThemeHandlerGuard(Option<gtk::glib::SignalHandlerId>);

impl Drop for ThemeHandlerGuard {
    fn drop(&mut self) {
        if let Some(id) = self.0.take() {
            adw::StyleManager::default().disconnect(id);
        }
    }
}

impl RichEditor {
    /// `remote_images` is `None` when the editor may load any picture. A
    /// quoted original whose remote content the reader has not loaded gives
    /// the list the editor is limited to instead (the user's own signature
    /// pictures): opening a reply must not fetch what reading the message
    /// did not (#295). The markup keeps every address, so the recipient
    /// gets the quote whole.
    pub fn new(initial_html: &str, remote_images: Option<&[String]>, plain: bool) -> Self {
        let plain = std::rc::Rc::new(std::cell::Cell::new(plain));
        let image_policy: std::rc::Rc<str> = image_policy_meta(remote_images).into();
        // The shared document-viewer context (issue #106): a default-context
        // view would bring up a second web process with browser-sized caches.
        // Spell checking rides the same context; refreshing it here keeps a
        // just-changed preference honored without a restart.
        apply_spellcheck();
        // The message handler carries the word under the caret out for a
        // mid-word spelling verdict (see the paste script's caretWord).
        let ucm = webkit6::UserContentManager::new();
        ucm.register_script_message_handler("hylkiSpell", None);
        // The block state under the caret (quote, bulleted or numbered list)
        // comes out through this one, to light the matching toolbar button.
        ucm.register_script_message_handler("hylkiFormat", None);
        let webview = webkit6::WebView::builder()
            .web_context(&super::message_view::shared_web_context())
            .user_content_manager(&ucm)
            .build();
        crate::memory_report::register_web_view(&webview, "editor");
        // Everything the view itself owns (its content manager's handlers,
        // its event controllers, its toolbar's buttons via the format-state
        // handler) holds the view WEAKLY. A strong clone in any of those
        // closures is a reference cycle GObject never breaks: the editor of
        // a closed composer stayed alive that way, and with it its 300 MB
        // WebKitWebProcess, for the rest of the session (#221).
        {
            let weak = webview.downgrade();
            ucm.connect_script_message_received(Some("hylkiSpell"), move |_, value| {
                let Some(v) = weak.upgrade() else { return };
                let word = value.to_str().to_string();
                let bad = crate::spell::word_is_misspelled(&word);
                exec(&v, &format!("window.__hylkiSpellMark({bad})"));
            });
        }
        let settings = webkit6::Settings::new();
        settings.set_enable_javascript(true);
        settings.set_enable_developer_extras(false);
        // A script error in the editor document (all our own code) is
        // otherwise completely silent.
        settings.set_enable_write_console_messages_to_stdout(true);
        webview.set_settings(&settings);
        // Transparent until the document paints: a fresh WebView otherwise
        // renders an opaque default surface (black) for its first frames,
        // which flashes while the inline composer slides over the reader.
        // The document itself paints `Canvas` below, so the editor's normal
        // ground appears with the first real frame.
        webview.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
        // The document paints asynchronously: the host panel slides in
        // smoothly and the editor's content would land a beat later as a
        // pop. Start the view invisible and fade it in when its load
        // finishes — content arrives as a fade into the settled panel.
        webview.set_opacity(0.0);
        // Starting the editor's web process takes a moment, longest the
        // first time: past a glance, a spinner says it is coming (#392).
        let spinner = gtk::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        spinner.set_can_target(false);
        spinner.set_visible(false);
        let shown = std::rc::Rc::new(std::cell::Cell::new(false));
        {
            let (sp, shown) = (spinner.downgrade(), shown.clone());
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                if let Some(sp) = sp.upgrade().filter(|_| !shown.get()) {
                    sp.set_visible(true);
                    sp.start();
                }
            });
        }
        let sp = spinner.downgrade();
        webview.connect_load_changed(move |v, ev| {
            if ev == webkit6::LoadEvent::Finished {
                shown.set(true);
                if let Some(sp) = sp.upgrade() {
                    sp.stop();
                    sp.set_visible(false);
                }
                fade_in(v);
                // Focus given before the document was there reached the
                // view but no caret: place it now.
                let root_focus = v.root().and_then(|r| r.focus());
                if root_focus
                    .is_some_and(|f| f == *v.upcast_ref::<gtk::Widget>() || f.is_ancestor(v))
                {
                    exec(v, PLACE_CARET);
                }
            }
        });
        {
            // Failsafe: whatever happens to the load, the editor must never
            // stay invisible.
            let (v, sp) = (webview.clone(), spinner.downgrade());
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
                if v.opacity() < 1.0 {
                    fade_in(&v);
                }
                if let Some(sp) = sp.upgrade() {
                    sp.stop();
                    sp.set_visible(false);
                }
            });
        }
        crate::web_fonts::load_html(
            &webview,
            &document(initial_html, &webview, &image_policy, plain.get()),
            Some("https://hylki.localhost/editor"),
        );
        // A live theme flip re-grounds the open document (#148): the scheme
        // and the ground are baked into the document at load, so without
        // this the editor stays in the scheme it was opened in. Deferred to
        // the next main-loop pass — the theme's named colors are only
        // re-resolved after the signal fires.
        let theme_handler = {
            let weak = webview.downgrade();
            adw::StyleManager::default().connect_dark_notify(move |sm| {
                reground(&weak, sm.is_dark());
            })
        };
        // An appearance theme moves the same colors with no scheme flip, so
        // the editor re-grounds for that too — and drops out of the list
        // once its view is gone.
        {
            let weak = webview.downgrade();
            crate::theme::connect_changed(move || {
                reground(&weak, adw::StyleManager::default().is_dark())
            });
        }

        // WebKitGTK never turns Ctrl+Z into an editing command: its key
        // binding translator forwards the key to a hidden GtkTextView and
        // listens for that widget's *signals*, and undo is a GtkTextView
        // action rather than a signal — so nothing comes back, and its own
        // custom table (bold, italic, tab, the newlines, Ctrl+Shift+V) has
        // no undo entry. The document's `execCommand('undo')` is refused
        // from script for the same reason it would be in a browser. Only
        // the widget API reaches the page's history, so the editor drives
        // it itself, and mirrors what it can do for the host to read.
        let text_history = std::rc::Rc::new(std::cell::Cell::new((false, false)));
        let history_cb: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(bool, bool)>>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        if let Some(state) = webview.editor_state() {
            let cell = text_history.clone();
            let cb = history_cb.clone();
            // Seeded before the first change so a host that asks early is
            // not told the document can undo when it cannot.
            cell.set((state.is_undo_available(), state.is_redo_available()));
            state.connect_changed(move |state| {
                let now = (state.is_undo_available(), state.is_redo_available());
                if cell.replace(now) == now {
                    return;
                }
                if let Some(f) = cb.borrow().as_ref() {
                    f(now.0, now.1);
                }
            });
        }
        {
            // Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y over the body, in the capture
            // phase so they are decided before WebKit sees the key at all.
            // A host with a wider history of its own — the composer, whose
            // undo also covers its attachments — catches them further up,
            // where capture reaches it first, and this never runs.
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = webview.downgrade();
            keys.connect_key_pressed(move |_, keyval, _, state| {
                let Some(v) = weak.upgrade() else {
                    return gtk::glib::Propagation::Proceed;
                };
                match history_key(keyval, state) {
                    Some(redo) => {
                        v.execute_editing_command(if redo {
                            webkit6::EDITING_COMMAND_REDO
                        } else {
                            webkit6::EDITING_COMMAND_UNDO
                        });
                        gtk::glib::Propagation::Stop
                    }
                    None => gtk::glib::Propagation::Proceed,
                }
            });
            webview.add_controller(keys);
        }

        // The stock editable menu's single "Paste" hides the plain/rich choice
        // behind the preference; the menu offers both, always, in its place.
        // Over an image the menu leads with image actions — cut, copy, and
        // demoting an inline picture to an ordinary attachment — acting on
        // the exact node the right-click landed on (`__hylkiCtxImg`).
        let attach_cb: std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(std::path::PathBuf)>>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        // Files to put in the text that arrived before the document did (a
        // message started from files dropped on the main window): they go
        // in once it has loaded, and not into the document being replaced.
        let loaded = std::rc::Rc::new(std::cell::Cell::new(false));
        let pending_files: std::rc::Rc<std::cell::RefCell<Vec<std::path::PathBuf>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let (loaded, pending, cb) = (loaded.clone(), pending_files.clone(), attach_cb.clone());
            let plain = plain.clone();
            webview.connect_load_changed(move |v, ev| match ev {
                webkit6::LoadEvent::Started => loaded.set(false),
                webkit6::LoadEvent::Finished => {
                    loaded.set(true);
                    let paths = std::mem::take(&mut *pending.borrow_mut());
                    if !paths.is_empty() {
                        let files: Vec<_> = paths.iter().map(gtk::gio::File::for_path).collect();
                        deliver_files(v, &files, None, &cb, !plain.get());
                    }
                }
                _ => {}
            });
        }
        // Rich to begin with; [`RichEditor::set_source`] moves it.
        let source: std::rc::Rc<std::cell::Cell<Option<SourceKind>>> =
            std::rc::Rc::new(std::cell::Cell::new(None));
        let menu_attach_cb = attach_cb.clone();
        let menu_history = text_history.clone();
        let menu_plain = plain.clone();
        let spell_langs_cb: SpellLangsCallback = Default::default();
        let menu_spell_cb = spell_langs_cb.clone();
        webview.connect_context_menu(move |view, menu, hit| {
            crate::ui::message_view::strip_navigation_items(menu);
            if hit.context_is_image() {
                // Stock image entries (copy/save/open variants) are replaced
                // by ours, which also know about the editable document.
                for item in menu.items().iter().filter(|i| {
                    matches!(
                        i.stock_action(),
                        webkit6::ContextMenuAction::CopyImageToClipboard
                            | webkit6::ContextMenuAction::CopyImageUrlToClipboard
                            | webkit6::ContextMenuAction::DownloadImageToDisk
                            | webkit6::ContextMenuAction::OpenImageInNewWindow
                    )
                }) {
                    menu.remove(item);
                }
                // The stock Cut/Copy already act on the image — the
                // right-click selected it. The one image-specific verb is
                // demoting it to an ordinary attachment; the script that
                // lifts it declines non-embedded (remote) images itself,
                // since the hit test doesn't surface data: URIs to check
                // against here.
                let action = gtk::gio::SimpleAction::new("hylki-img-attach", None);
                let v = view.clone();
                let cb = menu_attach_cb.clone();
                action.connect_activate(move |_, _| detach_ctx_image(&v, cb.clone()));
                menu.insert(
                    &webkit6::ContextMenuItem::from_gaction(
                        &action,
                        &i18n("Send as Attachment Instead"),
                        None,
                    ),
                    0,
                );
                menu.insert(&webkit6::ContextMenuItem::new_separator(), 1);
                // Sizes lead the menu: the fractions are of the writing
                // width, which is the width the recipient reads at, so
                // "Large" means the full column rather than some pixel count
                // that means nothing on the other end. Dragging the corner
                // handles does the same thing by hand.
                let mut at = 0;
                for (label, kind) in [
                    (i18n("Small"), "small"),
                    (i18n("Medium"), "medium"),
                    (i18n("Large"), "large"),
                    (i18n("Original Size"), "original"),
                ] {
                    let action = gtk::gio::SimpleAction::new(&format!("hylki-img-{kind}"), None);
                    let v = view.clone();
                    action.connect_activate(move |_, _| {
                        exec(&v, &format!("window.__hylkiSetImageSize('{kind}')"));
                    });
                    menu.insert(
                        &webkit6::ContextMenuItem::from_gaction(&action, &label, None),
                        at,
                    );
                    at += 1;
                }
                {
                    let action = gtk::gio::SimpleAction::new("hylki-img-fit", None);
                    let v = view.clone();
                    let hint = js_escape(&i18n(
                        "Will be recompressed to the size shown when this is sent",
                    ));
                    action.connect_activate(move |_, _| {
                        exec(&v, &format!("window.__hylkiToggleFit('{hint}')"));
                    });
                    menu.insert(
                        &webkit6::ContextMenuItem::from_gaction(
                            &action,
                            &i18n("Recompress to This Size on Send"),
                            None,
                        ),
                        at,
                    );
                    at += 1;
                }
                menu.insert(&webkit6::ContextMenuItem::new_separator(), at);
            }
            // Which dictionaries check the message, switched one at a time
            // without a trip to Settings (#365). It is the app-wide setting,
            // so every open composer follows.
            if !hit.context_is_image() && menu_spell_cb.borrow().is_some() {
                append_spelling_menu(menu, &menu_spell_cb);
            }
            let items = menu.items();
            let Some(pos) = items
                .iter()
                .position(|i| i.stock_action() == webkit6::ContextMenuAction::Paste)
            else {
                return false; // no paste here (nothing editable hit): stock menu as-is
            };
            for item in items.iter().filter(|i| {
                matches!(
                    i.stock_action(),
                    webkit6::ContextMenuAction::Paste
                        | webkit6::ContextMenuAction::PasteAsPlainText
                )
            }) {
                menu.remove(item);
            }
            let mut at = pos as i32;
            // Plain text has no formatting to keep, so it offers only the
            // paste it does.
            let plain = menu_plain.get();
            for (label, rich) in [
                (i18n("Paste with Formatting"), true),
                (i18n("Paste as Plain Text"), false),
            ] {
                if rich && plain {
                    continue;
                }
                let action = gtk::gio::SimpleAction::new(
                    if rich {
                        "hylki-paste-rich"
                    } else {
                        "hylki-paste-plain"
                    },
                    None,
                );
                let v = view.clone();
                let cb = menu_attach_cb.clone();
                action.connect_activate(move |_, _| paste_into(&v, rich, &cb, plain));
                menu.insert(
                    &webkit6::ContextMenuItem::from_gaction(&action, &label, None),
                    at,
                );
                at += 1;
            }
            // Undo and Redo lead an editable menu, as they do everywhere
            // else — the one place the body's text history is visible at
            // all, rather than only on the keys. Inserted last so the
            // paste group's index above is still the one it was measured
            // at. Over an image the sizes keep the top: that menu is about
            // the picture under the pointer, not the message.
            if !hit.context_is_image() {
                let (can_undo, can_redo) = menu_history.get();
                for (label, name, redo, on) in [
                    (i18n("Undo"), "hylki-undo", false, can_undo),
                    (i18n("Redo"), "hylki-redo", true, can_redo),
                ] {
                    let action = gtk::gio::SimpleAction::new(name, None);
                    action.set_enabled(on);
                    let v = view.clone();
                    action.connect_activate(move |_, _| {
                        v.execute_editing_command(if redo {
                            webkit6::EDITING_COMMAND_REDO
                        } else {
                            webkit6::EDITING_COMMAND_UNDO
                        });
                    });
                    menu.insert(
                        &webkit6::ContextMenuItem::from_gaction(&action, &label, None),
                        i32::from(redo),
                    );
                }
                menu.insert(&webkit6::ContextMenuItem::new_separator(), 2);
            }
            false
        });

        // Files dropped from a file manager never reach the document: WebKitGTK
        // hands the page a uri-list it then refuses to serve, so the drop
        // handler in PASTE_SCRIPT sees an empty `DataTransfer.files` and lets
        // WebKit insert the path as a link. Only the widget can see them, so
        // the widget takes them — declaring just `GdkFileList` leaves every
        // other drag (text, an image dragged out of the reader) to WebKit.
        {
            let drop = gtk::DropTarget::new(
                gtk::gdk::FileList::static_type(),
                gtk::gdk::DragAction::COPY,
            );
            drop.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = webview.downgrade();
            let cb = attach_cb.clone();
            let src = source.clone();
            let plain = plain.clone();
            drop.connect_drop(move |_, value, x, y| {
                let Some(v) = weak.upgrade() else {
                    return false;
                };
                let Ok(list) = value.get::<gtk::gdk::FileList>() else {
                    return false;
                };
                // Source mode has no document to hold a picture: everything
                // dropped becomes an attachment, and the user writes the
                // reference to it themselves.
                if src.get().is_some() {
                    return attach_all(&list.files(), &cb);
                }
                deliver_files(&v, &list.files(), Some((x, y)), &cb, !plain.get())
            });
            webview.add_controller(drop);
        }

        // The editor and (later) its preview are pages of one stack, so a
        // preview swaps in without the editor document being torn down and
        // rebuilt — which would cost the undo history and the caret.
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        let edit = gtk::Overlay::new();
        edit.set_child(Some(&webview));
        edit.add_overlay(&spinner);
        stack.add_named(&edit, Some("edit"));

        let expanded = crate::config::load_privacy().toolbar_expanded;
        let (toolbar, toolbar_commands, toolbar_end, bar_state) =
            build_toolbar(&webview, &stack, &attach_cb, expanded);
        // `fmtState` in PASTE_SCRIPT posts where the caret is: the block
        // kinds it sits in (`q` quote, `u` bulleted list, `o` numbered
        // list), its paragraph style and its font. A set toggle also means a
        // click on it leaves that block instead of opening another (issue
        // #137, EmmanuelP's follow-up).
        ucm.connect_script_message_received(Some("hylkiFormat"), move |_, value| {
            bar_state.update(&value.to_str());
        });

        let frame = gtk::Frame::new(None);
        frame.set_vexpand(true);
        // Same card treatment as the address fields' boxed-list: shadow and
        // radius from libadwaita, with the WebView clipped to the corners.
        frame.add_css_class("card");
        // Found by name when a sent reply's editor turns into its card in
        // the conversation (send_morph.rs).
        frame.add_css_class("compose-editor-card");
        frame.set_overflow(gtk::Overflow::Hidden);
        frame.set_child(Some(&stack));

        {
            // Ctrl+. and Ctrl+; open WebKitGTK's own emoji chooser, but
            // what is picked in it never reaches the page (#348; WebKitGTK
            // 2.50 to 2.54 at least, a bare WebView does the same). The
            // editor opens GTK's chooser itself, at the caret, and types
            // the emoji in.
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = webview.downgrade();
            let anchor = stack.downgrade();
            keys.connect_key_pressed(move |_, keyval, _, state| {
                use gtk::gdk::{Key, ModifierType};
                let mods = state
                    & (ModifierType::CONTROL_MASK
                        | ModifierType::SHIFT_MASK
                        | ModifierType::ALT_MASK);
                if mods != ModifierType::CONTROL_MASK
                    || !matches!(keyval, Key::period | Key::semicolon)
                {
                    return gtk::glib::Propagation::Proceed;
                }
                let (Some(v), Some(anchor)) = (weak.upgrade(), anchor.upgrade()) else {
                    return gtk::glib::Propagation::Proceed;
                };
                show_emoji_chooser(&v, &anchor);
                gtk::glib::Propagation::Stop
            });
            webview.add_controller(keys);
        }

        let bx = gtk::Box::new(gtk::Orientation::Vertical, 6);
        bx.append(&toolbar);
        bx.append(&frame);

        RichEditor {
            widget: bx,
            webview,
            toolbar_commands,
            toolbar_end,
            stack,
            preview: std::rc::Rc::new(std::cell::RefCell::new(None)),
            source: source.clone(),
            plain,
            attach_cb,
            loaded,
            pending_files,
            text_history,
            history_cb,
            spell_langs_cb,
            _theme_handler: std::rc::Rc::new(ThemeHandlerGuard(Some(theme_handler))),
            image_policy,
            zoom: std::rc::Rc::new(std::cell::Cell::new(1.0)),
        }
    }

    /// Step the document's own text history back (`redo` false) or forward.
    /// Asynchronous like every other page command: [`RichEditor::can_undo`]
    /// only catches up when WebKit reports the new state.
    pub fn undo(&self) {
        self.webview
            .execute_editing_command(webkit6::EDITING_COMMAND_UNDO);
    }

    pub fn redo(&self) {
        self.webview
            .execute_editing_command(webkit6::EDITING_COMMAND_REDO);
    }

    /// Run any WebCore editing command against the document ("DeleteBackward",
    /// "InsertParagraph", …). The widget API is the only route to them that
    /// reaches the page's own history — the same reason [`RichEditor::undo`]
    /// cannot go through `execCommand`.
    pub fn editing_command(&self, command: &str) {
        self.webview.execute_editing_command(command);
    }

    /// Whether the body has anything left to take back. Read from the
    /// mirrored editor state, so it is a main-loop pass or two behind a
    /// command just issued — a host must treat "still true" right after
    /// [`RichEditor::undo`] as "ask again", not as a miscount.
    pub fn can_undo(&self) -> bool {
        self.text_history.get().0
    }

    pub fn can_redo(&self) -> bool {
        self.text_history.get().1
    }

    /// Called with `(can_undo, can_redo)` whenever the body's text history
    /// changes: an edit, a command, or a reload that drops it.
    pub fn connect_history_changed(&self, f: impl Fn(bool, bool) + 'static) {
        *self.history_cb.borrow_mut() = Some(Box::new(f));
    }

    /// Offer the Spelling Language submenu, and receive the setting it
    /// makes: the comma-separated language list, empty for the system
    /// language (#365).
    pub fn connect_spell_languages(&self, f: impl Fn(String) + 'static) {
        *self.spell_langs_cb.borrow_mut() = Some(Box::new(f));
    }

    /// What "Send as Attachment Instead" does with the lifted image: the
    /// host receives the temp file's path and adds it to its attachments.
    pub fn connect_send_as_attachment(&self, f: impl Fn(std::path::PathBuf) + 'static) {
        *self.attach_cb.borrow_mut() = Some(Box::new(f));
    }

    /// Replace the editor contents with `content` (HTML), in rich mode, or
    /// flattened to plain text when [`RichEditor::set_plain`] says so.
    pub fn set_html(&self, content: &str) {
        self.source.set(None);
        self.show_preview(None);
        crate::web_fonts::load_html(
            &self.webview,
            &document(content, &self.webview, &self.image_policy, self.plain.get()),
            Some("https://hylki.localhost/editor"),
        );
    }

    /// Put the editor into source mode holding `text`: a monospace
    /// plain-text surface with no formatting toolbar, which the composer
    /// converts on the way out.
    pub fn set_source(&self, kind: SourceKind, text: &str) {
        self.source.set(Some(kind));
        self.show_preview(None);
        crate::web_fonts::load_html(
            &self.webview,
            &source_document(text, &self.webview),
            Some("https://hylki.localhost/editor"),
        );
    }

    /// Show the message as plain text from the next [`RichEditor::set_html`]
    /// on: the document flattens itself to the text that is sent, in the
    /// monospace font, and pastes and drops bring in text alone (#424).
    pub fn set_plain(&self, on: bool) {
        self.plain.set(on);
    }

    /// The source language the editor is in, or `None` in rich mode.
    pub fn source_kind(&self) -> Option<SourceKind> {
        self.source.get()
    }

    /// Read the source text back, exactly as typed.
    pub fn extract_source(&self, cb: impl FnOnce(String) + 'static) {
        self.webview.evaluate_javascript(
            "window.__hylkiSourceText ? window.__hylkiSourceText() : ''",
            None,
            None,
            gtk::gio::Cancellable::NONE,
            move |res| cb(res.map(|v| v.to_str().to_string()).unwrap_or_default()),
        );
    }

    /// Show `html` rendered in place of the editor, or go back to editing.
    /// The preview is read-only and runs no scripts — it is a look at the
    /// message, not a second editor.
    pub fn show_preview(&self, html: Option<&str>) {
        let Some(html) = html else {
            self.stack.set_visible_child_name("edit");
            return;
        };
        // Taken out first: a `match` on `borrow()` holds that borrow for the
        // whole match, and the arm below needs `borrow_mut()`.
        let existing = self.preview.borrow().clone();
        let view = match existing {
            Some(v) => v,
            None => {
                let settings = webkit6::Settings::new();
                settings.set_enable_javascript(false);
                settings.set_enable_developer_extras(false);
                let v = webkit6::WebView::builder()
                    .web_context(&super::message_view::shared_web_context())
                    .settings(&settings)
                    .build();
                crate::memory_report::register_web_view(&v, "editor preview");
                v.set_zoom_level(self.zoom.get());
                v.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
                self.stack.add_named(&v, Some("preview"));
                *self.preview.borrow_mut() = Some(v.clone());
                v
            }
        };
        crate::web_fonts::load_html(
            &view,
            &preview_document(html, &view, &self.image_policy),
            Some("https://hylki.localhost/preview"),
        );
        self.stack.set_visible_child_name("preview");
    }

    pub fn grab_focus(&self) {
        self.webview.grab_focus();
        if self.source.get().is_some() {
            // The document's own field is what takes the caret.
            exec(
                &self.webview,
                "var t=document.getElementById('src');if(t)t.focus();",
            );
        } else {
            exec(&self.webview, PLACE_CARET);
        }
    }

    /// Run a JavaScript snippet against the editor document (e.g. to swap the
    /// signature block when the From account changes).
    pub fn run_js(&self, js: &str) {
        exec(&self.webview, js);
    }

    /// Run a snippet and hand back what it returns, as text ("" when it
    /// failed).
    pub fn eval(&self, js: &str, cb: impl FnOnce(String) + 'static) {
        self.webview
            .evaluate_javascript(js, None, None, gtk::gio::Cancellable::NONE, move |res| {
                cb(res.map(|v| v.to_str().to_string()).unwrap_or_default())
            });
    }

    /// Whether keyboard focus is currently inside the editor's WebView — the
    /// guard a host's paste shortcut uses so it never hijacks Ctrl+V aimed at
    /// an address entry.
    pub fn has_focus(&self) -> bool {
        self.webview
            .root()
            .and_then(|r| r.focus())
            .is_some_and(|f| {
                f == *self.webview.upcast_ref::<gtk::Widget>() || f.is_ancestor(&self.webview)
            })
    }

    /// Put the pictures among `paths` in the text at the caret, and hand
    /// the rest to the attachment list, as a drop on the text does. Before
    /// the document has loaded they wait for it.
    pub fn insert_files(&self, paths: &[std::path::PathBuf]) {
        if !self.loaded.get() {
            self.pending_files
                .borrow_mut()
                .extend(paths.iter().cloned());
            return;
        }
        let files: Vec<_> = paths.iter().map(gtk::gio::File::for_path).collect();
        deliver_files(
            &self.webview,
            &files,
            None,
            &self.attach_cb,
            !self.plain.get(),
        );
    }

    /// Paste the clipboard into the editor, keeping (`rich`) or stripping the
    /// clipboard's formatting for this one paste, whatever the standing
    /// preference says.
    pub fn paste(&self, rich: bool) {
        paste_into(&self.webview, rich, &self.attach_cb, self.plain.get());
    }

    /// Whether the body has been edited since it was loaded (async read of a JS
    /// flag set on the first `input` event). Used to avoid saving a pristine,
    /// quote-only reply to Drafts when the reader navigates away.
    pub fn is_dirty(&self, cb: impl FnOnce(bool) + 'static) {
        self.webview.evaluate_javascript(
            "String(!!window.__hylkiDirty)",
            None,
            None,
            gtk::gio::Cancellable::NONE,
            move |res| cb(res.map(|v| v.to_str() == "true").unwrap_or(false)),
        );
    }

    /// Show or hide the formatting commands: hidden while the message is
    /// composed as plain text (#180) or as source, where they would go
    /// nowhere. The row itself stays, carrying whatever the host put at its
    /// end — a chooser that vanished with the buttons would be the one
    /// control needed to get them back.
    pub fn set_formatting_visible(&self, on: bool) {
        self.toolbar_commands.set_tools_shown(on);
    }

    /// Show the text, and the preview, at `percent` of its size. Page zoom
    /// rather than a font size, so nothing about it reaches the message.
    pub fn set_zoom(&self, percent: u32) {
        let z = f64::from(percent) / 100.0;
        self.zoom.set(z);
        self.webview.set_zoom_level(z);
        if let Some(v) = self.preview.borrow().as_ref() {
            v.set_zoom_level(z);
        }
    }

    /// Keep the text area at least `height` pixels tall, whatever the
    /// toolbar above it takes: a host with a fixed-size slot would
    /// otherwise lose text to a wrapped or expanded toolbar.
    pub fn set_text_height(&self, height: i32) {
        self.stack.set_height_request(height);
    }

    /// The far end of the formatting row, for a host's own controls.
    pub fn toolbar_end(&self) -> &gtk::Box {
        &self.toolbar_end
    }

    /// Read the current body HTML asynchronously.
    pub fn extract_html(&self, cb: impl FnOnce(String) + 'static) {
        self.webview.evaluate_javascript(
            "window.__hylkiBodyHtml()",
            None,
            None,
            gtk::gio::Cancellable::NONE,
            move |res| cb(res.map(|v| v.to_str().to_string()).unwrap_or_default()),
        );
    }

    /// Read the body for *sending*: any picture armed with "Recompress to
    /// This Size on Send" is recut to the size it is drawn at first. Drafts
    /// go through [`RichEditor::extract`], which never touches the pixels —
    /// a save must not cost quality.
    pub fn extract_for_send(&self, cb: impl FnOnce(String, String) + 'static) {
        self.read_body("window.__hylkiBodyHtmlForSend()", cb);
    }

    /// Read the current body HTML and a plain-text rendering asynchronously.
    pub fn extract(&self, cb: impl FnOnce(String, String) + 'static) {
        self.read_body("window.__hylkiBodyHtml()", cb);
    }

    fn read_body(&self, reader: &str, cb: impl FnOnce(String, String) + 'static) {
        // JSON, not a separator: the value comes back as a C string, so a NUL
        // between the halves cut the text off and plain-text mail went out
        // empty (#297).
        self.webview.evaluate_javascript(
            &format!("JSON.stringify([{reader}, window.__hylkiBodyText()])"),
            None,
            None,
            gtk::gio::Cancellable::NONE,
            move |res| {
                let json = res.map(|v| v.to_str().to_string()).unwrap_or_default();
                let (html, text) = split_body(&json);
                cb(html, text);
            },
        );
    }
}

/// The HTML and the text of the body from `read_body`'s JSON pair.
fn split_body(json: &str) -> (String, String) {
    serde_json::from_str::<(String, String)>(json).unwrap_or_default()
}

/// Whether a keystroke is an undo (`Some(false)`) or a redo (`Some(true)`).
/// Ctrl+Y is bound alongside Ctrl+Shift+Z because both spellings are in wide
/// use and the app's own history (#200) already answers to each.
pub fn history_key(keyval: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> Option<bool> {
    if !state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
        return None;
    }
    let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
    if !shift && keyval == gtk::gdk::Key::z {
        return Some(false);
    }
    if (shift && matches!(keyval, gtk::gdk::Key::z | gtk::gdk::Key::Z))
        || keyval == gtk::gdk::Key::y
    {
        return Some(true);
    }
    None
}

/// Give the rich document's body the caret. Keyboard focus on the view
/// alone left no caret in it, so a reply opened with nowhere to type
/// until the body was clicked (#266). A caret already in the body stays
/// where it is; otherwise it goes to the top, above any quote. Only a
/// contenteditable body is touched: the source document's field takes
/// focus itself.
const PLACE_CARET: &str = "(function(){var b=document.body;\
    if(!b||!b.isContentEditable)return;\
    var s=getSelection();\
    var kept=s.rangeCount&&b.contains(s.anchorNode)?s.getRangeAt(0).cloneRange():null;\
    b.focus();\
    var r=kept;\
    if(!r){r=document.createRange();r.setStart(b,0);r.collapse(true);}\
    s.removeAllRanges();s.addRange(r);})()";

/// GTK's emoji chooser over the editor, pointing at the caret (#348). It is
/// parented only while it is open, so a closed composer leaves nothing
/// behind.
fn show_emoji_chooser(webview: &webkit6::WebView, anchor: &gtk::Stack) {
    let weak = webview.downgrade();
    let anchor = anchor.downgrade();
    webview.evaluate_javascript(
        "window.__hylkiEmojiMark ? window.__hylkiEmojiMark() : ''",
        None,
        None,
        gtk::gio::Cancellable::NONE,
        move |res| {
            let (Some(v), Some(anchor)) = (weak.upgrade(), anchor.upgrade()) else {
                return;
            };
            let chooser = gtk::EmojiChooser::new();
            chooser.set_parent(&anchor);
            let at: Vec<f64> = res
                .map(|r| r.to_str().to_string())
                .unwrap_or_default()
                .split(',')
                .filter_map(|n| n.trim().parse().ok())
                .collect();
            if let [x, y, w, h] = at[..] {
                // CSS pixels are widget pixels times the zoom.
                let z = v.zoom_level();
                let (x, y) = v
                    .compute_point(
                        &anchor,
                        &gtk::graphene::Point::new((x * z) as f32, (y * z) as f32),
                    )
                    .map_or((x * z, y * z), |p| (p.x() as f64, p.y() as f64));
                chooser.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                    x as i32,
                    y as i32,
                    (w * z).max(1.0) as i32,
                    (h * z).max(1.0) as i32,
                )));
            }
            let weak = v.downgrade();
            chooser.connect_emoji_picked(move |_, emoji| {
                let Some(v) = weak.upgrade() else { return };
                v.grab_focus();
                let text = serde_json::to_string(emoji).unwrap_or_default();
                exec(
                    &v,
                    &format!("window.__hylkiEmojiPut && window.__hylkiEmojiPut({text})"),
                );
            });
            chooser.connect_closed(|c| {
                // Unparented after the pick has been handled, not inside the
                // signal that is still running on it.
                let c = c.clone();
                gtk::glib::idle_add_local_once(move || c.unparent());
            });
            chooser.popup();
        },
    );
}

fn exec(webview: &webkit6::WebView, js: &str) {
    webview.evaluate_javascript(js, None, None, gtk::gio::Cancellable::NONE, |_| {});
}

/// Fade the editor view to full opacity. The animation object is parked on
/// the view itself so it stays alive for its 200ms (a dropped animation
/// stops); a repeat call replaces the parked one harmlessly.
fn fade_in(webview: &webkit6::WebView) {
    let anim = adw::TimedAnimation::new(
        webview,
        webview.opacity(),
        1.0,
        200,
        adw::CallbackAnimationTarget::new({
            let v = webview.clone();
            move |o| v.set_opacity(o)
        }),
    );
    anim.set_easing(adw::Easing::EaseOutCubic);
    anim.play();
    unsafe {
        webview.set_data("hylki-fade-anim", anim);
    }
}

/// Escape a string for inclusion inside a single-quoted JavaScript string.
pub fn js_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\x3c"),
            _ => out.push(c),
        }
    }
    out
}

impl Drop for RichEditor {
    /// The last handle takes the web process with it. A WebKit view keeps
    /// internal references to itself after it is unparented (measured: two
    /// on a bare view, one here), so it is never finalized by its owners
    /// letting go, and its WebKitWebProcess — a few hundred MB, invisible
    /// to the Flatpak sandbox's process tree — lived on for every composer
    /// ever closed (#221: six of them in one user's session monitor).
    /// Deferred past the composer's slide-out so the document does not
    /// blank mid-animation.
    fn drop(&mut self) {
        if std::rc::Rc::strong_count(&self._theme_handler) != 1 {
            return; // another handle still uses the view
        }
        let views: Vec<webkit6::WebView> = std::iter::once(self.webview.clone())
            .chain(self.preview.borrow().clone())
            .collect();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
            for v in &views {
                crate::memory_report::release_web_view(v);
            }
        });
    }
}

#[cfg(test)]
mod body_tests {
    use super::documents::css_monospace_family;
    use super::*;

    /// The text half survives the trip out of WebKit: a plain-text message
    /// went out empty when the halves were joined with a NUL (#297).
    #[test]
    fn both_halves_of_the_body_arrive() {
        let json = r#"["<p>Hi</p>","Hi\nthere \u0000 \"quoted\""]"#;
        assert_eq!(
            split_body(json),
            (
                "<p>Hi</p>".to_string(),
                "Hi\nthere \u{0} \"quoted\"".to_string()
            )
        );
        assert_eq!(split_body("null"), (String::new(), String::new()));
    }

    /// The plain-text view's font is the desktop's monospace family, quoted
    /// so a name with spaces or digits holds, with the generic one behind
    /// it (#424).
    #[test]
    fn plain_text_uses_the_desktop_monospace_family() {
        assert_eq!(
            css_monospace_family("Adwaita Mono 11"),
            "\"Adwaita Mono\",monospace"
        );
        assert_eq!(
            css_monospace_family("Source Code Pro Bold 10"),
            "\"Source Code Pro\",monospace"
        );
        assert_eq!(css_monospace_family("Monospace 10"), "monospace");
        assert_eq!(css_monospace_family(""), "monospace");
        assert_eq!(
            css_monospace_family("Evil\"}body{x 10"),
            "\"Evilbodyx\",monospace"
        );
    }
}

#[cfg(test)]
mod pinned_tests {
    //! Characterization tests (restructure, area D).
    use super::scripts::{EMOJI_SCRIPT, HISTORY_SCRIPT, PASTE_SCRIPT};
    use super::signature::{find_src_attr, quoted_value};
    use super::*;

    fn fnv(s: &str) -> (usize, u64) {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        (s.len(), h)
    }

    /// The scripts go into every editor page as they are: pin their bytes so
    /// moving them into files cannot change them.
    #[test]
    fn embedded_scripts_are_unchanged() {
        assert_eq!(fnv(HISTORY_SCRIPT), (1287, 0x8e997368982bb599));
        assert_eq!(fnv(EMOJI_SCRIPT), (1621, 0x748b009b19f3f0ec));
        assert_eq!(fnv(PASTE_SCRIPT), (37862, 0x6fa3db610728331e));
    }

    #[test]
    fn history_keys() {
        use gtk::gdk::{Key, ModifierType as M};
        let c = M::CONTROL_MASK;
        let cs = c | M::SHIFT_MASK;
        assert_eq!(history_key(Key::z, c), Some(false));
        assert_eq!(history_key(Key::z, cs), Some(true));
        assert_eq!(history_key(Key::Z, cs), Some(true));
        assert_eq!(history_key(Key::Z, c), None);
        assert_eq!(history_key(Key::y, c), Some(true));
        assert_eq!(history_key(Key::y, cs), Some(true));
        assert_eq!(history_key(Key::z, M::empty()), None);
        assert_eq!(history_key(Key::x, c), None);
    }

    #[test]
    fn js_escape_quotes_breaks_and_tags() {
        assert_eq!(
            js_escape("a'b\"c\\d\ne\rf\tg<h>"),
            "a\\'b\\\"c\\\\d\\ne\\rf\\tg\\x3ch>"
        );
        assert_eq!(js_escape("plain"), "plain");
    }

    #[test]
    fn src_attributes_are_found_after_a_space_in_any_case() {
        assert_eq!(find_src_attr("<img SRC=\"a\">"), Some(5));
        assert_eq!(find_src_attr("<img data-src=\"a\" src='b'>"), Some(18));
        assert_eq!(find_src_attr("src=\"a\""), None);
        assert_eq!(quoted_value("src=\"a b\" x"), Some(("src=\"", "a b")));
        assert_eq!(quoted_value("src='c'"), Some(("src='", "c")));
        assert_eq!(quoted_value("src=c"), None);
        assert_eq!(quoted_value("src=\"open"), None);
    }

    #[test]
    fn a_signature_with_a_less_than_sign_is_taken_as_html() {
        assert_eq!(signature_to_html("a < b"), "a < b");
        assert_eq!(signature_to_html("a & b\nc"), "a &amp; b<br>c");
    }
}
