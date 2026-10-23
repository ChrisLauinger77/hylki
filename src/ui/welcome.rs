//! First-run welcome wizard: a guided, five-step setup shown when Hylki starts
//! with no accounts configured.
//!
//! The whole window is the theme's own ground, light or dark, with the wordmark as the
//! hero, and content floating on window-colored cards — deliberate and warm,
//! not a form dump. Steps: welcome → import from GNOME Online Accounts
//! (skippable) → add an account by hand (provider presets, custom OAuth) →
//! privacy → personalize → done. Accounts and settings apply through the app's existing
//! pipelines ([`WelcomeOutput`]), so the wizard owns no persistence of its own.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config::AccountConfig;
use crate::config::Protocol;
use crate::i18n::{i18n, i18n_f};
use crate::ui::accounts::{Provider, PROVIDERS};
use crate::worker::{self, ConnTest};

mod goa;
mod manual;
mod messages;
mod pages;
mod steps;
mod wordmark;

use goa::rebuild_goa_rows;
use manual::{form_has_input, wizard_providers};
pub use messages::{WelcomeCmd, WelcomeInput, WelcomeOutput, WelcomePrefs};
use pages::{card, entrance, page, pill, scrolled, tagline, title};
use wordmark::{bind_wordmark_to_position, HERO_SIZE, HERO_TOP};
pub(crate) use wordmark::{
    wordmark_height, wordmark_height_of, wordmark_picture, wordmark_picture_of, Wordmark,
};

pub struct Welcome {
    goa: Vec<crate::goa::GoaMailAccount>,
    /// Emails added so far (either path), shown on the account page and used
    /// to relabel the final button.
    added: Vec<String>,
    /// Generation counter for connection tests: clicking Test & Add again
    /// supersedes the running test (its late result is ignored), so the
    /// button never has to lock while a slow/wrong server times out.
    test_seq: u32,
    /// Something was imported from Online Accounts, or added by hand: that
    /// page's footer button reads Continue rather than Skip.
    goa_imported: bool,
    manual_added: bool,
}

pub struct WelcomeWidgets {
    carousel: adw::Carousel,
    back_btn: gtk::Button,
    wordmark_frame: gtk::Box,
    wordmark_box: gtk::Box,
    // Account page.
    provider_row: adw::ComboRow,
    hint_lbl: gtk::Label,
    name_row: adw::EntryRow,
    email_row: adw::EntryRow,
    pass_row: adw::PasswordEntryRow,
    server_exp: adw::ExpanderRow,
    host_row: adw::EntryRow,
    port_row: adw::EntryRow,
    smtp_row: adw::EntryRow,
    smtp_port_row: adw::EntryRow,
    oauth_client_id_row: adw::EntryRow,
    oauth_secret_row: adw::PasswordEntryRow,
    oauth_auth_url_row: adw::EntryRow,
    oauth_token_url_row: adw::EntryRow,
    oauth_scope_row: adw::EntryRow,
    status_lbl: gtk::Label,
    test_spinner: gtk::Spinner,
    add_btn: gtk::Button,
    /// The manual page's title, which becomes "Add another email account"
    /// once one came in from Online Accounts.
    acct_title: gtk::Label,
    // Online Accounts page.
    goa_empty: gtk::Label,
    goa_list: gtk::ListBox,
    goa_status: gtk::Label,
    footer_next: gtk::Button,
    // Prefs pages.
    sw_remote: adw::SwitchRow,
    sw_gravatar: adw::SwitchRow,
    sw_logos: adw::SwitchRow,
    sw_notif: adw::SwitchRow,
    preview_row: adw::ComboRow,
    sw_avatars: adw::SwitchRow,
    sw_threading: adw::SwitchRow,
    /// The gallery's latest pick, read when the wizard finishes.
    icon_choice: std::rc::Rc<std::cell::RefCell<String>>,
}

/// Extra space above the Online Accounts page's list (or its empty note)
/// and its buttons.
const GOA_GAP: i32 = 26;

impl Component for Welcome {
    type Init = ();
    type Input = WelcomeInput;
    type Output = WelcomeOutput;
    type CommandOutput = WelcomeCmd;
    type Root = adw::Window;
    type Widgets = WelcomeWidgets;

    fn init_root() -> Self::Root {
        let win = adw::Window::new();
        win.set_default_size(600, 700);
        win.add_css_class("welcome-window");
        win.set_title(Some(i18n("Welcome to Hylki").as_str()));
        win
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let existing: Vec<String> = crate::config::load()
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.email.to_ascii_lowercase())
            .collect();
        let mut goa = crate::goa::list_mail_accounts();
        goa.retain(|g| g.can_connect() && !existing.contains(&g.email.to_ascii_lowercase()));
        let model = Welcome {
            goa,
            added: Vec::new(),
            test_seq: 0,
            goa_imported: false,
            manual_added: false,
        };

        let carousel = adw::Carousel::new();
        carousel.set_interactive(false); // guided: buttons drive the flow
        carousel.set_hexpand(true);
        carousel.set_vexpand(true);

        // ---- The wordmark: lives ABOVE the carousel, on every page. It
        // starts hero-sized and mid-window; Get Started shrinks it to 100px
        // and floats it to the top (see WelcomeInput::Next), where it stays.
        // The positioning margins live on the box and the size on the Clamp,
        // so the entrance animation (which drives the clamp's own margin)
        // never fights the choreography.
        // Sizing a Picture directly is a trap twice over: width_request is a
        // floor, and the texture's natural size wins over the request under
        // centering. The reliable cap (see the palette's clip saga) is an
        // Overlay whose MAIN child is an empty spacer — an empty box's
        // natural size is exactly its size request, so the animation drives
        // the spacer and the clipped Picture simply fills whatever the
        // overlay was given.
        let wordmark_pic = wordmark_picture_of(Wordmark::WithIconWhite, 300);
        let wordmark_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wordmark_frame.set_size_request(
            HERO_SIZE as i32,
            wordmark_height_of(Wordmark::WithIconWhite, HERO_SIZE),
        );
        let wordmark_overlay = gtk::Overlay::new();
        wordmark_overlay.set_child(Some(&wordmark_frame));
        wordmark_overlay.add_overlay(&wordmark_pic);
        wordmark_overlay.set_clip_overlay(&wordmark_pic, true);
        wordmark_overlay.set_halign(gtk::Align::Center);
        let wordmark_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wordmark_box.set_halign(gtk::Align::Center);
        wordmark_box.set_margin_top(HERO_TOP);
        wordmark_box.append(&wordmark_overlay);

        // ---- Page 1: hero (tagline + start; the wordmark floats above) ----
        let hero = gtk::Box::new(gtk::Orientation::Vertical, 18);
        hero.set_halign(gtk::Align::Center);
        let tag = tagline(&i18n(
            "A clean, fast home for your mail.\nLet's set things up — it takes about a minute.",
        ));
        tag.set_margin_top(16);
        // The interface language, first of all (#179): System, English and
        // every shipped translation. A pick restarts Hylki into it, so the
        // rest of the wizard reads in the chosen language.
        let lang_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        lang_row.set_halign(gtk::Align::Center);
        // 64px below the tagline: the hero's 18px spacing plus this.
        lang_row.set_margin_top(46);
        let lang_label = gtk::Label::new(Some(i18n("Language").as_str()));
        lang_label.add_css_class("welcome-hint");
        lang_row.append(&lang_label);
        let choices = crate::ui::preferences::language_choices();
        let choice_labels: Vec<&str> = choices.iter().map(|(l, _)| l.as_str()).collect();
        let lang = gtk::DropDown::from_strings(&choice_labels);
        lang.set_valign(gtk::Align::Center);
        let current = crate::config::load_language();
        lang.set_selected(choices.iter().position(|(_, c)| *c == current).unwrap_or(0) as u32);
        {
            let s = sender.clone();
            lang.connect_selected_notify(move |d| {
                s.input_sender()
                    .emit(WelcomeInput::LanguageChanged(d.selected()))
            });
        }
        lang_row.append(&lang);
        // Fair warning: a pick restarts the app.
        let lang_note = gtk::Label::new(Some(
            i18n("Choosing another language restarts Hylki: it closes for a moment and this wizard comes back in that language.").as_str(),
        ));
        lang_note.add_css_class("welcome-hint");
        lang_note.add_css_class("caption");
        lang_note.set_wrap(true);
        lang_note.set_justify(gtk::Justification::Center);
        lang_note.set_max_width_chars(46);
        lang_note.set_margin_top(2);
        let start = pill(&i18n("Get Started"));
        start.set_margin_top(10);
        {
            let s = sender.clone();
            start.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::Next));
        }
        hero.append(&tag);
        hero.append(&lang_row);
        hero.append(&lang_note);
        hero.append(&start);
        let hero_page = page(&hero);
        hero_page.set_valign(gtk::Align::Start);
        hero_page.set_margin_top(6);
        carousel.append(&scrolled(&hero_page));
        entrance(&[
            wordmark_overlay.clone().upcast(),
            tag.clone().upcast(),
            lang_row.clone().upcast(),
            lang_note.clone().upcast(),
            start.clone().upcast(),
        ]);
        // HYLKI_SHOWCASE_WIZARD_LANG=<n> picks choice n on the drop-down
        // after 4 s, what a click there does (for testing the restart).
        if let Some(n) = std::env::var("HYLKI_SHOWCASE_WIZARD_LANG")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
        {
            let lang = lang.clone();
            gtk::glib::timeout_add_seconds_local_once(4, move || lang.set_selected(n));
        }

        // ---- Page 2: GNOME Online Accounts ----
        // The accounts already signed in on this computer, Google among
        // them (its only sign-in), one click each. Microsoft has its own
        // sign-in on the next page (#329). Nothing here is required: the
        // footer reads Skip until something is added.
        let goa_pg = gtk::Box::new(gtk::Orientation::Vertical, 14);
        let import = gtk::Image::from_icon_name("arrow-into-box-symbolic");
        import.set_pixel_size(36);
        import.add_css_class("welcome-check");
        goa_pg.append(&import);
        goa_pg.append(&title(&i18n("Import your online accounts")));
        goa_pg.append(&tagline(&i18n(
            "Mail accounts signed in under GNOME Settings → Online Accounts can be added here in one click. Google accounts are only supported through GNOME Online Accounts.",
        )));
        let goa_list = card();
        goa_list.set_visible(!model.goa.is_empty());
        // As far from the tagline as the buttons are from the list.
        goa_list.set_margin_top(GOA_GAP);
        goa_pg.append(&goa_list);
        let goa_empty = tagline(&i18n("No mail accounts were found in Online Accounts."));
        goa_empty.add_css_class("welcome-hint");
        goa_empty.set_visible(model.goa.is_empty());
        // The empty note sits as far from the tagline as the buttons sit
        // from it: 47px each to the eye, page spacing and margin together.
        goa_empty.set_margin_top(GOA_GAP);
        goa_pg.append(&goa_empty);
        // Hidden until an import reports, so an empty line never widens the
        // gap above the buttons.
        let goa_status = gtk::Label::new(None);
        goa_status.add_css_class("welcome-hint");
        goa_status.set_wrap(true);
        goa_status.set_justify(gtk::Justification::Center);
        goa_status.set_visible(false);
        goa_pg.append(&goa_status);
        let goa_links = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        goa_links.set_halign(gtk::Align::Center);
        // 4px more: a button's edge has none of a text line's leading.
        goa_links.set_margin_top(GOA_GAP + 4);
        let open_goa = gtk::Button::with_label(&i18n("Open Online Accounts…"));
        let rescan = gtk::Button::with_label(&i18n("Scan Again"));
        for b in [&open_goa, &rescan] {
            b.add_css_class("pill");
            b.add_css_class("welcome-secondary");
        }
        {
            let s = sender.clone();
            open_goa
                .connect_clicked(move |_| s.input_sender().emit(WelcomeInput::OpenOnlineAccounts));
            let s = sender.clone();
            rescan.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::RescanGoa));
        }
        goa_links.append(&open_goa);
        goa_links.append(&rescan);
        goa_pg.append(&goa_links);
        carousel.append(&scrolled(&page(&goa_pg)));

        // ---- Page 3: an account by hand ----
        let acct = gtk::Box::new(gtk::Orientation::Vertical, 14);
        let acct_title = title(&i18n("Add your email account"));
        acct.append(&acct_title);

        let form = card();
        let provider_row = adw::ComboRow::new();
        provider_row.set_title(&i18n("Provider"));
        let labels: Vec<&str> = wizard_providers().map(|p| p.wizard_label()).collect();
        provider_row.set_model(Some(&gtk::StringList::new(&labels)));
        // The providers' marks before their names, as in Settings.
        provider_row.set_factory(Some(&crate::ui::accounts::provider_factory()));
        // Default to the plain IMAP/POP3 entry.
        let manual = wizard_providers()
            .position(|p| p.wizard_is_manual())
            .unwrap_or(0);
        provider_row.set_selected(manual as u32);
        {
            let s = sender.clone();
            provider_row.connect_selected_notify(move |_| {
                s.input_sender().emit(WelcomeInput::ProviderChanged)
            });
        }
        let name_row = adw::EntryRow::new();
        name_row.set_title(&i18n("Your name"));
        let email_row = adw::EntryRow::new();
        email_row.set_title(&i18n("Email address"));
        let pass_row = adw::PasswordEntryRow::new();
        pass_row.set_title(&i18n("Password"));
        let server_exp = adw::ExpanderRow::new();
        server_exp.set_title(&i18n("Server details"));
        server_exp.set_subtitle(&i18n("Filled in for known providers"));
        // The default provider is the plain "IMAP/POP3 Account" entry,
        // whose whole point is filling these in — start them open. Picking a
        // known provider collapses them (ProviderChanged), picking the
        // generic one re-opens them.
        server_exp.set_expanded(true);
        let host_row = adw::EntryRow::new();
        host_row.set_title(&i18n("IMAP server"));
        let port_row = adw::EntryRow::new();
        port_row.set_title(&i18n("IMAP port"));
        port_row.set_text("993");
        let smtp_row = adw::EntryRow::new();
        smtp_row.set_title(&i18n("SMTP server"));
        let smtp_port_row = adw::EntryRow::new();
        smtp_port_row.set_title(&i18n("SMTP port"));
        smtp_port_row.set_text("587");
        server_exp.add_row(&host_row);
        server_exp.add_row(&port_row);
        server_exp.add_row(&smtp_row);
        server_exp.add_row(&smtp_port_row);
        // Custom OAuth's client, the same fields as the account editor's.
        let oauth_client_id_row = adw::EntryRow::new();
        oauth_client_id_row.set_title(&i18n("OAuth Client ID"));
        let oauth_secret_row = adw::PasswordEntryRow::new();
        oauth_secret_row.set_title(&i18n("OAuth Client Secret (optional)"));
        let oauth_auth_url_row = adw::EntryRow::new();
        oauth_auth_url_row.set_title(&i18n("Authorization URL"));
        let oauth_token_url_row = adw::EntryRow::new();
        oauth_token_url_row.set_title(&i18n("Token URL"));
        let oauth_scope_row = adw::EntryRow::new();
        oauth_scope_row.set_title(&i18n("Scopes (space-separated)"));
        form.append(&provider_row);
        form.append(&name_row);
        form.append(&email_row);
        form.append(&pass_row);
        for row in [
            oauth_client_id_row.upcast_ref::<gtk::Widget>(),
            oauth_secret_row.upcast_ref(),
            oauth_auth_url_row.upcast_ref(),
            oauth_token_url_row.upcast_ref(),
            oauth_scope_row.upcast_ref(),
        ] {
            row.set_visible(false);
            form.append(row);
        }
        form.append(&server_exp);
        acct.append(&form);

        let hint_lbl = gtk::Label::new(None);
        hint_lbl.add_css_class("welcome-hint");
        hint_lbl.set_halign(gtk::Align::Start);
        hint_lbl.set_wrap(true);
        hint_lbl.set_visible(false);
        acct.append(&hint_lbl);

        let add_btn = pill(&i18n("Test & Add"));
        add_btn.set_margin_top(4);
        {
            let s = sender.clone();
            add_btn.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::TestAndAdd));
        }
        acct.append(&add_btn);
        // The result line sits under the button, account name and verdict
        // together on one line — with a spinner while a test runs.
        let status_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_box.set_halign(gtk::Align::Center);
        let test_spinner = gtk::Spinner::new();
        test_spinner.set_visible(false);
        let status_lbl = gtk::Label::new(None);
        status_lbl.add_css_class("welcome-hint");
        status_lbl.set_wrap(true);
        status_lbl.set_justify(gtk::Justification::Center);
        status_box.append(&test_spinner);
        status_box.append(&status_lbl);
        acct.append(&status_box);

        // Full width, as the form was when the Online Accounts note beside
        // it set the page's width.
        let acct_page = page(&acct);
        acct_page.set_halign(gtk::Align::Fill);
        carousel.append(&scrolled(&acct_page));

        // ---- Page 4: privacy ----
        let priv_pg = gtk::Box::new(gtk::Orientation::Vertical, 14);
        priv_pg.append(&title(&i18n("Privacy, your way")));
        priv_pg.append(&tagline(&i18n("Hylki sends no telemetry, ever. These control what leaves your machine while you read.")));
        let priv_card = card();
        let sw_remote = adw::SwitchRow::new();
        sw_remote.set_title(&i18n("Block remote images"));
        sw_remote.set_subtitle(&i18n("Stops senders tracking when and where you open mail"));
        sw_remote.set_active(!crate::config::load_privacy().auto_remote_content);
        let sw_gravatar = adw::SwitchRow::new();
        sw_gravatar.set_title(&i18n("Fetch Gravatar portraits"));
        sw_gravatar.set_subtitle(&i18n("Asks gravatar.com about each sender's address"));
        sw_gravatar.set_active(crate::config::load_privacy().gravatar);
        let sw_logos = adw::SwitchRow::new();
        sw_logos.set_title(&i18n("Fetch sender logos"));
        sw_logos.set_subtitle(&i18n("Looks up company icons for the message list"));
        sw_logos.set_active(crate::config::load_privacy().sender_logos);
        let sw_notif = adw::SwitchRow::new();
        sw_notif.set_title(&i18n("Show message content in notifications"));
        sw_notif.set_subtitle(&i18n("Off keeps senders and subjects off the lock screen"));
        sw_notif.set_active(crate::config::load_privacy().notification_content);
        priv_card.append(&sw_remote);
        priv_card.append(&sw_gravatar);
        priv_card.append(&sw_logos);
        priv_card.append(&sw_notif);
        priv_pg.append(&priv_card);
        carousel.append(&scrolled(&page(&priv_pg)));

        // ---- Page 5: personalize ----
        let pers = gtk::Box::new(gtk::Orientation::Vertical, 14);
        pers.append(&title(&i18n("Make it yours")));
        pers.append(&tagline(&i18n(
            "A few popular choices — everything can be changed later in Settings.",
        )));
        let pers_card = card();
        let preview_row = adw::ComboRow::new();
        preview_row.set_title(&i18n("Preview lines"));
        preview_row.set_subtitle(&i18n("Message text shown under each subject"));
        preview_row.set_model(Some(&gtk::StringList::new(&[
            i18n("None").as_str(),
            i18n("1 line").as_str(),
            i18n("2 lines").as_str(),
        ])));
        preview_row.set_selected(crate::config::load_preview_lines().min(2));
        let sw_avatars = adw::SwitchRow::new();
        sw_avatars.set_title(&i18n("Sender avatars"));
        sw_avatars.set_subtitle(&i18n("Colorful initials beside each message"));
        sw_avatars.set_active(crate::config::load_privacy().avatars);
        let sw_threading = adw::SwitchRow::new();
        sw_threading.set_title(&i18n("Conversation view"));
        sw_threading.set_subtitle(&i18n("Group messages into threads"));
        sw_threading.set_active(crate::config::load_privacy().threading);
        pers_card.append(&preview_row);
        pers_card.append(&sw_avatars);
        pers_card.append(&sw_threading);
        pers.append(&pers_card);

        // The app icon: the whole set as one sideways-scrolling row on its
        // own card, the default ringed.
        let icon_hdr = gtk::Label::new(Some(i18n("Pick your app icon").as_str()));
        icon_hdr.add_css_class("welcome-section");
        icon_hdr.set_halign(gtk::Align::Start);
        icon_hdr.set_margin_top(6);
        pers.append(&icon_hdr);
        // With an icon set outside Hylki on the launcher (#252), nothing is
        // preselected, and only an actual pick replaces it.
        let icon_choice = std::rc::Rc::new(std::cell::RefCell::new(
            if crate::app_icon::custom_icon().is_some() {
                String::new()
            } else {
                crate::config::load_app_icon()
                    .unwrap_or_else(|| crate::app_icon::DEFAULT_ID.to_string())
            },
        ));
        let icon_card = card();
        let icon_row = gtk::ListBoxRow::new();
        icon_row.set_activatable(false);
        icon_row.set_focusable(false);
        let strip = {
            let choice = icon_choice.clone();
            crate::ui::icon_picker::strip(
                &icon_choice.borrow(),
                52,
                std::rc::Rc::new(move |id: &str| *choice.borrow_mut() = id.to_string()),
            )
        };
        strip.set_margin_top(8);
        strip.set_margin_start(6);
        strip.set_margin_end(6);
        icon_row.set_child(Some(&strip));
        icon_card.append(&icon_row);
        pers.append(&icon_card);
        carousel.append(&scrolled(&page(&pers)));

        // ---- Page 6: done ----
        let done = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let check = gtk::Image::from_icon_name("verified-checkmark-symbolic");
        check.set_pixel_size(72);
        check.add_css_class("welcome-check");
        done.append(&check);
        done.append(&title(&i18n("You're all set")));
        let done_sub = tagline("");
        done_sub.set_use_markup(true);
        done_sub.set_markup(&i18n_f(
            "Enjoy! If you encounter an issue or have a feature request,\nplease open an issue on our <a href=\"{url}\">Github</a>.",
            &[("url", "https://github.com/hyprlab/hylki")],
        ));
        done.append(&done_sub);
        carousel.append(&scrolled(&page(&done)));

        // ---- Chrome: draggable top strip with Back button ----
        let tv = adw::ToolbarView::new();
        let hb = adw::HeaderBar::new();
        hb.set_show_title(false);
        hb.add_css_class("flat");
        let back_btn = gtk::Button::from_icon_name("go-previous-symbolic");
        back_btn.add_css_class("flat");
        back_btn.set_visible(false);
        {
            let s = sender.clone();
            back_btn.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::Back));
        }
        hb.pack_start(&back_btn);
        tv.add_top_bar(&hb);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&wordmark_box);
        content.append(&carousel);
        // Fixed footer: Continue lives with the dots so it never scrolls
        // away with a tall page, and reads Finish on the last one. Hidden on
        // the hero page, which has its own Get Started.
        let footer_next = pill(&i18n("Continue"));
        footer_next.set_visible(false);
        footer_next.set_margin_top(16);
        footer_next.set_margin_bottom(8);
        {
            let s = sender.clone();
            footer_next.connect_clicked(move |_| s.input_sender().emit(WelcomeInput::Next));
        }
        content.append(&footer_next);
        let dots = adw::CarouselIndicatorDots::new();
        dots.set_carousel(Some(&carousel));
        dots.set_margin_bottom(14);
        content.append(&dots);
        {
            let btn = footer_next.clone();
            let s = sender.clone();
            carousel.connect_position_notify(move |car| {
                let r = car.position().round() as u32;
                btn.set_visible(r >= 1);
                s.input_sender().emit(WelcomeInput::RefreshFooter);
            });
        }
        // Typing in the manual form turns its Skip into Continue.
        for entry in [
            name_row.upcast_ref::<gtk::Editable>(),
            email_row.upcast_ref(),
            pass_row.upcast_ref(),
            host_row.upcast_ref(),
            smtp_row.upcast_ref(),
            oauth_client_id_row.upcast_ref(),
            oauth_secret_row.upcast_ref(),
            oauth_auth_url_row.upcast_ref(),
            oauth_token_url_row.upcast_ref(),
            oauth_scope_row.upcast_ref(),
        ] {
            let s = sender.clone();
            entry.connect_changed(move |_| s.input_sender().emit(WelcomeInput::RefreshFooter));
        }
        tv.set_content(Some(&content));
        root.set_content(Some(&tv));

        let widgets = WelcomeWidgets {
            carousel,
            back_btn,
            wordmark_frame,
            wordmark_box,
            provider_row,
            hint_lbl,
            name_row,
            email_row,
            pass_row,
            server_exp,
            host_row,
            port_row,
            smtp_row,
            smtp_port_row,
            oauth_client_id_row,
            oauth_secret_row,
            oauth_auth_url_row,
            oauth_token_url_row,
            oauth_scope_row,
            status_lbl,
            test_spinner,
            add_btn,
            goa_empty,
            goa_list,
            goa_status,
            footer_next,
            acct_title,
            sw_remote,
            sw_gravatar,
            sw_logos,
            sw_notif,
            preview_row,
            sw_avatars,
            sw_threading,
            icon_choice,
        };
        rebuild_goa_rows(&widgets.goa_list, &model.goa, &sender);
        bind_wordmark_to_position(
            &widgets.carousel,
            &widgets.wordmark_frame,
            &widgets.wordmark_box,
        );

        // Screenshot hook, mirroring the main window's: HYLKI_SHOWCASE=path
        // captures the wizard after a beat; HYLKI_SHOWCASE_PAGE=N walks to
        // that page first.
        if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
            let win = root.clone();
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(4, move || {
                // Walk pages via Next so the capture includes everything a
                // real click drives (the wordmark choreography included).
                let n = std::env::var("HYLKI_SHOWCASE_PAGE")
                    .unwrap_or_default()
                    .parse::<u32>()
                    .unwrap_or(0);
                // Spaced out so each Next lands after the previous scroll
                // settles (position() reads fractional mid-scroll).
                for i in 0..n {
                    let s = s.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(700 * i as u64),
                        move || s.input_sender().emit(WelcomeInput::Next),
                    );
                }
                let win = win.clone();
                let path = path.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(2000 + 700 * n as u64),
                    move || {
                        crate::app::showcase_capture(win.upcast_ref(), &path);
                    },
                );
            });
        }
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            WelcomeInput::LanguageChanged(i) => self.on_language_changed(i, sender),
            WelcomeInput::Next => self.on_next(widgets, sender),
            WelcomeInput::Back => self.on_back(widgets),
            WelcomeInput::ProviderChanged => self.on_provider_changed(widgets),
            WelcomeInput::TestAndAdd => self.on_test_and_add(widgets, sender),
            WelcomeInput::ImportGoa(index) => self.on_import_goa(index, widgets, sender),
            WelcomeInput::RescanGoa => self.on_rescan_goa(widgets, sender),
            WelcomeInput::OpenOnlineAccounts => crate::ui::accounts::open_online_accounts(),
            WelcomeInput::RefreshFooter => self.refresh_footer(widgets),
            WelcomeInput::Finish => self.on_finish(widgets, sender, root),
        }
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match message {
            WelcomeCmd::Tested {
                account,
                result,
                seq,
            } => self.on_tested(account, result, seq, widgets, sender),
            WelcomeCmd::SignedIn {
                account,
                result,
                seq,
            } => self.on_signed_in(account, result, seq, widgets, sender),
        }
    }
}

impl Welcome {
    /// The footer button reads Skip on the two account pages while nothing
    /// was added there and, on the manual page, nothing is typed in the
    /// form; Finish on the last page; Continue everywhere else.
    fn refresh_footer(&self, widgets: &WelcomeWidgets) {
        let page = widgets.carousel.position().round() as u32;
        let label = if page + 1 >= widgets.carousel.n_pages() {
            i18n("Finish")
        } else if match page {
            1 => !self.goa_imported,
            2 => !self.manual_added && !form_has_input(widgets),
            _ => false,
        } {
            i18n("Skip")
        } else {
            i18n("Continue")
        };
        widgets.footer_next.set_label(&label);
    }
}

/// A fully-defaulted account for the wizard's manual form to build on.
pub(crate) fn blank_account() -> AccountConfig {
    AccountConfig {
        name: String::new(),
        email: String::new(),
        protocol: Default::default(),
        imap_host: String::new(),
        imap_port: 993,
        smtp_host: String::new(),
        smtp_port: 587,
        username: String::new(),
        password: String::new(),
        smtp_separate: false,
        jmap_token: false,
        jmap_generic: false,
        tls_accept_hostname_mismatch: false,
        security: None,
        smtp_username: String::new(),
        smtp_password: String::new(),
        color: None,
        emoji: None,
        avatar: None,
        gravatar: false,
        signature: None,
        signature_html: false,
        signatures: Vec::new(),
        default_signature: None,
        label: None,
        aliases: Vec::new(),
        enabled: true,
        goa_id: None,
        goa_mail_disabled: false,
        goa_enabled_before_mail_disabled: true,
        oauth: false,
        oauth_settings: None,
        oauth_refresh: String::new(),
        push: None,
        folder_roles: Default::default(),
        hidden_folders: Vec::new(),
        folders_seeded: false,
        sent_copy_path: None,
        server_saves_sent: false,
        reply_copies: Default::default(),
        empty_junk_days: 0,
        empty_trash_days: 0,
        clean_lists_days: 0,
        pgp_key: None,
        in_unified: true,
        folder_sort: None,
        sign_by_default: false,
        plain_subject: false,
        graph_directory: false,
        graph_directory_only: false,
        graph_label_names: false,
    }
}

#[cfg(test)]
mod tests {
    use super::blank_account;

    #[test]
    fn a_blank_account_has_the_form_defaults() {
        let a = blank_account();
        assert_eq!((a.imap_port, a.smtp_port), (993, 587));
        assert_eq!(a.protocol, crate::config::Protocol::Imap);
        assert!(a.enabled && a.in_unified && a.goa_enabled_before_mail_disabled);
        assert!(!a.smtp_separate && !a.oauth && !a.gravatar && !a.server_saves_sent);
        assert!(!a.jmap_token && !a.jmap_generic && !a.tls_accept_hostname_mismatch);
        assert!(!a.sign_by_default && !a.plain_subject && !a.folders_seeded);
        assert!(!a.graph_directory && !a.graph_directory_only && !a.graph_label_names);
        assert!(!a.goa_mail_disabled && !a.signature_html);
        assert!(a.name.is_empty() && a.email.is_empty() && a.username.is_empty());
        assert!(a.imap_host.is_empty() && a.smtp_host.is_empty());
        assert!(a.password.is_empty() && a.smtp_password.is_empty());
        assert!(a.smtp_username.is_empty() && a.oauth_refresh.is_empty());
        assert!(a.goa_id.is_none() && a.push.is_none() && a.security.is_none());
        assert!(a.color.is_none() && a.emoji.is_none() && a.avatar.is_none());
        assert!(a.signature.is_none() && a.default_signature.is_none());
        assert!(a.label.is_none() && a.oauth_settings.is_none() && a.pgp_key.is_none());
        assert!(a.sent_copy_path.is_none() && a.folder_sort.is_none());
        assert!(a.signatures.is_empty() && a.aliases.is_empty() && a.hidden_folders.is_empty());
        assert_eq!(
            (a.empty_junk_days, a.empty_trash_days, a.clean_lists_days),
            (0, 0, 0)
        );
    }
}
