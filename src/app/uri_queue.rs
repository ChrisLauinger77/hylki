//! Mail handed in from outside before or after the window exists: mailto: and
//! mid: links, .eml files and files to attach. Each is queued until the app
//! can take it, then sent on as an AppMsg.

use adw::prelude::*;

use super::AppMsg;

/// Route a `mailto:` URI to the app (from main's `connect_open`).
pub(crate) fn queue_mailto(uri: String) {
    match MAILTO_SENDER.get() {
        Some(s) => {
            let _ = s.send(AppMsg::OpenMailto(uri));
        }
        None => MAILTO_PENDING.lock().unwrap().push(uri),
    }
}

/// A `mailto:` handed to the app before its component was up. `queue_mailto`
/// runs on GApplication's `open` signal, which can fire before `AppModel::init`
/// installs the sender — anything early waits here and is drained by init.
pub(super) static MAILTO_PENDING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub(super) static MAILTO_SENDER: std::sync::OnceLock<relm4::Sender<AppMsg>> =
    std::sync::OnceLock::new();

/// Turn a `mailto:` URI into a composer prefill (RFC 6068: address part plus
/// to/cc/bcc/subject/body query keys, all percent-encoded).
pub(super) fn parse_mailto(uri: &str) -> Option<crate::ui::compose::ComposePrefill> {
    let rest = uri.strip_prefix("mailto:")?;
    let (addr, query) = rest.split_once('?').unwrap_or((rest, ""));
    // Leading slashes aren't part of any address: Nautilus's "Send by email"
    // opens `mailto:///?attach=…` (#90), and sloppy generators write
    // `mailto://user@host` URL-style.
    let addr = addr.trim_start_matches('/');
    // `+` stays literal: plus-addressing (`user+tag@example.com`) is real.
    let mut to = crate::percent::decode(addr, false);
    let (mut cc, mut bcc, mut subject, mut body) =
        (String::new(), String::new(), String::new(), String::new());
    let mut attachments: Vec<std::path::PathBuf> = Vec::new();
    // The Email portal writes `attachment=` paths raw, unescaped (#339), so
    // an `&` inside a file name splits the query: a piece with no `=` after
    // an attachment is the rest of its path.
    let mut pairs: Vec<String> = Vec::new();
    for piece in query.split('&').filter(|p| !p.is_empty()) {
        let continues = !piece.contains('=')
            && pairs.last().is_some_and(|p| {
                let key = p
                    .split_once('=')
                    .map_or("", |(k, _)| k)
                    .to_ascii_lowercase();
                key == "attach" || key == "attachment"
            });
        match pairs.last_mut() {
            Some(last) if continues => {
                last.push('&');
                last.push_str(piece);
            }
            _ => pairs.push(piece.to_string()),
        }
    }
    for pair in &pairs {
        let (k, raw) = pair.split_once('=').unwrap_or((pair, ""));
        let v = crate::percent::decode(raw, false);
        match k.to_ascii_lowercase().as_str() {
            // A second `to` joins the address part, comma-separated.
            "to" if !v.is_empty() => {
                if !to.is_empty() {
                    to.push_str(", ");
                }
                to.push_str(&v);
            }
            "cc" => cc = v,
            "bcc" => bcc = v,
            "subject" => subject = v,
            "body" => body = v,
            // Nautilus's "Send by email" (and xdg-email) pass the files as
            // attach= parameters (#90), the Email portal as attachment=: an
            // absolute path or a file:// URI. Only absolute paths are
            // accepted; the caller re-checks that each names a real file
            // before attaching.
            "attach" | "attachment" if !v.is_empty() => {
                let path = if v.starts_with("file:") {
                    // A URI of its own: GIO undoes its escapes and any
                    // `localhost` host.
                    gtk::gio::File::for_uri(&v).path()
                } else {
                    Some(std::path::PathBuf::from(&v))
                };
                // A raw path with a `%` in its name decodes into another
                // name; when that one is not there and the raw one is, the
                // raw one was meant.
                let path = path.map(|p| {
                    let literal = std::path::Path::new(raw);
                    if raw != v && !p.exists() && literal.exists() {
                        literal.to_path_buf()
                    } else {
                        p
                    }
                });
                if let Some(p) = path.filter(|p| p.is_absolute()) {
                    attachments.push(p);
                }
            }
            _ => {}
        }
    }
    // The rich editor takes HTML; the mailto body is plain text.
    let body_html = if body.is_empty() {
        String::new()
    } else {
        gtk::glib::markup_escape_text(&body)
            .replace("\r\n", "\n")
            .replace('\n', "<br>")
    };
    Some(crate::ui::compose::ComposePrefill {
        to,
        cc,
        bcc,
        subject,
        body_html,
        attachments,
        ..Default::default()
    })
}

/// Route a `mid:` URI to the app (from main's `connect_open`).
pub(crate) fn queue_mid(uri: String) {
    match MAILTO_SENDER.get() {
        Some(s) => {
            let _ = s.send(AppMsg::OpenMid(uri));
        }
        None => MID_PENDING.lock().unwrap().push(uri),
    }
}

/// `mid:` links that arrived before the component was up (see `MAILTO_PENDING`).
pub(super) static MID_PENDING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The Message-ID a `mid:` URI (RFC 2392) names, normalized the way the cache
/// stores them: percent-decoded, angle brackets and whitespace stripped,
/// lowercased, and without any `/cid` part. `None` for anything else.
pub(super) fn parse_mid(uri: &str) -> Option<String> {
    let rest = uri
        .strip_prefix("mid:")
        .or_else(|| uri.strip_prefix("MID:"))?;
    // GLib normalizes a scheme it does not know to `mid:///…` on the way
    // through GFile (and decodes some of the escapes), so leading slashes
    // are not part of the id.
    let decoded = crate::percent::decode(rest.trim_start_matches('/'), false);
    // An optional `/content-id` follows the message-id. Slashes are legal
    // inside a message-id's left part (GitHub's have several), so only a
    // slash after the `@`, in the domain part, ends the id.
    let at = decoded.find('@')?;
    let id = match decoded[at..].find('/') {
        Some(i) => &decoded[..at + i],
        None => &decoded,
    };
    let id = id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
        .to_ascii_lowercase();
    (!id.is_empty()).then_some(id)
}

/// Route a message file (.eml) to the app (from main's `connect_open`, #398).
pub(crate) fn queue_message_file(path: std::path::PathBuf) {
    match MAILTO_SENDER.get() {
        Some(s) => {
            let _ = s.send(AppMsg::OpenMessageFile(path));
        }
        None => FILE_PENDING.lock().unwrap().push(path),
    }
}

/// Message files that arrived before the component was up (see `MAILTO_PENDING`).
pub(super) static FILE_PENDING: std::sync::Mutex<Vec<std::path::PathBuf>> =
    std::sync::Mutex::new(Vec::new());

/// Whether a file handed in is a message to read rather than one to attach:
/// a `.eml`, the usual name for a message saved whole, or Outlook's `.msg`
/// (#398).
pub(crate) fn is_message_file(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("eml") || e.eq_ignore_ascii_case("msg"))
}

/// The name Save as File offers: the subject, without what a file name
/// cannot hold, and `.eml`.
pub(super) fn eml_file_name(subject: &str) -> String {
    let stem: String = subject
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let stem = stem.trim().trim_matches('.');
    let stem: String = stem.chars().take(80).collect();
    if stem.is_empty() {
        "message.eml".to_string()
    } else {
        format!("{stem}.eml")
    }
}

/// The folder id a message read from a file carries (#398): no folder has
/// it, so nothing moves, deletes or refetches the message.
pub(super) const FILE_FOLDER: u32 = u32::MAX;

/// Route files to attach to a fresh composer (from main's `connect_open`).
pub(crate) fn queue_attach_files(paths: Vec<std::path::PathBuf>) {
    match MAILTO_SENDER.get() {
        Some(s) => {
            let _ = s.send(AppMsg::OpenWithFiles(paths));
        }
        None => ATTACH_PENDING.lock().unwrap().push(paths),
    }
}

/// Files handed in (via `connect_open`) before the app's component was up —
/// same early-arrival race as `MAILTO_PENDING`, queued separately since each
/// batch opens its own composer.
pub(super) static ATTACH_PENDING: std::sync::Mutex<Vec<Vec<std::path::PathBuf>>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
mod tests {
    #[test]
    fn mailto_uris_become_composer_prefills() {
        let p = super::parse_mailto("mailto:ann@example.com").unwrap();
        assert_eq!(p.to, "ann@example.com");
        assert!(p.subject.is_empty() && p.body_html.is_empty());

        let p = super::parse_mailto(
            "mailto:ann@example.com?subject=Hi%20there&cc=bob@x.org&body=line%20one%0Aline%20two",
        )
        .unwrap();
        assert_eq!(p.to, "ann@example.com");
        assert_eq!(p.subject, "Hi there");
        assert_eq!(p.cc, "bob@x.org");
        assert_eq!(p.body_html, "line one<br>line two");

        // Plus-addressing survives: '+' is literal in mailto, never a space.
        let p = super::parse_mailto("mailto:user%2Btag@example.com?to=a+b@x.org").unwrap();
        assert_eq!(p.to, "user+tag@example.com, a+b@x.org");

        // A body with markup arrives escaped, not interpreted.
        let p = super::parse_mailto("mailto:a@b.c?body=%3Cscript%3E").unwrap();
        assert_eq!(p.body_html, "&lt;script&gt;");

        assert!(super::parse_mailto("https://example.com").is_none());

        // Nautilus-style attachments (#90): plain absolute paths and file://
        // URIs, percent-decoded, several allowed; relative paths refused.
        // Nautilus opens `mailto:///?…` — the slashes are not an address.
        let p = super::parse_mailto(
            "mailto:///?attach=/home/u/a%20b.pdf&attach=file:///tmp/c.png&attach=../etc/passwd",
        )
        .unwrap();
        assert_eq!(p.to, "");
        assert_eq!(
            p.attachments,
            vec![
                std::path::PathBuf::from("/home/u/a b.pdf"),
                std::path::PathBuf::from("/tmp/c.png"),
            ],
        );

        // The Email portal (#339): attachment=, raw paths, an `&` in a name,
        // and file:// URIs with their own escapes or a localhost host.
        let p = super::parse_mailto(
            "mailto:a@b.c?subject=&body=&attachment=/home/u/Mes Docs/A & B été.pdf\
             &attachment=file://localhost/tmp/x%20y.txt",
        )
        .unwrap();
        assert_eq!(
            p.attachments,
            vec![
                std::path::PathBuf::from("/home/u/Mes Docs/A & B été.pdf"),
                std::path::PathBuf::from("/tmp/x y.txt"),
            ],
        );
    }

    /// A `mid:` link names a Message-ID the way the cache stores it: no
    /// brackets, lowercase, percent-decoded, without a `/cid` part.
    #[test]
    fn mid_links_normalize_to_the_cached_form() {
        let want = Some("cah5b6q=abc123@example.com".to_string());
        assert_eq!(super::parse_mid("mid:CAH5B6Q=abc123@example.com"), want);
        assert_eq!(super::parse_mid("mid:CAH5B6Q%3Dabc123%40example.com"), want);
        assert_eq!(
            super::parse_mid("mid:%3CCAH5B6Q%3Dabc123%40example.com%3E"),
            want
        );
        assert_eq!(
            super::parse_mid("mid:<CAH5B6Q=abc123@example.com>/part1"),
            want
        );
        assert_eq!(super::parse_mid("MID:CAH5B6Q=abc123@example.com"), want);
        assert_eq!(
            super::parse_mid("mid:///%3CCAH5B6Q=abc123@example.com%3E"),
            want
        );
        // Slashes inside the id are part of it; one after the domain is a cid.
        assert_eq!(
            super::parse_mid("mid:hyprlab/hylki/issues/128/5558573653@github.com/att1"),
            Some("hyprlab/hylki/issues/128/5558573653@github.com".to_string())
        );
        assert_eq!(super::parse_mid("mid:"), None);
        assert_eq!(super::parse_mid("mid:not-an-id"), None);
        assert_eq!(super::parse_mid("mailto:a@b.c"), None);
    }

    #[test]
    fn save_as_file_offers_the_subject() {
        assert_eq!(
            super::eml_file_name("Re: Q3 roadmap / review?"),
            "Re- Q3 roadmap - review-.eml"
        );
        assert_eq!(super::eml_file_name("   "), "message.eml");
        assert_eq!(super::eml_file_name("..."), "message.eml");
        assert!(super::is_message_file(std::path::Path::new("/tmp/A.EML")));
        assert!(!super::is_message_file(std::path::Path::new("/tmp/a.pdf")));
    }
}
