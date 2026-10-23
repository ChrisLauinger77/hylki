//! Rebuilding the whole sidebar from its data (`rebuild_normal`), and
//! restyling the unread counts in place.

use super::dnd::{attach_folder_list_drop, folder_drag_source, folder_drop_target, with_drop_line};
use super::menus::{attach_folder_context_menu, folder_sort_menu, show_sidebar_menu};
use super::tree::{hidden_by_collapse, is_main_row, nests_in_inbox, path_is_under};
use super::types::UnifiedRow;
use super::widgets::{
    account_gravatar, build_folder_row, filter_icon, heading_chevron, section_heading, style_badge,
    with_unread_overlay, ROW_LEFT_INSET,
};
use super::*;

impl Sidebar {
    /// Rebuild the list: optional unified row, then per-account headers with
    /// animated folder revealers, and refresh the per-account color rules.
    pub(super) fn rebuild_normal(
        &mut self,
        pinned: &gtk::Box,
        container: &gtk::Box,
        footer: &gtk::Box,
        sender: &ComponentSender<Self>,
    ) {
        use crate::config::SectionPlacement::{AboveAccounts, BelowAccounts};
        // A list about to go keeps no keyboard focus: GTK would hand it to
        // the first list left, All Inboxes, and a list selects the row it
        // focuses, opening All Inboxes over the folder on screen. A folder's
        // menu (Add to Favorites, #367) rebuilds right under its own list.
        if let Some(root) = container.root() {
            let doomed = root.focus().is_some_and(|f| {
                [pinned, container, footer]
                    .iter()
                    .any(|b| f.is_ancestor(*b))
            });
            if doomed {
                root.set_focus(None::<&gtk::Widget>);
            }
        }
        // Keep the scroll offset: rebuilding otherwise snaps the sidebar to
        // the top — felt on every folder drag-and-drop, whose optimistic move
        // rebuilds immediately under the pointer.
        let scroller = container
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>();
        let saved_scroll = scroller.as_ref().map(|s| s.vadjustment().value());
        // Freeze the sidebar's last-rendered pixels over the swap: even with
        // the scroll pinned, tearing down and recreating every row can
        // shimmer for a frame. The snapshot covers the rebuild and lifts a
        // few frames later, once the fresh tree has painted beneath it —
        // identical pixels, so the crossover is invisible. The snapshot is of
        // the overlay's whole child (pinned block + scroller + footer),
        // matching the area the freeze-frame Picture stretches over.
        //
        // A toggle between the full sidebar and the icon rail is different:
        // the pane's width animates underneath the snapshot for 200ms (the
        // app's `animate_sidebar`), so the snapshot keeps the width it was
        // taken at (never stretched to the moving pane — that was a visible
        // smear of every icon) and fades out over the same 200ms while the
        // fresh rows, centred in the pane as it moves, come through beneath.
        let rail_toggle = self.built_collapsed != self.collapsed;
        self.built_collapsed = self.collapsed;
        if let (Some(freeze), Some(area)) = (self.freeze_frame.clone(), pinned.parent()) {
            if container.first_child().is_some() || pinned.first_child().is_some() {
                use gtk::gdk::prelude::PaintableExt;
                // Take before skipping: the fade's done handler clears the
                // same slot, so the borrow must be over by then.
                let fading = self.freeze_fade.borrow_mut().take();
                if let Some(prev) = fading {
                    prev.skip();
                }
                let pending = self.freeze_timer.borrow_mut().take();
                if let Some(prev) = pending {
                    prev.remove();
                }
                let live = gtk::WidgetPaintable::new(Some(&area));
                freeze.set_paintable(Some(&live.current_image()));
                freeze.set_size_request(area.width().max(1), -1);
                freeze.set_opacity(1.0);
                freeze.set_visible(true);
                if rail_toggle {
                    let target = adw::CallbackAnimationTarget::new({
                        let freeze = freeze.clone();
                        move |v| freeze.set_opacity(v)
                    });
                    let anim = adw::TimedAnimation::new(&freeze, 1.0, 0.0, 200, target);
                    anim.set_easing(adw::Easing::EaseOutCubic);
                    anim.connect_done({
                        let freeze = freeze.clone();
                        let slot = self.freeze_fade.clone();
                        move |_| {
                            slot.borrow_mut().take();
                            freeze.set_visible(false);
                            freeze.set_opacity(1.0);
                        }
                    });
                    *self.freeze_fade.borrow_mut() = Some(anim.clone());
                    anim.play();
                } else {
                    let timer =
                        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(80), {
                            let freeze = freeze.clone();
                            let slot = self.freeze_timer.clone();
                            move || {
                                slot.borrow_mut().take();
                                freeze.set_visible(false);
                            }
                        });
                    *self.freeze_timer.borrow_mut() = Some(timer);
                }
            }
        }
        while let Some(child) = pinned.first_child() {
            pinned.remove(&child);
        }
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }
        while let Some(child) = footer.first_child() {
            footer.remove(&child);
        }
        if self.collapsed {
            pinned.add_css_class("rail-collapsed");
            container.add_css_class("rail-collapsed");
            footer.add_css_class("rail-collapsed");
        } else {
            pinned.remove_css_class("rail-collapsed");
            container.remove_css_class("rail-collapsed");
            footer.remove_css_class("rail-collapsed");
        }
        // Unread dots in place of counts (Settings → Sidebar → Icon rail):
        // a style on the rail's containers, so every mini chip built below
        // shrinks to a dot without each builder knowing.
        for w in [pinned, container, footer] {
            if self.collapsed && self.rail_dots {
                w.add_css_class("rail-dots");
            } else {
                w.remove_css_class("rail-dots");
            }
        }
        self.revealers.clear();
        self.chevrons.clear();
        self.folder_lists.clear();
        self.custom_folder_lists.clear();
        self.custom_revealers.clear();
        self.custom_chevrons.clear();
        self.unified_list = None;
        self.footer_list = None;
        self.attachments_row = None;
        self.contacts_row = None;
        self.outbox_list = None;
        self.folder_badges.clear();
        self.tree_chevrons.clear();
        self.tree_row_revealers.clear();
        self.main_folders.clear();
        self.main_row_revealers.clear();
        self.unified_badge = None;
        self.unified_revealer = None;
        self.unified_chevron = None;
        self.unified_inbox_list = None;
        self.unified_inboxes.clear();
        self.unified_inbox_badges.clear();
        self.filtered_badges.clear();
        self.account_circle_badges.clear();
        self.tag_sections.clear();
        self.kind_widgets.clear();

        // No accounts yet: show a prompt to add the first one instead of an empty
        // sidebar (the app is blank in this state).
        if self.sections.is_empty() {
            let s = sender.clone();
            let add = gtk::Button::new();
            add.add_css_class("suggested-action");
            add.add_css_class("pill");
            add.set_valign(gtk::Align::Center);
            add.set_halign(gtk::Align::Center);
            add.connect_clicked(move |_| {
                let _ = s.output(SidebarOutput::AddAccount);
            });
            if self.collapsed {
                add.set_icon_name("list-add-symbolic");
                add.set_tooltip_text(Some(i18n("Add account").as_str()));
                add.set_margin_top(12);
                container.append(&add);
            } else {
                let label_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                label_box.append(&gtk::Image::from_icon_name("list-add-symbolic"));

                let add_label = gtk::Label::new(Some(i18n("Add first account").as_str()));
                add_label.set_wrap(true);
                add_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                add_label.set_justify(gtk::Justification::Center);
                add_label.set_xalign(0.5);
                add_label.set_hexpand(true);
                label_box.append(&add_label);

                add.set_halign(gtk::Align::Fill);
                add.set_child(Some(&label_box));
                let empty = gtk::Box::new(gtk::Orientation::Vertical, 12);
                empty.set_valign(gtk::Align::Start);
                empty.set_margin_top(36);
                empty.set_margin_start(16);
                empty.set_margin_end(16);
                let hint = gtk::Label::new(Some(i18n("No accounts yet").as_str()));
                hint.add_css_class("dim-label");
                empty.append(&hint);
                empty.append(&add);
                container.append(&empty);
            }
            return;
        }

        let sections = self.sections.clone();

        // "New message" — the compose action. Expanded, the pill sits alone
        // and centred (Refresh lives in the app's header bar, top-left across
        // from the menu). The collapsed rail's header only has room for the
        // menu button, so Refresh stacks here instead — directly below it.
        {
            let bar = gtk::Box::new(
                if self.collapsed {
                    gtk::Orientation::Vertical
                } else {
                    gtk::Orientation::Horizontal
                },
                0,
            );

            self.sync_stack = None;
            self.sync_spinner = None;
            self.sync_button = None;
            if self.collapsed {
                // Refresh, showing a spinner while any account syncs.
                let refresh = gtk::Button::new();
                refresh
                    .set_tooltip_text(Some(i18n("Refresh or long-press for Status Bar").as_str()));
                refresh.add_css_class("flat");
                refresh.set_valign(gtk::Align::Center);
                refresh.set_halign(gtk::Align::Center);
                let stack = gtk::Stack::new();
                stack.set_transition_type(gtk::StackTransitionType::Crossfade);
                let icon = gtk::Image::from_icon_name("view-refresh-symbolic");
                stack.add_named(&icon, Some("icon"));
                let spinner = gtk::Spinner::new();
                spinner.set_spinning(self.busy);
                stack.add_named(&spinner, Some("spinner"));
                stack.set_visible_child_name(if self.busy { "spinner" } else { "icon" });
                refresh.set_child(Some(&stack));
                let s = sender.clone();
                refresh.connect_clicked(move |_| {
                    let _ = s.output(SidebarOutput::RefreshRequested);
                });
                // Long-press reveals the status bar (same as the header
                // refresh); claiming the sequence suppresses the click.
                let long = gtk::GestureLongPress::new();
                let s = sender.clone();
                long.connect_pressed(move |gesture, _, _| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let _ = s.output(SidebarOutput::StatusBarRequested);
                });
                refresh.add_controller(long);
                crate::ui::show_refresh_offline(&refresh, self.offline);
                bar.append(&refresh);
                self.sync_button = Some(refresh);
                self.sync_stack = Some(stack);
                self.sync_spinner = Some(spinner);
            }

            // The compose button, drawn like a row so it matches the
            // sidebar's look. Expanded: full width like the rows below, icon
            // and label centred, and a caret at the end, as the composer's
            // Send has for Send Later, opening the menu a right-click opens
            // (#360). The collapsed rail shows the icon alone, its menu on a
            // right-click only.
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::None);
            list.add_css_class("navigation-sidebar");
            let row = gtk::ListBoxRow::new();
            row.add_css_class("compose-row");
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("folder-row");
            let menu_sender = sender.clone();
            let compose_menu = move |widget: &gtk::Widget, x: f64, y: f64| {
                let (blank, from_template) = (menu_sender.clone(), menu_sender.clone());
                show_context_menu(
                    widget,
                    x,
                    y,
                    vec![vec![
                        MenuEntry::new(i18n("New Message"), move || {
                            let _ = blank.output(SidebarOutput::ComposeRequested);
                        })
                        .icon("mail-message-new-symbolic"),
                        MenuEntry::new(i18n("New Message from Template…"), move || {
                            let _ =
                                from_template.output(SidebarOutput::ComposeFromTemplateRequested);
                        })
                        .icon("folder-templates-symbolic"),
                    ]],
                );
            };
            if self.collapsed {
                // The rail has no room for a label; the icon carries it there.
                let img = gtk::Image::from_icon_name("mail-message-new-symbolic");
                img.add_css_class("folder-icon");
                pin_icon_size(&img);
                hbox.set_halign(gtk::Align::Center);
                row.set_tooltip_text(Some(i18n("New Message").as_str()));
                hbox.append(&img);
            } else {
                hbox.set_spacing(0);
                hbox.add_css_class("compose-split");
                let main = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                main.set_hexpand(true);
                main.set_halign(gtk::Align::Center);
                let icon = gtk::Image::from_icon_name("mail-message-new-symbolic");
                icon.add_css_class("folder-icon");
                main.append(&icon);
                let label = gtk::Label::new(Some(i18n("New Message").as_str()));
                label.add_css_class("account-name");
                // The pill must be able to shrink with the sidebar (down to its
                // 180px minimum) — otherwise the whole column's minimum width
                // exceeds the pane and every row highlight overflows the edge.
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                main.append(&label);
                hbox.append(&main);
                hbox.append(&gtk::Separator::new(gtk::Orientation::Vertical));
                // Its own click: the row's activation, which composes, never
                // sees it.
                let caret = gtk::Button::from_icon_name("pan-down-symbolic");
                caret.add_css_class("flat");
                caret.add_css_class("compose-caret");
                caret.set_focus_on_click(false);
                caret.set_valign(gtk::Align::Center);
                caret.set_tooltip_text(Some(i18n("New Message from Template…").as_str()));
                let menu = compose_menu.clone();
                let quiet = self.quiet.clone();
                caret.connect_clicked(move |b| {
                    if !quiet.get() {
                        menu(
                            b.upcast_ref(),
                            f64::from(b.width()) / 2.0,
                            f64::from(b.height()),
                        );
                    }
                });
                hbox.append(&caret);
            }
            row.set_child(Some(&hbox));
            // A right-click offers a template to start from (#360).
            let right_click = gtk::GestureClick::new();
            right_click.set_button(gtk::gdk::BUTTON_SECONDARY);
            right_click.connect_pressed(move |gesture, _, x, y| {
                let Some(widget) = gesture.widget() else {
                    return;
                };
                compose_menu(&widget, x, y);
            });
            row.add_controller(right_click);
            list.append(&row);
            let s = sender.clone();
            let quiet = self.quiet.clone();
            list.connect_row_activated(move |_, _| {
                if quiet.get() {
                    return;
                }
                let _ = s.output(SidebarOutput::ComposeRequested);
            });

            if !self.collapsed {
                // Full width, like the rows below it; the centred label
                // carries the action on its own.
                list.set_hexpand(true);
                list.set_halign(gtk::Align::Fill);
            }
            bar.append(&list);
            pinned.append(&bar);
        }

        // The unified section: the "All Inboxes" row and the Starred / Sent /
        // Drafts rows (Settings → Sidebar → Unified), all built alike: a
        // header row that opens the merged view and a caret that opens the
        // list beneath. It heads the scrolling sidebar rather than the pinned
        // area: with several lists open at once it can stand taller than a
        // short window, and pinned it would have forced the window taller
        // than the screen.
        let mut unified_rows: Vec<UnifiedRow> = Vec::new();
        if self.show_unified {
            unified_rows.push(UnifiedRow::Kind(FolderKind::Inbox));
        }
        unified_rows.extend(
            self.unified_kinds
                .listed()
                .into_iter()
                .map(UnifiedRow::Kind),
        );
        // Smart folders (#417) close the section, and stand alone with a
        // single account.
        if !self.smart_folders.is_empty() {
            unified_rows.push(UnifiedRow::Smart);
        }
        let unified_sections: Vec<SectionData> =
            sections.iter().filter(|s| s.in_unified).cloned().collect();
        self.build_unified_run(container, &unified_rows, &unified_sections, sender);

        // Contacts and Attachments live in the pinned footer against the
        // sidebar's bottom edge — they keep out of the way of the account
        // list and never scroll off. A faint rule sets them apart from
        // whatever the scroller above ends on. ONE list box holds both rows,
        // so they read as one gapless section (beta 1.18.0b feedback) and
        // selecting one automatically clears the other.
        if self.show_attachments || self.contacts_in_footer() {
            let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
            sep.add_css_class("footer-separator");
            footer.append(&sep);

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.add_css_class("navigation-sidebar");
            list.add_css_class("sidebar-footer");

            // "Contacts" row: the in-app contacts view, where Settings →
            // Sidebar puts it by default. Right-click offers GNOME Contacts.
            if self.contacts_in_footer() {
                let row = self.footer_row("x-office-address-book-symbolic", &i18n("Contacts"));
                let right_click = gtk::GestureClick::new();
                right_click.set_button(3);
                let s = sender.clone();
                right_click.connect_pressed(move |gesture, _, x, y| {
                    let Some(widget) = gesture.widget() else {
                        return;
                    };
                    let s2 = s.clone();
                    show_context_menu(
                        &widget,
                        x,
                        y,
                        vec![vec![MenuEntry::new(
                            i18n("Open GNOME Contacts"),
                            move || {
                                let _ = s2.output(SidebarOutput::OpenGnomeContacts);
                            },
                        )
                        .icon("adw-external-link-symbolic")]],
                    );
                });
                row.add_controller(right_click);
                list.append(&row);
                self.contacts_row = Some(row);
            }

            // "Attachments" row — a gallery of every inbox attachment. The
            // very last row in the sidebar, below Contacts.
            if self.show_attachments {
                let row = self.footer_row("mail-attachment-symbolic", &i18n("Attachments"));
                list.append(&row);
                self.attachments_row = Some(row);
            }

            let s = sender.clone();
            let attachments_row = self.attachments_row.clone();
            let contacts_row = self.contacts_row.clone();
            let quiet = self.quiet.clone();
            list.connect_row_selected(move |_, row| {
                if quiet.get() {
                    return;
                }
                let Some(row) = row else { return };
                if Some(row) == attachments_row.as_ref() {
                    s.input(SidebarInput::AttachmentsRowSelected);
                } else if Some(row) == contacts_row.as_ref() {
                    s.input(SidebarInput::ContactsRowClicked);
                }
            });
            footer.append(&list);
            self.footer_list = Some(list);
        }

        // "Outbox" row — only while something is waiting to be sent. It sits
        // directly above the accounts so a stuck message is impossible to miss.
        if self.outbox_count > 0 {
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.add_css_class("navigation-sidebar");

            let row = gtk::ListBoxRow::new();
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("folder-row");
            let img = gtk::Image::from_icon_name("mail-send-symbolic");
            img.add_css_class("folder-icon");
            pin_icon_size(&img);
            let badge = gtk::Label::new(Some(&self.outbox_count.to_string()));
            style_badge(&badge, 5);
            if self.collapsed {
                hbox.set_halign(gtk::Align::Center);
                row.set_tooltip_text(Some(&format!(
                    "Outbox — {} waiting to be sent",
                    self.outbox_count
                )));
                hbox.append(&img);
            } else {
                if self.chevrons_left {
                    img.set_margin_start(ROW_LEFT_INSET);
                }
                hbox.append(&img);
                let label = gtk::Label::new(Some(i18n("Outbox").as_str()));
                label.set_hexpand(true);
                label.set_halign(gtk::Align::Start);
                label.add_css_class("account-name");
                hbox.append(&label);
                hbox.append(&badge);
            }
            row.set_child(Some(&hbox));
            list.append(&row);

            let s = sender.clone();
            let quiet = self.quiet.clone();
            list.connect_row_selected(move |_, row| {
                if quiet.get() {
                    return;
                }
                if row.is_some() {
                    s.input(SidebarInput::OutboxRowSelected);
                }
            });
            container.append(&list);
            self.outbox_list = Some(list);
        }

        // Favorites, Filters, Tags and Contacts placed above the accounts,
        // drawn like the unified rows.
        let rows_above = self.placed_rows(AboveAccounts);
        if !rows_above.is_empty() {
            self.build_unified_run(container, &rows_above, &sections, sender);
        }

        // The account sections — unless Settings (or the main menu) has
        // them off, for those who work from the unified section alone.
        let account_sections: Vec<&SectionData> = if self.show_accounts && !self.focus_hide_accounts
        {
            sections.iter().collect()
        } else {
            Vec::new()
        };
        // Every account section goes in one revealer, so Focus Mode can
        // slide them all away together (and back).
        let accounts_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let accounts_revealer = gtk::Revealer::new();
        accounts_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        accounts_revealer.set_transition_duration(0);
        accounts_revealer.set_reveal_child(!self.reveal_accounts_late);
        accounts_revealer.set_child(Some(&accounts_box));
        let any_accounts = !account_sections.is_empty();
        for (section_idx, section) in account_sections.into_iter().enumerate() {
            let id = section.account.id;

            // Header: avatar circle + name/email on the left, chevron on the right.
            let header = gtk::Button::new();
            header.add_css_class("flat");
            header.add_css_class("account-header");
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);

            // A configured label wins (same as the All Inboxes sub-rows);
            // otherwise the account's name, then its address.
            let name_str = if !section.account.label.trim().is_empty()
                && section.account.label != section.account.email
            {
                section.account.label.clone()
            } else if section.account.name.trim().is_empty() {
                section.account.email.clone()
            } else {
                section.account.name.clone()
            };

            let circle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            circle.add_css_class("account-circle");
            circle.add_css_class(&format!("acct-color-{id}"));
            // Ringed in the account's color, as the list's avatars are in
            // All Inboxes: a picture covers the color the disc would show.
            circle.add_css_class(&format!("acct-ring-{id}"));
            circle.set_valign(gtk::Align::Center);
            // Keep it a perfect circle: a fixed square that never stretches with
            // the row. (Without this the glyph's hexpand propagates up and the
            // circle widens into an oval whenever a name column sits beside it.)
            circle.set_halign(gtk::Align::Center);
            circle.set_hexpand(false);
            circle.set_size_request(30, 30);
            // Drawn ink-centred (see `ui::initials`), not a label: a lone
            // letter or an emoji sits exactly in the middle of the disc.
            // The account's own Gravatar leads when it asked for one and the
            // address has one (#189); the picture and the emoji are what it
            // falls back to.
            let gravatar = account_gravatar(&section.account.email);
            let glyph: gtk::Widget = match (&gravatar, &section.avatar, &section.emoji) {
                (Some(texture), ..) => {
                    circle.set_overflow(gtk::Overflow::Hidden);
                    crate::ui::initials::picture_from_texture(texture, 30).upcast()
                }
                (None, Some(path), _) => {
                    // A picture fills the disc; the disc's rounded corners
                    // clip it into a circle.
                    circle.set_overflow(gtk::Overflow::Hidden);
                    crate::ui::initials::avatar_picture(path, 30).upcast()
                }
                (None, None, Some(em)) if !em.is_empty() => {
                    crate::ui::initials::glyph_picture(em, &section.color, 0.55, 30).upcast()
                }
                _ => crate::ui::initials::glyph_picture(
                    &account_initials(&name_str, &section.account.email),
                    &section.color,
                    0.47,
                    30,
                )
                .upcast(),
            };
            circle.append(&glyph);
            // While this account's section is collapsed its Inbox row (and the
            // chip on it) is hidden inside the revealer, so surface the inbox
            // unread count on the avatar instead — mirroring how the collapsed
            // "All Inboxes" rail badges its icon.
            let inbox_unread = section
                .folders
                .iter()
                .find(|f| f.kind == FolderKind::Inbox)
                .map(|f| f.unread)
                .unwrap_or(0);
            let (circle_overlay, circle_badge) = with_unread_overlay(&circle, inbox_unread);
            circle_overlay.set_halign(gtk::Align::Center);
            circle_overlay.set_hexpand(false);
            // Folded for the rail (Settings → Sidebar → Icon rail) reads as
            // collapsed here, without touching the saved state.
            let folded = self.account_shown_folded(section);
            circle_badge.set_visible(folded && inbox_unread > 0);
            self.account_circle_badges.insert(id, circle_badge);
            if !self.collapsed && self.chevrons_left {
                // Leaves room for the overlaid disclosure chevron, which
                // would otherwise sit right on top of the avatar — a bit
                // more than the minimum, so the two aren't touching.
                circle_overlay.set_margin_start(ROW_LEFT_INSET + 8);
            }

            // Chevron is tracked even when collapsed so per-account toggles
            // still update an icon; it's only shown in the expanded layout —
            // leading (overlaid on the header's left edge, reserving no
            // layout space, like the All Inboxes row's) or classic trailing,
            // per Settings → Chevron placement.
            let chevron = gtk::Image::from_icon_name(if folded {
                "pan-end-symbolic"
            } else {
                "pan-down-symbolic"
            });
            chevron.set_valign(gtk::Align::Center);
            // A button of its own, as the unified rows' chevrons are: it
            // takes its click before the header does.
            let chev_btn = gtk::Button::new();
            chev_btn.set_child(Some(&chevron));
            chev_btn.add_css_class("flat");
            chev_btn.add_css_class("chevron-btn");
            chev_btn.set_valign(gtk::Align::Center);
            chev_btn.set_tooltip_text(Some(i18n("Show or hide folders").as_str()));
            {
                let s = sender.input_sender().clone();
                chev_btn.connect_clicked(move |_| {
                    let _ = s.send(SidebarInput::ToggleCollapseLocal(id));
                });
            }

            if self.collapsed {
                hbox.append(&circle_overlay);
                hbox.set_halign(gtk::Align::Center);
                header.set_tooltip_text(Some(&name_str));
            } else {
                hbox.append(&circle_overlay);
                let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
                vbox.set_hexpand(true);
                vbox.set_valign(gtk::Align::Center);
                // Clearance for the unread chip that overlays the circle's
                // corner while the section is collapsed.
                vbox.set_margin_start(6);
                let name = gtk::Label::new(Some(&name_str));
                name.set_halign(gtk::Align::Start);
                name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                name.add_css_class("account-name");
                let email = gtk::Label::new(Some(&section.account.email));
                email.set_halign(gtk::Align::Start);
                email.set_ellipsize(gtk::pango::EllipsizeMode::End);
                email.add_css_class("account-email");
                vbox.append(&name);
                vbox.append(&email);
                hbox.append(&vbox);
                if self.chevrons_left {
                    chevron.set_pixel_size(15);
                    chev_btn.add_css_class("row-disclosure-chevron");
                    chev_btn.set_halign(gtk::Align::Start);
                    let overlay = gtk::Overlay::new();
                    overlay.set_child(Some(&hbox));
                    overlay.add_overlay(&chev_btn);
                    header.set_child(Some(&overlay));
                } else {
                    chev_btn.add_css_class("account-chevron-end");
                    hbox.append(&chev_btn);
                }
            }

            if header.child().is_none() {
                header.set_child(Some(&hbox));
            }
            if self.collapsed || self.account_click_folds {
                // The rail's avatar, or every header by choice: a click
                // anywhere opens or folds the account.
                let s = sender.input_sender().clone();
                header.connect_clicked(move |_| {
                    let _ = s.send(SidebarInput::ToggleCollapseLocal(id));
                });
            } else {
                // As the unified rows: the chevron's own button opens or
                // folds the account, and so does a double click anywhere
                // else on the header; a plain click there does nothing.
                let press = gtk::GestureClick::new();
                press.set_button(gtk::gdk::BUTTON_PRIMARY);
                press.set_propagation_phase(gtk::PropagationPhase::Capture);
                let s = sender.input_sender().clone();
                let (head, chev) = (header.downgrade(), chev_btn.downgrade());
                press.connect_pressed(move |_, presses, x, y| {
                    let (Some(head), Some(chev)) = (head.upgrade(), chev.upgrade()) else {
                        return;
                    };
                    let on_chevron = head
                        .pick(x, y, gtk::PickFlags::DEFAULT)
                        .is_some_and(|w| w == chev || w.is_ancestor(&chev));
                    if presses == 2 && !on_chevron {
                        let _ = s.send(SidebarInput::ToggleCollapseLocal(id));
                    }
                });
                header.add_controller(press);
            }
            // Right-click an account: act on the account / its inbox.
            let inbox_id = section
                .folders
                .iter()
                .find(|f| f.kind == FolderKind::Inbox)
                .map(|f| f.id);
            let own_order = !section.folder_order.is_empty();
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_SECONDARY);
            let cs = sender.clone();
            let header_w = header.clone();
            click.connect_pressed(move |_, _, x, y| {
                let mut items: Vec<(&str, CtxAction)> = Vec::new();
                if let Some(fid) = inbox_id {
                    items.push((
                        i18n_noop("Mark Inbox as Read"),
                        CtxAction::MarkFolderRead {
                            account_id: id,
                            folder_id: fid,
                        },
                    ));
                    items.push((
                        i18n_noop("Refresh"),
                        CtxAction::RefreshFolder {
                            account_id: id,
                            folder_id: fid,
                        },
                    ));
                }
                items.push((i18n_noop("New Folder…"), CtxAction::NewFolder(id)));
                if own_order {
                    items.push((
                        i18n_noop("Reset Folder Order"),
                        CtxAction::ResetFolderOrder(id),
                    ));
                }
                items.push((
                    i18n_noop("Account Settings…"),
                    CtxAction::OpenAccountSettings(id),
                ));
                items.push((i18n_noop("Remove Account…"), CtxAction::RemoveAccount(id)));
                show_sidebar_menu(&header_w, x, y, items, &cs);
            });
            header.add_controller(click);

            // While dragging a message, hovering the account header for 500ms
            // expands a collapsed account so its folders become drop targets.
            let motion = gtk::DropControllerMotion::new();
            let ms = sender.input_sender().clone();
            let timer: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>> =
                std::rc::Rc::new(std::cell::RefCell::new(None));
            let t_enter = timer.clone();
            motion.connect_enter(move |_, _, _| {
                if t_enter.borrow().is_some() {
                    return;
                }
                let ms = ms.clone();
                let t = t_enter.clone();
                let src = gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(500),
                    move || {
                        *t.borrow_mut() = None;
                        let _ = ms.send(SidebarInput::ExpandForDrop(id));
                    },
                );
                *t_enter.borrow_mut() = Some(src);
            });
            let t_leave = timer.clone();
            motion.connect_leave(move |_| {
                if let Some(src) = t_leave.borrow_mut().take() {
                    src.remove();
                }
            });
            header.add_controller(motion);
            // The first account sits 10px further below the unified block
            // above it (when there is one), so the two read as separate
            // groups (Jason, 2026-09-13).
            if section_idx == 0 && container.first_child().is_some() {
                header.set_margin_top(10);
            }
            accounts_box.append(&header);

            // Animated folder list.
            let revealer = gtk::Revealer::new();
            revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
            revealer.set_transition_duration(0);
            revealer.set_reveal_child(!folded);

            // Split folders: essential (Inbox/Sent/Trash/Archive/…) are always
            // shown; user-created "custom" folders are tucked under a collapsible
            // "Folders" section. `section.folders` is already essential-first, so
            // the essential list holds row indices 0..E and the custom list E..
            // The folders inside the Inbox go with it, under its row,
            // when the account nests them (#345).
            let nest = nests_in_inbox(&section.folders);
            let essential: Vec<&Folder> = section
                .folders
                .iter()
                .filter(|f| is_main_row(f, nest))
                .collect();
            let custom: Vec<&Folder> = section
                .folders
                .iter()
                .filter(|f| !is_main_row(f, nest))
                .collect();
            let e = essential.len() as i32;
            let inside: Vec<&Folder> = essential
                .iter()
                .copied()
                .filter(|f| f.kind == FolderKind::Custom)
                .collect();
            let collapsed_nodes = self.tree_collapsed.get(&id).cloned().unwrap_or_default();

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.add_css_class("navigation-sidebar");
            for (index, folder) in essential.iter().enumerate() {
                let icon = filter_icon(section, folder);
                let nested = folder.kind == FolderKind::Custom;
                // With folders under the Inbox, every main row has the
                // expander slot the tree rows have, so their names align:
                // a chevron on the Inbox and on a nested folder with
                // sub-folders, a spacer on the rest.
                let has_children = nest
                    && !self.collapsed
                    && match folder.kind {
                        FolderKind::Inbox => !inside.is_empty(),
                        FolderKind::Custom => {
                            inside.iter().any(|g| path_is_under(&g.path, &folder.path))
                        }
                        _ => false,
                    };
                let lead: Option<gtk::Widget> = if !nest || self.collapsed {
                    None
                } else if has_children {
                    let img = gtk::Image::from_icon_name("pan-end-symbolic");
                    img.add_css_class("tree-expander-icon");
                    if !collapsed_nodes.contains(&folder.path) {
                        img.add_css_class("open");
                    }
                    img.set_pixel_size(12);
                    let btn = gtk::Button::new();
                    btn.set_child(Some(&img));
                    btn.add_css_class("flat");
                    btn.add_css_class("tree-expander");
                    btn.set_valign(gtk::Align::Center);
                    btn.set_tooltip_text(Some(i18n("Show or hide sub-folders").as_str()));
                    let st = sender.input_sender().clone();
                    let path = folder.path.clone();
                    btn.connect_clicked(move |_| {
                        let _ = st.send(SidebarInput::ToggleFolderNode {
                            account_id: id,
                            path: path.clone(),
                        });
                    });
                    self.tree_chevrons.insert((id, folder.path.clone()), img);
                    Some(btn.upcast())
                } else {
                    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    spacer.set_width_request(TREE_EXPANDER_WIDTH);
                    Some(spacer.upcast())
                };
                let depth = if nested {
                    folder_depth(folder, &inside) + 1
                } else {
                    0
                };
                let (row, badge) = build_folder_row(
                    folder,
                    self.collapsed,
                    depth,
                    lead.as_ref(),
                    self.chevrons_left,
                    icon,
                );
                if nested {
                    // Hidden while the Inbox or a parent is folded, as in
                    // the Folders section.
                    let hidden = hidden_by_collapse(&folder.path, &collapsed_nodes);
                    if let Some(content) = row.child() {
                        row.set_child(gtk::Widget::NONE);
                        let rev = gtk::Revealer::new();
                        rev.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                        rev.set_transition_duration(0);
                        rev.set_child(Some(&content));
                        rev.set_reveal_child(!hidden);
                        row.set_child(Some(&rev));
                        self.main_row_revealers
                            .entry(id)
                            .or_default()
                            .insert(index, rev);
                    }
                    row.set_visible(!hidden);
                    if !self.collapsed {
                        let payload = format!("vireo-folder\t{id}\t{}", folder.path);
                        row.add_controller(folder_drag_source(&row, payload));
                    }
                } else if !self.collapsed && essential.len() > 1 {
                    // The main folders can be put in another order among
                    // themselves; where they sit on the server is not
                    // theirs to change.
                    let payload = format!("vireo-folder-main\t{id}\t{}", folder.path);
                    row.add_controller(folder_drag_source(&row, payload));
                }
                list.append(&row);
                if let Some(badge) = badge {
                    self.folder_badges.insert((id, folder.id), badge);
                }
            }
            let s2 = sender.input_sender().clone();
            let quiet = self.quiet.clone();
            list.connect_row_selected(move |_, row| {
                if quiet.get() {
                    return;
                }
                if let Some(row) = row {
                    let _ = s2.send(SidebarInput::FolderRowSelected {
                        account_id: id,
                        index: row.index(),
                    });
                }
            });
            let (list_box, list_line) = with_drop_line(&list);
            attach_folder_list_drop(
                &list,
                &list_line,
                id,
                essential.iter().map(|f| (*f).clone()).collect(),
                // The folders inside the Inbox take folders dropped on them.
                inside.iter().map(|f| f.path.clone()).collect(),
                section.hierarchy.clone(),
                sender,
            );
            self.main_folders
                .insert(id, essential.iter().map(|f| (*f).clone()).collect());
            attach_folder_context_menu(
                &list,
                id,
                essential.iter().map(|f| (*f).clone()).collect(),
                section.filtered.iter().map(|f| f.path.clone()).collect(),
                self.favorite_paths(id),
                section.has_filters,
                sender,
            );

            // The collapsible custom-folders list + its "Folders" toggle header.
            let custom_list = gtk::ListBox::new();
            custom_list.set_selection_mode(gtk::SelectionMode::Single);
            custom_list.add_css_class("navigation-sidebar");
            let custom_revealer = gtk::Revealer::new();
            let custom_chevron = gtk::Image::from_icon_name(if section.custom_expanded {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            });
            let mut folders_heading: Option<gtk::Widget> = None;
            if !custom.is_empty() {
                for folder in &custom {
                    let depth = folder_depth(folder, &custom);
                    // The expander slot (#51): a chevron for folders with
                    // sub-folders, an equal-width spacer for leaves so names
                    // at one depth stay aligned. Rail mode has no room for
                    // either.
                    let has_children = custom.iter().any(|g| path_is_under(&g.path, &folder.path));
                    let lead: Option<gtk::Widget> = if self.collapsed {
                        None
                    } else if has_children {
                        // One right-pointing caret; the "open" class rotates it
                        // 90° via a CSS transition, so toggling spins smoothly
                        // instead of swapping glyphs.
                        let img = gtk::Image::from_icon_name("pan-end-symbolic");
                        img.add_css_class("tree-expander-icon");
                        if !collapsed_nodes.contains(&folder.path) {
                            img.add_css_class("open");
                        }
                        img.set_pixel_size(12);
                        let btn = gtk::Button::new();
                        btn.set_child(Some(&img));
                        btn.add_css_class("flat");
                        btn.add_css_class("tree-expander");
                        btn.set_valign(gtk::Align::Center);
                        btn.set_tooltip_text(Some(i18n("Show or hide sub-folders").as_str()));
                        let st = sender.input_sender().clone();
                        let path = folder.path.clone();
                        btn.connect_clicked(move |_| {
                            let _ = st.send(SidebarInput::ToggleFolderNode {
                                account_id: id,
                                path: path.clone(),
                            });
                        });
                        self.tree_chevrons.insert((id, folder.path.clone()), img);
                        Some(btn.upcast())
                    } else {
                        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                        spacer.set_width_request(TREE_EXPANDER_WIDTH);
                        Some(spacer.upcast())
                    };
                    let (row, badge) = build_folder_row(
                        folder,
                        self.collapsed,
                        depth,
                        lead.as_ref(),
                        self.chevrons_left,
                        filter_icon(section, folder),
                    );
                    // Hidden while any ancestor is collapsed; the row still
                    // exists, so selection indices stay stable. Its content
                    // sits in a revealer so user toggles slide open/closed —
                    // built with no transition (rebuilds must reach full
                    // height in one pass; see the scroll-jump saga), armed to
                    // 200ms alongside the section revealers below.
                    let hidden = hidden_by_collapse(&folder.path, &collapsed_nodes);
                    if let Some(content) = row.child() {
                        row.set_child(gtk::Widget::NONE);
                        let rev = gtk::Revealer::new();
                        rev.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                        rev.set_transition_duration(0);
                        rev.set_child(Some(&content));
                        rev.set_reveal_child(!hidden);
                        row.set_child(Some(&rev));
                        self.tree_row_revealers.entry(id).or_default().push(rev);
                    }
                    row.set_visible(!hidden);
                    // Custom folders can be picked up and dropped on a new
                    // parent (#51), or on a sibling's edge to reorder them.
                    if !self.collapsed {
                        let payload = format!("vireo-folder\t{id}\t{}", folder.path);
                        row.add_controller(folder_drag_source(&row, payload));
                    }
                    custom_list.append(&row);
                    if let Some(badge) = badge {
                        self.folder_badges.insert((id, folder.id), badge);
                    }
                }
                self.custom_folders
                    .insert(id, custom.iter().map(|f| (*f).clone()).collect());
                let s3 = sender.input_sender().clone();
                let quiet = self.quiet.clone();
                custom_list.connect_row_selected(move |_, row| {
                    if quiet.get() {
                        return;
                    }
                    if let Some(row) = row {
                        // Offset past the essential folders into `section.folders`.
                        let _ = s3.send(SidebarInput::FolderRowSelected {
                            account_id: id,
                            index: e + row.index(),
                        });
                    }
                });
                let (custom_box, custom_line) = with_drop_line(&custom_list);
                attach_folder_list_drop(
                    &custom_list,
                    &custom_line,
                    id,
                    custom.iter().map(|f| (*f).clone()).collect(),
                    custom.iter().map(|f| f.path.clone()).collect(),
                    section.hierarchy.clone(),
                    sender,
                );
                custom_revealer.set_child(Some(&custom_box));
                attach_folder_context_menu(
                    &custom_list,
                    id,
                    custom.iter().map(|f| (*f).clone()).collect(),
                    section.filtered.iter().map(|f| f.path.clone()).collect(),
                    self.favorite_paths(id),
                    section.has_filters,
                    sender,
                );

                custom_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
                custom_revealer.set_transition_duration(0);
                custom_revealer.set_reveal_child(section.custom_expanded);

                let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                hb.add_css_class("folder-row");
                let mut folders_chevron: Option<gtk::Button> = None;
                if self.collapsed {
                    hb.set_halign(gtk::Align::Center);
                    hb.append(&gtk::Image::from_icon_name("folder-symbolic"));
                } else {
                    if self.chevrons_left {
                        // A chevron glyph's ink sits further into its canvas
                        // than a regular icon's, so the folder rows' full
                        // inset would land it visually right of the icons
                        // above — a smaller value keeps the same column.
                        custom_chevron.set_margin_start(2);
                    }
                    let btn = heading_chevron(&custom_chevron);
                    btn.set_tooltip_text(Some(
                        if section.gmail {
                            i18n("Show or hide labels")
                        } else {
                            i18n("Show or hide folders")
                        }
                        .as_str(),
                    ));
                    hb.append(&btn);
                    folders_chevron = Some(btn);
                    // Gmail's own word for them: its folders are labels.
                    let len = custom.len().to_string();
                    let heading = if section.gmail {
                        i18n_f("Labels ({len})", &[("len", &len)])
                    } else {
                        i18n_f("Folders ({len})", &[("len", &len)])
                    };
                    let lbl = gtk::Label::new(Some(&heading));
                    lbl.set_halign(gtk::Align::Start);
                    lbl.set_hexpand(true);
                    hb.append(&lbl);
                }
                let folders_toggle = section_heading(
                    &hb,
                    folders_chevron.as_ref(),
                    &[],
                    SidebarInput::ToggleCustomFoldersLocal(id),
                    sender,
                );
                if self.collapsed {
                    folders_toggle.set_tooltip_text(Some(
                        if section.gmail {
                            i18n("Labels")
                        } else {
                            i18n("Folders")
                        }
                        .as_str(),
                    ));
                }
                // Right-click: how this account's folders are sorted.
                let click = gtk::GestureClick::new();
                click.set_button(gtk::gdk::BUTTON_SECONDARY);
                let cs = sender.clone();
                let section_c = section.clone();
                let toggle_w = folders_toggle.clone();
                click.connect_pressed(move |_, _, x, y| {
                    show_context_menu_with_header(
                        &toggle_w,
                        x,
                        y,
                        Some(i18n("Sort Folders").as_str()),
                        folder_sort_menu(&section_c, &cs),
                    );
                });
                folders_toggle.add_controller(click);
                // Dropping a folder on the section header moves it to the
                // account's top level ("" — resolved to the namespace root).
                folders_toggle.add_controller(folder_drop_target(id, String::new(), sender));
                folders_heading = Some(folders_toggle);
            }

            // "+ Add Folder" button at the bottom of the list for quick creation.
            let add_btn = gtk::Button::new();
            add_btn.add_css_class("flat");
            add_btn.add_css_class("add-folder-btn");
            let add_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            add_box.add_css_class("folder-row");
            let add_img = gtk::Image::from_icon_name("list-add-symbolic");
            pin_icon_size(&add_img);
            add_box.append(&add_img);
            if self.collapsed {
                add_box.set_halign(gtk::Align::Center);
                add_btn.set_tooltip_text(Some(i18n("Add Folder").as_str()));
            } else {
                if self.chevrons_left {
                    add_img.set_margin_start(4);
                }
                let lbl = gtk::Label::new(Some(i18n("Add Folder").as_str()));
                lbl.set_halign(gtk::Align::Start);
                lbl.set_hexpand(true);
                add_box.append(&lbl);
            }
            add_btn.set_child(Some(&add_box));
            let cs = sender.clone();
            add_btn.connect_clicked(move |_| {
                let _ = cs.output(SidebarOutput::Context(CtxAction::NewFolder(id)));
            });

            let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
            wrap.append(&list_box);
            // The account's own Tags section, above its folder hierarchy:
            // every tag scoped to this account, there whatever the unified
            // section shows. (Its filtered folders are marked in place in
            // the hierarchy instead — see `filter_icon`.)
            if !self.tags.is_empty() {
                self.build_tags_section(&wrap, Slot::Account(id), sender);
            }
            if !custom.is_empty() {
                if let Some(h) = &folders_heading {
                    wrap.append(h);
                }
                wrap.append(&custom_revealer);
            }
            wrap.append(&add_btn);
            revealer.set_child(Some(&wrap));
            accounts_box.append(&revealer);

            self.revealers.insert(id, revealer);
            self.chevrons.insert(id, chevron);
            self.folder_lists.insert(id, list);
            self.custom_folder_lists.insert(id, custom_list);
            self.custom_revealers.insert(id, custom_revealer);
            self.custom_chevrons.insert(id, custom_chevron);
        }

        if any_accounts {
            container.append(&accounts_revealer);
            if std::mem::take(&mut self.reveal_accounts_late) {
                // Slide them in once the rebuild's freeze-frame has lifted
                // (80ms), so the whole of the motion is seen.
                let r = accounts_revealer.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(90),
                    move || {
                        r.set_transition_duration(crate::ui::FOCUS_ANIM_MS);
                        r.set_reveal_child(true);
                    },
                );
            }
        }
        self.accounts_revealer = any_accounts.then_some(accounts_revealer);

        // And those placed after the last account.
        let rows_below = self.placed_rows(BelowAccounts);
        if !rows_below.is_empty() {
            self.build_unified_run(container, &rows_below, &sections, sender);
        }

        // Per-account avatar colors (background + readable text), and the
        // ring every account circle wears. The ring stands a pixel off the
        // disc: flush against a disc of its own color it would only make
        // the disc bigger.
        let mut css = String::new();
        for s in &sections {
            let text = crate::color::readable_text(&s.color);
            css.push_str(&format!(
                ".acct-color-{0} {{ background-color: {1}; }} \
                 .acct-color-{0} label {{ color: {2}; }} \
                 .acct-tint-{0} {{ color: {1}; }} \
                 .acct-ring-{0} {{ outline: 2px solid {1}; outline-offset: 1px; }}\n",
                s.account.id, s.color, text
            ));
        }
        self.color_provider.load(css);

        // The revealers were built with no transition so the rebuilt content
        // reaches full height in the very first layout pass; hand them their
        // real animation back once that pass is done, for user toggles.
        {
            let revs: Vec<gtk::Revealer> = self
                .revealers
                .values()
                .chain(self.custom_revealers.values())
                .chain(self.tree_row_revealers.values().flatten())
                .cloned()
                .chain(self.unified_revealer.clone())
                .chain(self.tag_sections.values().map(|w| w.revealer.clone()))
                .chain(self.kind_widgets.values().map(|w| w.revealer.clone()))
                .collect();
            gtk::glib::idle_add_local_once(move || {
                for r in &revs {
                    r.set_transition_duration(200);
                }
            });
        }

        // Restore the scroll offset before anything paints: an idle-time
        // restore let one frame render at the top first — a visible flash on
        // every rebuild. The adjustment's `changed` signal fires while the
        // fresh rows are being measured (same layout pass), so pinning the
        // value there means no frame ever shows the wrong offset. The pin
        // holds through the freeze-frame window — layout can keep settling
        // for a few frames — then a timer finalises and disconnects.
        if let (Some(pos), Some(scroller)) = (saved_scroll, scroller) {
            if pos > 0.0 {
                let adj = scroller.vadjustment();
                adj.set_value(pos);
                let handler: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SignalHandlerId>>> =
                    std::rc::Rc::new(std::cell::RefCell::new(None));
                *handler.borrow_mut() = Some(adj.connect_changed(move |adj| {
                    adj.set_value(pos);
                }));
                let adj = scroller.vadjustment();
                let handler = handler.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(120),
                    move || {
                        adj.set_value(pos);
                        if let Some(id) = handler.borrow_mut().take() {
                            adj.disconnect(id);
                        }
                    },
                );
            }
        }
        self.restyle_seen();
    }

    /// Grey the unread counts of mail already looked at (#343): a chip is
    /// "seen" unless one of the folders it counts has mail new since. With
    /// the setting off, none is.
    pub(super) fn restyle_seen(&self) {
        let seen = |keys: &mut dyn Iterator<Item = &(u32, u32)>| match &self.fresh {
            None => false,
            Some(fresh) => {
                for k in keys {
                    if fresh.contains(k) {
                        return false;
                    }
                }
                true
            }
        };
        let mark = |label: &gtk::Label, on: bool| {
            if on {
                label.add_css_class("seen");
            } else {
                label.remove_css_class("seen");
            }
        };
        for (key, label) in self
            .folder_badges
            .iter()
            .chain(&self.unified_inbox_badges)
            .chain(self.filtered_badges.values().flatten())
            .chain(self.kind_widgets.values().flat_map(|w| &w.row_badges))
        {
            mark(label, seen(&mut std::iter::once(key)));
        }
        if let Some(label) = &self.unified_badge {
            mark(label, seen(&mut self.unified_inbox_badges.keys()));
        }
        for w in self.kind_widgets.values() {
            if let Some(b) = &w.badge {
                mark(b, seen(&mut w.row_badges.keys()));
            }
        }
        for section in &self.sections {
            if let Some(label) = self.account_circle_badges.get(&section.account.id) {
                let inbox = section
                    .folders
                    .iter()
                    .find(|f| f.kind == FolderKind::Inbox)
                    .map(|f| (section.account.id, f.id));
                mark(label, seen(&mut inbox.iter()));
            }
        }
    }

    /// A row of the pinned footer: an icon and a name, the icon alone
    /// (the name in its tooltip) on the rail.
    fn footer_row(&self, icon: &str, name: &str) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        hbox.add_css_class("folder-row");
        let img = gtk::Image::from_icon_name(icon);
        img.add_css_class("folder-icon");
        pin_icon_size(&img);
        if self.collapsed {
            hbox.set_halign(gtk::Align::Center);
            row.set_tooltip_text(Some(name));
            hbox.append(&img);
        } else {
            if self.chevrons_left {
                img.set_margin_start(ROW_LEFT_INSET);
            }
            hbox.append(&img);
            let label = gtk::Label::new(Some(name));
            label.set_hexpand(true);
            label.set_halign(gtk::Align::Start);
            label.add_css_class("account-name");
            hbox.append(&label);
        }
        row.set_child(Some(&hbox));
        row
    }
}
