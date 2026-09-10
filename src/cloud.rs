//! Cloud attachments (#144): upload a file to cloud storage and share it
//! by public link, so a large file travels as a link in the message
//! instead of an attachment. Three kinds of account:
//!
//! * **Nextcloud, ownCloud, OpenCloud**: WebDAV under
//!   `remote.php/dav/files/<user>/` for the upload and the OCS
//!   files-sharing endpoint for the public link, signed in with an app
//!   password.
//! * **Dropbox**: the HTTP API v2, signed in with OAuth (PKCE, so only an
//!   app key is needed; the refresh token is what the keyring holds).
//!   Files over the single-request limit go up in an upload session.
//! * **Seafile**: the web API, signed in with the account password (turned
//!   into an API token on the spot) or a pasted API token. Uploads go
//!   into a library, made when missing.
//! * **OneDrive**: through a GNOME Online Accounts Microsoft 365 account,
//!   whose token GOA hands out and refreshes; nothing of ours in the
//!   keyring. Microsoft Graph: simple or session upload, `createLink`.
//!
//! Accounts live in `cloud.toml` beside the other settings; each secret is
//! in the system keyring under the account's [`CloudAccount::key`].

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloudKind {
    /// Nextcloud, ownCloud or OpenCloud: WebDAV + OCS.
    #[default]
    Nextcloud,
    Dropbox,
    Seafile,
    #[serde(rename = "onedrive")]
    OneDrive,
}

impl CloudKind {
    pub const ALL: [CloudKind; 4] = [CloudKind::Nextcloud, CloudKind::OneDrive, CloudKind::Dropbox, CloudKind::Seafile];

    /// Signed in through GNOME Online Accounts: no secret of ours.
    pub fn via_goa(self) -> bool {
        self == CloudKind::OneDrive
    }

    /// GOA's `ProviderType` for the kinds that go through it.
    pub fn goa_provider(self) -> Option<&'static str> {
        match self {
            CloudKind::OneDrive => Some("ms_graph"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CloudAccount {
    /// How the account is shown ("Work Nextcloud").
    pub name: String,
    #[serde(default)]
    pub kind: CloudKind,
    /// The server's base URL, e.g. `https://cloud.example.com` (not used
    /// by Dropbox).
    #[serde(default)]
    pub url: String,
    /// The login (for Dropbox, the account's e-mail, filled in by the
    /// sign-in).
    #[serde(default)]
    pub user: String,
    /// The folder uploads go to, under the user's files (for Seafile,
    /// inside the library).
    #[serde(default = "default_folder")]
    pub folder: String,
    /// Seafile: the library uploads go to, made when missing.
    #[serde(default = "default_folder")]
    pub library: String,
    /// Dropbox: the app key of the Dropbox app the sign-in runs as; empty
    /// means the build's own (see `oauth::provider_credentials`).
    #[serde(default)]
    pub client_id: String,
    /// OneDrive: the GNOME Online Accounts account id.
    #[serde(default)]
    pub goa_id: String,
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
            kind: CloudKind::Nextcloud,
            url: String::new(),
            user: String::new(),
            folder: default_folder(),
            library: default_folder(),
            client_id: String::new(),
            goa_id: String::new(),
            expire_days: 7,
            password: false,
        }
    }

    /// The base URL as the requests want it: a scheme, no trailing slash.
    pub fn base(&self) -> String {
        match self.kind {
            CloudKind::Dropbox => return "https://www.dropbox.com".to_string(),
            CloudKind::OneDrive => return "https://onedrive.live.com".to_string(),
            _ => {}
        }
        let u = self.url.trim().trim_end_matches('/');
        if u.contains("://") {
            u.to_string()
        } else {
            format!("https://{u}")
        }
    }

    /// The keyring key for this account's secret: the app password, the
    /// Seafile password or token, or the Dropbox refresh token.
    pub fn key(&self) -> String {
        match self.kind {
            CloudKind::Dropbox => format!("cloud:dropbox|{}", self.user.trim()),
            CloudKind::OneDrive => format!("cloud:goa|{}", self.goa_id.trim()),
            _ => format!("cloud:{}|{}", self.base(), self.user.trim()),
        }
    }

    /// Whether the keyring holds a secret for this account: not for one
    /// signed in through GOA, which keeps the sign-in itself.
    pub fn has_secret(&self) -> bool {
        !self.kind.via_goa()
    }

    /// Where the account is, for a list row: the server, or the service.
    pub fn where_shown(&self) -> String {
        match self.kind {
            CloudKind::Dropbox => "Dropbox".to_string(),
            CloudKind::OneDrive => "OneDrive".to_string(),
            _ => self.base(),
        }
    }

    fn folder_clean(&self) -> String {
        let f = self.folder.trim().trim_matches('/');
        if f.is_empty() {
            default_folder()
        } else {
            f.to_string()
        }
    }

    fn library_clean(&self) -> String {
        let l = self.library.trim();
        if l.is_empty() {
            default_folder()
        } else {
            l.to_string()
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

/// The upload's name and size, checked before anything goes over the wire.
fn local_file(path: &Path) -> Result<(String, u64), String> {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "the file has no name".to_string())?;
    let meta = std::fs::metadata(path).map_err(|e| format!("could not read {name}: {e}"))?;
    Ok((name, meta.len()))
}

/// The expiry date the account asks for, YYYY-MM-DD.
fn expiry(account: &CloudAccount) -> Option<String> {
    (account.expire_days > 0).then(|| {
        (chrono::Local::now() + chrono::Duration::days(account.expire_days as i64))
            .format("%Y-%m-%d")
            .to_string()
    })
}

/// Percent-encode one path segment for a URL.
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

/// Percent-encode a whole path, keeping its slashes.
fn pct_path(p: &str) -> String {
    p.split('/').map(seg).collect::<Vec<_>>().join("/")
}

fn http_err(what: &str, e: ureq::Error) -> String {
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

/// The readable part of an error body: the message field of a JSON error
/// when there is one, else the first bit of the text.
fn short_error(body: &str) -> String {
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

/// Check the account signs in; answers with the display name.
pub fn verify(account: &CloudAccount, secret: &str) -> Result<String, String> {
    match account.kind {
        CloudKind::Nextcloud => nextcloud_verify(account, secret),
        CloudKind::Dropbox => {
            let access = dropbox_access(account, secret)?;
            dropbox_whoami(&access).map(|(_, name)| name)
        }
        CloudKind::Seafile => {
            let token = seafile_token(account, secret)?;
            seafile_whoami(account, &token)
        }
        CloudKind::OneDrive => {
            let token = goa_token(account)?;
            onedrive_whoami(&token)
        }
    }
}

/// Upload `path` into the account's folder and share it by public link.
/// A file already there by that name is left alone: the upload takes
/// another name instead. The account's `expire_days` and `password` say
/// how the link is made (the composer hands in a copy with the user's
/// choices for this upload); `fixed_password` is the password to use
/// when one is wanted, else one is generated per file.
pub fn upload_and_share(
    account: &CloudAccount,
    secret: &str,
    path: &Path,
    fixed_password: Option<&str>,
) -> Result<ShareResult, String> {
    match account.kind {
        CloudKind::Nextcloud => nextcloud_upload_and_share(account, secret, path, fixed_password),
        CloudKind::Dropbox => dropbox_upload_and_share(account, secret, path, fixed_password),
        CloudKind::Seafile => seafile_upload_and_share(account, secret, path, fixed_password),
        CloudKind::OneDrive => onedrive_upload_and_share(account, path, fixed_password),
    }
}

/// The link's download password: none unless the account (as handed in)
/// wants one; then the fixed one when given, else a fresh one.
fn share_password(account: &CloudAccount, fixed: Option<&str>) -> Option<String> {
    if !account.password {
        return None;
    }
    match fixed.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => Some(p.to_string()),
        None => Some(generate_password()),
    }
}

// ---------------------------------------------------------------------
// Nextcloud, ownCloud, OpenCloud
// ---------------------------------------------------------------------

fn auth(account: &CloudAccount, password: &str) -> String {
    let raw = format!("{}:{}", account.user.trim(), password);
    format!("Basic {}", crate::oauth::base64_encode(raw.as_bytes()))
}

fn dav_root(account: &CloudAccount) -> String {
    format!("{}/remote.php/dav/files/{}", account.base(), seg(account.user.trim()))
}

/// The OCS user endpoint answers with the display name.
fn nextcloud_verify(account: &CloudAccount, password: &str) -> Result<String, String> {
    let url = format!("{}/ocs/v2.php/cloud/user?format=json", account.base());
    let v: serde_json::Value = ureq::get(&url)
        .set("Authorization", &auth(account, password))
        .set("OCS-APIRequest", "true")
        .timeout(Duration::from_secs(30))
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

fn nextcloud_upload_and_share(
    account: &CloudAccount,
    password: &str,
    path: &Path,
    fixed: Option<&str>,
) -> Result<ShareResult, String> {
    let (name, size) = local_file(path)?;
    let folder = account.folder_clean();
    let root = dav_root(account);
    let authz = auth(account, password);
    let timeout = Duration::from_secs(60 * 60);

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
        remote = stamped_name(&name);
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
    let expires = expiry(account);
    let pw = share_password(account, fixed);
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
    .timeout(Duration::from_secs(60))
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

/// `report-20260909-141500.pdf`: the name with the time in it, for when
/// the plain one is taken.
fn stamped_name(name: &str) -> String {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{stamp}.{ext}"),
        _ => format!("{name}-{stamp}"),
    }
}

// ---------------------------------------------------------------------
// Dropbox
// ---------------------------------------------------------------------

const DROPBOX_API: &str = "https://api.dropboxapi.com/2";
const DROPBOX_CONTENT: &str = "https://content.dropboxapi.com/2";
/// Above this a file goes up in an upload session (the single-request
/// limit is 150 MB).
const DROPBOX_SINGLE_LIMIT: u64 = 150 * 1024 * 1024;
/// Session chunks: a multiple of 4 MiB, well under the 150 MB cap.
const DROPBOX_CHUNK: usize = 32 * 1024 * 1024;

/// The app key the sign-in runs as: the account's own, else the build's.
pub fn dropbox_client_id(account: &CloudAccount) -> String {
    let own = account.client_id.trim();
    if !own.is_empty() {
        return own.to_string();
    }
    crate::oauth::provider_credentials("dropbox").0
}

fn dropbox_settings(account: &CloudAccount) -> crate::config::OAuthSettings {
    crate::config::OAuthSettings {
        auth_url: "https://www.dropbox.com/oauth2/authorize".to_string(),
        token_url: "https://api.dropboxapi.com/oauth2/token".to_string(),
        client_id: dropbox_client_id(account),
        client_secret: String::new(),
        scopes: "account_info.read files.content.write sharing.write".to_string(),
    }
}

/// Sign in to Dropbox in the browser (blocking; call off the UI thread).
/// Answers with the refresh token to keep, the account's e-mail (its
/// login, for the keyring key) and its display name.
pub fn dropbox_connect(account: &CloudAccount) -> Result<(String, String, String), String> {
    let settings = dropbox_settings(account);
    if settings.client_id.is_empty() {
        return Err("Enter the app key of a Dropbox app first.".to_string());
    }
    let flow = crate::oauth::run_flow(&settings).map_err(|e| format!("Dropbox sign-in failed: {e}"))?;
    let access = crate::oauth::refresh_access_token(&settings, &flow.refresh_token)
        .map_err(|e| format!("Dropbox sign-in failed: {e}"))?;
    let (email, name) = dropbox_whoami(&access)?;
    Ok((flow.refresh_token, email, name))
}

/// Access tokens last four hours; keep the ones minted, by refresh token.
static DROPBOX_ACCESS: Mutex<Option<HashMap<String, (String, Instant)>>> = Mutex::new(None);

fn dropbox_access(account: &CloudAccount, refresh: &str) -> Result<String, String> {
    if let Ok(mut g) = DROPBOX_ACCESS.lock() {
        if let Some((tok, made)) = g.get_or_insert_with(HashMap::new).get(refresh) {
            if made.elapsed() < Duration::from_secs(3 * 3600) {
                return Ok(tok.clone());
            }
        }
    }
    let settings = dropbox_settings(account);
    if settings.client_id.is_empty() {
        return Err("The Dropbox app key is missing: open the account's settings and enter it.".to_string());
    }
    let tok = crate::oauth::refresh_access_token(&settings, refresh)
        .map_err(|e| format!("Dropbox refused the sign-in ({e}). Open Settings, Cloud Storage, and connect the account again."))?;
    if let Ok(mut g) = DROPBOX_ACCESS.lock() {
        g.get_or_insert_with(HashMap::new).insert(refresh.to_string(), (tok.clone(), Instant::now()));
    }
    Ok(tok)
}

/// JSON with every non-ASCII character escaped, as the `Dropbox-API-Arg`
/// header wants it.
fn dropbox_arg(v: &serde_json::Value) -> String {
    let mut out = String::new();
    for c in v.to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{:04x}", u));
            }
        }
    }
    out
}

fn dropbox_err(what: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) => {
            format!("{what}: Dropbox refused the sign-in. Open Settings, Cloud Storage, and connect the account again.")
        }
        ureq::Error::Status(code, resp) => {
            let text = resp.into_string().unwrap_or_default();
            format!("{what}: {} (HTTP {code})", short_error(&text).trim())
        }
        ureq::Error::Transport(t) => format!("{what}: {t}"),
    }
}

/// One RPC-style call: JSON in, JSON out.
fn dropbox_rpc(access: &str, endpoint: &str, arg: &serde_json::Value) -> Result<serde_json::Value, ureq::Error> {
    ureq::post(&format!("{DROPBOX_API}/{endpoint}"))
        .set("Authorization", &format!("Bearer {access}"))
        .timeout(Duration::from_secs(60))
        .send_json(arg)?
        .into_json()
        .map_err(ureq::Error::from)
}

/// The signed-in account: (e-mail, display name).
fn dropbox_whoami(access: &str) -> Result<(String, String), String> {
    let v = dropbox_rpc(access, "users/get_current_account", &serde_json::Value::Null)
        .map_err(|e| dropbox_err("Could not sign in", e))?;
    let email = v["email"].as_str().unwrap_or("").to_string();
    let name = v["name"]["display_name"].as_str().unwrap_or("").to_string();
    if email.is_empty() {
        return Err("Dropbox answered, but without an account.".to_string());
    }
    let name = if name.is_empty() { email.clone() } else { name };
    Ok((email, name))
}

/// One content call: the argument in the header, bytes in the body.
fn dropbox_content(
    access: &str,
    endpoint: &str,
    arg: &serde_json::Value,
    body: impl Read,
    len: u64,
) -> Result<serde_json::Value, ureq::Error> {
    ureq::post(&format!("{DROPBOX_CONTENT}/{endpoint}"))
        .set("Authorization", &format!("Bearer {access}"))
        .set("Dropbox-API-Arg", &dropbox_arg(arg))
        .set("Content-Type", "application/octet-stream")
        .set("Content-Length", &len.to_string())
        .timeout(Duration::from_secs(60 * 60))
        .send(body)?
        .into_json()
        .map_err(ureq::Error::from)
}

fn dropbox_upload_and_share(
    account: &CloudAccount,
    refresh: &str,
    path: &Path,
    fixed: Option<&str>,
) -> Result<ShareResult, String> {
    let (name, size) = local_file(path)?;
    let access = dropbox_access(account, refresh)?;
    let remote_path = format!("/{}/{}", account.folder_clean(), name);
    // Missing folders are made on the way; a taken name is renamed by
    // Dropbox ("report (1).pdf") and the answer says what it became.
    let commit = serde_json::json!({
        "path": remote_path,
        "mode": "add",
        "autorename": true,
        "mute": true,
    });
    let mut file = std::fs::File::open(path).map_err(|e| format!("could not read {name}: {e}"))?;

    let meta = if size <= DROPBOX_SINGLE_LIMIT {
        dropbox_content(&access, "files/upload", &commit, file, size).map_err(|e| dropbox_err("Upload failed", e))?
    } else {
        // An upload session: start with the first chunk, append the
        // middle ones, finish with the last and the commit. The file is
        // over the single-request limit, so the first chunk is never the
        // last.
        let mut buf = vec![0u8; DROPBOX_CHUNK];
        let mut offset: u64 = 0;
        let mut session = String::new();
        loop {
            let mut n = 0;
            while n < buf.len() {
                let r = file.read(&mut buf[n..]).map_err(|e| format!("could not read {name}: {e}"))?;
                if r == 0 {
                    break;
                }
                n += r;
            }
            if n == 0 {
                return Err("Upload failed: the file ended early.".to_string());
            }
            let last = offset + n as u64 >= size;
            let chunk = &buf[..n];
            if session.is_empty() {
                let v = dropbox_content(&access, "files/upload_session/start", &serde_json::json!({"close": false}), chunk, n as u64)
                    .map_err(|e| dropbox_err("Upload failed", e))?;
                session = v["session_id"].as_str().unwrap_or("").to_string();
                if session.is_empty() {
                    return Err("Upload failed: Dropbox opened no upload session.".to_string());
                }
            } else if last {
                let arg = serde_json::json!({
                    "cursor": {"session_id": session, "offset": offset},
                    "commit": commit,
                });
                break dropbox_content(&access, "files/upload_session/finish", &arg, chunk, n as u64)
                    .map_err(|e| dropbox_err("Upload failed", e))?;
            } else {
                let arg = serde_json::json!({
                    "cursor": {"session_id": session, "offset": offset},
                    "close": false,
                });
                dropbox_content(&access, "files/upload_session/append_v2", &arg, chunk, n as u64)
                    .map_err(|e| dropbox_err("Upload failed", e))?;
            }
            offset += n as u64;
        }
    };
    let remote = meta["name"].as_str().unwrap_or(&name).to_string();
    let lower = meta["path_lower"].as_str().unwrap_or(&remote_path).to_string();

    // The public link. Passwords and expiry are settings only paid plans
    // may set; Dropbox says so with a settings_error.
    let expires = expiry(account);
    let pw = share_password(account, fixed);
    let mut settings = serde_json::json!({"audience": "public", "access": "viewer"});
    if let Some(p) = &pw {
        settings["requested_visibility"] = "password".into();
        settings["link_password"] = p.as_str().into();
    }
    if let Some(d) = &expires {
        settings["expires"] = format!("{d}T00:00:00Z").into();
    }
    let arg = serde_json::json!({"path": lower, "settings": settings});
    let url = match dropbox_rpc(&access, "sharing/create_shared_link_with_settings", &arg) {
        Ok(v) => v["url"].as_str().unwrap_or("").to_string(),
        Err(ureq::Error::Status(409, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            if text.contains("shared_link_already_exists") {
                let v = dropbox_rpc(&access, "sharing/list_shared_links", &serde_json::json!({"path": lower, "direct_only": true}))
                    .map_err(|e| dropbox_err("Uploaded, but could not find the share link", e))?;
                v["links"][0]["url"].as_str().unwrap_or("").to_string()
            } else if text.contains("settings_error") {
                return Err("Uploaded, but Dropbox would not make the link: link passwords and expiry dates need a paid Dropbox plan. Turn them off in the account's settings.".to_string());
            } else {
                return Err(format!("Uploaded, but could not create the share link: {}", short_error(&text)));
            }
        }
        Err(e) => return Err(dropbox_err("Uploaded, but could not create the share link", e)),
    };
    if url.is_empty() {
        return Err("Uploaded, but Dropbox made no share link.".to_string());
    }
    Ok(ShareResult { name: remote, size, url, password: pw, expires })
}

// ---------------------------------------------------------------------
// Seafile
// ---------------------------------------------------------------------

fn seafile_auth(token: &str) -> String {
    format!("Token {token}")
}

fn seafile_err(what: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) | ureq::Error::Status(403, _) => {
            format!("{what}: the server refused the sign-in. Check the e-mail and the password or API token.")
        }
        ureq::Error::Status(404, _) => format!("{what}: not found. Check the server URL."),
        ureq::Error::Status(code, resp) => {
            let text = resp.into_string().unwrap_or_default();
            format!("{what}: {} (HTTP {code})", short_error(&text).trim())
        }
        ureq::Error::Transport(t) => format!("{what}: {t}"),
    }
}

/// Sign in with the password and a two-step verification code, once:
/// the token that comes back is what the keyring keeps from then on, as
/// the password alone would not get past the code at upload time.
/// Answers with the token and the account's display name.
pub fn seafile_login_with_code(account: &CloudAccount, password: &str, code: &str) -> Result<(String, String), String> {
    let url = format!("{}/api2/auth-token/", account.base());
    let v: serde_json::Value = ureq::post(&url)
        .set("X-SEAFILE-OTP", code.trim())
        .timeout(Duration::from_secs(30))
        .send_form(&[("username", account.user.trim()), ("password", password)])
        .map_err(|e| match e {
            ureq::Error::Status(400, resp) => {
                format!("Could not sign in: {}", short_error(&resp.into_string().unwrap_or_default()))
            }
            e => seafile_err("Could not sign in", e),
        })?
        .into_json()
        .map_err(|e| format!("Could not read the server's answer: {e}"))?;
    let token = v["token"].as_str().unwrap_or("").to_string();
    if token.is_empty() {
        return Err("Could not sign in: the server gave no token.".to_string());
    }
    let who = seafile_whoami(account, &token)?;
    Ok((token, who))
}

/// The API token for the account: the secret is the password, turned
/// into a token by the auth-token endpoint, or already a token (from a
/// two-step sign-in, or obtained another way and pasted).
fn seafile_token(account: &CloudAccount, secret: &str) -> Result<String, String> {
    let url = format!("{}/api2/auth-token/", account.base());
    let login = ureq::post(&url)
        .timeout(Duration::from_secs(30))
        .send_form(&[("username", account.user.trim()), ("password", secret)]);
    let login_err = match login {
        Ok(resp) => {
            let v: serde_json::Value = resp.into_json().map_err(|e| format!("Could not read the server's answer: {e}"))?;
            match v["token"].as_str() {
                Some(t) if !t.is_empty() => return Ok(t.to_string()),
                _ => "Could not sign in: the server gave no token.".to_string(),
            }
        }
        Err(ureq::Error::Status(400, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            if text.to_ascii_lowercase().contains("otp") || text.contains("two") {
                "Could not sign in: the account uses two-step verification. Enter the current code from your authenticator app in the account's settings and press Check Connection.".to_string()
            } else {
                format!("Could not sign in: {}", short_error(&text))
            }
        }
        Err(e) => seafile_err("Could not sign in", e),
    };
    // Not a password, then: perhaps a token.
    if seafile_whoami(account, secret).is_ok() {
        return Ok(secret.to_string());
    }
    Err(login_err)
}

/// The signed-in account's name (or e-mail).
fn seafile_whoami(account: &CloudAccount, token: &str) -> Result<String, String> {
    let v: serde_json::Value = ureq::get(&format!("{}/api2/account/info/", account.base()))
        .set("Authorization", &seafile_auth(token))
        .timeout(Duration::from_secs(30))
        .call()
        .map_err(|e| seafile_err("Could not sign in", e))?
        .into_json()
        .map_err(|e| format!("Could not read the server's answer: {e}"))?;
    let email = v["email"].as_str().unwrap_or("");
    let name = v["name"].as_str().unwrap_or("");
    if email.is_empty() && name.is_empty() {
        return Err("The server answered, but not like a Seafile: is the URL its root?".to_string());
    }
    Ok(if name.is_empty() { email.to_string() } else { name.to_string() })
}

/// The id of the account's library, made when there is none by that name.
fn seafile_library(account: &CloudAccount, token: &str) -> Result<String, String> {
    let base = account.base();
    let want = account.library_clean();
    let repos: serde_json::Value = ureq::get(&format!("{base}/api2/repos/?type=mine"))
        .set("Authorization", &seafile_auth(token))
        .timeout(Duration::from_secs(60))
        .call()
        .map_err(|e| seafile_err("Could not list the libraries", e))?
        .into_json()
        .map_err(|e| format!("Could not read the library list: {e}"))?;
    if let Some(list) = repos.as_array() {
        // An exact name first, then any case.
        for exact in [true, false] {
            for r in list {
                let name = r["name"].as_str().unwrap_or("");
                let hit = if exact { name == want } else { name.eq_ignore_ascii_case(&want) };
                if hit && !r["encrypted"].as_bool().unwrap_or(false) {
                    if let Some(id) = r["id"].as_str() {
                        return Ok(id.to_string());
                    }
                }
            }
        }
    }
    let v: serde_json::Value = ureq::post(&format!("{base}/api2/repos/"))
        .set("Authorization", &seafile_auth(token))
        .timeout(Duration::from_secs(60))
        .send_form(&[("name", want.as_str()), ("desc", "Files shared from Vireo")])
        .map_err(|e| seafile_err("Could not create the library", e))?
        .into_json()
        .map_err(|e| format!("Could not read the new library's answer: {e}"))?;
    v["repo_id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "Could not create the library: the server gave no id.".to_string())
}

/// The multipart body of a Seafile upload, streamed: the fields and the
/// file's head, the file itself, the closing boundary.
fn multipart_upload(
    fields: &[(&str, &str)],
    name: &str,
    file: std::fs::File,
    size: u64,
) -> Result<(String, u64, impl Read), String> {
    let boundary = format!("----VireoUpload{}", crate::rng::token(24).map_err(|e| e.to_string())?);
    let mut head = String::new();
    for (k, v) in fields {
        head.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"));
    }
    let safe = name.replace('\\', "_").replace('"', "_").replace(['\r', '\n'], " ");
    head.push_str(&format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{safe}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    ));
    let tail = format!("\r\n--{boundary}--\r\n");
    let len = head.len() as u64 + size + tail.len() as u64;
    let body = std::io::Cursor::new(head.into_bytes())
        .chain(file)
        .chain(std::io::Cursor::new(tail.into_bytes()));
    Ok((format!("multipart/form-data; boundary={boundary}"), len, body))
}

fn seafile_upload_and_share(
    account: &CloudAccount,
    secret: &str,
    path: &Path,
    fixed: Option<&str>,
) -> Result<ShareResult, String> {
    let (name, size) = local_file(path)?;
    let token = seafile_token(account, secret)?;
    let authz = seafile_auth(&token);
    let base = account.base();
    let repo = seafile_library(account, &token)?;

    // The folder inside the library, one segment at a time: the mkdir
    // wants its parent to exist.
    let folder = account.folder.trim().trim_matches('/').to_string();
    let mut dir = String::new();
    for part in folder.split('/').filter(|p| !p.is_empty()) {
        dir.push('/');
        dir.push_str(part);
        let url = format!("{base}/api2/repos/{repo}/dir/?p={}", pct_path(&dir));
        let there = ureq::get(&url).set("Authorization", &authz).timeout(Duration::from_secs(60)).call();
        match there {
            Ok(_) => {}
            Err(ureq::Error::Status(404, _)) => {
                ureq::post(&url)
                    .set("Authorization", &authz)
                    .timeout(Duration::from_secs(60))
                    .send_form(&[("operation", "mkdir")])
                    .map_err(|e| seafile_err("Could not create the folder", e))?;
            }
            Err(e) => return Err(seafile_err("Could not look at the folder", e)),
        }
    }
    let parent = if dir.is_empty() { "/".to_string() } else { dir.clone() };

    // The upload goes to a one-off link from the file server.
    let link: serde_json::Value = ureq::get(&format!("{base}/api2/repos/{repo}/upload-link/?p={}", pct_path(&parent)))
        .set("Authorization", &authz)
        .timeout(Duration::from_secs(60))
        .call()
        .map_err(|e| seafile_err("Could not get an upload link", e))?
        .into_json()
        .map_err(|e| format!("Could not read the upload link: {e}"))?;
    let link = link.as_str().unwrap_or("").to_string();
    if link.is_empty() {
        return Err("Could not get an upload link: the server gave none.".to_string());
    }
    let file = std::fs::File::open(path).map_err(|e| format!("could not read {name}: {e}"))?;
    // replace=0: a taken name gets a numbered one, reported back.
    let (ctype, len, body) = multipart_upload(&[("parent_dir", parent.as_str()), ("replace", "0")], &name, file, size)?;
    let sep = if link.contains('?') { '&' } else { '?' };
    let uploaded: serde_json::Value = ureq::post(&format!("{link}{sep}ret-json=1"))
        .set("Authorization", &authz)
        .set("Content-Type", &ctype)
        .set("Content-Length", &len.to_string())
        .timeout(Duration::from_secs(60 * 60))
        .send(body)
        .map_err(|e| seafile_err("Upload failed", e))?
        .into_json()
        .map_err(|e| format!("Upload failed: could not read the answer: {e}"))?;
    let remote = uploaded[0]["name"]
        .as_str()
        .or_else(|| uploaded["name"].as_str())
        .unwrap_or(&name)
        .to_string();

    // The share link.
    let file_path = if dir.is_empty() { format!("/{remote}") } else { format!("{dir}/{remote}") };
    let expires = expiry(account);
    let pw = share_password(account, fixed);
    let days = account.expire_days.to_string();
    let mut form: Vec<(&str, &str)> = vec![("repo_id", repo.as_str()), ("path", file_path.as_str())];
    if let Some(p) = &pw {
        form.push(("password", p.as_str()));
    }
    if expires.is_some() {
        form.push(("expire_days", days.as_str()));
    }
    let v: serde_json::Value = ureq::post(&format!("{base}/api/v2.1/share-links/"))
        .set("Authorization", &authz)
        .timeout(Duration::from_secs(60))
        .send_form(&form)
        .map_err(|e| seafile_err("Uploaded, but could not create the share link", e))?
        .into_json()
        .map_err(|e| format!("Uploaded, but could not read the share answer: {e}"))?;
    let url = v["link"].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        return Err(format!(
            "Uploaded, but the server made no share link: {}",
            v["error_msg"].as_str().unwrap_or("no link in the answer")
        ));
    }
    Ok(ShareResult { name: remote, size, url, password: pw, expires })
}

// ---------------------------------------------------------------------
// OneDrive, through GNOME Online Accounts
// ---------------------------------------------------------------------

/// A fresh access token for the account's GOA account.
fn goa_token(account: &CloudAccount) -> Result<String, String> {
    let id = account.goa_id.trim();
    if id.is_empty() {
        return Err("The account is not linked to a GNOME Online Accounts account: open its settings and choose one.".to_string());
    }
    crate::goa::oauth_token(id).ok_or_else(|| {
        "GNOME Online Accounts gave no token for the account. Check it under Settings, Online Accounts; it may need signing in again.".to_string()
    })
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// An error from a JSON API: the message inside, with the status.
fn api_err(what: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) => {
            format!("{what}: the service refused the sign-in. Check the account under Settings, Online Accounts.")
        }
        ureq::Error::Status(code, resp) => {
            let text = resp.into_string().unwrap_or_default();
            format!("{what}: {} (HTTP {code})", api_message(&text).trim())
        }
        ureq::Error::Transport(t) => format!("{what}: {t}"),
    }
}

/// The message of a Graph error body (`error.message`), else the plain
/// short form.
fn api_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(m) = v["error"]["message"].as_str() {
            return m.chars().take(200).collect();
        }
    }
    short_error(body)
}

/// Read whole chunks off a file: `buf.len()` bytes, or what is left.
fn read_chunk(file: &mut std::fs::File, buf: &mut [u8], name: &str) -> Result<usize, String> {
    let mut n = 0;
    while n < buf.len() {
        let r = file.read(&mut buf[n..]).map_err(|e| format!("could not read {name}: {e}"))?;
        if r == 0 {
            break;
        }
        n += r;
    }
    Ok(n)
}

// ----- OneDrive -----

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
/// Above this the file goes up in an upload session (the simple upload
/// takes up to 250 MB; stay well under).
const ONEDRIVE_SINGLE_LIMIT: u64 = 60 * 1024 * 1024;
/// Session chunks must be multiples of 320 KiB.
const ONEDRIVE_CHUNK: usize = 32 * 320 * 1024;

fn graph_json(req: ureq::Request, body: Option<&serde_json::Value>) -> Result<serde_json::Value, ureq::Error> {
    let resp = match body {
        Some(b) => req.send_json(b)?,
        None => req.call()?,
    };
    resp.into_json().map_err(ureq::Error::from)
}

/// `/me/drive/root:/a/b` for a folder path, `/me/drive/root` for none.
fn onedrive_item(path: &str) -> String {
    if path.is_empty() {
        format!("{GRAPH}/me/drive/root")
    } else {
        format!("{GRAPH}/me/drive/root:/{}", pct_path(path))
    }
}

/// The signed-in Microsoft account's display name.
fn onedrive_whoami(token: &str) -> Result<String, String> {
    let v = graph_json(
        ureq::get(&format!("{GRAPH}/me/drive?$select=id,owner"))
            .set("Authorization", &bearer(token))
            .timeout(Duration::from_secs(60)),
        None,
    )
    .map_err(|e| api_err("Could not reach OneDrive", e))?;
    let name = v["owner"]["user"]["displayName"].as_str().unwrap_or("");
    if v["id"].as_str().unwrap_or("").is_empty() {
        return Err("OneDrive answered, but without a drive.".to_string());
    }
    Ok(if name.is_empty() { "OneDrive".to_string() } else { name.to_string() })
}

/// Make sure the upload folder exists, one segment at a time.
fn onedrive_folder(token: &str, folder: &str) -> Result<String, String> {
    let mut dir = String::new();
    for part in folder.split('/').filter(|p| !p.is_empty()) {
        let next = if dir.is_empty() { part.to_string() } else { format!("{dir}/{part}") };
        let there = ureq::get(&format!("{}?$select=id", onedrive_item(&next)))
            .set("Authorization", &bearer(token))
            .timeout(Duration::from_secs(60))
            .call();
        match there {
            Ok(_) => {}
            Err(ureq::Error::Status(404, _)) => {
                let url = if dir.is_empty() {
                    format!("{GRAPH}/me/drive/root/children")
                } else {
                    format!("{}:/children", onedrive_item(&dir))
                };
                graph_json(
                    ureq::post(&url).set("Authorization", &bearer(token)).timeout(Duration::from_secs(60)),
                    Some(&serde_json::json!({"name": part, "folder": {}, "@microsoft.graph.conflictBehavior": "fail"})),
                )
                .map_err(|e| api_err("Could not create the folder", e))?;
            }
            Err(e) => return Err(api_err("Could not look at the folder", e)),
        }
        dir = next;
    }
    Ok(dir)
}

fn onedrive_upload_and_share(account: &CloudAccount, path: &Path, fixed: Option<&str>) -> Result<ShareResult, String> {
    let (name, size) = local_file(path)?;
    let token = goa_token(account)?;
    let dir = onedrive_folder(&token, &account.folder_clean())?;
    let remote_path = if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") };
    let mut file = std::fs::File::open(path).map_err(|e| format!("could not read {name}: {e}"))?;

    // A taken name is renamed by OneDrive ("report 1.pdf"); the answer
    // says what it became.
    let meta = if size <= ONEDRIVE_SINGLE_LIMIT {
        let url = format!("{}:/content?@microsoft.graph.conflictBehavior=rename", onedrive_item(&remote_path));
        ureq::put(&url)
            .set("Authorization", &bearer(&token))
            .set("Content-Length", &size.to_string())
            .set("Content-Type", "application/octet-stream")
            .timeout(Duration::from_secs(60 * 60))
            .send(file)
            .map_err(|e| api_err("Upload failed", e))?
            .into_json::<serde_json::Value>()
            .map_err(|e| format!("Upload failed: could not read the answer: {e}"))?
    } else {
        let open = graph_json(
            ureq::post(&format!("{}:/createUploadSession", onedrive_item(&remote_path)))
                .set("Authorization", &bearer(&token))
                .timeout(Duration::from_secs(60)),
            Some(&serde_json::json!({"item": {"@microsoft.graph.conflictBehavior": "rename", "name": name}})),
        )
        .map_err(|e| api_err("Upload failed", e))?;
        let session = open["uploadUrl"].as_str().unwrap_or("").to_string();
        if session.is_empty() {
            return Err("Upload failed: OneDrive opened no upload session.".to_string());
        }
        let mut buf = vec![0u8; ONEDRIVE_CHUNK];
        let mut offset: u64 = 0;
        loop {
            let n = read_chunk(&mut file, &mut buf, &name)?;
            if n == 0 {
                return Err("Upload failed: the file ended early.".to_string());
            }
            let end = offset + n as u64 - 1;
            let resp = ureq::put(&session)
                .set("Content-Length", &n.to_string())
                .set("Content-Range", &format!("bytes {offset}-{end}/{size}"))
                .timeout(Duration::from_secs(60 * 60))
                .send_bytes(&buf[..n])
                .map_err(|e| api_err("Upload failed", e))?;
            offset += n as u64;
            if offset >= size {
                break resp
                    .into_json::<serde_json::Value>()
                    .map_err(|e| format!("Upload failed: could not read the answer: {e}"))?;
            }
        }
    };
    let id = meta["id"].as_str().unwrap_or("").to_string();
    if id.is_empty() {
        return Err("Upload failed: OneDrive gave the file no id.".to_string());
    }
    let remote = meta["name"].as_str().unwrap_or(&name).to_string();

    // The public link. Expiry and passwords take a OneDrive for Business
    // or Microsoft 365 subscription; a personal account says no.
    let expires = expiry(account);
    let pw = share_password(account, fixed);
    let mut body = serde_json::json!({"type": "view", "scope": "anonymous"});
    if let Some(d) = &expires {
        body["expirationDateTime"] = format!("{d}T00:00:00Z").into();
    }
    if let Some(p) = &pw {
        body["password"] = p.as_str().into();
    }
    let v = graph_json(
        ureq::post(&format!("{GRAPH}/me/drive/items/{id}/createLink"))
            .set("Authorization", &bearer(&token))
            .timeout(Duration::from_secs(60)),
        Some(&body),
    )
    .map_err(|e| match e {
        ureq::Error::Status(code, resp) if (expires.is_some() || pw.is_some()) && (code == 400 || code == 403) => {
            let text = resp.into_string().unwrap_or_default();
            format!(
                "Uploaded, but OneDrive would not make the link: {}. Link expiry and download passwords need a Microsoft 365 subscription or OneDrive for Business; on a free personal OneDrive set Links expire after to 0 and turn the password off, under Settings, Cloud Storage.",
                api_message(&text).trim()
            )
        }
        e => api_err("Uploaded, but could not create the share link", e),
    })?;
    let url = v["link"]["webUrl"].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        return Err("Uploaded, but OneDrive made no share link.".to_string());
    }
    Ok(ShareResult { name: remote, size, url, password: pw, expires })
}

// ---------------------------------------------------------------------

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
        assert_eq!(pct_path("/Vireo/Q3 report.pdf"), "/Vireo/Q3%20report.pdf");
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

    #[test]
    fn old_cloud_toml_still_reads_as_nextcloud() {
        let text = "[[accounts]]\nname = \"Work\"\nurl = \"https://cloud.example.com\"\nuser = \"me\"\n";
        let f: CloudFile = toml::from_str(text).unwrap();
        assert_eq!(f.accounts[0].kind, CloudKind::Nextcloud);
        assert_eq!(f.accounts[0].key(), "cloud:https://cloud.example.com|me");
        let mut d = CloudAccount::empty();
        d.kind = CloudKind::Dropbox;
        d.user = "me@example.com".into();
        assert_eq!(d.key(), "cloud:dropbox|me@example.com");
        let back = toml::to_string(&CloudFile { accounts: vec![d] }).unwrap();
        assert!(back.contains("kind = \"dropbox\""));
    }

    #[test]
    fn goa_kinds_have_no_secret_and_their_own_key() {
        let mut a = CloudAccount::empty();
        a.kind = CloudKind::OneDrive;
        a.goa_id = "account_123".into();
        assert!(!a.has_secret());
        assert_eq!(a.key(), "cloud:goa|account_123");
        let back = toml::to_string(&CloudFile { accounts: vec![a] }).unwrap();
        assert!(back.contains("kind = \"onedrive\""));
        assert_eq!(onedrive_item("Vireo/Q3 report"), "https://graph.microsoft.com/v1.0/me/drive/root:/Vireo/Q3%20report");
        assert_eq!(onedrive_item(""), "https://graph.microsoft.com/v1.0/me/drive/root");
    }

    #[test]
    fn the_link_password_follows_the_choice() {
        let mut a = CloudAccount::empty();
        assert_eq!(share_password(&a, Some("typed")), None);
        a.password = true;
        assert_eq!(share_password(&a, Some(" typed ")).as_deref(), Some("typed"));
        assert_eq!(share_password(&a, Some("  ")).map(|p| p.len()), Some(12));
        assert_eq!(share_password(&a, None).map(|p| p.len()), Some(12));
    }

    #[test]
    fn dropbox_header_arg_is_ascii() {
        let arg = dropbox_arg(&serde_json::json!({"path": "/Vireo/caf\u{e9} \u{1F4CE}.pdf"}));
        assert!(arg.is_ascii());
        assert!(arg.contains("caf\\u00e9"));
        assert!(arg.contains("\\ud83d\\udcce"));
    }

    #[test]
    fn multipart_body_has_the_right_length() {
        let dir = std::env::temp_dir().join(format!("vireo-mp-{}", std::process::id()));
        std::fs::write(&dir, b"hello").unwrap();
        let f = std::fs::File::open(&dir).unwrap();
        let (ctype, len, mut body) = multipart_upload(&[("parent_dir", "/"), ("replace", "0")], "a \"b\".txt", f, 5).unwrap();
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, len);
        assert!(ctype.starts_with("multipart/form-data; boundary="));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("filename=\"a _b_.txt\""));
        assert!(text.contains("\r\n\r\nhello\r\n--"));
        let _ = std::fs::remove_file(&dir);
    }
}
