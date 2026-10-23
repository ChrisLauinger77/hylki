//! Address completion: the suggestion list, its ranking, and the popover under
//! a recipient field.

use super::messages::{ComposeInput, ComposeOutput};
use super::types::Field;
use super::Compose;
use super::ComposeWidgets;
use crate::contacts::Suggestion;
use crate::ui::recipient_row::RecipientRow;
use adw::prelude::*;
use relm4::prelude::*;

/// Replace the recipient being typed with the chosen suggestion, as a pill.
/// A group puts in its members, less those the field already has; one that
/// hides its addresses puts them in Bcc, whichever field it was typed in.
pub(super) fn complete_field(row: &RecipientRow, bcc: &RecipientRow, sug: &Suggestion) {
    let Some(group) = &sug.group else {
        row.complete(&sug.display());
        return;
    };
    let target = if group.hide_addresses { bcc } else { row };
    let present: std::collections::HashSet<String> =
        crate::worker::parse_recipients(&target.text())
            .into_iter()
            .map(|(_, email)| email.to_lowercase())
            .collect();
    let members = group
        .members
        .iter()
        .filter(|m| !present.contains(&m.email.to_lowercase()))
        .map(|m| crate::worker::format_recipient(&m.name, &m.email))
        .collect::<Vec<_>>()
        .join(", ");
    if target == row {
        row.complete(&members);
    } else {
        row.complete("");
        bcc.set_visible(true);
        bcc.add_recipient(&members);
    }
}

/// Keep addresses sent from another composer while the initial lookup was
/// running. The loaded contact metadata (including groups and own-address
/// ranking) wins; addresses added during the lookup must not disappear.
pub(super) fn merge_loaded_suggestions(current: &mut Vec<Suggestion>, mut loaded: Vec<Suggestion>) {
    for recent in current.drain(..) {
        if let Some(s) = loaded
            .iter_mut()
            .find(|s| s.group.is_none() && s.email.eq_ignore_ascii_case(&recent.email))
        {
            // The snapshot may already include that send: do not count it twice.
            s.score = s.score.max(recent.score);
            if s.name.trim().is_empty() || s.name == s.email {
                s.name = recent.name;
            }
        } else {
            loaded.push(recent);
        }
    }
    *current = loaded;
}

/// The recipient being typed, after the field's pills.
pub(super) fn recipient_token(row: &RecipientRow) -> String {
    row.typed()
}

/// Whether the field has the keyboard. The row holds its focus in the
/// GtkText inside it, so the row's own has_focus() is never true.
pub(super) fn recipient_focused(row: &RecipientRow) -> bool {
    row.root()
        .and_downcast::<gtk::Window>()
        .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w))
        .is_some_and(|focus| focus == *row.upcast_ref::<gtk::Widget>() || focus.is_ancestor(row))
}

impl Compose {
    /// The addresses in the From account's mail, when that account
    /// suggests only from its directory and its own mail (#410).
    fn narrowed(&self) -> Option<&std::collections::HashSet<String>> {
        self.own_mail
            .get(&self.from_account)
            .map(|own| &own.addresses)
    }

    /// Suggestions matching `token`, ranked best-first (prefix match, then most
    /// frequently used), capped to a handful.
    pub(super) fn ranked_matches(&self, token: &str) -> Vec<Suggestion> {
        let narrowed = self.narrowed();
        let mut matches: Vec<Suggestion> = self
            .suggestions
            .iter()
            .filter(|s| {
                s.matches(token) && narrowed.is_none_or(|own| own.contains(&s.email.to_lowercase()))
            })
            .cloned()
            .collect();
        // A directory matched on attributes the entry may not show (a
        // surname under a display name), so its answers are not filtered
        // again; one already known from Contacts or mail is not repeated,
        // but is marked as in the directory (#307).
        let in_directory: std::collections::HashSet<String> = self
            .directory_matches
            .iter()
            .map(|s| s.email.to_lowercase())
            .collect();
        for s in &mut matches {
            s.from_directory |= in_directory.contains(&s.email.to_lowercase());
        }
        let known: std::collections::HashSet<String> =
            matches.iter().map(|s| s.email.to_lowercase()).collect();
        let mut seen = std::collections::HashSet::new();
        matches.extend(
            self.directory_matches
                .iter()
                .filter(|s| {
                    !known.contains(&s.email.to_lowercase()) && seen.insert(s.email.to_lowercase())
                })
                .cloned(),
        );
        crate::contacts::rank(&mut matches, token);
        matches.truncate(8);
        matches
    }

    /// Filter suggestions by the recipient fragment being typed and show them in
    /// a popover under the field; clicking one completes that recipient.
    pub(super) fn show_completion(&mut self, field: Field, row: &RecipientRow, bcc: &RecipientRow) {
        let token = row.typed();
        if token.is_empty() {
            self.completion_open.set(false);
            self.completion.popdown();
            return;
        }
        let matches = self.ranked_matches(&token);
        if matches.is_empty() {
            self.completion_open.set(false);
            self.completion.popdown();
            return;
        }

        // Attach the popover to the active field (only re-parent when it moves).
        if self.completion_field != Some(field) {
            if self.completion.parent().is_some() {
                self.completion.unparent();
            }
            self.completion.set_parent(row);
            self.completion_field = Some(field);
        }

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        // Keep focus in the entry so its key controller drives navigation.
        list.set_can_focus(false);
        list.add_css_class("autocomplete");
        let count = matches.len();
        for sug in matches {
            let item = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            item.set_margin_start(6);
            item.set_margin_end(6);
            item.set_margin_top(3);
            item.set_margin_bottom(3);
            // Mark where the suggestion came from: a directory, the address
            // book, or mail history.
            let icon = gtk::Image::from_icon_name(if sug.group.is_some() {
                "system-users-symbolic"
            } else if sug.from_directory {
                "x-office-address-book-symbolic"
            } else if sug.from_contacts {
                "avatar-default-symbolic"
            } else {
                "document-open-recent-symbolic"
            });
            icon.set_valign(gtk::Align::Center);
            icon.add_css_class("dim-label");
            item.append(&icon);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let title = gtk::Label::new(Some(&sug.name));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            text.append(&title);
            // A group says how many it reaches, and whether in Bcc.
            let subtitle = match &sug.group {
                Some(g) => crate::contacts::group_reach(g.members.len(), g.hide_addresses),
                None => sug.email.clone(),
            };
            if sug.name != subtitle {
                let sub = gtk::Label::new(Some(&subtitle));
                sub.set_halign(gtk::Align::Start);
                sub.set_xalign(0.0);
                sub.add_css_class("dim-label");
                sub.add_css_class("caption");
                text.append(&sub);
            }
            item.append(&text);
            let lbr = gtk::ListBoxRow::new();
            lbr.set_can_focus(false);
            lbr.set_child(Some(&item));
            list.append(&lbr);

            // Complete this recipient on click.
            let row2 = row.clone();
            let bcc = bcc.clone();
            let pop = self.completion.downgrade();
            let sug = sug.clone();
            let gesture = gtk::GestureClick::new();
            gesture.connect_released(move |_, _, _, _| {
                complete_field(&row2, &bcc, &sug);
                if let Some(p) = pop.upgrade() {
                    p.popdown();
                }
            });
            lbr.add_controller(gesture);
        }

        // Highlight the first suggestion so Enter accepts it immediately.
        if let Some(first) = list.row_at_index(0) {
            list.select_row(Some(&first));
        }
        self.completion_selected = 0;
        self.completion_count = count;
        self.completion_list = Some(list.clone());

        self.completion.set_child(Some(&list));
        self.completion.popup();
        self.completion_open.set(true);
    }
}

impl Compose {
    /// Handles [`ComposeInput::OpenContacts`].
    pub(super) fn on_open_contacts(
        &mut self,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
        root: &adw::BreakpointBin,
    ) {
        // Browse contacts; the chosen one is appended to the To field.
        let Some(win) = root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let to_row = widgets.to_row.clone();
        let s = sender.clone();
        crate::ui::contacts_browser::present(
            &win,
            move |contact| {
                to_row.add_recipient(&crate::worker::format_recipient(
                    &contact.name,
                    &contact.email,
                ));
            },
            move || {
                let _ = s.output(ComposeOutput::ShowContacts);
            },
        );
    }

    /// Handles [`ComposeInput::Suggest`].
    pub(super) fn on_suggest(
        &mut self,
        field: Field,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let row = match field {
            Field::To => &widgets.to_row,
            Field::Cc => &widgets.cc_row,
            Field::Bcc => &widgets.bcc_row,
        };
        self.directory_generation = self.directory_generation.wrapping_add(1);
        self.directory_matches.clear();
        let token = recipient_token(row);
        if token.chars().count() >= crate::directory::MIN_QUERY {
            // A directory is asked once the typing pauses, not per key.
            let generation = self.directory_generation;
            let s = sender.input_sender().clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                let _ = s.send(ComposeInput::DirectoryLookup {
                    field,
                    token,
                    generation,
                });
            });
        }
        self.show_completion(field, row, &widgets.bcc_row);
    }

    /// Handles [`ComposeInput::SuggestionsLoaded`].
    pub(super) fn on_suggestions_loaded(
        &mut self,
        loaded: Vec<Suggestion>,
        widgets: &ComposeWidgets,
    ) {
        tracing::debug!("composer suggestions loaded: {}", loaded.len());
        merge_loaded_suggestions(&mut self.suggestions, loaded);
        // The user may already be typing. Update that field's
        // matches without touching its text, caret or dirty state.
        for (field, row) in [
            (Field::To, &widgets.to_row),
            (Field::Cc, &widgets.cc_row),
            (Field::Bcc, &widgets.bcc_row),
        ] {
            if recipient_focused(row) {
                self.show_completion(field, row, &widgets.bcc_row);
                break;
            }
        }
    }

    /// Handles [`ComposeInput::DirectoryLookup`].
    pub(super) fn on_directory_lookup(
        &mut self,
        field: Field,
        token: String,
        generation: u64,
        sender: &ComponentSender<Self>,
    ) {
        if generation == self.directory_generation {
            let s = sender.input_sender().clone();
            // An account that suggests only its own asks its
            // directory alone, no LDAP and no other tenant.
            let own = self.own_mail.get(&self.from_account);
            let narrowed = own.is_some();
            if let Some(own) = own {
                tracing::debug!(
                    target: "hylki::graph",
                    "suggestions narrowed to {}: its directory and {} addresses from its mail",
                    own.account.email,
                    own.addresses.len()
                );
            }
            let graph = match own {
                Some(own) => std::sync::Arc::new(vec![own.account.clone()]),
                None => self.graph_directories.clone(),
            };
            std::thread::spawn(move || {
                // Both at once: the slower decides when the list
                // grows, not the two one after the other.
                let (matches, failed) = std::thread::scope(|scope| {
                    let org = (!graph.is_empty())
                        .then(|| scope.spawn(|| crate::graph_directory::search(&token, &graph)));
                    let (mut matches, failed) = if narrowed {
                        Default::default()
                    } else {
                        crate::directory::search(&token)
                    };
                    if let Some(found) = org.and_then(|h| h.join().ok()) {
                        matches.extend(found);
                    }
                    (matches, failed)
                });
                let _ = s.send(ComposeInput::DirectoryResults {
                    field,
                    token,
                    generation,
                    matches,
                    failed,
                });
            });
        }
    }

    /// A directory that cannot be asked says so once, whatever came of the
    /// typing meanwhile.
    pub(super) fn report_directory_failures(
        &mut self,
        failed: Vec<crate::directory::DirectoryFailure>,
        sender: &ComponentSender<Self>,
    ) {
        for f in failed {
            let text = f.text();
            if self.directory_errors_shown.insert(text.clone()) {
                let _ = sender.output(ComposeOutput::DirectoryError(text));
            }
        }
    }

    /// Handles [`ComposeInput::DirectoryResults`].
    pub(super) fn on_directory_results(
        &mut self,
        field: Field,
        token: String,
        generation: u64,
        matches: Vec<Suggestion>,
        widgets: &ComposeWidgets,
    ) {
        let row = match field {
            Field::To => &widgets.to_row,
            Field::Cc => &widgets.cc_row,
            Field::Bcc => &widgets.bcc_row,
        };
        if generation == self.directory_generation
            && recipient_token(row) == token
            && !matches.is_empty()
        {
            self.directory_matches = matches;
            if recipient_focused(row) {
                self.show_completion(field, row, &widgets.bcc_row);
            }
        }
    }

    /// Handles [`ComposeInput::AddSuggestions`].
    pub(super) fn on_add_suggestions(&mut self, account_id: u32, new: Vec<Suggestion>) {
        for n in new {
            let key = n.email.to_lowercase();
            if let Some(own) = self.own_mail.get_mut(&account_id) {
                own.addresses.insert(key.clone());
            }
            match self
                .suggestions
                .iter_mut()
                .find(|s| s.email.to_lowercase() == key)
            {
                Some(s) => {
                    s.score += 1;
                    if s.name.trim().is_empty() || s.name == s.email {
                        s.name = n.name;
                    }
                }
                None => self.suggestions.push(n),
            }
        }
    }

    /// Handles [`ComposeInput::CompletionMove`].
    pub(super) fn on_completion_move(&mut self, delta: i32) {
        if self.completion_count == 0 {
            return;
        }
        let max = self.completion_count as i32 - 1;
        let new = (self.completion_selected as i32 + delta).clamp(0, max) as usize;
        self.completion_selected = new;
        if let Some(list) = &self.completion_list {
            if let Some(row) = list.row_at_index(new as i32) {
                list.select_row(Some(&row));
            }
        }
    }

    /// Handles [`ComposeInput::CompletionAccept`].
    pub(super) fn on_completion_accept(&mut self, widgets: &ComposeWidgets) {
        let row = match self.completion_field {
            Some(Field::To) => &widgets.to_row,
            Some(Field::Cc) => &widgets.cc_row,
            Some(Field::Bcc) => &widgets.bcc_row,
            None => return,
        };
        let token = row.typed();
        if !token.is_empty() {
            let chosen = self
                .ranked_matches(&token)
                .into_iter()
                .nth(self.completion_selected);
            if let Some(sug) = chosen {
                complete_field(row, &widgets.bcc_row, &sug);
            }
        }
        self.completion_open.set(false);
        self.completion.popdown();
    }
}
