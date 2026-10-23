//! Taking a queued or saved message apart again for editing, and putting
//! back the Bcc a sent copy or a retry needs.

use crate::worker::compose::recipients::mailbox;
use crate::worker::render::attachments::attachment_parts;
use crate::worker::render::body::{escape_html, inline_cid_images};
use crate::worker::{format_recipient, parse_recipients};

/// Render an address header back into the form the composer's fields use —
/// `Ada Lovelace <ada@example.com>, bob@example.com` — keeping display names so
/// editing a queued message doesn't reduce everyone to a bare address.
pub(in crate::worker) fn addr_list(header: Option<&mail_parser::Address>) -> String {
    header
        .map(|a| {
            a.iter()
                .filter_map(|addr| {
                    let email = addr.address()?.trim();
                    if email.is_empty() {
                        return None;
                    }
                    Some(format_recipient(addr.name().unwrap_or_default(), email))
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// A queued message taken apart for editing: the fields a composer needs, plus
/// its attachments as bytes.
pub struct EditableMessage {
    /// The From as written: "Name <address>", or the bare address.
    pub from: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// Threading headers, stored the way an outgoing message carries them:
    /// without angle brackets, References space-separated.
    pub in_reply_to: String,
    pub references: String,
    /// The best body to edit: the HTML alternative when there is one, otherwise
    /// the plain text escaped into HTML (the composer edits HTML). Pictures
    /// pasted into the text come back as the `data:` URIs they were written as.
    pub body_html: String,
    /// The files, without the pictures already back in the text.
    pub attachments: Vec<crate::models::Attachment>,
}

/// Whoever the envelope names that none of the `headers` (address lists)
/// do: the Bcc recipients of bytes lettre stripped them from.
fn unnamed_rcpts(envelope_rcpts: &[String], headers: &[&str]) -> Vec<String> {
    let named: Vec<String> = headers
        .iter()
        .flat_map(|h| parse_recipients(h))
        .map(|(_, addr)| addr.to_lowercase())
        .collect();
    envelope_rcpts
        .iter()
        .filter(|addr| !named.contains(&addr.to_lowercase()))
        .cloned()
        .collect()
}

/// The bytes a sent copy is filed as (#476): the sent bytes with a `Bcc`
/// header naming `bcc`. lettre strips Bcc from what goes over SMTP, which is
/// right for the recipients and wrong for the sender's own copy, the only
/// place left that says who else was sent it. Header order is free, so it
/// goes first.
pub(crate) fn with_bcc(raw: &[u8], bcc: &str) -> Vec<u8> {
    let mailboxes: lettre::message::Mailboxes = parse_recipients(bcc)
        .into_iter()
        .filter_map(|(name, addr)| mailbox(&name, &addr).ok())
        .collect();
    if mailboxes.iter().next().is_none() {
        return raw.to_vec();
    }
    let mut headers = lettre::message::header::Headers::new();
    headers.set(lettre::message::header::Bcc::from(mailboxes));
    let mut out = headers.to_string().into_bytes();
    out.extend_from_slice(raw);
    out
}

/// [`with_bcc`] for a queued message, whose Bcc recipients survive only in
/// the stored envelope.
pub(crate) fn queued_with_bcc(raw: &[u8], envelope_rcpts: &[String]) -> Vec<u8> {
    let parsed = mail_parser::MessageParser::default().parse_headers(raw);
    let to = parsed
        .as_ref()
        .map(|p| addr_list(p.to()))
        .unwrap_or_default();
    let cc = parsed
        .as_ref()
        .map(|p| addr_list(p.cc()))
        .unwrap_or_default();
    let bcc = parsed
        .as_ref()
        .map(|p| addr_list(p.bcc()))
        .unwrap_or_default();
    with_bcc(
        raw,
        &unnamed_rcpts(envelope_rcpts, &[&to, &cc, &bcc]).join(", "),
    )
}

/// Take a queued message or a saved draft apart so it can be edited again.
///
/// The stored bytes are the source of truth, but they are not quite the whole
/// message: lettre strips `Bcc` from what goes on the wire, so a queued
/// message's Bcc recipients survive only in the stored envelope. Anything in
/// the envelope that isn't in To or Cc is therefore a Bcc recipient, and is
/// restored as one. A draft keeps its `Bcc` header and has no envelope.
pub fn editable_from_raw(raw: &[u8], envelope_rcpts: &[String]) -> EditableMessage {
    use mail_parser::{HeaderValue, MessageParser, MimeHeaders};

    let parsed = MessageParser::default().parse(raw);
    let from = parsed
        .as_ref()
        .and_then(|p| p.from())
        .and_then(|a| a.first())
        .and_then(|a| {
            Some(format_recipient(
                a.name().unwrap_or_default(),
                a.address()?.trim(),
            ))
        })
        .unwrap_or_default();
    let to = parsed
        .as_ref()
        .map(|p| addr_list(p.to()))
        .unwrap_or_default();
    let cc = parsed
        .as_ref()
        .map(|p| addr_list(p.cc()))
        .unwrap_or_default();
    let subject = parsed
        .as_ref()
        .and_then(|p| p.subject())
        .unwrap_or_default()
        .to_string();
    let ids = |hv: &HeaderValue| -> String {
        let strip = |t: &str| {
            t.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .trim()
                .to_string()
        };
        match hv {
            HeaderValue::Text(t) => strip(t),
            HeaderValue::TextList(v) => v.iter().map(|t| strip(t)).collect::<Vec<_>>().join(" "),
            _ => String::new(),
        }
    };
    let in_reply_to = parsed
        .as_ref()
        .map(|p| ids(p.in_reply_to()))
        .unwrap_or_default();
    let references = parsed
        .as_ref()
        .map(|p| ids(p.references()))
        .unwrap_or_default();

    let header_bcc = parsed
        .as_ref()
        .map(|p| addr_list(p.bcc()))
        .unwrap_or_default();
    let bcc = std::iter::once(header_bcc.clone())
        .filter(|b| !b.is_empty())
        .chain(unnamed_rcpts(envelope_rcpts, &[&to, &cc, &header_bcc]))
        .collect::<Vec<_>>()
        .join(", ");

    // Pictures pasted into the text went out as `cid:` parts (#113). They
    // come back into the text, and off the list of files, or the composer
    // would show a broken image and attach the picture a second time.
    let mut embedded: Vec<String> = Vec::new();
    let html = parsed
        .as_ref()
        .and_then(|p| {
            let html = p.body_html(0)?;
            let mut budget = 16 * 1024 * 1024;
            Some(inline_cid_images(
                &html,
                &p.parts,
                &mut budget,
                &mut embedded,
            ))
        })
        .unwrap_or_default();
    let body_html = if html.trim().is_empty() {
        let text = parsed
            .as_ref()
            .and_then(|p| p.body_text(0).map(|b| b.to_string()))
            .unwrap_or_default();
        // The composer edits HTML, so plain text has to be escaped and its line
        // breaks preserved — otherwise the whole body collapses into one line.
        text.lines()
            .map(|line| format!("<div>{}</div>", escape_html(line)))
            .collect::<Vec<_>>()
            .join("")
    } else {
        html
    };

    let attachments = parsed
        .as_ref()
        .map(|p| {
            attachment_parts(p, raw)
                .into_iter()
                .filter(|(id, _)| {
                    !p.parts
                        .get(*id)
                        .and_then(|part| part.content_id())
                        .is_some_and(|cid| embedded.iter().any(|e| e == cid))
                })
                .map(|(_, a)| a)
                .collect()
        })
        .unwrap_or_default();

    EditableMessage {
        from,
        to,
        cc,
        bcc,
        subject,
        in_reply_to,
        references,
        body_html,
        attachments,
    }
}
