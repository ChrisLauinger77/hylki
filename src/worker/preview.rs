//! The list preview: a few lines of text out of the first bytes of a body
//! part, whatever its charset, transfer encoding or multipart nesting.

/// Whether a (lowercased) Content-Type can hold text to preview, or contain
/// a part that does: text itself, a multipart, or an enclosed message. An
/// image, an archive or a document is a file, whatever its bytes decode to.
pub(super) fn text_like(ctype: &str) -> bool {
    ctype.starts_with("text/") || ctype.starts_with("multipart/") || ctype.starts_with("message/")
}

/// Whether decoded body bytes are a file rather than text: control
/// characters that no text carries (a zip opens `PK\x03\x04`, a PDF's
/// streams are full of them). A row showing such bytes as letters is worse
/// than a row showing nothing.
fn looks_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(512)];
    if sample.is_empty() {
        return false;
    }
    let control = sample
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0c) || b == 0x7f)
        .count();
    control > 0 && (sample.contains(&0) || control * 20 > sample.len())
}

/// Longest preview stored per message.
pub(super) const PREVIEW_CHARS: usize = 240;

/// Turn the first bytes of a message's body part into a one-line snippet for the
/// message list.
///
/// What arrives is raw MIME, cut off mid-stream: still transfer-encoded, perhaps
/// HTML, and (being a prefix) possibly ending mid-character or mid-tag. The
/// transfer encoding is declared in headers this fetch doesn't include, so it is
/// inferred from the bytes themselves — wrongly guessing plain text for base64
/// would show gibberish in the list, which is worse than showing nothing. The
/// charset is the one the part's MIME headers declared, when the caller has
/// them.
pub(super) fn preview_from_part(
    bytes: &[u8],
    charset: Option<&str>,
    encoding: Option<&str>,
) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // Section 1 can be a whole nested multipart rather than a body: descend to the
    // text inside it, which also carries the encoding and charset in its own
    // headers instead of leaving them to be guessed.
    if let Some(inner) = text_in_multipart(bytes, 0) {
        return finish_preview(inner);
    }
    // What the part says it is, when it says: the headers ride along with the
    // preview fetch, and a declaration beats any amount of inspection.
    let declared = encoding.map(|e| e.trim().to_ascii_lowercase());
    if let Some(e) = declared.as_deref().filter(|e| {
        e.starts_with("base64")
            || e.starts_with("quoted-printable")
            || matches!(*e, "7bit" | "8bit" | "binary")
    }) {
        let text = decode_mime_body(bytes, e, charset);
        if looks_binary(text.as_bytes()) {
            return String::new();
        }
        return finish_preview(text);
    }
    let decoded = if looks_like_base64(bytes) {
        decode_base64_prefix(bytes)
    } else if bytes.contains(&b'=') {
        decode_quoted_printable(bytes)
    } else {
        bytes.to_vec()
    };
    // Whatever the headers said or failed to say, a file's bytes are not a
    // preview (#241).
    if looks_binary(&decoded) {
        return String::new();
    }

    finish_preview(decode_text(&decoded, charset))
}

/// Text from a body's bytes, in the charset its headers declared (#159).
///
/// Everything here used to be read as UTF-8, so a bank's `iso-8859-2` mail
/// showed a replacement character for every accented letter in the list while
/// the reader — which hands the whole message to mail-parser — showed it fine.
/// mail-parser's charset table is used here too; UTF-8 and ASCII are read
/// directly.
fn decode_text(bytes: &[u8], charset: Option<&str>) -> String {
    let charset = charset
        .map(|c| c.trim().trim_matches('"').to_ascii_lowercase())
        .filter(|c| !c.is_empty());
    let declared_unicode = matches!(
        charset.as_deref(),
        Some("utf-8" | "utf8" | "us-ascii" | "ascii")
    );
    if let Some(cs) = charset.as_deref().filter(|_| !declared_unicode) {
        if let Some(decode) = mail_parser::decoders::charsets::map::charset_decoder(cs.as_bytes()) {
            return decode(bytes);
        }
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        // A fetch of the first N bytes can end mid-character: keep the whole
        // characters and drop the fragment rather than show it as noise.
        Err(e) if e.error_len().is_none() => {
            String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
        }
        // Declared UTF-8 that isn't: replacement characters mark the damage.
        Err(_) if declared_unicode => String::from_utf8_lossy(bytes).into_owned(),
        // 8-bit text with no (usable) charset: read it as Windows-1252, as
        // browsers and mail clients do, which at least shows Western European
        // accents rather than a row of replacement characters.
        Err(_) => mail_parser::decoders::charsets::single_byte::decoder_cp1252(bytes),
    }
}

/// The `charset=` parameter of a (lowercased) Content-Type value.
pub(super) fn charset_param(ctype: &str) -> Option<String> {
    ctype
        .split(';')
        .skip(1)
        .find_map(|p| {
            let (k, v) = p.split_once('=')?;
            (k.trim() == "charset").then(|| v.trim().trim_matches('"').to_string())
        })
        .filter(|c| !c.is_empty())
}

/// Decode as much of a (possibly truncated) base64 body as forms whole groups.
fn decode_base64_prefix(body: &[u8]) -> Vec<u8> {
    let clean: String = body
        .iter()
        .filter(|b| !b.is_ascii_whitespace())
        .map(|&b| b as char)
        .collect();
    // Only whole 4-character groups can be decoded; the tail of a truncated
    // fetch is dropped rather than turned into noise.
    let usable = &clean[..clean.len() - clean.len() % 4];
    crate::oauth::base64_decode(usable).unwrap_or_default()
}

/// Tidy decoded body text into the single line the list shows.
pub(super) fn finish_preview(decoded: String) -> String {
    // An HTML part becomes readable text; a plain part passes through.
    let plain = if decoded.contains('<') && decoded.contains('>') {
        crate::app::message_text(&decoded)
    } else {
        decoded
    };

    // Quoted replies say nothing about *this* message, so skip those lines.
    let body: String = plain
        .lines()
        .filter(|line| !line.trim_start().starts_with('>'))
        .collect::<Vec<_>>()
        .join("\n");
    let body = strip_link_noise(&body);
    // Marketing "preheader padding" — long chains of &nbsp; + zero-width
    // characters (zwnj, word-joiner, combining grapheme joiner, soft hyphen…)
    // meant to push the real body out of inbox previews. The zero-width
    // characters are NOT Unicode whitespace, so they would survive the
    // collapse below as a huge blank gap ending in the ellipsis. Drop them;
    // the non-breaking spaces between them then collapse away normally.
    let body: String = body
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{200b}'
                    ..='\u{200d}' // zero-width space/nbsp/joiners
                    | '\u{2060}'        // word joiner
                    | '\u{feff}'        // BOM / zero-width no-break space
                    | '\u{00ad}'        // soft hyphen
                    | '\u{034f}'        // combining grapheme joiner
                    | '\u{2800}' // Braille blank, another padder
            )
        })
        .collect();
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(p) = pgp_preview(&collapsed) {
        return p;
    }
    collapsed.chars().take(PREVIEW_CHARS).collect()
}

/// What the list says for an OpenPGP-encrypted message (#133), whose first
/// text is otherwise the PGP/MIME version stub ("Version: 1") or the
/// armour itself — neither of which says anything a reader wants to see.
pub(super) fn pgp_preview(collapsed: &str) -> Option<String> {
    let t = collapsed.trim();
    let stub = t.to_ascii_lowercase();
    let stub = stub.trim_start_matches("version:").trim();
    if stub == "1" || t.starts_with("-----BEGIN PGP MESSAGE-----") {
        return Some(crate::models::ENCRYPTED_PREVIEW.to_string());
    }
    None
}

/// Drop the link furniture that plain-text alternatives are built from, so the
/// preview starts where the message does.
///
/// A converter renders a link as `text ( url )`, so a mail whose first element is
/// a linked logo — every marketing template — begins with a bare tracking URL.
/// Hylki showed that URL as the preview where Apple Mail shows the greeting. The
/// parenthesised URLs go, and any URL-only lines left at the front go with them;
/// if that is the whole message, it stays as it was, since a link is better than
/// an empty row.
fn strip_link_noise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let after = rest[open + 1..].trim_start();
        let is_url = after.starts_with("http://") || after.starts_with("https://");
        let close = rest[open..].find(')').map(|i| open + i);
        match (is_url, close) {
            // `( https://… )` — a rendered link, and nothing else in the brackets.
            (true, Some(close)) if !rest[open..close].trim_end_matches(')').contains('\n') => {
                out.push_str(&rest[..open]);
                rest = &rest[close + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);

    // Whatever is left at the top that is only a link.
    let trimmed: String = out
        .lines()
        .skip_while(|line| {
            let l = line.trim();
            l.is_empty() || (is_url(l) && l.split_whitespace().count() == 1)
        })
        .collect::<Vec<_>>()
        .join("\n");

    if trimmed.trim().is_empty() {
        text.to_string()
    } else {
        trimmed
    }
}

/// Whether a word is a bare URL.
fn is_url(word: &str) -> bool {
    word.starts_with("http://") || word.starts_with("https://")
}

/// The text of the first readable part of a MIME multipart, or `None` if this
/// isn't one.
///
/// `text/plain` is preferred and `text/html` taken only if there is no plain
/// alternative, which matches what the reader shows. Nesting is followed a couple
/// of levels deep — enough for `mixed(alternative(…))` — and no further, since a
/// preview is not worth unbounded recursion through a hostile message.
pub(super) fn text_in_multipart(part: &[u8], depth: usize) -> Option<String> {
    if depth > 3 {
        return None;
    }
    // The boundary opens the part — or follows a short preamble ("This is a
    // multi-part message in MIME format.") when what arrived is a whole body
    // rather than a part. A boundary delimiter, not a message that merely
    // opens with a dash: two dashes, more after them, no spaces.
    let boundary = part
        .split(|&b| b == b'\n')
        .take(10)
        .map(|line| {
            let end = line
                .iter()
                .rposition(|b| !b.is_ascii_whitespace())
                .map_or(0, |i| i + 1);
            &line[..end]
        })
        .find(|line| line.starts_with(b"--") && line.len() >= 3 && !line.contains(&b' '))?;
    let mut html: Option<String> = None;
    for chunk in split_on(part, boundary).skip(1) {
        let start = chunk
            .iter()
            .position(|b| !matches!(b, b'\r' | b'\n'))
            .unwrap_or(chunk.len());
        let chunk = &chunk[start..];
        if chunk.starts_with(b"--") {
            break; // closing delimiter: no more parts
        }
        let Some((headers, body)) = split_mime_headers(chunk) else {
            continue;
        };
        let headers = String::from_utf8_lossy(headers);
        let ctype = mime_header(&headers, "content-type").unwrap_or_default();
        let encoding = mime_header(&headers, "content-transfer-encoding").unwrap_or_default();
        let charset = charset_param(&ctype);
        if ctype.starts_with("multipart/") {
            if let Some(inner) = text_in_multipart(body, depth + 1) {
                return Some(inner);
            }
        } else if ctype.starts_with("text/plain") {
            return Some(decode_mime_body(body, &encoding, charset.as_deref()));
        } else if ctype.starts_with("text/html") && html.is_none() {
            html = Some(decode_mime_body(body, &encoding, charset.as_deref()));
        }
    }
    html
}

/// The pieces of `hay` between occurrences of `sep`, like `str::split`.
fn split_on<'a>(hay: &'a [u8], sep: &'a [u8]) -> impl Iterator<Item = &'a [u8]> {
    let mut rest = Some(hay);
    std::iter::from_fn(move || {
        let cur = rest?;
        match cur.windows(sep.len()).position(|w| w == sep) {
            Some(i) => {
                rest = Some(&cur[i + sep.len()..]);
                Some(&cur[..i])
            }
            None => {
                rest = None;
                Some(cur)
            }
        }
    })
}

/// Split a MIME part into its header block and its body.
fn split_mime_headers(part: &[u8]) -> Option<(&[u8], &[u8])> {
    if let Some(i) = part.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((&part[..i], &part[i + 4..]));
    }
    part.windows(2)
        .position(|w| w == b"\n\n")
        .map(|i| (&part[..i], &part[i + 2..]))
}

/// One header's value, lowercased, from a MIME part's header block.
pub(super) fn mime_header(headers: &str, name: &str) -> Option<String> {
    // Unfold first: a parameter on a continuation line ("Content-Type:
    // text/html;\r\n\tcharset=iso-8859-2") belongs to the header above it.
    let unfolded = headers
        .replace("\r\n", "\n")
        .replace("\n ", " ")
        .replace("\n\t", " ");
    unfolded
        .lines()
        .find(|l| {
            // Compare as bytes: a header block reaches us through
            // `from_utf8_lossy`, so a line can carry a multibyte character
            // that `name.len()` falls inside — slicing there would panic and
            // take the account's mail thread with it. A name is ASCII, so a
            // byte-wise match also proves `name.len()` is a char boundary,
            // which is what makes the slice below safe.
            l.len() > name.len()
                && l.as_bytes()[..name.len()].eq_ignore_ascii_case(name.as_bytes())
                && l[name.len()..].trim_start().starts_with(':')
        })
        .map(|l| {
            l[name.len()..].trim_start()[1..]
                .trim()
                .to_ascii_lowercase()
        })
}

/// Decode a part's body according to the encoding and charset it declares.
fn decode_mime_body(body: &[u8], encoding: &str, charset: Option<&str>) -> String {
    let bytes = if encoding.starts_with("base64") {
        decode_base64_prefix(body)
    } else if encoding.starts_with("quoted-printable") {
        decode_quoted_printable(body)
    } else {
        body.to_vec()
    };
    decode_text(&bytes, charset)
}

/// Whether a chunk looks like base64 rather than text: base64's alphabet only,
/// and long enough that a short plain word can't be mistaken for it.
///
/// An `=` is padding, and padding only ever closes a base64 stream — so one
/// anywhere else says this is not base64 however well the rest fits. That is
/// what quoted-printable looks like from here: `=E2=80=87=CD=8F` repeated is
/// nothing but letters, digits and `=`, and reading a marketing preheader's
/// worth of it as base64 turned a Cloudflare newsletter's preview into a row
/// of replacement characters. A truncated fetch of real base64 simply has no
/// padding in it at all.
pub(super) fn looks_like_base64(bytes: &[u8]) -> bool {
    let mut significant = 0;
    let mut padding = 0;
    for &c in bytes {
        if c.is_ascii_whitespace() {
            continue;
        }
        if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'=') {
            return false;
        }
        if c == b'=' {
            padding += 1;
        } else if padding > 0 {
            // Alphabet after padding: the `=` was not closing anything.
            return false;
        }
        significant += 1;
    }
    significant >= 40 && padding <= 2
}

/// Decode quoted-printable to the bytes it stands for, leaving anything
/// malformed as written (a truncated fetch can end mid-escape).
fn decode_quoted_printable(src: &[u8]) -> Vec<u8> {
    // The fetch behind a preview is a slice of a part, so it ends wherever the
    // byte count ran out — routinely in the middle of an escape. Left alone
    // that stub reaches the reader as literal text ("… =E"), so drop it: a
    // trailing "=" (which in quoted-printable is a soft line break anyway) and
    // a trailing "=" plus one hex digit. Anything else is malformed rather
    // than cut short, and is left as written.
    let src = match src.len().checked_sub(2).map(|at| &src[at..]) {
        Some(&[b'=', h]) if h.is_ascii_hexdigit() => &src[..src.len() - 2],
        _ if src.last() == Some(&b'=') => &src[..src.len() - 1],
        _ => src,
    };
    let mut buf = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'=' {
            // "=\r\n" is a soft line break: the line continues.
            if src.get(i + 1) == Some(&b'\r') && src.get(i + 2) == Some(&b'\n') {
                i += 3;
                continue;
            }
            if src.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            let hex = src.get(i + 1..i + 3).and_then(|h| {
                std::str::from_utf8(h)
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            });
            match hex {
                Some(b) => {
                    buf.push(b);
                    i += 3;
                    continue;
                }
                None => {
                    buf.push(b'=');
                    i += 1;
                    continue;
                }
            }
        }
        buf.push(src[i]);
        i += 1;
    }
    buf
}
