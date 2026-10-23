//! The reader header's action buttons: their order, folding into the
//! overflow menu when the window is narrow, and the menu itself.

use super::*;

/// Fallback threshold for folding the reader header's right-hand actions
/// into the overflow menu. Normally the threshold is *measured* at startup from the
/// real row (see the breakpoint in init) so it tracks the user's decoration
/// layout; this value only stands in if that measurement comes back empty.
pub(super) const READER_ACTIONS_BREAKPOINT: f64 = 490.0;

/// The reader header's action buttons and fold breakpoint, kept for
/// re-packing when the layout changes (see relayout_reader_toolbar).
pub(super) struct ReaderToolbarWidgets {
    /// Each action button inside its Focus Mode revealer.
    pub(super) buttons: Vec<(config::ToolbarItem, gtk::Widget)>,
    /// The Outbox's own buttons' revealers (Edit, Send, Send all).
    pub(super) outbox: Vec<gtk::Revealer>,
    pub(super) spinner: gtk::Widget,
    pub(super) bin: adw::BreakpointBin,
    pub(super) breakpoint: adw::Breakpoint,
    /// The row's cost with every action button gone (padding, spacing…).
    pub(super) base: i32,
    /// One action button's natural width.
    pub(super) button_w: i32,
    /// The window controls' width (re-measured when the decoration layout
    /// changes).
    pub(super) controls: std::cell::Cell<i32>,
}

impl AppModel {
    pub(super) fn remote_allowed(&self, m: &Message) -> bool {
        // A choice made for this one message wins over the standing policy,
        // in both directions: it is how remote content is shown (or put back)
        // for a single message when the banner is switched off.
        if let Some(&show) = self.remote_override.get(&(m.account_id, m.id)) {
            return show;
        }
        if self.auto_remote_content {
            return true;
        }
        let addr = m.from_addr.to_lowercase();
        self.allowed_senders.iter().any(|s| *s == addr)
    }

    /// The tag entries of the reader's menus (#71): one per tag, its swatch
    /// filled where the reader's target message carries it. None without a
    /// target or a tag.
    pub(super) fn reader_tag_entries(
        &self,
        sender: &ComponentSender<Self>,
    ) -> Option<Vec<crate::ui::context_menu::MenuEntry>> {
        if self.tags.is_empty() {
            return None;
        }
        // Several selected (#313): a tag is ticked when every one carries
        // it, since only then does choosing it take it off.
        let many = self.multi_selection();
        let target = match many.first() {
            Some(first) => {
                let mut all = first.clone();
                all.keywords = first.tag_keywords(&self.tags);
                all.keywords
                    .retain(|k| many.iter().all(|m| self.carries_tag(m, k)));
                all
            }
            None => self.reply_target()?,
        };
        let s = sender.input_sender().clone();
        Some(crate::ui::message_list::tag_menu_entries(
            &self.tags,
            &target,
            move |keyword, _add| {
                let _ = s.send(AppMsg::ToggleTagCurrent(keyword));
            },
        ))
    }

    /// The collapsed reader header's overflow menu: the right group of the
    /// toolbar (Tags, Read/Unread, Spam, Move To, Find, Print), same icons,
    /// enabled under the same conditions. The left group never folds.
    pub(super) fn show_reader_overflow_menu(&self, sender: &ComponentSender<Self>) {
        use crate::ui::context_menu::{show_context_menu, MenuEntry};

        // One entry per action, each with its own clone of the input sender.
        macro_rules! entry {
            ($label:expr, $icon:expr, $msg:expr, $enabled:expr) => {{
                let s = sender.input_sender().clone();
                MenuEntry::new($label, move || {
                    let _ = s.send($msg);
                })
                .icon(concat!($icon, "-symbolic"))
                .enabled($enabled)
            }};
        }

        let has_current = self.current.is_some();
        let focus = self.focus.active(config::FocusPart::ReaderToolbar);
        let sections = if self.showing_outbox {
            // The Outbox's own buttons (Edit, Send, Send all, Delete) sit in
            // the always-visible left group; only View Source is left to
            // fold here (queued rows have no context menu to carry it) —
            // unless Focus Mode folded that group too.
            let mut sections = Vec::new();
            if focus {
                let many = has_current || self.many_selected();
                sections.push(vec![
                    entry!(
                        i18n("Edit this message"),
                        "document-edit",
                        AppMsg::EditCurrentOutbox,
                        has_current
                    ),
                    entry!(
                        i18n("Try to send this message now"),
                        "mail-send",
                        AppMsg::SendCurrentOutbox,
                        has_current
                    ),
                    entry!(i18n("Send all"), "mail-send", AppMsg::RetryAllOutbox, true),
                    entry!(i18n("Delete"), "user-trash", AppMsg::Delete, many),
                ]);
            }
            sections.push(vec![entry!(
                i18n("View Source"),
                "code",
                AppMsg::ViewSource,
                has_current
            )]);
            sections
        } else {
            // Only the right group folds in here, in its own order; the left
            // group stays on the bar at every width. Per-message actions act
            // on the reply target: the open message, or — in a conversation —
            // the one highlighted card. With none (or several) highlighted
            // they grey out.
            use config::ToolbarItem as T;
            let target = self.reply_target();
            let acts = target.is_some();
            let many = acts || self.many_selected();
            let starred = target.as_ref().is_some_and(|m| m.starred);
            let target_unread = target.as_ref().is_some_and(|m| m.unread);
            let restorable = target.as_ref().is_some_and(|m| {
                self.folder_kind(m.account_id, m.folder_id)
                    .is_some_and(|k| matches!(k, FolderKind::Trash | FolderKind::Junk))
            });
            // Focus Mode folds the left group in as well, as a section of
            // its own ahead of the right group.
            let groups: Vec<&[T]> = if focus {
                vec![&self.reader_toolbar.left, &self.reader_toolbar.right]
            } else {
                vec![&self.reader_toolbar.right]
            };
            let mut sections = Vec::new();
            for group in groups {
                let mut section = Vec::new();
                for item in group {
                    match item {
                        T::Reply => section.push(entry!(
                            i18n("Reply"),
                            "mail-reply-sender",
                            AppMsg::Reply,
                            acts
                        )),
                        T::ReplyAll => section.push(entry!(
                            i18n("Reply All"),
                            "mail-reply-all",
                            AppMsg::ReplyAll,
                            acts
                        )),
                        T::Forward => section.push(entry!(
                            i18n("Forward"),
                            "mail-forward",
                            AppMsg::Forward,
                            acts
                        )),
                        T::Star => section.push(if starred {
                            entry!(
                                i18n("Remove Star"),
                                "hylki-non-starred",
                                AppMsg::ToggleStar,
                                many
                            )
                        } else {
                            entry!(i18n("Star"), "starred", AppMsg::ToggleStar, many)
                        }),
                        T::Archive => section.push(entry!(
                            i18n("Archive"),
                            "mail-archive",
                            AppMsg::Archive,
                            many
                        )),
                        T::Delete => {
                            section.push(entry!(i18n("Delete"), "user-trash", AppMsg::Delete, many))
                        }
                        T::Spam => section.push(if self.target_in_junk() {
                            entry!(
                                i18n("Not Spam"),
                                "mail-mark-notjunk",
                                AppMsg::MarkSpam,
                                many
                            )
                        } else {
                            entry!(
                                i18n("Mark as Spam"),
                                "mail-mark-junk",
                                AppMsg::MarkSpam,
                                many
                            )
                        }),
                        T::ReadUnread => section.push(if target_unread {
                            entry!(
                                i18n("Mark as Read"),
                                "hylki-mail-read",
                                AppMsg::ToggleReadCurrent,
                                many
                            )
                        } else {
                            entry!(
                                i18n("Mark as Unread"),
                                "mail-unread",
                                AppMsg::ToggleReadCurrent,
                                many
                            )
                        }),
                        // Tags (#71), where there are any and something to tag —
                        // behind a submenu, as in the message list's menu.
                        T::Tags => {
                            if let Some(entries) = self.reader_tag_entries(sender) {
                                section.push(
                                    MenuEntry::submenu(i18n("Tags"), vec![entries])
                                        .icon("tag-outline-symbolic"),
                                );
                            }
                        }
                        T::MoveTo => {
                            if restorable {
                                section.push(entry!(
                                    i18n("Move to Inbox"),
                                    "mail-inbox",
                                    AppMsg::MoveToInbox,
                                    acts
                                ));
                            }
                            section.push(entry!(
                                i18n("Move To…"),
                                "folder",
                                AppMsg::MoveToMenu,
                                many
                            ));
                        }
                        T::Find => section.push(entry!(
                            i18n("Find in Message"),
                            "loupe-with-arrow",
                            AppMsg::OpenReaderFind,
                            has_current
                        )),
                        // View Source is deliberately absent: it lives in the
                        // message list's context menu only (the Outbox variant
                        // above keeps it — queued rows have no such menu).
                        T::Print => section.push(entry!(
                            i18n("Print Preview"),
                            "printer",
                            AppMsg::PrintPreview,
                            has_current
                        )),
                    }
                }
                if !section.is_empty() {
                    sections.push(section);
                }
            }
            sections
        };

        let btn = &self.reader_overflow_btn;
        show_context_menu(btn, (btn.width() / 2) as f64, btn.height() as f64, sections);
    }

    /// Whether the reader header shows `item` right now: it must be in the
    /// saved layout, and a right-group button also needs the pane wide
    /// enough (the left group never folds). Delete alone also serves the
    /// Outbox; the inline composer hides the whole row.
    pub(super) fn toolbar_visible(&self, item: config::ToolbarItem) -> bool {
        use config::ToolbarSide;
        self.reader_compose.is_none()
            && (item == config::ToolbarItem::Delete || !self.showing_outbox)
            && match self.reader_toolbar.side(item) {
                Some(ToolbarSide::Left) => true,
                Some(ToolbarSide::Right) => !self.reader_actions_collapsed,
                None => false,
            }
    }

    /// The ⋯ overflow stands in for the folded right group: only while
    /// collapsed, and only when that group has something to offer (the
    /// Outbox keeps View Source there regardless).
    pub(super) fn reader_overflow_wanted(&self) -> bool {
        // Focus Mode folds the whole row in, so the ⋯ is the toolbar.
        if self.focus.active(config::FocusPart::ReaderToolbar) {
            return true;
        }
        self.reader_actions_collapsed
            && (self.showing_outbox || !self.reader_toolbar.right.is_empty())
    }

    /// Pack the header's action buttons in the saved order: the left group
    /// via pack_start, the right group via pack_end (which fills right to
    /// left, so in reverse), then the attachments spinner so it keeps the
    /// innermost slot of the right group. Buttons on neither side are left
    /// unpacked. Re-folds the row for the new count afterwards.
    pub(super) fn relayout_reader_toolbar(&self) {
        let (Some(header), Some(tb)) =
            (self.reader_header.get(), self.reader_toolbar_widgets.get())
        else {
            return;
        };
        for (_, w) in &tb.buttons {
            if w.parent().is_some() {
                header.remove(w);
            }
        }
        if tb.spinner.parent().is_some() {
            header.remove(&tb.spinner);
        }
        let find =
            |item: config::ToolbarItem| tb.buttons.iter().find(|(i, _)| *i == item).map(|(_, w)| w);
        // Each button's Focus Mode revealer slides towards the edge its
        // group sits against: GTK's SlideRight moves a folding child off to
        // the left, SlideLeft keeps it anchored and shrinks it from its
        // right, which is what an end-packed group needs.
        for item in &self.reader_toolbar.left {
            if let Some(w) = find(*item) {
                if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                    r.set_transition_type(gtk::RevealerTransitionType::SlideRight);
                }
                header.pack_start(w);
            }
        }
        for item in self.reader_toolbar.right.iter().rev() {
            if let Some(w) = find(*item) {
                if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                    r.set_transition_type(gtk::RevealerTransitionType::SlideLeft);
                }
                header.pack_end(w);
            }
        }
        header.pack_end(&tb.spinner);
        self.refresh_reader_breakpoint();
    }

    /// Point the fold breakpoint at the width the current layout needs: the
    /// row's fixed cost, the window controls, and one button's worth per
    /// shown item (hidden-for-now buttons like Tags without tags count too —
    /// that slack keeps the fold a step ahead of a squeeze).
    pub(super) fn refresh_reader_breakpoint(&self) {
        let Some(tb) = self.reader_toolbar_widgets.get() else {
            return;
        };
        let shown = (self.reader_toolbar.left.len() + self.reader_toolbar.right.len()) as i32;
        let threshold = if tb.button_w <= 0 {
            READER_ACTIONS_BREAKPOINT
        } else {
            (tb.base + tb.controls.get() + shown * tb.button_w) as f64 + 24.0
        };
        tracing::info!(
            "reader toolbar: {shown} buttons × {}px + base {}px + controls {}px → collapse below {threshold:.0}px",
            tb.button_w,
            tb.base,
            tb.controls.get()
        );
        tb.breakpoint
            .set_condition(Some(&adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MaxWidth,
                threshold,
                adw::LengthUnit::Px,
            )));
        tb.bin.queue_resize();
    }

    /// Tooltip for the toolbar's trash button: says when it will delete the
    /// whole multi-selection rather than just the open message.
    pub(super) fn delete_tooltip(&self) -> String {
        match self.list_selection.len() {
            n if n > 1 => i18n_f("Delete {n} messages", &[("n", &n.to_string())]),
            _ => i18n("Delete"),
        }
    }
}
