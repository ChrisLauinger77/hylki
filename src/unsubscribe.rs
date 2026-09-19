//! One-click unsubscribe.
//!
//! A mailing list that wants to be left announces how in its headers
//! (RFC 2369): `List-Unsubscribe` carries one or more `<uri>` handles, a
//! `mailto:` to write to and/or an `https:` page to visit. RFC 8058 adds
//! `List-Unsubscribe-Post: List-Unsubscribe=One-Click`, which promises that
//! a bare POST of that form body to the https handle unsubscribes the
//! recipient with no page, no login and no confirmation in between — the
//! mechanism the big providers' Unsubscribe buttons run on.
//!
//! The reader's button prefers the routes that need no browser: the
//! one-click POST first, then a mail to the `mailto:` handle from the
//! account the message arrived in. Only a list that offers nothing but a
//! web page sends the user to the browser, and the button says so.
//!
//! The handles are read once, when the message's body is fetched, and ride
//! with the sender check into the cache (see [`crate::verify`]); nothing here
//! touches the network until the user asks.

use crate::models::Unsubscribe;

/// The exact `List-Unsubscribe-Post` value RFC 8058 requires. Anything else
/// is not a one-click promise and the https handle is treated as a page.
const ONE_CLICK: &str = "List-Unsubscribe=One-Click";

/// Read the unsubscribe handles out of a parsed message's headers, or `None`
/// when the message offers no way to unsubscribe.
pub fn detect(parsed: &mail_parser::Message) -> Option<Unsubscribe> {
    let list_unsubscribe = raw_header_values(parsed, "List-Unsubscribe");
    let post = raw_header_values(parsed, "List-Unsubscribe-Post").into_iter().next();
    let list_id = raw_header_values(parsed, "List-Id").into_iter().next();
    from_headers(&list_unsubscribe, post.as_deref(), list_id.as_deref())
}

/// The raw, unfolded value of every header of that name. `mail_parser` parses
/// the `List-*` family as addresses, which mangles a `mailto:` with a query
/// string — the bytes it was given are read back instead.
fn raw_header_values(parsed: &mail_parser::Message, name: &str) -> Vec<String> {
    let raw = parsed.raw_message();
    parsed
        .headers()
        .iter()
        .filter(|h| h.name().eq_ignore_ascii_case(name))
        .filter_map(|h| raw.get(h.offset_start()..h.offset_end()))
        .map(|b| unfold(&String::from_utf8_lossy(b)))
        .filter(|v| !v.is_empty())
        .collect()
}

/// A folded header value as one line.
fn unfold(value: &str) -> String {
    value
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the handles from the header values themselves: every
/// `List-Unsubscribe` value, the `List-Unsubscribe-Post` value and the
/// `List-Id`, as they appear in the message.
pub fn from_headers(
    list_unsubscribe: &[String],
    post: Option<&str>,
    list_id: Option<&str>,
) -> Option<Unsubscribe> {
    let mut mailto = None;
    let mut https = None;
    let mut web = None;
    for value in list_unsubscribe {
        for handle in angle_handles(value) {
            let lower = handle.to_ascii_lowercase();
            if lower.starts_with("mailto:") {
                if mailto.is_none() && parse_mailto(&handle).is_some() {
                    mailto = Some(handle);
                }
            } else if lower.starts_with("https://") {
                if https.is_none() {
                    https = Some(handle.clone());
                }
                if web.is_none() {
                    web = Some(handle);
                }
            } else if lower.starts_with("http://") && web.is_none() {
                web = Some(handle);
            }
        }
    }
    // RFC 8058: the one-click promise holds only for an https handle, and
    // only when the header says exactly this.
    let one_click = match (post, https) {
        (Some(p), Some(url)) if p.trim().eq_ignore_ascii_case(ONE_CLICK) => Some(url),
        _ => None,
    };
    if one_click.is_none() && mailto.is_none() && web.is_none() {
        return None;
    }
    Some(Unsubscribe {
        one_click,
        mailto,
        web,
        list_id: list_id.map(list_id_of).unwrap_or_default(),
    })
}

/// The handles of a `List-Unsubscribe` value, in order. RFC 2369 wants each
/// in `<…>`, with comments (`(…)`) and anything else outside the brackets
/// skipped — but plenty of real mail (ArtStation through Amazon SES, for
/// one) writes the bare URL with no brackets at all, so a value without any
/// is read as bare handles separated by commas or whitespace.
fn angle_handles(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else { break };
        let handle = after[..end].trim();
        if !handle.is_empty() {
            out.push(handle.to_string());
        }
        rest = &after[end + 1..];
    }
    if out.is_empty() && !value.contains('<') {
        out.extend(
            value
                .split(|c: char| c == ',' || c.is_whitespace())
                .map(str::trim)
                .filter(|t| {
                    let l = t.to_ascii_lowercase();
                    l.starts_with("mailto:") || l.starts_with("http://") || l.starts_with("https://")
                })
                .map(str::to_string),
        );
    }
    out
}

/// The identifier out of a `List-Id` value (`Weekly digest <weekly.example.com>`
/// gives `weekly.example.com`), lowercased: the key a list is remembered by.
fn list_id_of(value: &str) -> String {
    let id = match (value.find('<'), value.rfind('>')) {
        (Some(s), Some(e)) if e > s => &value[s + 1..e],
        _ => value,
    };
    id.trim().to_ascii_lowercase()
}

/// Where a `mailto:` handle sends its unsubscribe request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailtoTarget {
    /// Comma-separated addresses, as the composer takes them.
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// Take a `mailto:` URI apart (RFC 6068): the addresses before `?`, then
/// `subject=`, `body=` and `to=` in the query, percent-decoded.
pub fn parse_mailto(uri: &str) -> Option<MailtoTarget> {
    let rest = strip_scheme(uri, "mailto:")?;
    let (addr_part, query) = match rest.split_once('?') {
        Some((a, q)) => (a, q),
        None => (rest, ""),
    };
    let mut to: Vec<String> = addr_part
        .split(',')
        .map(percent_decode)
        .map(|a| a.trim().to_string())
        .filter(|a| a.contains('@'))
        .collect();
    let mut subject = String::new();
    let mut body = String::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_decode(v);
        match k.to_ascii_lowercase().as_str() {
            "subject" => subject = v,
            "body" => body = v,
            "to" => to.extend(v.split(',').map(|a| a.trim().to_string()).filter(|a| a.contains('@'))),
            _ => {}
        }
    }
    if to.is_empty() {
        return None;
    }
    // A list that names no subject still gets a request it can recognise;
    // most mailto handles carry `subject=unsubscribe` anyway.
    if subject.trim().is_empty() {
        subject = "Unsubscribe".to_string();
    }
    if body.trim().is_empty() {
        body = subject.clone();
    }
    Some(MailtoTarget { to: to.join(", "), subject, body })
}

fn strip_scheme<'a>(uri: &'a str, scheme: &str) -> Option<&'a str> {
    let uri = uri.trim();
    (uri.len() >= scheme.len() && uri[..scheme.len()].eq_ignore_ascii_case(scheme))
        .then(|| &uri[scheme.len()..])
}

/// `%20` and `+` back to characters, for a mailto query.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(h) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16)
            {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Send the RFC 8058 one-click request: a POST of `List-Unsubscribe=One-Click`
/// to the list's https handle. Blocking; run it off the UI thread. Any 2xx
/// answer means the list took the request.
pub fn one_click_post(url: &str) -> Result<(), String> {
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err("the list's unsubscribe handle is not an https address".to_string());
    }
    match ureq::post(url)
        .set("User-Agent", crate::logo::USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .send_form(&[("List-Unsubscribe", "One-Click")])
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, _)) => Err(format!("the list's server answered {code}")),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn mailto_and_web_handles_are_told_apart() {
        let u = from_headers(
            &s(&["<mailto:leave@list.example?subject=unsubscribe>, <https://list.example/u/1>"]),
            None,
            None,
        )
        .unwrap();
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@list.example?subject=unsubscribe"));
        assert_eq!(u.web.as_deref(), Some("https://list.example/u/1"));
        assert_eq!(u.one_click, None, "no List-Unsubscribe-Post, no one-click promise");
        assert!(u.list_id.is_empty());
    }

    #[test]
    fn one_click_needs_the_exact_post_header_and_https() {
        let hdr = s(&["<https://list.example/u/1>, <mailto:leave@list.example>"]);
        let u = from_headers(&hdr, Some("List-Unsubscribe=One-Click"), None).unwrap();
        assert_eq!(u.one_click.as_deref(), Some("https://list.example/u/1"));
        let u = from_headers(&hdr, Some("list-unsubscribe=one-click "), None).unwrap();
        assert_eq!(u.one_click.as_deref(), Some("https://list.example/u/1"), "case and space are forgiven");
        let u = from_headers(&hdr, Some("something-else"), None).unwrap();
        assert_eq!(u.one_click, None);
        let u = from_headers(&s(&["<http://list.example/u/1>"]), Some("List-Unsubscribe=One-Click"), None).unwrap();
        assert_eq!(u.one_click, None, "plain http is never one-click");
        assert_eq!(u.web.as_deref(), Some("http://list.example/u/1"));
    }

    #[test]
    fn comments_and_folding_are_skipped() {
        let hdr = s(&[
            "(Use this command to get off the list) <mailto:leave@list.example>,  <https://list.example/leave>",
        ]);
        let u = from_headers(&hdr, None, Some("Weekly digest <weekly.list.example>")).unwrap();
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@list.example"));
        assert_eq!(u.web.as_deref(), Some("https://list.example/leave"));
        assert_eq!(u.list_id, "weekly.list.example");
        assert_eq!(unfold("<mailto:a@b.c>,\r\n <https://x.y/z>"), "<mailto:a@b.c>, <https://x.y/z>");
    }

    #[test]
    fn bare_handles_without_brackets_are_taken_too() {
        // ArtStation, through Amazon SES: the URL alone, no brackets.
        let hdr = s(&["https://www.artstation.com/unsubscribe/notifications/21d2?kind%5B%5D=project_publish"]);
        let u = from_headers(&hdr, None, None).unwrap();
        assert_eq!(
            u.web.as_deref(),
            Some("https://www.artstation.com/unsubscribe/notifications/21d2?kind%5B%5D=project_publish")
        );
        assert!(!u.direct(), "a page only: the button opens the browser");
        let u = from_headers(&s(&["mailto:leave@x.example, https://x.example/u"]), None, None).unwrap();
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@x.example"));
        assert_eq!(u.web.as_deref(), Some("https://x.example/u"));
        // A bracketed value keeps the strict reading: text outside is not a handle.
        let u = from_headers(&s(&["https://ignored.example <mailto:a@b.c>"]), None, None).unwrap();
        assert_eq!(u.web, None);
    }

    #[test]
    fn a_message_with_no_handles_offers_nothing() {
        assert_eq!(from_headers(&[], None, None), None);
        assert_eq!(from_headers(&s(&["nothing in brackets"]), None, None), None);
        assert_eq!(from_headers(&s(&["<ftp://odd.example/x>"]), None, None), None);
    }

    #[test]
    fn mailto_targets_are_taken_apart() {
        let t = parse_mailto("mailto:leave@list.example?subject=unsubscribe%20me&body=please+go").unwrap();
        assert_eq!(t.to, "leave@list.example");
        assert_eq!(t.subject, "unsubscribe me");
        assert_eq!(t.body, "please go");
        let t = parse_mailto("MAILTO:a@x.example,b@x.example").unwrap();
        assert_eq!(t.to, "a@x.example, b@x.example");
        assert_eq!(t.subject, "Unsubscribe", "a subject is always sent");
        assert_eq!(t.body, "Unsubscribe");
        assert_eq!(parse_mailto("mailto:?subject=x"), None, "no address, no target");
        assert_eq!(parse_mailto("https://x.example"), None);
    }

    #[test]
    fn the_demo_newsletter_offers_one_click_and_mail() {
        let raw = crate::backend::demo_headers(10).expect("the demo newsletter has headers");
        let check = crate::verify::check_sender(raw.as_bytes());
        let u = check.unsubscribe.expect("handles");
        assert!(u.one_click.is_some() && u.mailto.is_some() && u.direct());
        assert_eq!(u.list_id, "digest.this-week-in-rust.org");
        assert_eq!(check.trust, crate::models::SenderTrust::Pass);
    }

    /// Probe a saved message (or header block): `HYLKI_UNSUB_PROBE=<file>
    /// cargo test --bin hylki unsubscribe::tests::probe_file -- --ignored
    /// --nocapture` prints what the check reads out of it.
    #[test]
    #[ignore]
    fn probe_file() {
        let Ok(path) = std::env::var("HYLKI_UNSUB_PROBE") else { return };
        let raw = std::fs::read(&path).expect("readable file");
        let check = crate::verify::check_sender(&raw);
        eprintln!("{path}: trust {:?}, unsubscribe {:#?}", check.trust, check.unsubscribe);
    }

    #[test]
    fn detect_reads_the_raw_headers_of_a_message() {
        let raw = b"From: Digest <digest@list.example>\r\n\
List-Id: The digest <digest.list.example>\r\n\
List-Unsubscribe: <mailto:leave@list.example?subject=unsubscribe>,\r\n <https://list.example/u/abc>\r\n\
List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
Subject: Hello\r\n\r\nBody\r\n";
        let parsed = mail_parser::MessageParser::default().parse(raw.as_slice()).unwrap();
        let u = detect(&parsed).unwrap();
        assert_eq!(u.one_click.as_deref(), Some("https://list.example/u/abc"));
        assert_eq!(u.mailto.as_deref(), Some("mailto:leave@list.example?subject=unsubscribe"));
        assert_eq!(u.list_id, "digest.list.example");
        let plain = b"From: a@b.c\r\nSubject: x\r\n\r\nhi\r\n";
        let parsed = mail_parser::MessageParser::default().parse(plain.as_slice()).unwrap();
        assert_eq!(detect(&parsed), None);
    }
}
