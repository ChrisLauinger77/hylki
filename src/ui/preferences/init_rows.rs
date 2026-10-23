//! Filling the settings rows from the saved values when the window is
//! built, one step per page, in the order `init` runs them.

use super::rows::no_truncate;
use super::*;

impl Preferences {
    /// Settings never truncates: every combo gets a factory whose labels never
    /// ellipsize (see `no_truncate`).
    pub(super) fn make_combos_whole(&self, widgets: &PreferencesWidgets) {
        for row in [
            &widgets.fetch_row,
            &widgets.preview_lines_row,
            &widgets.message_theme_row,
            &widgets.card_actions_row,
            &widgets.app_theme_row,
            &widgets.text_scale_row,
            &widgets.settings_open_row,
            &widgets.tray_icon_row,
            &widgets.launcher_badge_row,
            &widgets.date_style_row,
            &widgets.clock_style_row,
            &widgets.language_row,
            &widgets.renderer_row,
            &widgets.chevron_side_row,
            &widgets.section_style_row,
            &widgets.filtered_placement_row,
            &widgets.tags_placement_row,
            &widgets.favorites_placement_row,
            &widgets.contacts_placement_row,
            &widgets.start_view_row,
            &widgets.folder_sort_row,
        ] {
            no_truncate(row);
        }

        // The install-command field is read-only: the row's pencil, which
        // says "type here", would be a lie.
    }

    /// Settings → General and Privacy: the Files install command, language,
    /// renderer and the privacy switches.
    pub(super) fn init_general_rows(&self, widgets: &PreferencesWidgets, init: &PrefInit) {
        hide_edit_icon(widgets.nautilus_cmd_row.upcast_ref());

        // Language combo: the system's, then every catalogue shipped.
        let choices = language_choices();
        let lang_labels: Vec<&str> = choices.iter().map(|(l, _)| l.as_str()).collect();
        widgets
            .language_row
            .set_model(Some(&gtk::StringList::new(&lang_labels)));
        widgets.language_row.set_selected(
            choices
                .iter()
                .position(|(_, c)| *c == init.language)
                .unwrap_or(0) as u32,
        );

        widgets.renderer_row.set_model(Some(&gtk::StringList::new(&[
            &i18n("Automatic"),
            "Vulkan",
            "OpenGL",
            &i18n("Software"),
        ])));
        let renderer = crate::config::load_renderer();
        widgets
            .renderer_row
            .set_selected(RENDERERS.iter().position(|r| *r == renderer).unwrap_or(0) as u32);

        widgets
            .auto_remote_content_row
            .set_active(init.auto_remote_content);
        widgets
            .show_remote_banner_row
            .set_active(init.show_remote_banner);
        widgets
            .show_spoof_banner_row
            .set_active(init.show_spoof_banner);
        widgets.gravatar_row.set_active(init.gravatar);
        widgets.avatars_row.set_active(init.avatars);
        widgets
            .own_mailbox_face_row
            .set_active(init.own_mailbox_face);
        widgets.tag_row_tint_row.set_active(init.tag_row_tint);
        widgets.sender_logos_row.set_active(init.sender_logos);
    }

    /// Settings → Notifications: mail checks, alerts and the new-mail sound.
    pub(super) fn init_notification_rows(&mut self, widgets: &PreferencesWidgets, init: &PrefInit) {
        // Mail-check interval combo.
        let labels_owned: Vec<String> = FETCH_INTERVALS.iter().map(|(l, _)| i18n(l)).collect();
        let labels: Vec<&str> = labels_owned.iter().map(String::as_str).collect();
        widgets
            .fetch_row
            .set_model(Some(&gtk::StringList::new(&labels)));
        let selected = FETCH_INTERVALS
            .iter()
            .position(|(_, secs)| *secs == init.fetch_interval_secs)
            .unwrap_or(0);
        widgets.fetch_row.set_selected(selected as u32);
        widgets.push_row.set_active(init.push);
        widgets.metered_row.set_active(init.metered_save_data);
        widgets.notifications_row.set_active(init.notifications);
        widgets
            .notification_content_row
            .set_active(init.notification_content);
        // Set before the handlers can act on it: the self already holds
        // these values, so the echo is ignored rather than played.
        widgets.sound_row.set_active(self.sound.enabled);
        let labels_owned: Vec<String> = sound_labels();
        let labels: Vec<&str> = labels_owned.iter().map(String::as_str).collect();
        widgets
            .sound_choice_row
            .set_model(Some(&gtk::StringList::new(&labels)));
        widgets
            .sound_choice_row
            .set_selected(sound_index(&self.sound.sound));
        widgets
            .sound_focused_row
            .set_active(self.sound.when_focused);
        widgets
            .notify_mark_read_row
            .set_active(init.notification_buttons.mark_read);
        widgets
            .notify_archive_row
            .set_active(init.notification_buttons.archive);
        widgets
            .notify_delete_row
            .set_active(init.notification_buttons.delete);
        widgets
            .notify_reply_row
            .set_active(init.notification_buttons.reply);
        widgets
            .notify_forward_row
            .set_active(init.notification_buttons.forward);
        widgets
            .notify_spam_row
            .set_active(init.notification_buttons.spam);
        widgets
            .show_attachments_row
            .set_active(init.show_attachments);
    }

    /// Settings → Sidebar: the unified section, placements, start view, folder
    /// order and the icon rail.
    pub(super) fn init_sidebar_rows(&self, widgets: &PreferencesWidgets, init: &PrefInit) {
        widgets.show_unified_row.set_active(init.show_unified);
        widgets
            .unified_chip_all_inboxes_row
            .set_active(init.unified_chips.all_inboxes);
        widgets
            .unified_chip_starred_row
            .set_active(init.unified_chips.starred);
        widgets
            .unified_chip_drafts_row
            .set_active(init.unified_chips.drafts);
        widgets
            .unified_chip_archive_row
            .set_active(init.unified_chips.archive);
        widgets
            .unified_chip_junk_row
            .set_active(init.unified_chips.junk);
        widgets
            .unified_chip_trash_row
            .set_active(init.unified_chips.trash);
        widgets
            .unified_starred_row
            .set_active(init.unified_kinds.starred);
        widgets.unified_sent_row.set_active(init.unified_kinds.sent);
        widgets
            .unified_drafts_row
            .set_active(init.unified_kinds.drafts);
        widgets
            .unified_archive_row
            .set_active(init.unified_kinds.archive);
        widgets.unified_junk_row.set_active(init.unified_kinds.junk);
        widgets
            .unified_trash_row
            .set_active(init.unified_kinds.trash);
        widgets
            .section_style_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Unified style").as_str(),
                i18n("Tree style").as_str(),
            ])));
        widgets.section_style_row.set_selected(u32::from(
            init.section_style == crate::config::SectionStyle::Tree,
        ));
        for (row, placement) in [
            (&widgets.filtered_placement_row, init.filtered_placement),
            (&widgets.tags_placement_row, init.tags_placement),
            (&widgets.favorites_placement_row, init.favorites_placement),
        ] {
            row.set_model(Some(&gtk::StringList::new(&[
                i18n("Above the accounts").as_str(),
                i18n("Below the accounts").as_str(),
                i18n("Off").as_str(),
            ])));
            row.set_selected(placement_index(placement));
        }
        widgets
            .contacts_placement_row
            .set_model(Some(&contacts_placement_model()));
        widgets
            .contacts_placement_row
            .set_selected(contacts_placement_index(init.contacts_placement));
        widgets
            .start_view_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("All Inboxes").as_str(),
                i18n("The last account's inbox").as_str(),
                i18n("The last folder").as_str(),
            ])));
        widgets.start_view_row.set_selected(match init.start_view {
            crate::config::StartView::AllInboxes => 0,
            crate::config::StartView::AccountInbox => 1,
            crate::config::StartView::LastFolder => 2,
        });
        let sort_labels: Vec<String> = crate::config::FolderSort::ALL
            .iter()
            .map(|s| s.label())
            .collect();
        let sort_labels: Vec<&str> = sort_labels.iter().map(String::as_str).collect();
        widgets
            .folder_sort_row
            .set_model(Some(&gtk::StringList::new(&sort_labels)));
        widgets
            .folder_sort_row
            .set_selected(init.folder_sort.index());
        widgets
            .chevron_side_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Left").as_str(),
                i18n("Right").as_str(),
            ])));
        widgets
            .chevron_side_row
            .set_selected(if init.chevrons_left { 0 } else { 1 });
        widgets
            .account_click_row
            .set_active(init.account_click_folds);
        widgets
            .sidebar_hover_expand_row
            .set_active(init.sidebar_hover_expand);
        widgets
            .remember_sidebar_row
            .set_active(init.remember_sidebar);
        widgets.remember_rail_row.set_active(init.remember_rail);
        widgets.rail_dots_row.set_active(init.rail_dots);
        widgets.seen_counts_row.set_active(init.seen_counts);
        widgets
            .rail_fold_row
            .set_enable_expansion(init.rail_fold.enabled);
        // Unread counts and Fold up expanded items start folded each
        // launch and stay as they are left for the rest of the session,
        // a rebuilt Settings window included. Switching expansion on opens
        // a row, so its state is set after that.
        for (row, i) in [(&widgets.unified_chips_row, 0), (&widgets.rail_fold_row, 1)] {
            row.set_expanded(SESSION_OPEN.with(|o| o.get()[i]));
            row.connect_expanded_notify(move |row| {
                SESSION_OPEN.with(|o| {
                    let mut open = o.get();
                    open[i] = row.is_expanded();
                    o.set(open);
                });
            });
        }
        widgets
            .rail_fold_accounts_row
            .set_active(init.rail_fold.accounts);
        widgets
            .rail_fold_all_inboxes_row
            .set_active(init.rail_fold.all_inboxes);
        widgets
            .rail_fold_starred_row
            .set_active(init.rail_fold.starred);
        widgets.rail_fold_sent_row.set_active(init.rail_fold.sent);
        widgets
            .rail_fold_drafts_row
            .set_active(init.rail_fold.drafts);
        widgets
            .rail_fold_archive_row
            .set_active(init.rail_fold.archive);
        widgets.rail_fold_junk_row.set_active(init.rail_fold.junk);
        widgets.rail_fold_trash_row.set_active(init.rail_fold.trash);
        widgets
            .rail_fold_filtered_row
            .set_active(init.rail_fold.filtered);
        widgets.rail_fold_tags_row.set_active(init.rail_fold.tags);
    }

    /// Settings → Message List.
    pub(super) fn init_list_rows(&self, widgets: &PreferencesWidgets, init: &PrefInit) {
        let preview_labels_owned = [
            i18n("Off"),
            i18n("1 line"),
            i18n("2 lines"),
            i18n("3 lines"),
        ];
        let preview_labels: Vec<&str> = preview_labels_owned.iter().map(String::as_str).collect();
        widgets
            .preview_lines_row
            .set_model(Some(&gtk::StringList::new(&preview_labels)));
        widgets
            .preview_lines_row
            .set_selected(init.preview_lines.min(3));
        widgets
            .list_layout_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Cards"),
                &i18n("Single line"),
                &i18n("Automatic"),
            ])));
        no_truncate(&widgets.list_layout_row);
        widgets.list_headings_row.set_active(init.list_headings);
        widgets.list_fit_row.set_active(!init.list_column_scroll);
        widgets
            .group_by_date_row
            .set_active(init.list_group_by_date);
        widgets.pills_below_row.set_active(init.list_pills_below);
        widgets
            .list_search_open_row
            .set_active(init.list_search_open);
        for (row, bottom) in [
            (&widgets.list_search_place_row, init.list_search_bottom),
            (&widgets.list_bulk_place_row, init.list_bulk_bar_bottom),
        ] {
            row.set_model(Some(&gtk::StringList::new(&[
                &i18n("Top of the list"),
                &i18n("Bottom of the list"),
            ])));
            row.set_selected(u32::from(bottom));
            no_truncate(row);
        }
        widgets
            .list_layout_row
            .set_selected(match init.list_layout {
                crate::config::ListLayout::Cards => 0,
                crate::config::ListLayout::SingleLine => 1,
                crate::config::ListLayout::Automatic => 2,
            });
    }

    /// Running in the background, then the theme gallery and the app-icon strip
    /// (the strip fills in on idle).
    pub(super) fn init_background_and_galleries(
        &self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
        sender: &ComponentSender<Self>,
    ) {
        widgets.background_row.set_active(init.run_in_background);
        widgets.autostart_row.set_active(init.autostart);
        // Starting at login only means anything if Hylki stays running.
        widgets.autostart_row.set_sensitive(init.run_in_background);
        {
            let autostart_row = widgets.autostart_row.clone();
            widgets.background_row.connect_active_notify(move |row| {
                autostart_row.set_sensitive(row.is_active());
            });
        }
        // Theme: title in the row's own voice, the gallery beneath — the
        // same shape as the app-icon row under it. A pick goes straight
        // out and is painted at once.
        {
            let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
            body.set_margin_top(12);
            body.set_margin_bottom(12);
            body.set_margin_start(12);
            body.set_margin_end(12);
            let title = gtk::Label::new(Some(i18n("Theme").as_str()));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            body.append(&title);
            let s = sender.clone();
            let gallery = crate::ui::theme_picker::gallery(
                &init.theme,
                std::rc::Rc::new(move |id: &str| {
                    let _ = s.output(PrefOutput::SetTheme(id.to_string()));
                }),
            );
            gallery.set_margin_top(10);
            body.append(&gallery);
            widgets.theme_row.set_child(Some(&body));
        }

        // App icon: title in the row's own voice, the gallery beneath.
        // Picks go straight out; the app applies and offers the restart.
        {
            let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
            body.set_margin_top(12);
            body.set_margin_bottom(8);
            body.set_margin_start(12);
            body.set_margin_end(12);
            let title = gtk::Label::new(Some(i18n("App icon").as_str()));
            title.set_halign(gtk::Align::Start);
            title.set_xalign(0.0);
            body.append(&title);
            // An icon set outside Hylki stays until a pick here (#252): say
            // so, and ring no tile, since none of them is what shows.
            let custom = crate::app_icon::custom_icon().is_some();
            let note = gtk::Label::new(Some(
                i18n("The launcher shows an icon set elsewhere. Picking one here replaces it.")
                    .as_str(),
            ));
            note.add_css_class("dim-label");
            note.add_css_class("caption");
            note.set_halign(gtk::Align::Start);
            note.set_xalign(0.0);
            note.set_wrap(true);
            note.set_visible(custom);
            body.append(&note);
            widgets.app_icon_row.set_child(Some(&body));
            // The icon gallery decodes the whole catalogue; it fills in a
            // moment after the window is up rather than holding it back.
            let s = sender.clone();
            let app_icon = if custom {
                String::new()
            } else {
                init.app_icon.clone()
            };
            gtk::glib::idle_add_local_full(gtk::glib::Priority::LOW, move || {
                let s = s.clone();
                let note = note.clone();
                let strip = crate::ui::icon_picker::strip(
                    &app_icon,
                    56,
                    std::rc::Rc::new(move |id: &str| {
                        note.set_visible(false);
                        let _ = s.output(PrefOutput::SetAppIcon(id.to_string()));
                    }),
                );
                strip.set_margin_top(6);
                body.append(&strip);
                gtk::glib::ControlFlow::Break
            });
        }
    }

    /// The tray icon and the launcher badge.
    pub(super) fn init_tray_rows(&self, widgets: &PreferencesWidgets, init: &PrefInit) {
        widgets.tray_row.set_active(init.tray);
        let tray_icon_labels_owned: Vec<String> = TRAY_ICONS.iter().map(|(l, _)| i18n(l)).collect();
        let tray_icon_labels: Vec<&str> =
            tray_icon_labels_owned.iter().map(String::as_str).collect();
        widgets
            .tray_icon_row
            .set_model(Some(&gtk::StringList::new(&tray_icon_labels)));
        let tray_icon_sel = TRAY_ICONS
            .iter()
            .position(|(_, t)| *t == init.tray_icon)
            .unwrap_or(0);
        widgets.tray_icon_row.set_selected(tray_icon_sel as u32);
        widgets.tray_mail_row.set_active(init.tray_mail);
        let badge_labels: Vec<String> = LAUNCHER_BADGES.iter().map(|(l, _)| i18n(l)).collect();
        let badge_labels: Vec<&str> = badge_labels.iter().map(String::as_str).collect();
        widgets
            .launcher_badge_row
            .set_model(Some(&gtk::StringList::new(&badge_labels)));
        let badge_sel = LAUNCHER_BADGES
            .iter()
            .position(|(_, b)| *b == init.launcher_badge)
            .unwrap_or(1);
        widgets.launcher_badge_row.set_selected(badge_sel as u32);
        // The icon choice and the menu's mail list only mean anything with
        // a tray icon shown.
        widgets.tray_icon_row.set_sensitive(init.tray);
        widgets.tray_mail_row.set_sensitive(init.tray);
        {
            let tray_icon_row = widgets.tray_icon_row.clone();
            let tray_mail_row = widgets.tray_mail_row.clone();
            widgets.tray_row.connect_active_notify(move |row| {
                tray_icon_row.set_sensitive(row.is_active());
                tray_mail_row.set_sensitive(row.is_active());
            });
        }
    }

    /// Keyboard shortcuts, downloaded mail and the console.
    pub(super) fn init_keyboard_and_system_rows(
        &self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
    ) {
        widgets.single_key_row.set_active(init.single_key_shortcuts);
        widgets.gmail_keys_row.set_active(init.gmail_keys);
        // Gmail's keys are a kind of single-key shortcut, so they mean
        // nothing with those off (#392): the switch keeps its state but
        // greys out, as the tray's own options do.
        widgets
            .single_key_row
            .bind_property("active", &widgets.gmail_keys_row, "sensitive")
            .sync_create()
            .build();
        widgets
            .keep_downloads_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Always"),
                &i18n("A year"),
                &i18n("90 days"),
                &i18n("30 days"),
            ])));
        widgets.keep_downloads_row.set_selected(
            KEEP_DOWNLOADS_DAYS
                .iter()
                .position(|d| *d == init.keep_downloads_days)
                .unwrap_or(0) as u32,
        );
        widgets.console_mode_row.set_active(init.console_mode);
        widgets
            .status_bar_row
            .set_model(Some(&gtk::StringList::new(&[
                &i18n("Bottom of the window"),
                &i18n("Top of the window"),
            ])));
        widgets
            .status_bar_row
            .set_selected(u32::from(init.status_bar_top));
        widgets
            .read_mark_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("When displayed").as_str(),
                i18n("After two seconds").as_str(),
                i18n("Manually").as_str(),
            ])));
    }

    /// Date and clock styles, the app and message themes, and the text size.
    pub(super) fn init_date_and_theme_rows(&self, widgets: &PreferencesWidgets, init: &PrefInit) {
        // Date and clock combos.
        let date_labels_owned: Vec<String> = DATE_STYLES.iter().map(|(l, _)| i18n(l)).collect();
        let date_labels: Vec<&str> = date_labels_owned.iter().map(String::as_str).collect();
        widgets
            .date_style_row
            .set_model(Some(&gtk::StringList::new(&date_labels)));
        widgets.date_style_row.set_selected(
            DATE_STYLES
                .iter()
                .position(|(_, s)| *s == init.date_style)
                .unwrap_or(0) as u32,
        );
        let clock_labels_owned: Vec<String> = CLOCK_STYLES.iter().map(|(l, _)| i18n(l)).collect();
        let clock_labels: Vec<&str> = clock_labels_owned.iter().map(String::as_str).collect();
        widgets
            .clock_style_row
            .set_model(Some(&gtk::StringList::new(&clock_labels)));
        widgets.clock_style_row.set_selected(
            CLOCK_STYLES
                .iter()
                .position(|(_, s)| *s == init.clock_style)
                .unwrap_or(0) as u32,
        );

        // Message-content appearance combo.
        let app_theme_labels_owned: Vec<String> = APP_THEMES.iter().map(|(l, _)| i18n(l)).collect();
        let app_theme_labels: Vec<&str> =
            app_theme_labels_owned.iter().map(String::as_str).collect();
        widgets
            .app_theme_row
            .set_model(Some(&gtk::StringList::new(&app_theme_labels)));
        let app_theme_sel = APP_THEMES
            .iter()
            .position(|(_, t)| *t == init.app_theme)
            .unwrap_or(0);
        widgets.app_theme_row.set_selected(app_theme_sel as u32);
        let text_scale_labels_owned: Vec<String> = crate::text_scale::STEPS
            .iter()
            .map(|p| match p {
                100 => i18n("Default"),
                p => i18n_f("{percent}%", &[("percent", &p.to_string())]),
            })
            .collect();
        let text_scale_labels: Vec<&str> =
            text_scale_labels_owned.iter().map(String::as_str).collect();
        widgets
            .text_scale_row
            .set_model(Some(&gtk::StringList::new(&text_scale_labels)));
        let text_scale_sel = crate::text_scale::STEPS
            .iter()
            .position(|p| *p == init.text_scale)
            .unwrap_or(1);
        widgets.text_scale_row.set_selected(text_scale_sel as u32);

        let theme_labels_owned: Vec<String> = MESSAGE_THEMES.iter().map(|(l, _)| i18n(l)).collect();
        let theme_labels: Vec<&str> = theme_labels_owned.iter().map(String::as_str).collect();
        widgets
            .message_theme_row
            .set_model(Some(&gtk::StringList::new(&theme_labels)));
        let theme_sel = MESSAGE_THEMES
            .iter()
            .position(|(_, t)| *t == init.message_theme)
            .unwrap_or(0);
        widgets.message_theme_row.set_selected(theme_sel as u32);
    }

    /// The reader's font and color overrides, and the swipe and palette timing
    /// rows.
    pub(super) fn init_reader_fonts_and_gestures(
        &self,
        widgets: &PreferencesWidgets,
        init: &PrefInit,
    ) {
        // The reader's font override (#56): the button shows the chosen font,
        // or the interface font when none was chosen yet (so what it shows is
        // what the reader will use); the row only responds while the switch
        // is on.
        {
            let desc = if init.reader_font.trim().is_empty() {
                gtk::Settings::default()
                    .and_then(|s| s.gtk_font_name())
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "Cantarell 11".to_string())
            } else {
                init.reader_font.clone()
            };
            widgets
                .reader_font_button
                .set_font_desc(&gtk::pango::FontDescription::from_string(&desc));
            widgets.reader_font_row.set_sensitive(init.override_fonts);
            let font_row = widgets.reader_font_row.clone();
            widgets
                .override_fonts_row
                .connect_active_notify(move |row| {
                    font_row.set_sensitive(row.is_active());
                });
        }
        widgets.override_fonts_row.set_active(init.override_fonts);
        widgets.override_colors_row.set_active(init.override_colors);
        // Plain-text messages in monospace (#181): the font button shows
        // the desktop's monospace font until another is chosen.
        {
            let desc = if init.plain_font.trim().is_empty() {
                crate::desktop::monospace_font()
            } else {
                init.plain_font.clone()
            };
            widgets
                .plain_font_button
                .set_font_desc(&gtk::pango::FontDescription::from_string(&desc));
            widgets.plain_font_row.set_sensitive(init.plain_monospace);
            let font_row = widgets.plain_font_row.clone();
            widgets.plain_mono_row.connect_active_notify(move |row| {
                font_row.set_sensitive(row.is_active());
            });
        }
        widgets.plain_mono_row.set_active(init.plain_monospace);

        // Trackpad swipe sensitivity: 1 (libadwaita's own, far too long a
        // swipe for most trackpads) to 10, in half steps.
        let adj = gtk::Adjustment::new(
            init.swipe_sensitivity,
            crate::config::SWIPE_SENSITIVITY_MIN,
            crate::config::SWIPE_SENSITIVITY_MAX,
            0.5,
            1.0,
            0.0,
        );
        widgets.swipe_sensitivity_row.set_digits(1);
        widgets.swipe_sensitivity_row.set_adjustment(Some(&adj));

        // Hover-palette delay spinner (0–3000ms, step 50).
        // Actions palette timeout: 1–30 seconds.
        let adj = gtk::Adjustment::new(init.palette_collapse_secs as f64, 1.0, 30.0, 1.0, 5.0, 0.0);
        widgets.palette_collapse_row.set_adjustment(Some(&adj));
        let adj = gtk::Adjustment::new(
            init.card_palette_collapse_secs as f64,
            1.0,
            30.0,
            1.0,
            5.0,
            0.0,
        );
        widgets.card_palette_collapse_row.set_adjustment(Some(&adj));

        widgets
            .settings_open_row
            .set_model(Some(&gtk::StringList::new(&[
                i18n("Settings").as_str(),
                i18n("Accounts").as_str(),
            ])));
        widgets
            .settings_open_row
            .set_selected(if init.settings_open_accounts { 1 } else { 0 });
    }
}
