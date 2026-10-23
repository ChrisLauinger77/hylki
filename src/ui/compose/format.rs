//! The composing formats (rich, Markdown, HTML, plain), switching between them,
//! and the body's zoom.

use super::messages::ComposeInput;
use super::Compose;
use crate::config::ComposeFormat;
use crate::i18n::{i18n, i18n_f};
use crate::ui::context_menu::MenuEntry;
use adw::prelude::*;
use relm4::prelude::*;

/// A body as Markdown, keeping the empty line a reply opens with above its
/// quote and signature. The conversion drops leading blank lines, which
/// left the caret at the start of the "On … wrote:" line, so whatever was
/// typed joined it.
pub(super) fn markdown_body(html: &str) -> String {
    let md = crate::markdown::from_html(html);
    let top = html.trim_start();
    if !md.is_empty() && (top.starts_with("<p><br></p>") || top.starts_with("<div><br></div>")) {
        format!("\n\n{md}")
    } else {
        md
    }
}

/// The icon a composing format carries, in the header and in its menu.
pub(super) fn format_icon(format: ComposeFormat) -> &'static str {
    match format {
        ComposeFormat::Rich => "format-text-rich-symbolic",
        ComposeFormat::Markdown => "markdown-symbolic",
        ComposeFormat::Html => "code-symbolic",
        ComposeFormat::Plain => "text-x-generic-symbolic",
    }
}

/// What a format is called where the user meets it.
pub(super) fn format_label(format: ComposeFormat) -> String {
    match format {
        ComposeFormat::Rich => i18n("Rich text"),
        ComposeFormat::Markdown => i18n("Markdown"),
        ComposeFormat::Html => i18n("HTML source"),
        ComposeFormat::Plain => i18n("Plain text"),
    }
}

/// The one-line explanation under each format in the menu.
fn format_hint(format: ComposeFormat) -> String {
    match format {
        ComposeFormat::Rich => i18n("Format as you type; it is sent as HTML"),
        ComposeFormat::Markdown => i18n("Write Markdown; it is sent as formatted mail"),
        ComposeFormat::Html => i18n("Write the message's HTML by hand"),
        ComposeFormat::Plain => i18n("No formatting at all"),
    }
}

/// The zoom of the body in Markdown, HTML and plain text, in percent: the
/// session's, so the next composer opens at the size the last one was left
/// at. Rich text stays at 100, where what is on screen is what is sent.
pub(super) static ZOOM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(100);

/// What HYLKI_SHOWCASE_PLAIN_LOG reports about the body (demo only).
pub(super) const SHOWCASE_PLAIN_LOG: &str = r#"JSON.stringify({plain:!!window.__hylkiPlain,
font:document.body?getComputedStyle(document.body).fontFamily:'',
bold:document.body&&document.body.querySelector('b,strong')?getComputedStyle(document.body.querySelector('b,strong')).fontWeight:'',
html:document.body?document.body.innerHTML:document.documentElement.outerHTML.slice(0,400),
text:window.__hylkiBodyText?window.__hylkiBodyText():''})"#;

impl Compose {
    /// Whether the body zooms in its present format; see [`ComposeInput::Zoom`].
    pub fn zooms(&self) -> bool {
        self.zoomable.get()
    }

    /// The format chooser's icon and tooltip for the format it is currently
    /// set to, and whether the preview toggle can afford its label.
    pub(super) fn dress_format_buttons(&self) {
        self.format_btn.set_icon_name(format_icon(self.format));
        self.format_btn.set_tooltip_text(Some(
            i18n_f(
                "Writing in {format}",
                &[("format", &format_label(self.format))],
            )
            .as_str(),
        ));
        // The one label on the row goes when there is no width for it, the
        // same trade the header makes when it folds.
        let preview = i18n("Preview");
        self.preview_content
            .set_label(if self.narrow { "" } else { preview.as_str() });
    }

    /// The four formats as menu entries, the current one wearing a tick in
    /// place of its icon (as the OpenPGP toggles do).
    pub(super) fn format_entries(&self, sender: &ComponentSender<Self>) -> Vec<MenuEntry> {
        [
            ComposeFormat::Rich,
            ComposeFormat::Markdown,
            ComposeFormat::Html,
            ComposeFormat::Plain,
        ]
        .into_iter()
        .map(|format| {
            let s = sender.clone();
            let label = format!("{} — {}", format_label(format), format_hint(format));
            MenuEntry::new(&label, move || {
                s.input_sender().emit(ComposeInput::SetFormat(format))
            })
            .icon(format_icon(format))
            .selected(format == self.format)
        })
        .collect()
    }
}
