//! Header values for display: RFC 2047 decoding, quoted display names, and
//! the date labels list rows show.

/// Format a unix timestamp the same way [`format_date`] labels mail dates.
pub(super) fn label_from_timestamp(ts: i64) -> String {
    let now = crate::datefmt::now();
    if crate::datefmt::day_key(ts) == crate::datefmt::day_key(now) {
        crate::datefmt::time(ts)
    } else if crate::datefmt::year(ts) == crate::datefmt::year(now) {
        crate::datefmt::day_month(ts)
    } else {
        crate::datefmt::day_month_year(ts)
    }
}

/// Undo an IMAP quoted string's backslash escapes. imap-proto hands ENVELOPE
/// strings back as they were on the wire, so a server that keeps the header's
/// own quotes sends `"\"Sender Name\""` and the backslashes would reach the
/// list (#312).
pub(super) fn unescape_quoted(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter().copied().peekable();
    while let Some(b) = bytes.next() {
        // Only the two escapes a quoted string has: a backslash before
        // anything else came in a literal and is the name's own.
        match bytes.peek() {
            Some(&next @ (b'\\' | b'"')) if b == b'\\' => {
                out.push(next);
                bytes.next();
            }
            _ => out.push(b),
        }
    }
    out
}

/// [`unescape_quoted`] for a string already stored.
pub(crate) fn unescape_quoted_str(s: &str) -> String {
    if s.contains('\\') {
        String::from_utf8_lossy(&unescape_quoted(s.as_bytes())).into_owned()
    } else {
        s.to_string()
    }
}

/// A display name without the quotes its header wrapped it in. RFC 3501 has
/// the server give the phrase alone, but some pass `"Sender Name"` through
/// whole (#312). Called on stored names too, which may still carry the
/// escapes an earlier build kept.
pub(crate) fn clean_display_name(name: &str) -> String {
    let name = unescape_quoted_str(name);
    let trimmed = name.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|n| n.strip_suffix('"'))
        .map(str::trim)
        .unwrap_or(trimmed)
        .to_string()
}

pub(super) fn bytes_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Decode a header value that may be RFC 2047 encoded ("=?UTF-8?…?=").
///
/// Some senders (notably Mailchimp) emit a single encoded-word far longer than
/// RFC 2047's 75-character limit — e.g. The Marginalian's newsletter subjects.
/// The decoder aborts on over-long words by default, which would leave the raw
/// `=?utf-8?Q?…?=` gibberish in the UI; we ask it to decode them anyway, as
/// Apple Mail and Thunderbird do.
pub(crate) fn decode_header(raw: &[u8]) -> String {
    use rfc2047_decoder::{Decoder, RecoverStrategy};
    let decode = |bytes: &[u8]| -> Option<String> {
        Decoder::new()
            .too_long_encoded_word_strategy(RecoverStrategy::Decode)
            .decode(bytes)
            .ok()
    };
    let first = decode(raw).unwrap_or_else(|| String::from_utf8_lossy(raw).into_owned());
    if !first.contains("=?") {
        return first;
    }
    // Still carrying raw encoded words — some senders put literal spaces
    // inside them ("…_DPD ?="), which the decoder refuses. Repair and retry.
    repair_spaces_in_encoded_words(&first)
        .and_then(|fixed| decode(fixed.as_bytes()))
        .filter(|s| !s.contains("=?"))
        .unwrap_or(first)
}

/// Map illegal raw spaces inside RFC 2047 encoded words to their legal
/// spelling (`_` for Q encoding, dropped for B), so the word decodes instead
/// of showing as `=?UTF-8?Q?…?=` gibberish. `None` when nothing needed it.
fn repair_spaces_in_encoded_words(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut repaired = false;
    while let Some(start) = rest.find("=?") {
        let (head, tail) = rest.split_at(start);
        out.push_str(head);
        let Some(end) = tail.find("?=") else {
            out.push_str(tail);
            rest = "";
            break;
        };
        let word = &tail[..end + 2];
        let parts: Vec<&str> = word.split('?').collect();
        // "=?charset?enc?text?=" splits to ["=", cs, enc, text…, "="].
        if parts.len() >= 5 && word.contains(' ') {
            let charset = parts[1];
            let enc = parts[2];
            let text = word[..word.len() - 2].splitn(4, '?').nth(3).unwrap_or("");
            let fixed = if enc.eq_ignore_ascii_case("q") {
                text.replace(' ', "_")
            } else {
                text.replace(' ', "")
            };
            if fixed != text {
                repaired = true;
            }
            out.push_str("=?");
            out.push_str(charset);
            out.push('?');
            out.push_str(enc);
            out.push('?');
            out.push_str(&fixed);
            out.push_str("?=");
        } else {
            out.push_str(word);
        }
        rest = &tail[end + 2..];
    }
    out.push_str(rest);
    repaired.then_some(out)
}

/// Compact date label from a unix timestamp (same style as [`format_date`]).
pub(super) fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    label_from_timestamp(ts)
}

/// Parse an RFC 2822 date into a compact label and a sortable unix timestamp.
pub(super) fn format_date(raw: &[u8]) -> (String, i64) {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim();
    let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) else {
        return (s.to_string(), 0);
    };
    (label_from_timestamp(dt.timestamp()), dt.timestamp())
}
