//! Finding an Exchange server's EWS address from an email address (#391),
//! the way Outlook does with Autodiscover's plain-XML form: the domain's own
//! address first, then its `autodiscover.` host, then a redirect from the
//! plain-HTTP one. Without an answer, `mail.<domain>`'s usual address is
//! the guess the setup offers.

use super::http::{Connection, Endpoint, HttpError, TlsOptions};

const REQUEST_SCHEMA: &str = "http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006";
const RESPONSE_SCHEMA: &str = "http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a";

/// How many times an Autodiscover answer may send the request on to
/// another address or server.
const MAX_REDIRECTS: usize = 5;

/// The EWS address for `email`, signing in to the Autodiscover service as
/// `login`; `Err` says why no address was found.
pub async fn discover(email: &str, login: &str, password: &str, tls: TlsOptions) -> Result<String, String> {
    let mut email = email.trim().to_string();
    let mut last_error = String::from("no Autodiscover service answered");
    let mut redirects = 0;
    'address: loop {
        let Some((_, domain)) = email.rsplit_once('@') else {
            return Err("not an email address".into());
        };
        let domain = domain.to_ascii_lowercase();
        let mut candidates = vec![
            format!("https://{domain}/autodiscover/autodiscover.xml"),
            format!("https://autodiscover.{domain}/autodiscover/autodiscover.xml"),
        ];
        if let Some(target) = http_redirect(&domain).await {
            candidates.push(target);
        }
        for url in candidates {
            let Some(endpoint) = Endpoint::parse(&url) else { continue };
            let mut conn = Connection::new(endpoint, tls, login, password);
            match conn.post(request(&email).as_bytes()).await {
                Ok(body) => match read_response(&String::from_utf8_lossy(&body)) {
                    Answer::Ews(url) => return Ok(url),
                    Answer::RedirectAddr(addr) if redirects < MAX_REDIRECTS => {
                        tracing::info!(target: "hylki::ews", "autodiscover: redirected to address {addr}");
                        redirects += 1;
                        email = addr;
                        continue 'address;
                    }
                    Answer::RedirectUrl(_) | Answer::RedirectAddr(_) if redirects >= MAX_REDIRECTS => {
                        return Err("too many Autodiscover redirects".into());
                    }
                    Answer::RedirectUrl(url) => {
                        redirects += 1;
                        if let Some(found) = follow(&url, &email, login, password, tls).await {
                            return Ok(found);
                        }
                    }
                    Answer::Error(e) => last_error = e,
                    Answer::Nothing => last_error = format!("{url}: no EWS address in the answer"),
                    Answer::RedirectAddr(_) => unreachable!(),
                },
                Err(HttpError::Auth(_)) => {
                    return Err("the Autodiscover service refused the user name or password".into());
                }
                Err(e) => {
                    tracing::debug!(target: "hylki::ews", "autodiscover {url}: {e}");
                    last_error = format!("{url}: {e}");
                }
            }
        }
        return Err(last_error);
    }
}

/// Post the request to an Autodiscover address another answer pointed at.
async fn follow(url: &str, email: &str, login: &str, password: &str, tls: TlsOptions) -> Option<String> {
    let endpoint = Endpoint::parse(url).filter(|e| e.tls)?;
    let mut conn = Connection::new(endpoint, tls, login, password);
    let body = conn.post(request(email).as_bytes()).await.ok()?;
    match read_response(&String::from_utf8_lossy(&body)) {
        Answer::Ews(url) => Some(url),
        _ => None,
    }
}

/// The address a plain-HTTP request to `autodiscover.<domain>` is redirected
/// to, when that is an HTTPS address: the third place Outlook looks. Nothing
/// is sent over plain HTTP but the bare request.
async fn http_redirect(domain: &str) -> Option<String> {
    let endpoint = Endpoint::parse(&format!("http://autodiscover.{domain}/autodiscover/autodiscover.xml"))?;
    let mut conn = Connection::new(endpoint, TlsOptions::default(), "", "");
    match conn.get_redirect().await {
        Some(target) if target.to_ascii_lowercase().starts_with("https://") => Some(target),
        _ => None,
    }
}

fn request(email: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <Autodiscover xmlns=\"{REQUEST_SCHEMA}\"><Request>\
         <EMailAddress>{}</EMailAddress>\
         <AcceptableResponseSchema>{RESPONSE_SCHEMA}</AcceptableResponseSchema>\
         </Request></Autodiscover>",
        xml_escape(email)
    )
}

#[derive(Debug, PartialEq)]
enum Answer {
    Ews(String),
    RedirectAddr(String),
    RedirectUrl(String),
    Error(String),
    Nothing,
}

/// What an Autodiscover answer says: the EWS address of the account's
/// external protocol (EXPR, reachable from anywhere) before its internal
/// one (EXCH), or where to ask instead.
fn read_response(xml: &str) -> Answer {
    if let Some(action) = element(xml, "Action") {
        match action.trim() {
            "redirectAddr" => {
                if let Some(addr) = element(xml, "RedirectAddr") {
                    return Answer::RedirectAddr(addr.trim().to_string());
                }
            }
            "redirectUrl" => {
                if let Some(url) = element(xml, "RedirectUrl") {
                    return Answer::RedirectUrl(url.trim().to_string());
                }
            }
            _ => {}
        }
    }
    let mut internal = None;
    let mut external = None;
    let mut rest = xml;
    while let Some(start) = rest.find("<Protocol>") {
        let block = &rest[start..];
        let end = block.find("</Protocol>").map_or(block.len(), |e| e + "</Protocol>".len());
        let block = &block[..end];
        let url = element(block, "EwsUrl").or_else(|| element(block, "ExternalEwsUrl"));
        match element(block, "Type").map(str::trim) {
            Some("EXPR") => external = external.or(url),
            Some("EXCH") => internal = internal.or(url),
            _ => {}
        }
        rest = &rest[start + end..];
    }
    if let Some(url) = external.or(internal) {
        return Answer::Ews(xml_unescape(url.trim()));
    }
    if let Some(message) = element(xml, "Message") {
        return Answer::Error(xml_unescape(message.trim()));
    }
    Answer::Nothing
}

/// The text of the first `<name>` element, namespace prefixes aside.
fn element<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = xml;
    loop {
        let at = rest.find('<')?;
        rest = &rest[at + 1..];
        let tag_end = rest.find('>')?;
        let tag = &rest[..tag_end];
        let tag_name = tag.split_whitespace().next().unwrap_or("");
        let local = tag_name.rsplit(':').next().unwrap_or(tag_name);
        if local == name && !tag.ends_with('/') {
            let body = &rest[tag_end + 1..];
            let close = body.find("</")?;
            return Some(&body[..close]);
        }
        rest = &rest[tag_end..];
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The usual EWS address of a domain's mail server, the setup's guess when
/// Autodiscover finds nothing.
pub fn guess(email: &str) -> Option<String> {
    let (_, domain) = email.trim().rsplit_once('@')?;
    Some(format!("https://mail.{}/EWS/Exchange.asmx", domain.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_external_address_wins() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
    <User><DisplayName>Alice</DisplayName></User>
    <Account>
      <AccountType>email</AccountType>
      <Action>settings</Action>
      <Protocol>
        <Type>EXCH</Type>
        <Server>EX01.corp.local</Server>
        <EwsUrl>https://ex01.corp.local/EWS/Exchange.asmx</EwsUrl>
      </Protocol>
      <Protocol>
        <Type>EXPR</Type>
        <Server>mail.uni.example</Server>
        <EwsUrl>https://mail.uni.example/EWS/Exchange.asmx</EwsUrl>
      </Protocol>
    </Account>
  </Response>
</Autodiscover>"#;
        assert_eq!(read_response(xml), Answer::Ews("https://mail.uni.example/EWS/Exchange.asmx".into()));
        let internal_only = xml.replace("<Type>EXPR</Type>", "<Type>WEB</Type>");
        assert_eq!(read_response(&internal_only), Answer::Ews("https://ex01.corp.local/EWS/Exchange.asmx".into()));
    }

    #[test]
    fn redirects_and_errors_are_read() {
        let addr = "<Account><Action>redirectAddr</Action><RedirectAddr>alice@new.example</RedirectAddr></Account>";
        assert_eq!(read_response(addr), Answer::RedirectAddr("alice@new.example".into()));
        let err = r#"<Response><Error Time="1" Id="2"><ErrorCode>500</ErrorCode><Message>The email address can't be found.</Message></Error></Response>"#;
        assert_eq!(read_response(err), Answer::Error("The email address can't be found.".into()));
        assert_eq!(guess("Alice@Uni.Example").unwrap(), "https://mail.uni.example/EWS/Exchange.asmx");
        assert!(request("a&b@x.example").contains("a&amp;b@x.example"));
    }
}

/// Against a real Autodiscover service, past the DNS step:
/// `EWS_AUTODISCOVER='https://127.0.0.1:18443/Autodiscover/Autodiscover.xml|alice@ews.test|alice-pass-1'`
/// cargo test --bin hylki ews::autodiscover::live -- --ignored --nocapture
#[cfg(test)]
mod live {
    #[tokio::test]
    #[ignore]
    async fn answers_with_an_ews_address() {
        let Ok(spec) = std::env::var("EWS_AUTODISCOVER") else { return };
        let f: Vec<&str> = spec.split('|').collect();
        let found = super::follow(f[0], f[1], f[1], f[2], Default::default()).await;
        println!("found: {found:?}");
        assert!(found.is_some_and(|u| u.contains("/EWS/Exchange.asmx")));
    }
}
