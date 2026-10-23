//! Tags: their colors, the tag views, and the tag finder.

use super::*;

/// A running tag-finder scan (see `AppMsg::FindTags`).
pub(super) struct TagScan {
    pub(super) gen: u32,
    pub(super) remaining: usize,
    pub(super) found: Vec<(u32, Vec<KeywordFinding>)>,
}

impl AppModel {
    /// With a tag view open, ask the accounts in its scope to bring their
    /// cached keywords in line with the server (#166). An account scanned
    /// within the last few seconds is left alone: reopening a view, or a
    /// refresh right after opening one, must not walk every folder twice.
    pub(super) fn sync_tag_keywords(&mut self) {
        const PAUSE: std::time::Duration = std::time::Duration::from_secs(15);
        let Some((scope, _)) = self.tag_view.clone() else {
            return;
        };
        let keywords: Vec<String> = self.tags.iter().map(|t| t.keyword.clone()).collect();
        if keywords.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let ids: Vec<u32> = self
            .accounts
            .iter()
            .map(|a| a.id)
            .filter(|id| self.in_tag_scope(scope, *id))
            .collect();
        for id in ids {
            if self
                .keyword_sync_at
                .get(&id)
                .is_some_and(|t| now.duration_since(*t) < PAUSE)
            {
                continue;
            }
            self.keyword_sync_at.insert(id, now);
            self.send_to(
                id,
                MailRequest::RefreshKeywords {
                    keywords: keywords.clone(),
                },
            );
        }
    }

    /// Whether a tag view covers an account: its own, or for the unified
    /// Tags row every account merged into the unified section (#267).
    fn in_tag_scope(&self, scope: Option<u32>, account_id: u32) -> bool {
        scope.map_or_else(|| self.in_unified(account_id), |id| id == account_id)
    }

    /// The keywords a tag view answers: one, or — the unified Tags row —
    /// every configured tag.
    fn tag_view_keywords(&self, kw: &Option<String>) -> Vec<String> {
        match kw {
            Some(k) => vec![k.clone()],
            None => self.tags.iter().map(|t| t.keyword.clone()).collect(),
        }
    }

    /// Show the open tag view from its last result at once (an empty
    /// loading list if it never ran); [`refresh_tag_view`] brings it up to
    /// date behind.
    pub(super) fn show_tag_view(&self) {
        let Some(key) = self.tag_view.as_ref() else {
            return;
        };
        match self.tag_view_cache.get(key) {
            Some(cached) => self.message_list.emit(MessageListInput::SetMessages {
                messages: cached.clone(),
            }),
            None => self.message_list.emit(MessageListInput::SetLoading),
        }
    }

    /// Read the open tag view's rows from the on-disk index off the main
    /// thread (reading tens of thousands of rows per account is what made
    /// a tag click pause), answering with [`AppMsg::TagViewLoaded`]. The
    /// demo's mail never reaches the database, so its view is assembled
    /// here from the folders loaded so far.
    pub(super) fn refresh_tag_view(&mut self, sender: &ComponentSender<Self>) {
        let Some(key) = self.tag_view.clone() else {
            return;
        };
        if demo_mode() && self.config.is_empty() {
            let out = self.assemble_tag_view(&key, Vec::new());
            self.tag_view_cache.insert(key, out.clone());
            self.message_list
                .emit(MessageListInput::SetMessages { messages: out });
            return;
        }
        if self.tag_view_loading {
            self.tag_view_dirty = true;
            return;
        }
        self.tag_view_loading = true;
        let (scope, kw) = key.clone();
        // Each keyword with its tag's name, which a Gmail label goes by.
        let keywords: Vec<(String, String)> = self
            .tag_view_keywords(&kw)
            .into_iter()
            .map(|k| {
                let name = self
                    .tags
                    .iter()
                    .find(|t| t.keyword.eq_ignore_ascii_case(&k))
                    .map(|t| t.name.clone());
                let name = name.unwrap_or_else(|| k.clone());
                (k, name)
            })
            .collect();
        let accounts: Vec<u32> = self
            .accounts
            .iter()
            .map(|a| a.id)
            .filter(|id| self.in_tag_scope(scope, *id))
            .collect();
        let s = sender.clone();
        std::thread::spawn(move || {
            let mut rows: Vec<(u32, String, Message)> = Vec::new();
            if let Ok(cache) = crate::cache::Cache::open() {
                for account_id in accounts {
                    for (kw, name) in &keywords {
                        for (path, m) in cache.messages_with_keyword(account_id, kw, name) {
                            rows.push((account_id, path, m));
                        }
                    }
                }
            }
            s.input(AppMsg::TagViewLoaded { key, rows });
        });
    }

    /// The tag view (#71) from its rows: every message of every account in
    /// scope carrying the tag, newest first. Trash and Junk keep their tags
    /// but stay out; Gmail's per-label copies of one message collapse to one
    /// row, the inbox copy where there is one (its actions land where
    /// expected).
    pub(super) fn assemble_tag_view(
        &self,
        key: &(Option<u32>, Option<String>),
        rows: Vec<(u32, String, Message)>,
    ) -> Vec<Message> {
        let (scope, kw) = key;
        let in_scope = |account_id: u32| self.in_tag_scope(*scope, account_id);
        let keywords = self.tag_view_keywords(kw);
        let mut out: Vec<Message> = Vec::new();
        let mut seen_rows: std::collections::HashSet<(u32, u32, u32)> =
            std::collections::HashSet::new();
        for (account_id, path, mut m) in rows {
            let Some(folders) = self.folders.get(&account_id) else {
                continue;
            };
            let Some(f) = folders.iter().find(|f| f.path == path) else {
                continue;
            };
            if matches!(f.kind, FolderKind::Trash | FolderKind::Junk) {
                continue;
            }
            m.folder_id = f.id;
            // A message with several tags answers several keywords.
            if seen_rows.insert((m.account_id, m.folder_id, m.uid)) {
                out.push(m);
            }
        }
        // The demo's mail never reaches the database, so its tag view reads
        // the folders loaded so far instead.
        if out.is_empty() && demo_mode() && self.config.is_empty() {
            for ((account_id, folder_id), messages) in &self.message_cache {
                if !in_scope(*account_id)
                    || matches!(
                        self.folder_kind(*account_id, *folder_id),
                        Some(FolderKind::Trash | FolderKind::Junk)
                    )
                {
                    continue;
                }
                out.extend(
                    messages
                        .iter()
                        .filter(|m| keywords.iter().any(|k| m.has_keyword(k)))
                        .cloned(),
                );
            }
        }
        let inbox_first =
            |m: &Message| self.folder_kind(m.account_id, m.folder_id) != Some(FolderKind::Inbox);
        out.sort_by(|a, b| {
            b.timestamp
                .cmp(&a.timestamp)
                .then_with(|| inbox_first(a).cmp(&inbox_first(b)))
        });
        let mut seen: std::collections::HashSet<(u32, String)> = std::collections::HashSet::new();
        out.retain(|m| {
            m.message_id.is_empty() || seen.insert((m.account_id, m.message_id.clone()))
        });
        out
    }

    /// The tag colors as CSS: `.tag-<keyword>` fills (chips), the same
    /// class on a `.tag-tint` widget colors its glyph instead, and with the
    /// setting on, `.rowtint-tag-<keyword>` washes a message row.
    pub(super) fn refresh_tag_css(&self) {
        let mut css = String::new();
        for t in &self.tags {
            let class = t.css_class();
            let text = crate::color::readable_text(&t.color);
            css.push_str(&format!(
                ".{class} {{ background-color: {color}; color: {text}; }} \
                 .{class} label {{ color: {text}; }} \
                 .tag-tint.{class} {{ color: {color}; background-color: transparent; }}\n",
                color = t.color,
            ));
            // Rows carry their `rowtint-` class whatever the setting; only
            // the rules come and go, so switching it restyles no row by hand.
            if self.tag_row_tint {
                css.push_str(&format!(
                    ".message-list > row:not(:selected) > .message-item.rowtint-{class}:not(.swiping) .message-row {{ \
                       background-color: alpha({color}, 0.18); }} \
                     .message-list > row:hover:not(:selected) > .message-item.rowtint-{class}:not(.swiping) .message-row {{ \
                       background-color: alpha({color}, 0.28); }}\n",
                    color = t.color,
                ));
            }
        }
        // The colors a Gmail label no tag stands for can wear (#374).
        for (hue, color) in crate::config::TAG_COLORS.iter().enumerate() {
            let text = crate::color::readable_text(color);
            css.push_str(&format!(
                ".tag-chip.label-chip.label-hue-{hue} {{ background-color: {color}; color: {text}; }}\n"
            ));
        }
        self.tag_provider.load(css);
    }

    /// The tag finder's scan is over: fold every account's findings into one
    /// list (a keyword seen on two accounts is one entry), drop the keywords
    /// that are tags already, propose a name and color for each, and hand
    /// the report to the Tags page — bringing Settings back if it was closed
    /// meanwhile.
    pub(super) fn finish_tag_scan(&mut self, sender: &ComponentSender<Self>) {
        let Some(scan) = self.tag_scan.take() else {
            return;
        };
        let many = scan.found.len() > 1;
        let mut merged: Vec<(KeywordFinding, Vec<String>)> = Vec::new();
        for (account_id, findings) in scan.found {
            let label = self
                .accounts
                .iter()
                .find(|a| a.id == account_id)
                .map(|a| {
                    if a.name.trim().is_empty() {
                        a.email.clone()
                    } else {
                        a.name.clone()
                    }
                })
                .unwrap_or_default();
            for f in findings {
                if crate::worker::is_system_keyword(&f.keyword)
                    || self
                        .tags
                        .iter()
                        .any(|t| t.keyword.eq_ignore_ascii_case(&f.keyword))
                {
                    continue;
                }
                // One entry per account: its folders, named by the account
                // when more than one took part.
                let places: Vec<String> = if f.folders.is_empty() {
                    Vec::new()
                } else if many {
                    vec![format!("{label}: {}", f.folders.join(", "))]
                } else {
                    vec![f.folders.join(", ")]
                };
                match merged
                    .iter_mut()
                    .find(|(m, _)| m.keyword.eq_ignore_ascii_case(&f.keyword))
                {
                    Some((m, p)) => {
                        m.count += f.count;
                        if m.name.is_none() {
                            m.name = f.name.clone();
                        }
                        if m.color.is_none() {
                            m.color = f.color.clone();
                        }
                        p.extend(places);
                    }
                    None => merged.push((f, places)),
                }
            }
        }
        let mut taken: Vec<String> = self.tags.iter().map(|t| t.color.clone()).collect();
        let proposals: Vec<crate::ui::accounts::TagProposal> = merged
            .into_iter()
            .map(|(f, places)| {
                let name = f
                    .name
                    .clone()
                    .unwrap_or_else(|| config::Tag::name_for_keyword(&f.keyword));
                let color = f
                    .color
                    .clone()
                    .unwrap_or_else(|| config::Tag::color_for_keyword(&f.keyword, &taken));
                taken.push(color.clone());
                crate::ui::accounts::TagProposal {
                    tag: config::Tag {
                        name,
                        keyword: f.keyword,
                        color,
                    },
                    count: f.count,
                    places,
                }
            })
            .collect();
        self.open_settings_window(sender, true, false);
        if let Some(p) = &self.prefs {
            p.emit(PrefInput::ShowPageById("tags".to_string()));
        }
        if let Some(a) = &self.accounts_win {
            a.emit(crate::ui::accounts::AccountsInput::TagFindings(proposals));
        }
    }
}
