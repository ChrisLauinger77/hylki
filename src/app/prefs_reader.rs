//! Applying reader settings: zoom, Reader View, font and color overrides.

use super::*;

impl AppModel {
    /// The session's message zoom: every reader on screen follows.
    pub(super) fn set_zoom(&mut self, zoom: u32) {
        if zoom == self.zoom {
            return;
        }
        self.zoom = zoom;
        self.message_view.emit(MessageViewInput::SetZoom(zoom));
        for p in self.popouts.values() {
            p.controller.emit(MessageWindowInput::SetZoom(zoom));
        }
    }

    /// What Reader View does on each open: Focus Mode's "on" while it holds,
    /// else the setting.
    pub(super) fn effective_reader_default(&self) -> config::ReaderDefault {
        if self.focus.active(config::FocusPart::ReaderView) {
            config::ReaderDefault::On
        } else {
            self.reader_default
        }
    }

    /// Reader View for the message on screen: on in Focus Mode, else as saved.
    pub(super) fn effective_reader_mode(&self) -> bool {
        self.reader_mode || self.focus.active(config::FocusPart::ReaderView)
    }

    /// The reader's font-and-color override (#56) as the reader applies it:
    /// the font only when the switch is on, the interface font standing in
    /// where none was chosen.
    pub(super) fn reader_style(&self) -> config::ReaderStyle {
        let font = self.override_fonts.then(|| {
            if self.reader_font.trim().is_empty() {
                interface_font()
            } else {
                self.reader_font.clone()
            }
        });
        let plain_font = self.plain_monospace.then(|| {
            if self.plain_font.trim().is_empty() {
                crate::desktop::monospace_font()
            } else {
                self.plain_font.clone()
            }
        });
        config::ReaderStyle {
            font,
            colors: self.override_colors,
            plain_font,
        }
    }

    /// Hand the current reader style to the reader and every popped-out window.
    pub(super) fn push_reader_style(&self) {
        let style = self.reader_style();
        self.message_view
            .emit(MessageViewInput::SetReaderStyle(style.clone()));
        for p in self.popouts.values() {
            p.controller
                .emit(MessageWindowInput::SetReaderStyle(style.clone()));
        }
    }

    /// Tell the reader how card actions should show (⋯ toggle / auto on
    /// hover / always).
    pub(super) fn push_card_actions_mode(&self) {
        self.message_view
            .emit(MessageViewInput::SetCardActionsMode {
                hover_toggle: self.card_actions_hover,
                hover_auto: self.card_actions_auto,
            });
    }
}

/// The desktop's interface font ("Adwaita Sans 11") as a Pango description:
/// what the reader sets mail in when the font override is on but no font
/// was chosen (#56). Cantarell 11 when GTK has no settings (headless).
fn interface_font() -> String {
    gtk::Settings::default()
        .and_then(|s| s.gtk_font_name())
        .map(|f| f.to_string())
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| "Cantarell 11".to_string())
}
