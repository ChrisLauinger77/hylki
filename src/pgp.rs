//! OpenPGP (#133), first slice: decrypt and verify what arrives, through the
//! user's own GnuPG — `gpg` on the path, the keyring in `~/.gnupg`, the agent
//! (and its pinentry) for passphrases. Vireo never sees a secret key and never
//! writes anything decrypted to disk: the worker renders a decrypted message
//! straight to the reader and leaves the cache alone.
//!
//! What is recognised: PGP/MIME (RFC 3156) `multipart/encrypted` and
//! `multipart/signed`, and the older inline forms — an armoured
//! `-----BEGIN PGP MESSAGE-----` or a clear-signed block in the text body.
//! Signing and encrypting outgoing mail, and key management, are later slices.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::i18n::{i18n, i18n_f};
use crate::models::{PgpSignature, PgpStatus, PgpTrust};

/// A GnuPG to talk to: the system one by default, or one with its own home
/// directory (tests build a throwaway keyring).
#[derive(Debug, Clone, Default)]
pub struct Gpg {
    /// `GNUPGHOME` for the child process; `None` = the user's own.
    pub home: Option<PathBuf>,
}

impl Gpg {
    /// The user's GnuPG (honouring `VIREO_GNUPGHOME`, for trying an
    /// alternative keyring without touching the real one).
    pub fn system() -> Gpg {
        Gpg { home: std::env::var_os("VIREO_GNUPGHOME").map(PathBuf::from) }
    }

    fn command(&self) -> Command {
        let mut c = Command::new("gpg");
        // `--batch` keeps gpg off the terminal; the agent still asks for a
        // passphrase through pinentry, which is exactly the prompt we want.
        c.args(["--batch", "--no-tty", "--status-fd", "2", "--exit-on-status-write-error"]);
        if let Some(h) = &self.home {
            c.env("GNUPGHOME", h);
        }
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        c
    }
}

/// Whether a `gpg` answers at all. Checked once per process.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Command::new("gpg")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// The OpenPGP structure a raw message carries, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// PGP/MIME `multipart/encrypted`: the armoured ciphertext (part two).
    Encrypted { armored: Vec<u8> },
    /// PGP/MIME `multipart/signed`: the signed entity's exact bytes and the
    /// detached signature.
    Signed { data: Vec<u8>, signature: Vec<u8> },
    /// An armoured message inside a text body.
    InlineEncrypted { armored: Vec<u8> },
    /// A clear-signed block inside a text body.
    InlineSigned { text: Vec<u8> },
}

impl Shape {
    pub fn is_encrypted(&self) -> bool {
        matches!(self, Shape::Encrypted { .. } | Shape::InlineEncrypted { .. })
    }
}

/// Whether the message is OpenPGP-encrypted (either form). Cheap: no gpg.
/// The prefetcher asks this so it never triggers a passphrase prompt on its
/// own; only an open decrypts.
pub fn is_encrypted(raw: &[u8]) -> bool {
    detect(raw).is_some_and(|s| s.is_encrypted())
}

/// Find the OpenPGP structure in a raw message.
pub fn detect(raw: &[u8]) -> Option<Shape> {
    use mail_parser::{MessageParser, MimeHeaders, PartType};
    let parsed = MessageParser::default().parse(raw)?;
    let root = parsed.root_part();
    let ctype = parsed.content_type();
    let (ct_type, ct_sub, protocol, boundary) = match ctype {
        Some(ct) => (
            ct.ctype().to_ascii_lowercase(),
            ct.subtype().map(|s| s.to_ascii_lowercase()).unwrap_or_default(),
            ct.attribute("protocol").map(|p| p.to_ascii_lowercase()).unwrap_or_default(),
            ct.attribute("boundary").map(str::to_string),
        ),
        None => (String::new(), String::new(), String::new(), None),
    };
    if ct_type == "multipart" {
        let PartType::Multipart(children) = &root.body else {
            return None;
        };
        if ct_sub == "encrypted" && protocol == "application/pgp-encrypted" {
            // Part one is the version stub; part two carries the ciphertext.
            // Take the first part that looks like an armoured message rather
            // than trusting positions.
            for id in children {
                let part = parsed.part(*id)?;
                let text = part_text(part);
                if text.contains("-----BEGIN PGP MESSAGE-----") {
                    return Some(Shape::Encrypted { armored: text.into_bytes() });
                }
            }
            return None;
        }
        if ct_sub == "signed" && protocol == "application/pgp-signature" {
            let (first, second) = (children.first()?, children.get(1)?);
            let first = parsed.part(*first)?;
            let sig = parsed.part(*second)?;
            let boundary = boundary?;
            let data = signed_bytes(raw, first.offset_header, &boundary)?;
            let signature = part_text(sig).into_bytes();
            if !signature.contains_str("-----BEGIN PGP SIGNATURE-----") {
                return None;
            }
            return Some(Shape::Signed { data, signature });
        }
    }
    // Inline forms: the first text body.
    let text = parsed.body_text(0)?;
    if text.contains("-----BEGIN PGP MESSAGE-----") && text.contains("-----END PGP MESSAGE-----") {
        return Some(Shape::InlineEncrypted { armored: armored_block(&text, "MESSAGE").into_bytes() });
    }
    if text.contains("-----BEGIN PGP SIGNED MESSAGE-----") && text.contains("-----END PGP SIGNATURE-----") {
        return Some(Shape::InlineSigned { text: text.into_owned().into_bytes() });
    }
    None
}

trait ContainsStr {
    fn contains_str(&self, needle: &str) -> bool;
}

impl ContainsStr for Vec<u8> {
    fn contains_str(&self, needle: &str) -> bool {
        self.windows(needle.len()).any(|w| w == needle.as_bytes())
    }
}

/// A part's decoded text (an armoured block is text whatever it is labelled).
fn part_text(part: &mail_parser::MessagePart) -> String {
    use mail_parser::PartType;
    match &part.body {
        PartType::Text(t) | PartType::Html(t) => t.to_string(),
        PartType::Binary(b) | PartType::InlineBinary(b) => String::from_utf8_lossy(b).into_owned(),
        _ => String::new(),
    }
}

/// The armoured block between the BEGIN and END lines, inclusive.
fn armored_block(text: &str, kind: &str) -> String {
    let begin = format!("-----BEGIN PGP {kind}-----");
    let end = format!("-----END PGP {kind}-----");
    let (Some(b), Some(e)) = (text.find(&begin), text.find(&end)) else {
        return text.to_string();
    };
    text[b..e + end.len()].to_string()
}

/// The exact bytes the signature covers (RFC 3156 §5): the signed part from
/// its first header up to, but not including, the CRLF that precedes the
/// boundary delimiter. Found on the raw bytes, not through the parser, so
/// nothing is re-encoded on the way.
fn signed_bytes(raw: &[u8], start: usize, boundary: &str) -> Option<Vec<u8>> {
    if start >= raw.len() {
        return None;
    }
    let crlf = format!("\r\n--{boundary}");
    let lf = format!("\n--{boundary}");
    let rest = &raw[start..];
    let end = find(rest, crlf.as_bytes()).or_else(|| find(rest, lf.as_bytes()))?;
    Some(rest[..end].to_vec())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The outcome of handling a message: what to render, and the verdict.
#[derive(Debug, Clone)]
pub struct Unwrapped {
    /// The MIME entity to render and take attachments from: the decrypted
    /// message, or the signed part. `None` when there is nothing readable
    /// (undecryptable), in which case the reader shows the verdict instead.
    pub inner: Option<Vec<u8>>,
    pub status: PgpStatus,
}

/// Decrypt and/or verify a message, whatever form it takes. `None` when the
/// message carries no OpenPGP structure at all.
pub fn unwrap_message(raw: &[u8], gpg: &Gpg) -> Option<Unwrapped> {
    let shape = detect(raw)?;
    if !available() {
        let status = PgpStatus {
            encrypted: shape.is_encrypted(),
            decrypted: false,
            signature: PgpSignature::None,
            notes: vec![i18n("GnuPG (gpg) is not installed, so this message cannot be read.")],
        };
        // A signed message is still readable without gpg.
        let inner = match &shape {
            Shape::Signed { data, .. } => Some(data.clone()),
            Shape::InlineSigned { text } => Some(text_entity(text)),
            _ => None,
        };
        return Some(Unwrapped { inner, status });
    }
    Some(match shape {
        Shape::Encrypted { armored } | Shape::InlineEncrypted { armored } => {
            let out = run(gpg, &["--decrypt"], &armored, None);
            let outcome = parse_status(&out.status_lines);
            let decrypted = outcome.decryption_ok && !out.stdout.is_empty();
            let mut notes = Vec::new();
            let signature = signature_verdict(&outcome, &mut notes);
            if decrypted {
                notes.insert(0, i18n("Decrypted with your key."));
            } else if let Some(k) = outcome.no_seckey.first() {
                notes.insert(0, i18n_f("Encrypted to key {id}, which is not in your keyring.", &[("id", &key_display(k))]));
            } else if outcome.nodata {
                notes.insert(0, i18n("The encrypted block is damaged or not OpenPGP data."));
            } else if let Some(d) = out.detail.clone() {
                notes.insert(0, d);
            }
            let inner = decrypted.then(|| {
                // PGP/MIME wraps a full MIME entity; inline PGP is bare text.
                if looks_like_mime(&out.stdout) {
                    out.stdout.clone()
                } else {
                    text_entity(&out.stdout)
                }
            });
            Unwrapped { inner, status: PgpStatus { encrypted: true, decrypted, signature, notes } }
        }
        Shape::Signed { data, signature } => {
            let out = run(gpg, &["--verify"], &data, Some(&signature));
            let outcome = parse_status(&out.status_lines);
            let mut notes = Vec::new();
            let signature = signature_verdict(&outcome, &mut notes);
            if matches!(signature, PgpSignature::None) {
                if let Some(d) = out.detail.clone() {
                    notes.push(d);
                }
            }
            Unwrapped {
                inner: Some(data),
                status: PgpStatus { encrypted: false, decrypted: false, signature, notes },
            }
        }
        Shape::InlineSigned { text } => {
            // `--decrypt` on a clear-signed block verifies it and prints the
            // signed text, dearmoured.
            let out = run(gpg, &["--decrypt"], &text, None);
            let outcome = parse_status(&out.status_lines);
            let mut notes = Vec::new();
            let signature = signature_verdict(&outcome, &mut notes);
            let inner = if out.stdout.is_empty() { text_entity(&text) } else { text_entity(&out.stdout) };
            Unwrapped {
                inner: Some(inner),
                status: PgpStatus { encrypted: false, decrypted: false, signature, notes },
            }
        }
    })
}

/// Whether decrypted bytes are a MIME entity (headers first) rather than
/// bare text.
fn looks_like_mime(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let first = head.lines().next().unwrap_or("");
    let lower = first.to_ascii_lowercase();
    lower.starts_with("content-type:")
        || lower.starts_with("content-transfer-encoding:")
        || lower.starts_with("content-disposition:")
        || lower.starts_with("mime-version:")
}

/// Bare text as a `text/plain` entity the renderer understands.
fn text_entity(text: &[u8]) -> Vec<u8> {
    let mut out = b"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n".to_vec();
    out.extend_from_slice(text);
    out
}

/// What one gpg run produced.
#[derive(Debug, Default)]
struct Run {
    stdout: Vec<u8>,
    /// The `[GNUPG:] …` machine lines, prefix stripped.
    status_lines: Vec<String>,
    /// The last human line gpg printed, for a failure nobody parsed.
    detail: Option<String>,
}

/// Run gpg with `input` on stdin and, for a detached verification, the
/// signature in a temporary file (gpg takes the signature as a path and the
/// data on stdin; the other way round is not offered).
fn run(gpg: &Gpg, args: &[&str], input: &[u8], detached_sig: Option<&[u8]>) -> Run {
    let mut cmd = gpg.command();
    cmd.args(args);
    let sig_path = detached_sig.map(|sig| {
        let path = std::env::temp_dir().join(format!(
            "vireo-sig-{}-{}.asc",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::write(&path, sig);
        path
    });
    if let Some(p) = &sig_path {
        cmd.arg(p);
        cmd.arg("-");
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if let Some(p) = &sig_path {
                let _ = std::fs::remove_file(p);
            }
            return Run { detail: Some(format!("gpg: {e}")), ..Default::default() };
        }
    };
    // Feed stdin from a thread: gpg may start writing before it has read
    // everything, and a pipe full both ways is a deadlock.
    let stdin = child.stdin.take();
    let input = input.to_vec();
    let feeder = std::thread::spawn(move || {
        if let Some(mut s) = stdin {
            let _ = s.write_all(&input);
        }
    });
    let output = child.wait_with_output();
    let _ = feeder.join();
    if let Some(p) = &sig_path {
        let _ = std::fs::remove_file(p);
    }
    let Ok(output) = output else {
        return Run { detail: Some("gpg: no output".into()), ..Default::default() };
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut status_lines = Vec::new();
    let mut detail = None;
    for line in stderr.lines() {
        if let Some(s) = line.strip_prefix("[GNUPG:] ") {
            status_lines.push(s.to_string());
        } else if !line.trim().is_empty() {
            let l = line.trim_start_matches("gpg: ").trim().to_string();
            tracing::debug!(target: "vireo::pgp", "gpg: {l}");
            detail = Some(l);
        }
    }
    for s in &status_lines {
        tracing::debug!(target: "vireo::pgp", "[GNUPG:] {s}");
    }
    Run { stdout: output.stdout, status_lines, detail }
}

/// The signature-related lines of a gpg run, distilled.
#[derive(Debug, Default, PartialEq, Eq)]
struct Outcome {
    decryption_ok: bool,
    no_seckey: Vec<String>,
    nodata: bool,
    /// GOODSIG / BADSIG / EXPKEYSIG / REVKEYSIG / EXPSIG: (kind, key id, user id).
    sig: Option<(String, String, String)>,
    /// ERRSIG's key id when the key is missing (NO_PUBKEY, or rc 9).
    missing_key: Option<String>,
    trust: Option<PgpTrust>,
    fingerprint: Option<String>,
}

/// Read gpg's status lines (`doc/DETAILS` in the GnuPG source).
fn parse_status(lines: &[String]) -> Outcome {
    let mut o = Outcome::default();
    for line in lines {
        let mut it = line.splitn(2, ' ');
        let tag = it.next().unwrap_or("");
        let rest = it.next().unwrap_or("").trim();
        match tag {
            "DECRYPTION_OKAY" => o.decryption_ok = true,
            "NO_SECKEY" => o.no_seckey.push(rest.to_string()),
            "NODATA" => o.nodata = true,
            "GOODSIG" | "BADSIG" | "EXPKEYSIG" | "REVKEYSIG" | "EXPSIG" => {
                let (key, uid) = rest.split_once(' ').unwrap_or((rest, ""));
                // A later line never downgrades a verdict already read for
                // the same signature; the first one is the one gpg meant.
                if o.sig.is_none() {
                    o.sig = Some((tag.to_string(), key.to_string(), uid.to_string()));
                }
            }
            "VALIDSIG" => {
                if let Some(fpr) = rest.split(' ').next() {
                    o.fingerprint = Some(fpr.to_string());
                }
            }
            "ERRSIG" => {
                // ERRSIG <keyid> <pkalgo> <hashalgo> <sigclass> <time> <rc> [<fpr>]
                let f: Vec<&str> = rest.split(' ').collect();
                if f.get(5) == Some(&"9") {
                    o.missing_key = f.first().map(|s| s.to_string());
                }
            }
            "NO_PUBKEY" => o.missing_key = Some(rest.to_string()),
            "TRUST_UNDEFINED" => o.trust = Some(PgpTrust::Unknown),
            "TRUST_NEVER" => o.trust = Some(PgpTrust::Never),
            "TRUST_MARGINAL" => o.trust = Some(PgpTrust::Marginal),
            "TRUST_FULLY" | "TRUST_ULTIMATE" => o.trust = Some(PgpTrust::Full),
            _ => {}
        }
    }
    o
}

/// The verdict on the signature, with its explanatory lines.
fn signature_verdict(o: &Outcome, notes: &mut Vec<String>) -> PgpSignature {
    if let Some((kind, key, uid)) = &o.sig {
        let signer = if uid.is_empty() { key_display(key) } else { uid.clone() };
        let key_id = o.fingerprint.clone().unwrap_or_else(|| key.clone());
        return match kind.as_str() {
            "GOODSIG" => {
                let trust = o.trust.unwrap_or(PgpTrust::Unknown);
                notes.push(match trust {
                    PgpTrust::Full => i18n_f("Signing key {id} is trusted in your keyring.", &[("id", &key_display(&key_id))]),
                    PgpTrust::Marginal => i18n_f("Signing key {id} is only marginally trusted in your keyring.", &[("id", &key_display(&key_id))]),
                    PgpTrust::Never => i18n_f("Signing key {id} is marked as not trusted in your keyring.", &[("id", &key_display(&key_id))]),
                    PgpTrust::Unknown => i18n_f(
                        "Signing key {id} is in your keyring but not marked as trusted; check its fingerprint with the sender.",
                        &[("id", &key_display(&key_id))],
                    ),
                });
                PgpSignature::Good { signer, key_id, trust }
            }
            "BADSIG" => {
                notes.push(i18n_f("Signed with key {id}.", &[("id", &key_display(&key_id))]));
                PgpSignature::Bad { signer }
            }
            "EXPKEYSIG" => PgpSignature::ExpiredKey { signer },
            "REVKEYSIG" => PgpSignature::RevokedKey { signer },
            _ => PgpSignature::ExpiredSignature { signer },
        };
    }
    if let Some(k) = &o.missing_key {
        notes.push(i18n("Import the sender's public key to verify it."));
        return PgpSignature::NoKey { key_id: k.clone() };
    }
    PgpSignature::None
}

/// A key id or fingerprint in readable groups: the last sixteen hex digits,
/// four at a time.
pub fn key_display(id: &str) -> String {
    let hex: String = id.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_ascii_uppercase();
    let tail: String = hex.chars().rev().take(16).collect::<Vec<_>>().into_iter().rev().collect();
    tail.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn status_lines_distil_to_an_outcome() {
        let o = parse_status(&lines(&[
            "ENC_TO 1234567890ABCDEF 1 0",
            "DECRYPTION_OKAY",
            "GOODSIG 1234567890ABCDEF Ada Lovelace <ada@example.org>",
            "VALIDSIG 0123456789ABCDEF0123456789ABCDEF01234567 2026-09-07 1 0 4 0 1 8 00 0123456789ABCDEF0123456789ABCDEF01234567",
            "TRUST_ULTIMATE 0 pgp",
        ]));
        assert!(o.decryption_ok);
        assert_eq!(o.sig.as_ref().unwrap().0, "GOODSIG");
        assert_eq!(o.sig.as_ref().unwrap().2, "Ada Lovelace <ada@example.org>");
        assert_eq!(o.trust, Some(PgpTrust::Full));
        assert_eq!(o.fingerprint.as_deref(), Some("0123456789ABCDEF0123456789ABCDEF01234567"));

        let o = parse_status(&lines(&["ERRSIG 1234567890ABCDEF 1 8 00 1757000000 9 -", "NO_PUBKEY 1234567890ABCDEF"]));
        assert_eq!(o.missing_key.as_deref(), Some("1234567890ABCDEF"));
        assert!(o.sig.is_none());

        let o = parse_status(&lines(&["ENC_TO AAAA 1 0", "NO_SECKEY AAAA", "DECRYPTION_FAILED"]));
        assert!(!o.decryption_ok);
        assert_eq!(o.no_seckey, vec!["AAAA".to_string()]);
    }

    #[test]
    fn verdicts_follow_the_status() {
        let mut notes = Vec::new();
        let o = parse_status(&lines(&["GOODSIG AA Ada <a@b.c>", "TRUST_UNDEFINED 0 pgp"]));
        match signature_verdict(&o, &mut notes) {
            PgpSignature::Good { signer, trust, .. } => {
                assert_eq!(signer, "Ada <a@b.c>");
                assert_eq!(trust, PgpTrust::Unknown);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(notes.len(), 1);
        let o = parse_status(&lines(&["BADSIG AA Ada <a@b.c>"]));
        assert!(matches!(signature_verdict(&o, &mut notes), PgpSignature::Bad { .. }));
        let o = parse_status(&lines(&["EXPKEYSIG AA Ada <a@b.c>"]));
        assert!(matches!(signature_verdict(&o, &mut notes), PgpSignature::ExpiredKey { .. }));
        let o = parse_status(&lines(&["NO_PUBKEY AA"]));
        assert!(matches!(signature_verdict(&o, &mut notes), PgpSignature::NoKey { .. }));
        assert!(matches!(signature_verdict(&Outcome::default(), &mut notes), PgpSignature::None));
    }

    #[test]
    fn key_ids_read_in_groups_of_four() {
        assert_eq!(key_display("0123456789ABCDEF0123456789abcdef01234567"), "89AB CDEF 0123 4567");
        assert_eq!(key_display("1234567890ABCDEF"), "1234 5678 90AB CDEF");
    }

    const SIGNED: &str = "From: a@b.c\r\nTo: d@e.f\r\nSubject: hi\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/signed; micalg=pgp-sha256; protocol=\"application/pgp-signature\"; boundary=\"bnd\"\r\n\r\n\
--bnd\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhello signed\r\nsecond line\r\n\
--bnd\r\nContent-Type: application/pgp-signature; name=\"signature.asc\"\r\n\r\n\
-----BEGIN PGP SIGNATURE-----\r\n\r\nabc\r\n-----END PGP SIGNATURE-----\r\n--bnd--\r\n";

    /// The signed bytes run from the part's first header to just before the
    /// CRLF that precedes the boundary — exactly what the signer hashed.
    #[test]
    fn detects_pgp_mime_signed_and_takes_exact_bytes() {
        match detect(SIGNED.as_bytes()) {
            Some(Shape::Signed { data, signature }) => {
                assert_eq!(
                    String::from_utf8(data).unwrap(),
                    "Content-Type: text/plain; charset=utf-8\r\n\r\nhello signed\r\nsecond line"
                );
                assert!(String::from_utf8(signature).unwrap().starts_with("-----BEGIN PGP SIGNATURE-----"));
            }
            other => panic!("{other:?}"),
        }
    }

    const ENCRYPTED: &str = "From: a@b.c\r\nSubject: s\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"enc\"\r\n\r\n\
--enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\
--enc\r\nContent-Type: application/octet-stream; name=\"encrypted.asc\"\r\n\r\n\
-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----\r\n--enc--\r\n";

    #[test]
    fn detects_pgp_mime_encrypted_and_inline_forms() {
        match detect(ENCRYPTED.as_bytes()) {
            Some(Shape::Encrypted { armored }) => {
                let a = String::from_utf8(armored).unwrap();
                assert!(a.starts_with("-----BEGIN PGP MESSAGE-----"), "{a}");
            }
            other => panic!("{other:?}"),
        }
        assert!(is_encrypted(ENCRYPTED.as_bytes()));
        assert!(!is_encrypted(SIGNED.as_bytes()));

        let inline = "From: a@b.c\r\nContent-Type: text/plain\r\n\r\nSee below\r\n\
-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----\r\n";
        match detect(inline.as_bytes()) {
            Some(Shape::InlineEncrypted { armored }) => {
                assert_eq!(String::from_utf8(armored).unwrap(), "-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----");
            }
            other => panic!("{other:?}"),
        }
        let clear = "From: a@b.c\r\nContent-Type: text/plain\r\n\r\n-----BEGIN PGP SIGNED MESSAGE-----\r\nHash: SHA256\r\n\r\nhi\r\n-----BEGIN PGP SIGNATURE-----\r\n\r\nabc\r\n-----END PGP SIGNATURE-----\r\n";
        assert!(matches!(detect(clear.as_bytes()), Some(Shape::InlineSigned { .. })));
        assert_eq!(detect(b"From: a@b.c\r\nContent-Type: text/plain\r\n\r\nplain mail\r\n"), None);
        // A plain message that merely quotes the markers is not a signed one.
        let html_only = "From: a@b.c\r\nContent-Type: text/html\r\n\r\n<p>x</p>\r\n";
        assert_eq!(detect(html_only.as_bytes()), None);
    }

    #[test]
    fn mime_versus_bare_text_is_told_by_the_first_line() {
        assert!(looks_like_mime(b"Content-Type: text/plain\r\n\r\nx"));
        assert!(looks_like_mime(b"MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n"));
        assert!(!looks_like_mime(b"Hello there\r\n"));
        let e = text_entity(b"hi");
        assert!(e.starts_with(b"Content-Type: text/plain"));
    }

    /// The end-to-end path against a real gpg with a throwaway keyring:
    /// encrypt-and-sign a MIME entity, then watch it come back decrypted with
    /// the signature verified. Skipped where gpg is missing.
    #[test]
    fn round_trip_through_a_throwaway_keyring() {
        if !available() {
            eprintln!("gpg not installed; skipping");
            return;
        }
        let home = std::env::temp_dir().join(format!("vireo-gpg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let gpg = Gpg { home: Some(home.clone()) };
        let status = |args: &[&str], input: &[u8]| -> Run { run(&gpg, args, input, None) };
        let gen = status(&["--passphrase", "", "--pinentry-mode", "loopback", "--quick-gen-key", "Test Vireo <test@vireo.invalid>", "default", "default", "never"], b"");
        assert!(gen.status_lines.iter().any(|l| l.starts_with("KEY_CREATED")), "{gen:?}");
        let entity = b"Content-Type: text/plain; charset=utf-8\r\n\r\nsecret hello\r\n";
        let enc = status(&["--armor", "--trust-model", "always", "--pinentry-mode", "loopback", "--passphrase", "", "--recipient", "test@vireo.invalid", "--sign", "--encrypt"], entity);
        let armored = String::from_utf8(enc.stdout.clone()).unwrap();
        assert!(armored.contains("-----BEGIN PGP MESSAGE-----"), "{:?}", enc.status_lines);
        let mail = format!(
            "From: test@vireo.invalid\r\nSubject: s\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"enc\"\r\n\r\n\
             --enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\
             --enc\r\nContent-Type: application/octet-stream\r\n\r\n{armored}\r\n--enc--\r\n"
        );
        let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
        assert!(u.status.encrypted && u.status.decrypted, "{:?}", u.status);
        let inner = String::from_utf8(u.inner.clone().unwrap()).unwrap();
        assert!(inner.contains("secret hello"), "{inner}");
        assert!(matches!(u.status.signature, PgpSignature::Good { trust: PgpTrust::Full, .. }), "{:?}", u.status);

        // Detached signature over a MIME part, verified the RFC 3156 way.
        let part = b"Content-Type: text/plain; charset=utf-8\r\n\r\nsigned hello";
        let sig = status(&["--armor", "--pinentry-mode", "loopback", "--passphrase", "", "--detach-sign"], part);
        let sig = String::from_utf8(sig.stdout).unwrap();
        let mail = format!(
            "From: test@vireo.invalid\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/signed; micalg=pgp-sha256; protocol=\"application/pgp-signature\"; boundary=\"b\"\r\n\r\n\
             --b\r\n{}\r\n--b\r\nContent-Type: application/pgp-signature\r\n\r\n{sig}\r\n--b--\r\n",
            String::from_utf8_lossy(part)
        );
        let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
        assert!(!u.status.encrypted);
        assert!(matches!(u.status.signature, PgpSignature::Good { .. }), "{:?}", u.status);
        assert_eq!(u.inner.unwrap(), part.to_vec());

        // A tampered signed part fails.
        let mail = mail.replace("signed hello", "signed hellO");
        let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
        assert!(matches!(u.status.signature, PgpSignature::Bad { .. }), "{:?}", u.status);

        // Encrypted to a key we don't have: reported, nothing rendered.
        let other = "-----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA\r\n-----END PGP MESSAGE-----";
        let mail = format!("From: x@y.z\r\nContent-Type: text/plain\r\n\r\n{other}\r\n");
        let u = unwrap_message(mail.as_bytes(), &gpg).expect("recognised");
        assert!(u.status.encrypted && !u.status.decrypted);
        assert!(u.inner.is_none());

        let _ = std::process::Command::new("gpgconf").env("GNUPGHOME", &home).args(["--kill", "all"]).status();
        let _ = std::fs::remove_dir_all(&home);
    }
}
