//! Sender logos (opt-in): the brand's own site icon in place of coloured
//! initials, so mail from Apple, Amazon or PayPal is recognisable at a glance
//! (#30).
//!
//! The icon comes from the sender's own domain, the largest it declares first:
//! the icons its home page links (`<link rel="icon" sizes="192x192">`,
//! `apple-touch-icon`) and the ones in its web manifest — where the 512px
//! icons usually live — then the well-known paths, `apple-touch-icon.png`
//! (180px) and `favicon.ico` (16–48px, the last resort). No third-party service
//! is involved and no per-user identifier is sent, but the requests do tell
//! that domain your IP address — which is exactly what blocking remote content
//! avoids. So this is off by default and gated behind a Preferences switch, as
//! Gravatar is.
//!
//! One fetch per domain per session, remembered either way: a miss is cached too,
//! or every row from the same sender would ask again.
//!
//! Icons persist on disk (`~/.local/share/vireo/logos/<domain>.img`) so a
//! restart shows them without touching the network; a `.miss` marker remembers
//! a domain with nothing to give. Both go stale after a week: the next message
//! from that sender then re-asks the domain — a changed brand icon appears, a
//! domain that gained one is picked up — while the stale icon keeps showing in
//! the meantime.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

thread_local! {
    static CACHE: RefCell<HashMap<String, gtk::gdk::Texture>> = RefCell::new(HashMap::new());
    /// Domains with no usable icon, so they are asked once and not again.
    static MISSES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Domains whose weekly refresh has already been kicked off this session.
    static REFRESHED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// How long a stored icon (or miss) is trusted before the domain is re-asked.
const REFRESH_AFTER: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

fn store_dir() -> Option<PathBuf> {
    let dir = crate::config::data_base()?.join("vireo").join("logos");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

fn img_path(domain: &str) -> Option<PathBuf> {
    Some(store_dir()?.join(format!("{domain}.img")))
}

fn miss_path(domain: &str) -> Option<PathBuf> {
    Some(store_dir()?.join(format!("{domain}.miss")))
}

/// Whether the file at `path` exists and was written within [`REFRESH_AFTER`].
fn fresh(path: &PathBuf) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < REFRESH_AFTER)
}

/// The registrable domain of an email address: the part that owns the brand.
///
/// Mail comes from `em6917.cloudways.com` or `mail.notifications.apple.com`, and
/// the icon lives at the domain those hang off. Two labels, or three when the
/// last two are a country-code pair like `co.uk`, which is a heuristic rather
/// than the public suffix list — close enough to point a favicon request at, and
/// wrong only for a handful of unusual suffixes.
pub fn domain_of(email: &str) -> Option<String> {
    let host = email.rsplit('@').next()?.trim().trim_end_matches('.');
    let host = host.to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') || host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() < 2 {
        return None;
    }
    let tail = &labels[labels.len().saturating_sub(2)..];
    let take = if labels.len() > 2 && tail[0].len() <= 3 && tail[1].len() == 2 {
        3
    } else {
        2
    };
    Some(labels[labels.len() - take.min(labels.len())..].join("."))
}

/// A previously decoded logo for this sender's domain (main thread only).
/// Falls back to the on-disk copy — however old — so a restart shows icons
/// without a single network request; [`wants_refresh`] handles staleness.
pub fn cached(email: &str) -> Option<gtk::gdk::Texture> {
    let domain = domain_of(email)?;
    if let Some(tex) = CACHE.with(|c| c.borrow().get(&domain).cloned()) {
        return Some(tex);
    }
    let bytes = img_path(&domain).and_then(|p| std::fs::read(p).ok())?;
    let tex = decode(&bytes)?;
    CACHE.with(|c| {
        c.borrow_mut().insert(domain, tex.clone());
    });
    Some(tex)
}

/// Whether the stored icon for this sender's domain is a week old — time to
/// look again in the background while the old one keeps showing. Says yes at
/// most once a session per domain, so a screenful of rows from one sender
/// doesn't fan out into a fetch per row.
pub fn wants_refresh(email: &str) -> bool {
    let Some(domain) = domain_of(email) else {
        return false;
    };
    let due = img_path(&domain).is_some_and(|p| p.exists() && !fresh(&p));
    due && REFRESHED.with(|r| r.borrow_mut().insert(domain))
}

/// Whether this domain has already been asked about and had nothing to give.
/// A miss remembered on disk expires after a week, so a domain that gains an
/// icon is eventually found.
pub fn known_missing(email: &str) -> bool {
    match domain_of(email) {
        Some(domain) => {
            MISSES.with(|m| m.borrow().contains(&domain))
                || miss_path(&domain).is_some_and(|p| fresh(&p))
        }
        // Nothing to look up counts as answered.
        None => true,
    }
}

/// Blocking fetch of a domain's icon. Call off the main thread.
///
/// A fresh on-disk copy answers without touching the network; otherwise the
/// domain is asked and the answer stored — icon or miss. When the network
/// fails with a stale copy in hand, the stale copy stands (and stays due for
/// refresh, so it is retried later).
pub fn fetch(email: &str) -> Option<Vec<u8>> {
    let domain = domain_of(email)?;
    let img = img_path(&domain);
    if let Some(p) = img.as_ref().filter(|p| fresh(p)) {
        if let Ok(bytes) = std::fs::read(p) {
            return Some(bytes);
        }
    }
    if miss_path(&domain).is_some_and(|p| fresh(&p)) {
        return None;
    }
    for url in candidate_urls(&domain) {
        if let Some(bytes) = get(&url) {
            if let Some(p) = img.as_ref() {
                let _ = std::fs::write(p, &bytes);
            }
            if let Some(p) = miss_path(&domain) {
                let _ = std::fs::remove_file(p);
            }
            return Some(bytes);
        }
    }
    if let Some(p) = img.as_ref().filter(|p| p.exists()) {
        // Nothing new, but yesterday's icon beats initials.
        return std::fs::read(p).ok();
    }
    if let Some(p) = miss_path(&domain) {
        let _ = std::fs::write(p, b"");
    }
    None
}

/// Where a site publishes its icon, largest first: what its home page and
/// web manifest declare, merged with the well-known paths (the root
/// `apple-touch-icon.png` counts as 180px, `favicon.ico` as 32px) — so a
/// declared 32px icon still loses to a root apple-touch-icon, and a declared
/// 512px one is tried before anything else.
fn candidate_urls(domain: &str) -> Vec<String> {
    let mut found = discover(domain);
    found.extend([
        (180, format!("https://{domain}/apple-touch-icon.png")),
        (180, format!("https://www.{domain}/apple-touch-icon.png")),
        (32, format!("https://{domain}/favicon.ico")),
        (32, format!("https://www.{domain}/favicon.ico")),
    ]);
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<String> = Vec::new();
    for (_, url) in found {
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// The icons a site's home page declares — its `<link rel="icon">`s and
/// `apple-touch-icon`s — and those in its web manifest, each with the size
/// the site claims for it. Empty when the page cannot be read.
fn discover(domain: &str) -> Vec<(u32, String)> {
    let Some((base, html)) = get_text(&format!("https://{domain}/"))
        .or_else(|| get_text(&format!("https://www.{domain}/")))
    else {
        return Vec::new();
    };
    let mut found = link_icons(&html, &base);
    if let Some(manifest) = link_manifest(&html, &base) {
        if let Some((mbase, json)) = get_text(&manifest) {
            found.extend(manifest_icons(&json, &mbase));
        }
    }
    found
}

/// The browser-ish identity sites see: plenty answer a bare library
/// identity with a challenge page instead of their icon.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux) Vireo";

/// A page or manifest as text, with the URL it was finally served from (so
/// relative links resolve against where redirects landed). Capped: the head
/// of a page is what matters, and a manifest is small.
fn get_text(url: &str) -> Option<(String, String)> {
    use std::io::Read;
    let resp = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "text/html,application/manifest+json,application/json;q=0.9,*/*;q=0.5")
        .timeout(std::time::Duration::from_secs(5))
        .call()
        .ok()?;
    let final_url = resp.get_url().to_string();
    let mut buf = Vec::new();
    resp.into_reader().take(512 * 1024).read_to_end(&mut buf).ok()?;
    Some((final_url, String::from_utf8_lossy(&buf).into_owned()))
}

/// The `<link>` tags of a page, each as its attributes (names lowercased,
/// entity `&amp;` unescaped in values). A tolerant scan, not a parser: enough
/// for the `rel`/`href`/`sizes`/`type` of icon links.
fn link_tags(html: &str) -> Vec<Vec<(String, String)>> {
    let lower = html.to_ascii_lowercase();
    let mut tags = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<link") {
        let start = at + i + 5;
        let Some(len) = html[start..].find('>') else { break };
        let tag = &html[start..start + len];
        at = start + len;
        if !tag.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let mut attrs = Vec::new();
        let mut rest = tag.trim();
        while !rest.is_empty() {
            let name_len = rest
                .find(|c: char| c == '=' || c.is_whitespace() || c == '/')
                .unwrap_or(rest.len());
            let name = rest[..name_len].to_ascii_lowercase();
            rest = rest[name_len..].trim_start();
            let mut value = String::new();
            if let Some(r) = rest.strip_prefix('=') {
                let r = r.trim_start();
                if let Some(q) = r.chars().next().filter(|c| *c == '"' || *c == '\'') {
                    let inner = &r[1..];
                    let end = inner.find(q).unwrap_or(inner.len());
                    value = inner[..end].to_string();
                    rest = inner[end..].strip_prefix(q).unwrap_or("").trim_start();
                } else {
                    let end = r.find(|c: char| c.is_whitespace()).unwrap_or(r.len());
                    value = r[..end].to_string();
                    rest = r[end..].trim_start();
                }
            } else {
                rest = rest.trim_start_matches('/').trim_start();
            }
            if !name.is_empty() {
                attrs.push((name, value.replace("&amp;", "&")));
            }
        }
        tags.push(attrs);
    }
    tags
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

/// The largest edge a `sizes` attribute claims ("32x32 192x192" → 192;
/// "any" or nothing → `None`).
fn largest_size(sizes: Option<&str>) -> Option<u32> {
    sizes?
        .split_whitespace()
        .filter_map(|s| s.split(['x', 'X']).next()?.parse::<u32>().ok())
        .max()
}

fn is_svg(href: &str, mime: Option<&str>) -> bool {
    mime.is_some_and(|t| t.to_ascii_lowercase().contains("svg"))
        || href.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase().ends_with(".svg")
}

/// The icon links a page declares, with their claimed (or assumed) sizes.
/// Vector icons are left out: a pixbuf loader would rasterise them at a
/// nominal 16px. `mask-icon`s are monochrome silhouettes, not the brand.
fn link_icons(html: &str, base: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for attrs in link_tags(html) {
        let Some(href) = attr(&attrs, "href").map(str::trim).filter(|h| !h.is_empty()) else { continue };
        let rel = attr(&attrs, "rel").unwrap_or("").to_ascii_lowercase();
        let rels: Vec<&str> = rel.split_whitespace().collect();
        if is_svg(href, attr(&attrs, "type")) || rels.contains(&"mask-icon") {
            continue;
        }
        let claimed = largest_size(attr(&attrs, "sizes"));
        let size = if rels.iter().any(|r| r.starts_with("apple-touch-icon")) {
            claimed.unwrap_or(180)
        } else if rels.contains(&"fluid-icon") {
            claimed.unwrap_or(128)
        } else if rels.contains(&"icon") {
            claimed.unwrap_or(48)
        } else {
            continue;
        };
        if let Some(url) = resolve_url(base, href) {
            out.push((size, url));
        }
    }
    out
}

/// The page's web manifest, if it links one.
fn link_manifest(html: &str, base: &str) -> Option<String> {
    link_tags(html).into_iter().find_map(|attrs| {
        let rel = attr(&attrs, "rel")?.to_ascii_lowercase();
        if !rel.split_whitespace().any(|r| r == "manifest") {
            return None;
        }
        resolve_url(base, attr(&attrs, "href")?.trim())
    })
}

/// The icons a web manifest lists, with their claimed sizes.
fn manifest_icons(json: &str, base: &str) -> Vec<(u32, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    v["icons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|icon| {
            let src = icon["src"].as_str()?.trim();
            if src.is_empty() || is_svg(src, icon["type"].as_str()) {
                return None;
            }
            let size = largest_size(icon["sizes"].as_str()).unwrap_or(48);
            Some((size, resolve_url(base, src)?))
        })
        .collect()
}

/// `href` as seen from `base` (an absolute URL): absolute, scheme-relative,
/// root-relative or relative to the base's directory. Only `http(s)`.
fn resolve_url(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(href.to_string());
    }
    if lower.starts_with("//") {
        return Some(format!("https:{href}"));
    }
    if href.contains(':') && !href.starts_with('/') && !href.starts_with('.') {
        // data:, mailto: and the like.
        return None;
    }
    let scheme_end = base.find("://")? + 3;
    let host_end = base[scheme_end..].find('/').map(|i| scheme_end + i).unwrap_or(base.len());
    let origin = &base[..host_end];
    if let Some(rest) = href.strip_prefix('/') {
        return Some(format!("{origin}/{rest}"));
    }
    let path = &base[host_end..];
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "/",
    };
    let mut segments: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let mut tail = href;
    loop {
        if let Some(r) = tail.strip_prefix("../") {
            segments.pop();
            tail = r;
        } else if let Some(r) = tail.strip_prefix("./") {
            tail = r;
        } else {
            break;
        }
    }
    let mut url = format!("{origin}/");
    for seg in segments {
        url.push_str(seg);
        url.push('/');
    }
    url.push_str(tail);
    Some(url)
}

fn get(url: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let resp = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .timeout(std::time::Duration::from_secs(5))
        .call()
        .ok()?;
    // Some sites answer every icon path with their home page and a 200 — pm.me
    // sends 347KB of HTML for `/favicon.ico`. Ask what it is before reading it.
    if !is_image_type(resp.content_type()) {
        return None;
    }
    let mut buf = Vec::new();
    resp.into_reader()
        .take(1_000_000)
        .read_to_end(&mut buf)
        .ok()?;
    // And a backstop for the ones that mislabel it.
    (!buf.is_empty() && !looks_like_markup(&buf)).then_some(buf)
}

/// Whether a response claims to be an image. An empty or unknown type is
/// allowed through — plenty of servers send `application/octet-stream` for an
/// `.ico`, and the bytes are checked either way.
fn is_image_type(content_type: &str) -> bool {
    let ct = content_type.trim().to_ascii_lowercase();
    let ct = ct.split(';').next().unwrap_or("").trim();
    ct.is_empty() || ct.starts_with("image/") || ct == "application/octet-stream"
}

/// Whether these bytes are a web page rather than an image — some sites answer a
/// missing icon with their home page and a 200.
fn looks_like_markup(bytes: &[u8]) -> bool {
    let head: String = bytes
        .iter()
        .take(64)
        .map(|b| *b as char)
        .collect::<String>()
        .trim_start()
        .to_ascii_lowercase();
    head.starts_with("<!doctype") || head.starts_with("<html") || head.starts_with("<?xml")
}

/// Decode icon bytes into a texture and cache them under the sender's domain.
///
/// `GdkTexture` reads PNG and JPEG; an `.ico` needs GdkPixbuf, which the platform
/// supplies loaders for. Failing to decode is remembered as a miss, so a domain
/// serving something unreadable is not asked on every row.
pub fn decode_and_cache(email: &str, bytes: &[u8]) -> Option<gtk::gdk::Texture> {
    let domain = domain_of(email)?;
    let tex = decode(bytes);
    match tex {
        Some(tex) => {
            CACHE.with(|c| {
                c.borrow_mut().insert(domain, tex.clone());
            });
            Some(tex)
        }
        None => {
            // Undecodable bytes: persist the miss (and drop the stored copy)
            // so the next session doesn't fetch and fail to decode them again.
            if let Some(p) = img_path(&domain) {
                let _ = std::fs::remove_file(p);
            }
            if let Some(p) = miss_path(&domain) {
                let _ = std::fs::write(p, b"");
            }
            remember_miss(&domain);
            None
        }
    }
}

/// Remember that a sender's domain has no usable icon.
pub fn remember_missing(email: &str) {
    if let Some(domain) = domain_of(email) {
        remember_miss(&domain);
    }
}

fn remember_miss(domain: &str) {
    MISSES.with(|m| {
        m.borrow_mut().insert(domain.to_string());
    });
}

fn decode(bytes: &[u8]) -> Option<gtk::gdk::Texture> {
    use gtk::gdk_pixbuf::prelude::*;
    use std::cell::Cell;
    use std::rc::Rc;

    // These textures live in the session-long cache above and are drawn at
    // avatar size, but sites publish `apple-touch-icon`s at up to 1024² — a
    // few MB of decoded pixels each, held forever per domain (issue #106).
    // Downscale during decode, exactly as avatars do; the pixel limit also
    // rejects decompression bombs. Going through a size-prepared PixbufLoader
    // covers PNG, JPEG and ICO alike.
    const MAX_PIXELS: i64 = 4_194_304;
    const THUMBNAIL_EDGE: i32 = 160;
    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
    let valid = Rc::new(Cell::new(false));
    loader.connect_size_prepared({
        let valid = valid.clone();
        move |loader, width, height| {
            if width <= 0 || height <= 0 || i64::from(width) * i64::from(height) > MAX_PIXELS {
                loader.set_size(1, 1);
                return;
            }
            valid.set(true);
            let longest = width.max(height);
            if longest > THUMBNAIL_EDGE {
                loader.set_size(
                    (width * THUMBNAIL_EDGE / longest).max(1),
                    (height * THUMBNAIL_EDGE / longest).max(1),
                );
            }
        }
    });
    if loader.write(bytes).is_err() {
        let _ = loader.close();
        return None;
    }
    loader.close().ok()?;
    if !valid.get() {
        return None;
    }
    let pixbuf = loader.pixbuf()?;
    Some(gtk::gdk::Texture::for_pixbuf(&pixbuf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brand_domain_is_found_under_its_sending_subdomains() {
        assert_eq!(domain_of("no-reply@apple.com").as_deref(), Some("apple.com"));
        assert_eq!(
            domain_of("news@mail.notifications.apple.com").as_deref(),
            Some("apple.com")
        );
        assert_eq!(
            domain_of("bounces+42933322-387e@em6917.cloudways.com").as_deref(),
            Some("cloudways.com")
        );
        // Country-code pairs keep three labels.
        assert_eq!(domain_of("a@mail.bbc.co.uk").as_deref(), Some("bbc.co.uk"));
        assert_eq!(domain_of("a@shop.example.com.au").as_deref(), Some("example.com.au"));
    }

    #[test]
    fn declared_icons_are_found_and_ranked_by_size() {
        let html = r##"<html><head>
            <link rel='stylesheet' href='/style.css'>
            <link rel="icon" href="/img/favicon-32x32.png" sizes="32x32" />
            <LINK REL="icon" HREF="//cdn.example.com/icon-192.png?v=2&amp;x=1" SIZES="192x192">
            <link rel="apple-touch-icon" href="touch.png">
            <link rel="icon" type="image/svg+xml" href="/icon.svg">
            <link rel="mask-icon" href="/pin.png" color="#000">
            <link rel="manifest" href="../site.webmanifest">
            </head></html>"##;
        let base = "https://www.example.com/a/b/index.html";
        let icons = link_icons(html, base);
        assert_eq!(
            icons,
            vec![
                (32, "https://www.example.com/img/favicon-32x32.png".to_string()),
                (192, "https://cdn.example.com/icon-192.png?v=2&x=1".to_string()),
                (180, "https://www.example.com/a/b/touch.png".to_string()),
            ]
        );
        assert_eq!(link_manifest(html, base).as_deref(), Some("https://www.example.com/a/site.webmanifest"));
        let manifest = r#"{"icons":[{"src":"/i/512.png","sizes":"512x512","type":"image/png"},
                                     {"src":"v.svg","sizes":"any","type":"image/svg+xml"},
                                     {"src":"i/any.png","sizes":"any"}]}"#;
        assert_eq!(
            manifest_icons(manifest, "https://www.example.com/a/site.webmanifest"),
            vec![
                (512, "https://www.example.com/i/512.png".to_string()),
                (48, "https://www.example.com/a/i/any.png".to_string()),
            ]
        );
        assert_eq!(largest_size(Some("16x16 48x48 32x32")), Some(48));
        assert_eq!(resolve_url("https://example.com", "favicon.ico").as_deref(), Some("https://example.com/favicon.ico"));
        assert_eq!(resolve_url("https://example.com/", "data:image/png;base64,AAAA"), None);
    }

    #[test]
    fn addresses_with_no_domain_to_ask_are_skipped() {
        assert_eq!(domain_of(""), None);
        assert_eq!(domain_of("someone"), None);
        assert_eq!(domain_of("someone@localhost"), None);
        // An IP literal is nobody's brand.
        assert_eq!(domain_of("a@192.168.1.1"), None);
        // Nothing to look up is treated as already answered, so no fetch is made.
        assert!(known_missing("someone"));
    }

    #[test]
    fn only_images_are_read() {
        assert!(is_image_type("image/png"));
        assert!(is_image_type("image/vnd.microsoft.icon; charset=utf-8"));
        assert!(is_image_type("image/x-icon"));
        // Servers that don't know what an .ico is get the benefit of the doubt;
        // the bytes are sniffed anyway.
        assert!(is_image_type(""));
        assert!(is_image_type("application/octet-stream"));
        // A home page is not an icon, and is not worth downloading to find out.
        assert!(!is_image_type("text/html; charset=utf-8"));
        assert!(!is_image_type("application/json"));
    }

    #[test]
    fn a_home_page_served_in_place_of_an_icon_is_rejected() {
        assert!(looks_like_markup(b"<!DOCTYPE html><html>"));
        assert!(looks_like_markup(b"  <html lang=\"en\">"));
        assert!(looks_like_markup(b"<?xml version=\"1.0\"?><svg"));
        assert!(!looks_like_markup(b"\x89PNG\r\n\x1a\n"));
        assert!(!looks_like_markup(b"\x00\x00\x01\x00"));
    }
}
