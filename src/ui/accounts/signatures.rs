//! The account editor's signatures (#445): the lazily made rich editor,
//! and the list of named signatures it edits one at a time.

use super::*;

/// What signature `i` is called in the chooser and the composer: its name,
/// or its place in the list when it has none.
pub(super) fn signature_label(signatures: &[NamedSignature], i: usize) -> String {
    match signatures.get(i).map(|s| s.name.trim()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => i18n_f("Signature {n}", &[("n", &(i + 1).to_string())]),
    }
}

/// Whether the editor's HTML is effectively empty (no visible content).
fn signature_is_empty(html: &str) -> bool {
    let stripped: String = {
        let mut out = String::new();
        let mut in_tag = false;
        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    };
    stripped.replace("&nbsp;", " ").trim().is_empty()
}

impl AccountsWindow {
    /// The signature editor, created (and mounted in its holder) on first
    /// use: a WebKit view is the slowest thing the panel would otherwise
    /// build, for a field most visits never reach.
    pub(super) fn sig_editor(&mut self, widgets: &AccountsWindowWidgets) -> &RichEditor {
        if self.sig_editor.is_none() {
            let editor = RichEditor::new("", None, false);
            // The text keeps its height when the toolbar wraps or is
            // expanded to a second row; the panel grows instead.
            editor.set_text_height(130);
            editor.toolbar_end().append(&self.sig_source_btn);
            widgets.sig_holder.append(&editor.widget);
            self.sig_editor = Some(editor);
        }
        self.sig_editor.as_ref().expect("created above")
    }

    /// Set "Edit HTML" and the formatting tools to the mode the signature
    /// editor is in: as source the tools would go nowhere, and the button
    /// goes back to the rendered signature.
    pub(super) fn sync_source_button(&mut self, widgets: &AccountsWindowWidgets) {
        let source = self.sig_editor(widgets).source_kind().is_some();
        self.sig_editor(widgets).set_formatting_visible(!source);
        self.sig_source_btn.set_label(&if source {
            i18n("Preview")
        } else {
            i18n("Edit HTML")
        });
    }

    /// The shown signature as HTML, from the rich editor or, while it is
    /// shown as source, from that source, cleaned as an imported one is.
    pub(super) fn read_signature(
        &mut self,
        widgets: &AccountsWindowWidgets,
        cb: impl FnOnce(String) + 'static,
    ) {
        let editor = self.sig_editor(widgets);
        if editor.source_kind().is_some() {
            editor.extract_source(move |text| cb(rich_editor::signature_from_source(&text, None)));
        } else {
            editor.extract_html(cb);
        }
    }

    /// Put an account's signatures in the editor (#445), the first one
    /// shown. An account without any gets one empty signature to type in,
    /// which new messages start with once it has text, as before there
    /// could be several.
    pub(super) fn load_signatures(
        &mut self,
        widgets: &AccountsWindowWidgets,
        mut signatures: Vec<NamedSignature>,
        mut default: Option<usize>,
    ) {
        if signatures.is_empty() {
            signatures.push(NamedSignature {
                name: i18n("Signature"),
                html: String::new(),
            });
            default = Some(0);
        }
        self.sig_edits = signatures;
        self.sig_default = default;
        self.sig_touched = false;
        self.sig_reading = false;
        self.show_signature(widgets, 0);
    }

    /// Show signature `i` in the editor and set the rows above it to it.
    /// Their notifies arrive after this and find nothing changed.
    pub(super) fn show_signature(&mut self, widgets: &AccountsWindowWidgets, i: usize) {
        self.sig_shown = i;
        let html = self
            .sig_edits
            .get(i)
            .map(|s| rich_editor::signature_to_html(&s.html))
            .unwrap_or_default();
        self.sig_editor(widgets).set_html(&html);
        self.sync_source_button(widgets);
        let labels: Vec<String> = (0..self.sig_edits.len())
            .map(|n| signature_label(&self.sig_edits, n))
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        widgets
            .sig_choice_row
            .set_model(Some(&gtk::StringList::new(&labels)));
        widgets.sig_choice_row.set_selected(i as u32);
        widgets.sig_name_row.set_text(
            self.sig_edits
                .get(i)
                .map(|s| s.name.as_str())
                .unwrap_or_default(),
        );
        widgets
            .sig_default_row
            .set_active(self.sig_default == Some(i));
        let several = self.sig_edits.len() > 1;
        widgets.sig_choice_row.set_visible(several);
        widgets.sig_name_row.set_visible(several);
    }

    /// Read the shown signature's text back out of the editor before
    /// another is shown (`add` makes a new one); the answer is
    /// [`AccountsInput::SigStored`].
    pub(super) fn read_back_signature(
        &mut self,
        widgets: &AccountsWindowWidgets,
        sender: &ComponentSender<Self>,
        add: bool,
    ) {
        self.sig_reading = true;
        let s = sender.clone();
        self.sig_editor(widgets)
            .is_dirty(move |dirty| s.input_sender().emit(AccountsInput::SigTextTouched(dirty)));
        let s = sender.clone();
        self.read_signature(widgets, move |html| {
            s.input_sender()
                .emit(AccountsInput::SigStored { html, add })
        });
    }

    /// The signatures Save writes: the empty ones left out, a missing name
    /// filled in, and the default followed to where it lands.
    pub(super) fn saved_signatures(&self) -> (Vec<NamedSignature>, Option<usize>) {
        let mut kept = Vec::new();
        let mut default = None;
        for (i, sig) in self.sig_edits.iter().enumerate() {
            if signature_is_empty(sig.html.trim()) {
                continue;
            }
            if self.sig_default == Some(i) {
                default = Some(kept.len());
            }
            kept.push(NamedSignature {
                name: signature_label(&self.sig_edits, i),
                html: sig.html.clone(),
            });
        }
        (kept, default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_emptiness_ignores_tags_and_spaces_but_also_images() {
        assert!(signature_is_empty(""));
        assert!(signature_is_empty("<p></p>"));
        assert!(signature_is_empty("<p> &nbsp; </p><br>"));
        // An image-only signature counts as empty (see Suspected bugs).
        assert!(signature_is_empty("<img src=\"cid:logo\">"));
        assert!(!signature_is_empty("<b>Jo</b>"));
        assert!(!signature_is_empty("a < b"));
    }

    #[test]
    fn a_signature_without_a_name_is_called_by_its_place() {
        let sigs = vec![
            NamedSignature {
                name: " Work ".into(),
                html: String::new(),
            },
            NamedSignature {
                name: "  ".into(),
                html: String::new(),
            },
        ];
        assert_eq!(signature_label(&sigs, 0), "Work");
        assert_eq!(signature_label(&sigs, 1), "Signature 2");
        assert_eq!(signature_label(&sigs, 4), "Signature 5");
    }
}
