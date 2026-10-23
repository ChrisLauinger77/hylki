//! Sending and saving: the From, the outgoing message, OpenPGP checks before a
//! send, and Send Later.

use super::messages::ComposeInput;
use super::types::ComposeAccount;
use super::Compose;
use super::ComposeWidgets;
use crate::config::ComposeFormat;
use crate::i18n::{i18n, i18n_f};
use crate::worker::OutgoingMessage;
use adw::prelude::*;
use relm4::prelude::*;

/// How often an edited message is saved to Drafts on its own (#340).
pub(super) const AUTOSAVE_SECS: u32 = 30;

/// An identity's From as the composer writes it out: "Name <address>".
pub(super) fn identity_display(a: &ComposeAccount) -> String {
    a.alias_from
        .clone()
        .unwrap_or_else(|| a.label.clone())
        .trim()
        .to_string()
}

/// Whether two Froms name the same person at the same address, however
/// each is quoted or spaced.
pub(super) fn same_from(a: &str, b: &str) -> bool {
    let one = |s: &str| {
        crate::worker::parse_recipients(s)
            .into_iter()
            .next()
            .map(|(n, e)| (n.trim().trim_matches('"').to_string(), e.to_lowercase()))
    };
    matches!((one(a), one(b)), (Some(x), Some(y)) if x == y)
}

/// Whether an OpenPGP send can go ahead (#133): a key of the user's own
/// for the From address (or the account's chosen key), and, to encrypt, a
/// public key for every recipient. The message names the first gap.
pub(super) fn pgp_send_check(
    from: &str,
    chosen_key: Option<&str>,
    fields: &[&str],
    encrypt: bool,
) -> Result<(), String> {
    let gpg = crate::pgp::Gpg::system();
    let own = chosen_key
        .and_then(|f| crate::pgp::secret_key_by_fingerprint(&gpg, f))
        .or_else(|| crate::pgp::secret_key_for(&gpg, from));
    if own.is_none() {
        return Err(crate::i18n::i18n_f(
            "There is no OpenPGP key of your own for {addr}. Generate one under Settings, OpenPGP, \
             or choose a key in the account's settings.",
            &[("addr", from)],
        ));
    }
    if encrypt {
        for field in fields {
            for (_, addr) in crate::worker::parse_recipients(field) {
                let addr = addr.as_str();
                if addr.is_empty() {
                    continue;
                }
                if crate::pgp::public_key_for(&gpg, addr).is_none() {
                    return Err(crate::i18n::i18n_f(
                        "There is no OpenPGP key for {addr}. Ask them for their public key, or fetch it \
                         under Settings, OpenPGP.",
                        &[("addr", addr)],
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Send Later presets (#145): `days` from today at `hour`:00, local time.
pub(super) fn preset_time(days: i64, hour: u32) -> i64 {
    use chrono::{Duration, Local, TimeZone};
    let day = (Local::now() + Duration::days(days)).date_naive();
    let ndt = day.and_hms_opt(hour, 0, 0).unwrap_or_default();
    Local
        .from_local_datetime(&ndt)
        .single()
        .map(|t| t.timestamp())
        .unwrap_or_else(crate::datefmt::now)
}

/// The coming Monday at `hour`:00 local time (a Monday today means next week's).
pub(super) fn next_monday(hour: u32) -> i64 {
    use chrono::Datelike;
    let today = chrono::Local::now().weekday().num_days_from_monday() as i64;
    let ahead = (7 - today) % 7;
    preset_time(if ahead == 0 { 7 } else { ahead }, hour)
}

/// The Send Later picker (#145): a calendar and an hour/minute pair, starting
/// from the scheduled time if there is one, else the next full hour. A time
/// already past is refused rather than queued to go at once by surprise.
pub(super) fn pick_send_time(
    parent: Option<&gtk::Window>,
    current: Option<i64>,
    sender: relm4::Sender<ComposeInput>,
) {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    let start = match current {
        Some(t) => Local
            .timestamp_opt(t, 0)
            .single()
            .unwrap_or_else(Local::now),
        None => {
            let n = Local::now() + chrono::Duration::hours(1);
            n.with_minute(0).and_then(|n| n.with_second(0)).unwrap_or(n)
        }
    };
    let dialog = adw::MessageDialog::new(parent, Some(i18n("Send later").as_str()), None);
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("ok", &i18n("Schedule"));
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let calendar = gtk::Calendar::new();
    calendar.set_year(start.year());
    calendar.set_month(start.month0() as i32);
    calendar.set_day(start.day() as i32);
    bx.append(&calendar);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_halign(gtk::Align::Center);
    let hour = gtk::SpinButton::with_range(0.0, 23.0, 1.0);
    hour.set_value(start.hour() as f64);
    hour.set_orientation(gtk::Orientation::Vertical);
    hour.set_wrap(true);
    let minute = gtk::SpinButton::with_range(0.0, 55.0, 5.0);
    minute.set_value((start.minute() / 5 * 5) as f64);
    minute.set_orientation(gtk::Orientation::Vertical);
    minute.set_wrap(true);
    row.append(&hour);
    row.append(&gtk::Label::new(Some(":")));
    row.append(&minute);
    bx.append(&row);
    dialog.set_extra_child(Some(&bx));
    let chosen = move || -> Option<i64> {
        let d = calendar.date();
        let ndt =
            chrono::NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32)?
                .and_hms_opt(hour.value_as_int() as u32, minute.value_as_int() as u32, 0)?;
        Local
            .from_local_datetime(&ndt)
            .single()
            .map(|t| t.timestamp())
    };
    dialog.connect_response(None, move |dlg, resp| {
        if resp != "ok" {
            return;
        }
        match chosen() {
            Some(t) if t > crate::datefmt::now() => {
                let _ = sender.send(ComposeInput::SendAt(t));
            }
            _ => {
                // An explicit time in the past is a slip, not a request to
                // send at once.
                let d = adw::MessageDialog::new(
                    dlg.transient_for().as_ref(),
                    Some(i18n("That time has passed").as_str()),
                    Some(i18n("Choose a time later than now.").as_str()),
                );
                d.add_response("ok", &i18n("OK"));
                d.present();
            }
        }
    });
    dialog.present();
}

impl Compose {
    /// The From to put on the wire: the picked identity's (`None` for the
    /// account's own), or the one typed in (#347), which has to hold one
    /// address.
    pub(super) fn chosen_from(&self, widgets: &ComposeWidgets) -> Result<Option<String>, String> {
        let idx = widgets.from_row.selected() as usize;
        let picked = self.accounts.get(idx).and_then(|a| a.alias_from.clone());
        if !self.custom_from {
            return Ok(picked);
        }
        let typed = widgets.custom_from_row.text().trim().to_string();
        let found = crate::worker::parse_recipients(&typed);
        let [(name, addr)] = found.as_slice() else {
            return Err(i18n(
                "The From has to be one address, such as Ann <ann@example.com>.",
            ));
        };
        let at = addr.split_once('@');
        if at.is_none_or(|(user, host)| {
            user.is_empty() || !host.contains('.') || addr.contains(char::is_whitespace)
        }) {
            return Err(i18n_f(
                "“{address}” is not an email address.",
                &[("address", addr)],
            ));
        }
        // Left as the picked identity reads: send as that.
        if self
            .accounts
            .get(idx)
            .is_some_and(|a| same_from(&identity_display(a), &typed))
        {
            return Ok(picked);
        }
        Ok(Some(if name.trim().is_empty() {
            addr.clone()
        } else {
            format!("{} <{addr}>", name.trim())
        }))
    }

    /// Read the body out of the editor in `format`'s own terms: the source
    /// text for a source format (with no plain-text rendering, since the
    /// source *is* text), the document's HTML and text otherwise.
    ///
    /// `for_send` picks the reader that recuts pictures armed for it; a
    /// draft save must never cost quality, so it passes false.
    pub(super) fn read_body_as(
        &self,
        format: ComposeFormat,
        for_send: bool,
        cb: impl FnOnce(String, String) + 'static,
    ) {
        if format.is_source() {
            self.editor
                .extract_source(move |src| cb(src, String::new()));
        } else if for_send {
            self.editor.extract_for_send(cb);
        } else {
            self.editor.extract(cb);
        }
    }

    /// The `(html, text)` parts of the outgoing message, from what the
    /// editor handed back. Markdown is rendered and keeps its source as the
    /// text part — Markdown being, deliberately, plain text that reads as
    /// itself. Hand-written HTML is sanitized and gets a Markdown rendering
    /// as its text part. Plain text sends no HTML part at all (#180).
    pub(super) fn outgoing_body(&self, body: String, text: String) -> (String, String) {
        match self.format {
            ComposeFormat::Rich => (body, text),
            ComposeFormat::Plain => (String::new(), text),
            ComposeFormat::Markdown => {
                let html = crate::markdown::sanitize_outgoing(&crate::markdown::to_html(&body));
                (html, body)
            }
            ComposeFormat::Html => {
                let text = crate::markdown::html_to_text(&body);
                (crate::markdown::sanitize_outgoing(&body), text)
            }
        }
    }

    /// Assemble an [`OutgoingMessage`] from the composed fields + attachments,
    /// carrying the draft origin so a saved/sent draft replaces its predecessor.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_outgoing(
        &self,
        from_account_id: u32,
        from_alias: Option<String>,
        to: String,
        cc: String,
        bcc: String,
        reply_to: String,
        subject: String,
        text: String,
        html: String,
    ) -> OutgoingMessage {
        OutgoingMessage {
            from_account_id,
            from_alias,
            to,
            cc,
            bcc,
            reply_to,
            subject,
            body: text,
            html,
            attachments: self
                .attachments
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            forwards: self.forwards.clone(),
            draft_origin: self.draft_origin.clone(),
            outbox_origin: self.outbox_origin,
            sign: self.sign,
            encrypt: self.encrypt,
            calendar: None,
            send_at: self.send_at,
        }
    }
}
