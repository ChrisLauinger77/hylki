//! Recipient fields as the composer types them: parsing, cleaning pasted
//! addresses, and the lettre mailboxes they become.

use lettre::message::Mailbox;
use lettre::Address;

use crate::worker::smtp::SmtpError;

/// Parse a recipient field ("Name <a@b>, c@d") into (name, email) pairs.
///
/// A comma inside a quoted name (`"Martin, Jason" <j@x>`) or inside the
/// brackets is not a separator. Nor is one in an unquoted name, which is what
/// a pasted or hand-typed `Martin, Jason <j@x>` has: a piece with no `@` that
/// runs into a `Name <addr>` is the front of that name.
pub fn parse_recipients(s: &str) -> Vec<(String, String)> {
    let mut pieces: Vec<String> = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut escaped, mut angle) = (false, false, false);
    for ch in s.chars() {
        match ch {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' if !quoted && !angle => {
                pieces.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    pieces.push(cur);

    let mut out = Vec::new();
    let mut carry = String::new();
    for (i, piece) in pieces.iter().enumerate() {
        let part = if carry.is_empty() {
            piece.trim().to_string()
        } else {
            format!("{carry},{piece}").trim().to_string()
        };
        carry.clear();
        if part.is_empty() {
            continue;
        }
        let next_is_named = pieces.get(i + 1).is_some_and(|n| n.contains('<'));
        if !part.contains('@') && next_is_named {
            carry = part;
            continue;
        }
        match (part.rfind('<'), part.rfind('>')) {
            (Some(lt), Some(gt)) if lt < gt => {
                out.push((unquote_name(&part[..lt]), clean_address(&part[lt + 1..gt])));
            }
            _ => out.push((String::new(), clean_address(&part))),
        }
    }
    out
}

/// Characters that take no space on screen: zero-width spaces and joiners,
/// direction marks and embeddings, the BOM and the soft hyphen.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

/// An address as typed or pasted, without the invisible characters a copy
/// from a web page or a chat brings along (#368). The server refuses an
/// address with one inside, and nobody can see why. A display name keeps
/// its characters: a zero-width non-joiner is part of how a Persian name is
/// spelled.
fn clean_address(raw: &str) -> String {
    raw.chars()
        .filter(|&c| !is_invisible(c))
        .map(|c| {
            if matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}') {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// The field with the invisible characters taken out of its addresses, when
/// it had any: the composer shows the cleaned text before it sends.
pub fn clean_recipient_field(field: &str) -> Option<String> {
    if !field
        .chars()
        .any(|c| is_invisible(c) || matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}'))
    {
        return None;
    }
    let clean = parse_recipients(field)
        .iter()
        .map(|(name, addr)| format_recipient(name, addr))
        .collect::<Vec<_>>()
        .join(", ");
    (clean != field.trim()).then_some(clean)
}

/// The first address in a recipient field that cannot be sent to, so the
/// composer can say so while the message is still open.
pub fn invalid_recipient(field: &str) -> Option<String> {
    parse_recipients(field)
        .into_iter()
        .map(|(_, addr)| addr)
        .find(|addr| addr.parse::<Address>().is_err())
}

/// A display name as written in a header, without its quotes and escapes.
fn unquote_name(raw: &str) -> String {
    let raw = raw.trim();
    let Some(inner) = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return raw.trim_matches('"').trim().to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' {
            chars.next().unwrap_or(c)
        } else {
            c
        });
    }
    out.trim().to_string()
}

/// "Name <addr>" for a recipient field (the address alone when there is no
/// name), quoting a name that has a comma or another character RFC 5322 keeps
/// out of a bare name, so [`parse_recipients`] reads it back whole.
pub fn format_recipient(name: &str, email: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name == email {
        return email.to_string();
    }
    if name.contains(|c: char| "()<>[]:;@\\,.\"".contains(c)) {
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\" <{email}>")
    } else {
        format!("{name} <{email}>")
    }
}

/// Build a `Name <addr>` mailbox from its parts. Never format the two into one
/// string and parse that back: an RFC 5322 display name has to be quoted unless
/// it is a bare atom, so "Alfonso Lizárraga", "Martin, Jason" or a name that is
/// itself an address ("a@b.com <a@b.com>", which is what an import with no
/// separate display name produces) all fail to parse and the send is rejected.
/// `Mailbox` quotes and encodes the name itself when the header is written.
pub(in crate::worker) fn mailbox(name: &str, addr: &str) -> Result<Mailbox, SmtpError> {
    let address: Address = addr
        .parse()
        .map_err(|e| format!("invalid email address {addr:?}: {e}"))?;
    let name = name.trim();
    // A "display name" that just repeats the address is noise in the header.
    if name.is_empty() || name.eq_ignore_ascii_case(addr.trim()) {
        Ok(Mailbox::new(None, address))
    } else {
        Ok(Mailbox::new(Some(name.to_string()), address))
    }
}
