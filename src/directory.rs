//! LDAP directories (#307), kept by Evolution Data Server.
//!
//! A directory is an EDS address book with the `ldap` backend, the same kind
//! Evolution makes, so one set up in either app shows in both and Hylki
//! carries no LDAP client of its own. The composer asks the directories as a
//! recipient is typed; nothing is copied down.
//!
//! EDS keeps a directory's password in the keyring only when a desktop
//! client that can prompt for it put it there. Hylki keeps its own copy under
//! `ldap:<uid>` and hands it over whenever EDS says the directory wants one.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::contacts::Suggestion;
use crate::i18n::i18n;

const SOURCE_IFACE: &str = "org.gnome.evolution.dataserver.Source";
const WRITABLE_IFACE: &str = "org.gnome.evolution.dataserver.Source.Writable";
const REMOVABLE_IFACE: &str = "org.gnome.evolution.dataserver.Source.Removable";
const MANAGER_PATH: &str = "/org/gnome/evolution/dataserver/SourceManager";
const MANAGER_IFACE: &str = "org.gnome.evolution.dataserver.SourceManager";

/// Fewer characters than this match too much of a large directory to be
/// worth the round trip.
pub const MIN_QUERY: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    StartTls,
    Ldaps,
    None,
}

impl Security {
    fn key(self) -> &'static str {
        match self {
            Security::StartTls => "starttls",
            Security::Ldaps => "ldaps",
            Security::None => "none",
        }
    }

    fn from_key(s: &str) -> Self {
        match s {
            "ldaps" => Security::Ldaps,
            "none" => Security::None,
            _ => Security::StartTls,
        }
    }

    pub fn default_port(self) -> u16 {
        if self == Security::Ldaps { 636 } else { 389 }
    }
}

#[derive(Debug, Clone)]
pub struct Directory {
    /// The EDS source UID; empty for one not made yet.
    pub uid: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub base_dn: String,
    /// Search the whole tree under the base, or only its first level.
    pub subtree: bool,
    /// Who to sign in as; empty searches anonymously.
    pub bind_dn: String,
    pub enabled: bool,
    /// The registry's object for the source, for writing and removing it.
    path: String,
    /// The source as EDS keeps it, so saving keeps the keys Hylki does not
    /// show (Evolution's own, such as the search filter and offline copy).
    data: String,
}

impl Directory {
    pub fn empty() -> Self {
        Directory {
            uid: String::new(),
            name: String::new(),
            host: String::new(),
            port: Security::StartTls.default_port(),
            security: Security::StartTls,
            base_dn: String::new(),
            subtree: true,
            bind_dn: String::new(),
            enabled: true,
            path: String::new(),
            data: String::new(),
        }
    }

    /// The name shown in lists: the given one, or the server.
    pub fn title(&self) -> String {
        if self.name.trim().is_empty() { self.host.clone() } else { self.name.clone() }
    }

    fn keyring_key(&self) -> String {
        format!("ldap:{}", self.uid)
    }

    fn from_source(uid: &str, path: &str, data: &str) -> Option<Self> {
        let kf = crate::platform::keyfile(data)?;
        let get = |group: &str, key: &str| kf.string(group, key).map(|s| s.to_string()).unwrap_or_default();
        if get("Address Book", "BackendName") != "ldap" {
            return None;
        }
        let security = Security::from_key(&get("LDAP Backend", "SecurityMethod"));
        Some(Directory {
            uid: uid.to_string(),
            name: get("Data Source", "DisplayName"),
            host: get("Authentication", "Host"),
            port: kf
                .integer("Authentication", "Port")
                .ok()
                .and_then(|p| u16::try_from(p).ok())
                .filter(|p| *p != 0)
                .unwrap_or(security.default_port()),
            security,
            base_dn: get("LDAP Backend", "RootDn"),
            subtree: get("LDAP Backend", "Scope") != "onelevel",
            bind_dn: if get("Authentication", "Method") == "none" {
                String::new()
            } else {
                get("Authentication", "User")
            },
            enabled: kf.boolean("Data Source", "Enabled").unwrap_or(true),
            path: path.to_string(),
            data: data.to_string(),
        })
    }

    /// The source key file for this directory, over what EDS had.
    fn to_source(&self) -> String {
        let kf = crate::platform::keyfile(&self.data).unwrap_or_else(gtk::glib::KeyFile::new);
        kf.set_string("Data Source", "DisplayName", &self.title());
        kf.set_boolean("Data Source", "Enabled", self.enabled);
        if self.data.is_empty() {
            // Where Evolution files its LDAP books.
            kf.set_string("Data Source", "Parent", "ldap-stub");
            kf.set_boolean("Offline", "StaySynchronized", false);
            kf.set_integer("LDAP Backend", "Limit", 100);
            kf.set_boolean("LDAP Backend", "CanBrowse", false);
            kf.set_boolean("Authentication", "RememberPassword", true);
        }
        kf.set_string("Address Book", "BackendName", "ldap");
        kf.set_string("Authentication", "Host", self.host.trim());
        kf.set_integer("Authentication", "Port", i32::from(self.port));
        kf.set_string("Authentication", "User", self.bind_dn.trim());
        let method = kf.string("Authentication", "Method").map(|s| s.to_string()).unwrap_or_default();
        let method = if self.bind_dn.trim().is_empty() {
            "none"
        } else if method == "ldap/simple-email" {
            // Evolution's sign-in by email address, which it resolves to
            // the entry itself: keep it.
            "ldap/simple-email"
        } else {
            "ldap/simple-binddn"
        };
        kf.set_string("Authentication", "Method", method);
        kf.set_string("LDAP Backend", "RootDn", self.base_dn.trim());
        kf.set_string("LDAP Backend", "Scope", if self.subtree { "subtree" } else { "onelevel" });
        kf.set_string("LDAP Backend", "SecurityMethod", self.security.key());
        kf.to_data().to_string()
    }
}

/// One session-bus connection for the process: EDS closes a book when the
/// connection that opened it goes, so a connection per search would open
/// and close the directory every time.
fn bus() -> Result<zbus::blocking::Connection, String> {
    static BUS: Mutex<Option<zbus::blocking::Connection>> = Mutex::new(None);
    let mut guard = BUS.lock().map_err(|e| e.to_string())?;
    if let Some(c) = guard.as_ref() {
        return Ok(c.clone());
    }
    let c = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    *guard = Some(c.clone());
    Ok(c)
}

fn registry() -> Result<String, String> {
    crate::contacts::sources_dest().ok_or_else(|| i18n("Evolution Data Server is not available"))
}

/// Every LDAP directory EDS knows, Evolution's included, by name.
pub fn list() -> Result<Vec<Directory>, String> {
    let sources = crate::contacts::registry_sources().ok_or_else(|| i18n("Evolution Data Server is not available"))?;
    let mut out: Vec<Directory> = sources
        .iter()
        .filter_map(|(path, uid, data)| Directory::from_source(uid, path, data))
        .collect();
    out.sort_by_key(|d| d.title().to_lowercase());
    Ok(out)
}

/// Make or update a directory; a new password replaces the kept one, none
/// keeps it. Answers the directory's UID.
pub fn save(dir: &Directory, password: Option<&str>) -> Result<String, String> {
    let conn = bus()?;
    let dest = registry()?;
    let mut dir = dir.clone();
    if dir.uid.is_empty() {
        dir.uid = gtk::glib::uuid_string_random().replace('-', "");
        let sources: HashMap<&str, String> = HashMap::from([(dir.uid.as_str(), dir.to_source())]);
        conn.call_method(Some(dest.as_str()), MANAGER_PATH, Some(MANAGER_IFACE), "CreateSources", &(sources,))
            .map_err(|e| e.to_string())?;
    } else {
        conn.call_method(Some(dest.as_str()), dir.path.as_str(), Some(WRITABLE_IFACE), "Write", &(dir.to_source(),))
            .map_err(|e| e.to_string())?;
    }
    if let Some(pw) = password.filter(|p| !p.is_empty()) {
        crate::config::store_cloud_password(&dir.keyring_key(), pw).map_err(|e| e.to_string())?;
    }
    // The open book signed in with the old settings.
    forget_book(&dir.uid);
    Ok(dir.uid)
}

/// Switch a directory on or off without touching the rest of it.
pub fn set_enabled(dir: &Directory, enabled: bool) -> Result<(), String> {
    let mut d = dir.clone();
    d.enabled = enabled;
    save(&d, None).map(|_| ())
}

pub fn remove(dir: &Directory) -> Result<(), String> {
    let conn = bus()?;
    let dest = registry()?;
    conn.call_method(Some(dest.as_str()), dir.path.as_str(), Some(REMOVABLE_IFACE), "Remove", &())
        .map_err(|e| e.to_string())?;
    crate::config::delete_cloud_password(&dir.keyring_key());
    forget_book(&dir.uid);
    Ok(())
}

/// Whether Hylki holds a password for the directory.
pub fn has_password(dir: &Directory) -> bool {
    crate::config::load_cloud_password(&dir.keyring_key()).is_some()
}

// ---------------------------------------------------------------------------
// Searching
// ---------------------------------------------------------------------------

/// The books this process has open, by source UID: (bus name, object path).
fn books() -> &'static Mutex<HashMap<String, (String, String)>> {
    static BOOKS: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();
    BOOKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn forget_book(uid: &str) {
    let book = books().lock().ok().and_then(|mut b| b.remove(uid));
    if let (Some((bus_name, path)), Ok(conn)) = (book, bus()) {
        let _ = conn.call_method(Some(bus_name.as_str()), path.as_str(), Some(crate::contacts::BOOK_IFACE), "Close", &());
    }
}

fn open(conn: &zbus::blocking::Connection, uid: &str) -> Result<(String, String), String> {
    if let Some(b) = books().lock().ok().and_then(|b| b.get(uid).cloned()) {
        return Ok(b);
    }
    let dest = crate::contacts::factory_dest().ok_or_else(|| i18n("Evolution Data Server is not available"))?;
    let book = crate::contacts::open_book(conn, &dest, uid)?;
    if let Ok(mut b) = books().lock() {
        b.insert(uid.to_string(), book.clone());
    }
    Ok(book)
}

fn contact_list(conn: &zbus::blocking::Connection, book: &(String, String), query: &str) -> Result<Vec<String>, String> {
    let reply = conn
        .call_method(Some(book.0.as_str()), book.1.as_str(), Some(crate::contacts::BOOK_IFACE), "GetContactList", &(query,))
        .map_err(|e| e.to_string())?;
    let (cards,): (Vec<String>,) = reply.body().deserialize().map_err(|e| e.to_string())?;
    Ok(cards)
}

/// Why EDS last asked for the directory's sign-in: "required", "rejected",
/// or empty once it is connected.
fn credentials_reason(conn: &zbus::blocking::Connection, dir: &Directory) -> String {
    let Ok(dest) = registry() else { return String::new() };
    conn.call_method(Some(dest.as_str()), dir.path.as_str(), Some(SOURCE_IFACE), "GetLastCredentialsRequiredArguments", &())
        .ok()
        .and_then(|r| r.body().deserialize::<(String, String, String, String, String)>().ok())
        .map(|(reason, ..)| reason)
        .unwrap_or_default()
}

fn authenticate(conn: &zbus::blocking::Connection, dir: &Directory, password: &str) -> Result<(), String> {
    let dest = registry()?;
    let credentials = vec![format!("username:{}", dir.bind_dn.trim()), format!("password:{password}")];
    conn.call_method(Some(dest.as_str()), dir.path.as_str(), Some(SOURCE_IFACE), "InvokeAuthenticate", &(credentials,))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Ask one directory, signing in first when EDS says it wants that.
fn query_directory(dir: &Directory, query: &str) -> Result<Vec<String>, String> {
    let conn = bus()?;
    let book = match open(&conn, &dir.uid) {
        Ok(b) => b,
        Err(e) => {
            forget_book(&dir.uid);
            return Err(e);
        }
    };
    let first = match contact_list(&conn, &book, query) {
        Ok(cards) => return Ok(cards),
        Err(e) => e,
    };
    // A book whose factory went away since: open it again, once.
    if first.contains("UnknownObject") || first.contains("ServiceUnknown") || first.contains("NoReply") {
        forget_book(&dir.uid);
        let book = open(&conn, &dir.uid)?;
        return contact_list(&conn, &book, query);
    }
    match credentials_reason(&conn, dir).as_str() {
        "required" | "rejected" if !dir.bind_dn.trim().is_empty() => {
            let Some(pw) = crate::config::load_cloud_password(&dir.keyring_key()) else {
                return Err(i18n("The directory needs a password. Add it in Settings → LDAP Directories."));
            };
            authenticate(&conn, dir, &pw)?;
        }
        _ => {}
    }
    // Signing in and connecting happen after the call returns: give the
    // backend a moment before calling the directory unreachable.
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        std::thread::sleep(Duration::from_millis(200));
        match contact_list(&conn, &book, query) {
            Ok(cards) => return Ok(cards),
            Err(e) if Instant::now() >= deadline => {
                if credentials_reason(&conn, dir) == "rejected" {
                    return Err(i18n("The directory did not accept the password."));
                }
                return Err(e);
            }
            Err(_) => {}
        }
    }
}

/// An EDS query string: a quoted, escaped literal.
fn sexp_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The people whose name, surname or address starts with `typed`, from
/// every directory that is switched on. Blocking: call it off the main
/// thread. A directory that fails is logged and skipped.
pub fn search(typed: &str) -> Vec<Suggestion> {
    let typed = typed.trim();
    if typed.chars().count() < MIN_QUERY || typed.len() > 128 {
        return Vec::new();
    }
    let dirs = match list() {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!("LDAP directories unavailable: {e}");
            return Vec::new();
        }
    };
    let q = sexp_str(typed);
    // EDS maps these onto the directory's attributes: cn and sn for the
    // name, mail for the address.
    let query = format!(
        "(or (beginswith \"full_name\" {q}) (beginswith \"email\" {q}) (beginswith \"nickname\" {q}) (beginswith \"file_as\" {q}))"
    );
    let mut out = Vec::new();
    for dir in dirs.iter().filter(|d| d.enabled) {
        match query_directory(dir, &query) {
            Ok(cards) => {
                tracing::debug!(directory = %dir.title(), count = cards.len(), "LDAP directory answered");
                for card in cards {
                    out.extend(suggestions_from_vcard(&card));
                }
            }
            Err(e) => tracing::warn!(directory = %dir.title(), "LDAP directory lookup failed: {e}"),
        }
    }
    out
}

/// Check a saved directory answers, signing in as it would for a search.
pub fn check(dir: &Directory) -> Result<(), String> {
    query_directory(dir, "(is \"email\" \"hylki-check@invalid\")").map(|_| ())
}

/// A suggestion per address on a directory entry.
fn suggestions_from_vcard(card: &str) -> Vec<Suggestion> {
    let unfolded = crate::contacts::unfold_vcard(card);
    let mut name = String::new();
    let mut emails = Vec::new();
    for line in unfolded.lines() {
        let Some((prop, value)) = crate::contacts::split_vcard_line(line) else { continue };
        let prop = prop.split(';').next().unwrap_or("").trim();
        let value = crate::contacts::unescape_vcard_text(value).trim().to_string();
        if prop.eq_ignore_ascii_case("FN") && name.is_empty() {
            name = value;
        } else if prop.eq_ignore_ascii_case("EMAIL") && value.contains('@') && !value.contains(['\r', '\n']) {
            emails.push(value);
        }
    }
    emails
        .into_iter()
        .map(|email| Suggestion { name: name.clone(), email, from_contacts: true, score: 0, own: false })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_entry_gives_a_suggestion_per_address() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Alan Turing\r\nEMAIL;TYPE=OTHER:alan@example.org\r\n\
                    EMAIL;TYPE=OTHER:turing@example.org\r\nEND:VCARD\r\n";
        let s = suggestions_from_vcard(card);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].name.as_str(), s[0].email.as_str()), ("Alan Turing", "alan@example.org"));
        assert_eq!(s[1].email, "turing@example.org");
        assert!(s.iter().all(|s| s.from_contacts));
    }

    #[test]
    fn query_text_is_quoted_for_eds() {
        assert_eq!(sexp_str(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn a_source_round_trips_and_keeps_what_hylki_does_not_show() {
        let data = "[Data Source]\nDisplayName=Work\nEnabled=true\nParent=ldap-stub\n\n\
                    [Address Book]\nBackendName=ldap\n\n\
                    [Authentication]\nHost=ldap.example.org\nPort=636\nUser=cn=me,dc=example,dc=org\nMethod=ldap/simple-binddn\n\n\
                    [LDAP Backend]\nRootDn=dc=example,dc=org\nScope=onelevel\nSecurityMethod=ldaps\nFilter=(ou=staff)\n";
        let d = Directory::from_source("abc", "/p", data).unwrap();
        assert_eq!(d.host, "ldap.example.org");
        assert_eq!(d.port, 636);
        assert_eq!(d.security, Security::Ldaps);
        assert!(!d.subtree);
        assert_eq!(d.bind_dn, "cn=me,dc=example,dc=org");
        let mut anon = d.clone();
        anon.bind_dn.clear();
        let out = anon.to_source();
        assert!(out.contains("Filter=(ou=staff)"));
        assert!(out.contains("Method=none"));
        assert!(Directory::from_source("x", "/p", "[Address Book]\nBackendName=carddav\n").is_none());
    }

    #[test]
    fn a_new_source_is_filed_where_evolution_files_them() {
        let mut d = Directory::empty();
        d.host = "ldap.example.org".into();
        d.base_dn = "dc=example,dc=org".into();
        let out = d.to_source();
        assert!(out.contains("Parent=ldap-stub"));
        assert!(out.contains("SecurityMethod=starttls"));
        assert!(out.contains("DisplayName=ldap.example.org"));
    }
}
