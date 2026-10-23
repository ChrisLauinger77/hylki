//! What the services have in common over HTTP: turning a failed request
//! into a sentence, recognizing Cloudflare's upload limit, and reading a
//! file in pieces.

use std::io::Read;
use std::path::Path;

use super::CloudAccount;

/// The piece size for an account behind Cloudflare: under the proxy's
/// 100 MB request limit with room for the multipart framing.
pub const CLOUDFLARE_CHUNK: u64 = 90 * 1000 * 1000;

/// The byte ranges an upload of `size` goes in, `chunk` at a time:
/// (offset, length), the last one shorter. A zero-byte file is one
/// empty range, so it is still created.
pub(super) fn chunk_ranges(size: u64, chunk: u64) -> Vec<(u64, u64)> {
    let chunk = chunk.max(1);
    if size == 0 {
        return vec![(0, 0)];
    }
    (0..size)
        .step_by(chunk as usize)
        .map(|off| (off, chunk.min(size - off)))
        .collect()
}

/// A reader over one piece of the file on disk.
pub(super) fn file_slice(
    path: &Path,
    name: &str,
    offset: u64,
    len: u64,
) -> Result<std::io::Take<std::fs::File>, String> {
    use std::io::Seek;
    let mut f = std::fs::File::open(path).map_err(|e| format!("could not read {name}: {e}"))?;
    f.seek(std::io::SeekFrom::Start(offset))
        .map_err(|e| format!("could not read {name}: {e}"))?;
    Ok(f.take(len))
}

pub(super) fn http_err(what: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) | ureq::Error::Status(403, _) => {
            format!("{what}: the server refused the login. Check the user name and the password.")
        }
        ureq::Error::Status(404, _) => format!("{what}: not found. Check the server URL."),
        ureq::Error::Status(code, resp) => {
            let text = resp.into_string().unwrap_or_default();
            let text = short_error(&text);
            format!("{what}: HTTP {code} {}", text.trim())
        }
        ureq::Error::Transport(t) => format!("{what}: {t}"),
    }
}

/// Cloudflare's proxy caps a request body at 100 MB; a bigger upload to a
/// server behind it comes back as its own 413 page (or the connection is
/// cut, seen as a 5xx or a transport error), which reads like the server
/// broke. When that is what happened to a file over the piece size on an
/// account without the switch, say so and name the switch.
pub(super) fn cloudflare_error(
    account: &CloudAccount,
    what: &str,
    name: &str,
    size: u64,
    e: &ureq::Error,
) -> Option<String> {
    let (status, via_cloudflare) = match e {
        ureq::Error::Status(code, resp) => {
            let server = resp.header("server").unwrap_or("").to_ascii_lowercase();
            (
                Some(*code),
                server.contains("cloudflare") || resp.header("cf-ray").is_some(),
            )
        }
        ureq::Error::Transport(_) => (None, false),
    };
    cloudflare_hint(account, what, name, size, status, via_cloudflare)
}

/// The wording, from what is known: the answer came from Cloudflare
/// itself (a 413 or a 5xx with its headers), or the connection was cut
/// while a file over the limit was on its way.
fn cloudflare_hint(
    account: &CloudAccount,
    what: &str,
    name: &str,
    size: u64,
    status: Option<u16>,
    via_cloudflare: bool,
) -> Option<String> {
    if account.cloudflare || size <= CLOUDFLARE_CHUNK {
        return None;
    }
    let switch = format!(
        "Turn on \"Server is behind Cloudflare\" for this account under Settings → Cloud Storage and try again: the file is then uploaded in {} pieces.",
        crate::models::human_size(CLOUDFLARE_CHUNK)
    );
    match (status, via_cloudflare) {
        (Some(413), true) => Some(format!(
            "{what}: {name} is {}, and Cloudflare, which sits in front of this server, refuses uploads over 100 MB (HTTP 413). {switch}",
            crate::models::human_size(size)
        )),
        (Some(code), true) if (500..600).contains(&code) => Some(format!(
            "{what}: Cloudflare, which sits in front of this server, cut off the upload of {name} ({}, HTTP {code}); it refuses uploads over 100 MB. {switch}",
            crate::models::human_size(size)
        )),
        (Some(413), false) => Some(format!(
            "{what}: the server refused {name} as too large ({}, HTTP 413). If it is reached through Cloudflare, that is the proxy's 100 MB limit: {switch}",
            crate::models::human_size(size)
        )),
        (None, _) => Some(format!(
            "{what}: the connection was cut while uploading {name} ({}). If the server is reached through Cloudflare, that is the proxy's 100 MB limit: {switch}",
            crate::models::human_size(size)
        )),
        _ => None,
    }
}

/// The readable part of an error body: the message field of a JSON error
/// when there is one, else the first bit of the text.
pub(super) fn short_error(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        for k in ["error_msg", "error_summary", "detail", "message", "error"] {
            if let Some(s) = v[k].as_str() {
                return s.chars().take(200).collect();
            }
        }
        if let Some(s) = v["ocs"]["meta"]["message"].as_str() {
            return s.chars().take(200).collect();
        }
    }
    body.chars().take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::dropbox::dropbox_err;
    use crate::cloud::onedrive::{api_err, api_message};
    use crate::cloud::seafile::seafile_err;

    #[test]
    fn chunk_ranges_cover_the_file_exactly() {
        assert_eq!(chunk_ranges(0, 90), vec![(0, 0)]);
        assert_eq!(chunk_ranges(90, 90), vec![(0, 90)]);
        assert_eq!(chunk_ranges(91, 90), vec![(0, 90), (90, 1)]);
        assert_eq!(
            chunk_ranges(250, 100),
            vec![(0, 100), (100, 100), (200, 50)]
        );
        let total: u64 = chunk_ranges(1_234_567, CLOUDFLARE_CHUNK)
            .iter()
            .map(|(_, l)| l)
            .sum();
        assert_eq!(total, 1_234_567);
        assert!(CLOUDFLARE_CHUNK < 100 * 1000 * 1000);
    }

    #[test]
    fn cloudflare_hint_names_the_limit_and_the_switch() {
        let mut a = CloudAccount::empty();
        let big = 150 * 1000 * 1000;
        // Cloudflare's own 413: unmistakable.
        let m = cloudflare_hint(&a, "Upload failed", "big.iso", big, Some(413), true).unwrap();
        assert!(
            m.contains("Cloudflare")
                && m.contains("100 MB")
                && m.contains("Server is behind Cloudflare"),
            "{m}"
        );
        assert!(
            m.contains(&format!("{} pieces", crate::models::human_size(90_000_000))),
            "{m}"
        );
        // A 502 with Cloudflare's headers: the proxy cut it off.
        assert!(
            cloudflare_hint(&a, "Upload failed", "big.iso", big, Some(502), true)
                .unwrap()
                .contains("cut off")
        );
        // Connection dropped, no headers to go by: a "possibly" hint.
        assert!(
            cloudflare_hint(&a, "Upload failed", "big.iso", big, None, false)
                .unwrap()
                .contains("If the server is reached through Cloudflare")
        );
        // A small file, or the switch already on, or a plain server error: not this.
        assert!(cloudflare_hint(&a, "Upload failed", "small.pdf", 1000, Some(413), true).is_none());
        assert!(cloudflare_hint(&a, "Upload failed", "big.iso", big, Some(500), false).is_none());
        a.cloudflare = true;
        assert!(cloudflare_hint(&a, "Upload failed", "big.iso", big, Some(413), true).is_none());
    }

    fn status(code: u16, body: &str) -> ureq::Error {
        ureq::Error::Status(code, ureq::Response::new(code, "Status", body).unwrap())
    }

    fn transport() -> ureq::Error {
        ureq::get("not a url").call().unwrap_err()
    }

    /// The four `ureq::Error` mappers, pinned before they are merged: each
    /// has its own refusal wording, its own set of codes and its own order
    /// of message and status.
    #[test]
    fn each_service_maps_http_errors_in_its_own_words() {
        let login = "Upload: the server refused the login. Check the user name and the password.";
        assert_eq!(http_err("Upload", status(401, "x")), login);
        assert_eq!(http_err("Upload", status(403, "x")), login);
        assert_eq!(
            http_err("Upload", status(404, "x")),
            "Upload: not found. Check the server URL."
        );
        assert_eq!(
            http_err("Upload", status(500, r#"{"message":"  boom  "}"#)),
            "Upload: HTTP 500 boom"
        );
        assert_eq!(http_err("Upload", status(502, "")), "Upload: HTTP 502 ");

        assert_eq!(
            dropbox_err("Upload", status(401, "x")),
            "Upload: Dropbox refused the sign-in. Open Settings, Cloud Storage, and connect the account again."
        );
        assert_eq!(
            dropbox_err(
                "Upload",
                status(409, r#"{"error_summary":"path/conflict/.."}"#)
            ),
            "Upload: path/conflict/.. (HTTP 409)"
        );
        assert_eq!(
            dropbox_err("Upload", status(403, "")),
            "Upload:  (HTTP 403)"
        );
        assert_eq!(
            dropbox_err("Upload", status(404, "gone")),
            "Upload: gone (HTTP 404)"
        );

        let sign_in = "Upload: the server refused the sign-in. Check the e-mail and the password or API token.";
        assert_eq!(seafile_err("Upload", status(401, "x")), sign_in);
        assert_eq!(seafile_err("Upload", status(403, "x")), sign_in);
        assert_eq!(
            seafile_err("Upload", status(404, "x")),
            "Upload: not found. Check the server URL."
        );
        assert_eq!(
            seafile_err("Upload", status(500, r#"{"error_msg":"quota"}"#)),
            "Upload: quota (HTTP 500)"
        );

        assert_eq!(
            api_err("Upload", status(401, "x")),
            "Upload: the service refused the sign-in. Check the account under Settings, Online Accounts."
        );
        assert_eq!(
            api_err("Upload", status(403, r#"{"error":{"message":"denied"}}"#)),
            "Upload: denied (HTTP 403)"
        );
        assert_eq!(
            api_err("Upload", status(404, "oops")),
            "Upload: oops (HTTP 404)"
        );

        // A transport failure reads the same from all four.
        let t = match transport() {
            ureq::Error::Transport(t) => t.to_string(),
            other => panic!("expected a transport error, got {other}"),
        };
        let expected = format!("Upload: {t}");
        assert_eq!(http_err("Upload", transport()), expected);
        assert_eq!(dropbox_err("Upload", transport()), expected);
        assert_eq!(seafile_err("Upload", transport()), expected);
        assert_eq!(api_err("Upload", transport()), expected);
    }

    #[test]
    fn error_bodies_are_cut_to_their_message() {
        assert_eq!(
            short_error(r#"{"detail":"d","message":"m"}"#),
            "d",
            "the first key in the list wins"
        );
        assert_eq!(
            short_error(r#"{"ocs":{"meta":{"message":"ocs says"}}}"#),
            "ocs says"
        );
        assert_eq!(
            short_error(&format!(r#"{{"error":"{}"}}"#, "e".repeat(250))),
            "e".repeat(200)
        );
        assert_eq!(short_error(&"t".repeat(300)), "t".repeat(160));
        // JSON without a string message falls back to the text itself.
        assert_eq!(
            short_error(r#"{"error":{"code":5}}"#),
            r#"{"error":{"code":5}}"#
        );
        assert_eq!(api_message(r#"{"error":{"message":"graph"}}"#), "graph");
        assert_eq!(api_message(r#"{"error":"plain"}"#), "plain");
    }
}
