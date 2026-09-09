//! Cloud attachments (#144): upload a file to a Nextcloud, ownCloud or
//! OpenCloud server over WebDAV and share it by public link, so a large
//! file travels as a link in the message instead of an attachment.
//!
//! Accounts live in `cloud.toml` beside the other settings; each app
//! password is in the system keyring under `cloud:<url>|<user>`. The
//! server API is the one every `*cloud` shares: WebDAV under
//! `remote.php/dav/files/<user>/` for the upload, and the OCS files-sharing
//! endpoint for the public link, with the optional expiry and password.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CloudAccount {
    /// How the account is shown ("Work Nextcloud").
    pub name: String,
    /// The server's base URL, e.g. `https://cloud.example.com`.
    pub url: String,
    pub user: String,
    /// The folder uploads go to, under the user's files.
    #[serde(default = "default_folder")]
    pub folder: String,
    /// Links expire this many days after upload; 0 keeps them.
    #[serde(default)]
    pub expire_days: u32,
    /// Protect every link with a generated download password.
    #[serde(default)]
    pub password: bool,
}

fn default_folder() -> String {
    "Vireo".to_string()
}

impl CloudAccount {
    pub fn empty() -> Self {
        CloudAccount {
            name: String::new(),
            url: String::new(),
            user: String::new(),
            folder: default_folder(),
            expire_days: 7,
            password: false,
        }
    }

    /// The base URL as the requests want it: a scheme, no trailing slash.
    pub fn base(&self) -> String {
        let u = self.url.trim().trim_end_matches('/');
        if u.contains("://") {
            u.to_string()
        } else {
            format!("https://{u}")
        }
    }

    /// The keyring key for this account's app password.
    pub fn key(&self) -> String {
        format!("cloud:{}|{}", self.base(), self.user.trim())
    }

    fn folder_clean(&self) -> String {
        let f = self.folder.trim().trim_matches('/');
        if f.is_empty() {
            default_folder()
        } else {
            f.to_string()
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct CloudFile {
    #[serde(default)]
    accounts: Vec<CloudAccount>,
}

fn path() -> Option<PathBuf> {
    Some(crate::config::config_base()?.join("vireo").join("cloud.toml"))
}

pub fn load_accounts() -> Vec<CloudAccount> {
    let Some(path) = path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    toml::from_str::<CloudFile>(&text).map(|f| f.accounts).unwrap_or_default()
}

pub fn save_accounts(accounts: &[CloudAccount]) {
    let Some(path) = path() else { return };
    let file = CloudFile { accounts: accounts.to_vec() };
    match toml::to_string_pretty(&file) {
        Ok(toml) => {
            if let Err(e) = crate::config::write_private_file(&path, &toml) {
                tracing::warn!("could not save cloud accounts: {e}");
            }
        }
        Err(e) => tracing::warn!("could not serialize cloud accounts: {e}"),
    }
}

/// What an upload produced: the public link and what it was made with.
#[derive(Clone, Debug)]
pub struct ShareResult {
    pub name: String,
    pub size: u64,
    pub url: String,
    /// The download password, when the account protects links.
    pub password: Option<String>,
    /// The expiry date (YYYY-MM-DD), when the account sets one.
    pub expires: Option<String>,
}

fn auth(account: &CloudAccount, password: &str) -> String {
    let raw = format!("{}:{}", account.user.trim(), password);
    format!("Basic {}", crate::oauth::base64_encode(raw.as_bytes()))
}

/// Percent-encode one path segment for a WebDAV URL.
fn seg(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn dav_root(account: &CloudAccount) -> String {
    format!("{}/remote.php/dav/files/{}", account.base(), seg(account.user.trim()))
}

fn http_err(what: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) | ureq::Error::Status(403, _) => {
            format!("{what}: the server refused the login. Check the user name and the app password.")
        }
        ureq::Error::Status(404, _) => format!("{what}: not found. Check the server URL."),
        ureq::Error::Status(code, resp) => {
            let text = resp.into_string().unwrap_or_default();
            let text: String = text.chars().take(160).collect();
            format!("{what}: HTTP {code} {}", text.trim())
        }
        ureq::Error::Transport(t) => format!("{what}: {t}"),
    }
}

/// Check the account signs in: the OCS user endpoint answers with the
/// display name.
pub fn verify(account: &CloudAccount, password: &str) -> Result<String, String> {
    let url = format!("{}/ocs/v2.php/cloud/user?format=json", account.base());
    let v: serde_json::Value = ureq::get(&url)
        .set("Authorization", &auth(account, password))
        .set("OCS-APIRequest", "true")
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| http_err("Could not sign in", e))?
        .into_json()
        .map_err(|e| format!("Could not read the server's answer: {e}"))?;
    let data = &v["ocs"]["data"];
    let name = data["displayname"]
        .as_str()
        .or_else(|| data["display-name"].as_str())
        .or_else(|| data["id"].as_str())
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        return Err("The server answered, but not like a Nextcloud: is the URL its root?".to_string());
    }
    Ok(name)
}

/// Upload `path` into the account's folder and share it by public link.
/// A file already there by that name is left alone: the upload takes a
/// name with the time in it instead.
pub fn upload_and_share(account: &CloudAccount, password: &str, path: &Path) -> Result<ShareResult, String> {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "the file has no name".to_string())?;
    let meta = std::fs::metadata(path).map_err(|e| format!("could not read {name}: {e}"))?;
    let size = meta.len();
    let folder = account.folder_clean();
    let root = dav_root(account);
    let authz = auth(account, password);
    let timeout = std::time::Duration::from_secs(60 * 60);

    // The folder: MKCOL says 405 when it already exists, which is fine.
    let mut dir = String::new();
    for part in folder.split('/').filter(|p| !p.is_empty()) {
        dir.push('/');
        dir.push_str(&seg(part));
        let r = ureq::request("MKCOL", &format!("{root}{dir}"))
            .set("Authorization", &authz)
            .call();
        match r {
            Ok(_) | Err(ureq::Error::Status(405, _)) => {}
            Err(e) => return Err(http_err("Could not create the folder", e)),
        }
    }

    // A name that is free.
    let mut remote = name.clone();
    let exists = |n: &str| {
        ureq::head(&format!("{root}{dir}/{}", seg(n)))
            .set("Authorization", &authz)
            .call()
            .is_ok()
    };
    if exists(&remote) {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        remote = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{stamp}.{ext}"),
            _ => format!("{name}-{stamp}"),
        };
    }

    let file = std::fs::File::open(path).map_err(|e| format!("could not read {name}: {e}"))?;
    ureq::put(&format!("{root}{dir}/{}", seg(&remote)))
        .set("Authorization", &authz)
        .set("Content-Length", &size.to_string())
        .set("Content-Type", "application/octet-stream")
        .timeout(timeout)
        .send(file)
        .map_err(|e| http_err("Upload failed", e))?;

    // The public link.
    let share_path = format!("/{folder}/{remote}");
    let expires = (account.expire_days > 0).then(|| {
        (chrono::Local::now() + chrono::Duration::days(account.expire_days as i64))
            .format("%Y-%m-%d")
            .to_string()
    });
    let pw = if account.password { Some(generate_password()) } else { None };
    let mut form: Vec<(&str, String)> = vec![
        ("path", share_path),
        ("shareType", "3".to_string()),
        ("permissions", "1".to_string()),
    ];
    if let Some(d) = &expires {
        form.push(("expireDate", d.clone()));
    }
    if let Some(p) = &pw {
        form.push(("password", p.clone()));
    }
    let form_ref: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let v: serde_json::Value = ureq::post(&format!(
        "{}/ocs/v2.php/apps/files_sharing/api/v1/shares?format=json",
        account.base()
    ))
    .set("Authorization", &authz)
    .set("OCS-APIRequest", "true")
    .timeout(std::time::Duration::from_secs(60))
    .send_form(&form_ref)
    .map_err(|e| http_err("Uploaded, but could not create the share link", e))?
    .into_json()
    .map_err(|e| format!("Uploaded, but could not read the share answer: {e}"))?;
    let url = v["ocs"]["data"]["url"].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        let msg = v["ocs"]["meta"]["message"].as_str().unwrap_or("no link in the answer");
        return Err(format!("Uploaded, but the server made no share link: {msg}"));
    }
    Ok(ShareResult { name: remote, size, url, password: pw, expires })
}

/// A download password people can read out: letters and digits, no
/// look-alikes, twelve long.
fn generate_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut buf = [0u8; 12];
    if crate::rng::fill(&mut buf).is_err() {
        // A clock-seeded fallback is still a password, if a weaker one.
        let t = crate::datefmt::now() as u64;
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (t.rotate_left(i as u32 * 5) & 0xff) as u8;
        }
    }
    buf.iter().map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char).collect()
}

/// "2.3 MB" for a link's caption.
pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "kB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_gets_a_scheme_and_loses_its_slash() {
        let mut a = CloudAccount::empty();
        a.url = "cloud.example.com/".into();
        assert_eq!(a.base(), "https://cloud.example.com");
        a.url = "http://localhost:8080/nextcloud/".into();
        assert_eq!(a.base(), "http://localhost:8080/nextcloud");
    }

    #[test]
    fn dav_segments_are_encoded() {
        assert_eq!(seg("Q3 report.pdf"), "Q3%20report.pdf");
        assert_eq!(seg("caf\u{e9}"), "caf%C3%A9");
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2_300_000), "2.3 MB");
    }

    #[test]
    fn passwords_are_readable_and_long_enough() {
        let p = generate_password();
        assert_eq!(p.len(), 12);
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
