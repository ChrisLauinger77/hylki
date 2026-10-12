//! One HTTP/1.1 connection to an Exchange server (#391), kept open.
//!
//! NTLM signs in a connection, not a request: the three messages, and every
//! request after them, have to travel on the same TCP connection, which a
//! pooled client does not promise. So each account holds its own, and signs
//! in again only when it has to open a new one. The TLS connection's
//! certificate hash goes into the NTLM sign-in as its channel binding.

use base64::Engine as _;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1::SendRequest;

use super::ntlm;

/// How long one request may take, connecting and signing in included. A
/// mailbox's large message can take a while to come down.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Where requests go: `https://host[:port]/path`.
#[derive(Clone, Debug, PartialEq)]
pub struct Endpoint {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Endpoint {
    pub fn parse(url: &str) -> Option<Endpoint> {
        let url = url.trim();
        let (tls, rest) = if let Some(r) = strip_ci(url, "https://") {
            (true, r)
        } else if let Some(r) = strip_ci(url, "http://") {
            (false, r)
        } else {
            (true, url)
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/EWS/Exchange.asmx"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => match p.parse() {
                Ok(p) => (h, p),
                Err(_) => (authority, if tls { 443 } else { 80 }),
            },
            _ => (authority, if tls { 443 } else { 80 }),
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            return None;
        }
        Some(Endpoint { tls, host: host.to_string(), port, path: path.to_string() })
    }

    pub fn url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        let default = if self.tls { 443 } else { 80 };
        if self.port == default {
            format!("{scheme}://{}{}", self.host, self.path)
        } else {
            format!("{scheme}://{}:{}{}", self.host, self.port, self.path)
        }
    }

    fn host_header(&self) -> String {
        let default = if self.tls { 443 } else { 80 };
        if self.port == default { self.host.clone() } else { format!("{}:{}", self.host, self.port) }
    }
}

fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)).then(|| &s[prefix.len()..])
}

/// How the certificate is checked: as for IMAP, a loopback host's or one
/// accepted in GNOME Settings passes, and the account can waive the name.
#[derive(Clone, Copy, Debug, Default)]
pub struct TlsOptions {
    pub accept_hostname_mismatch: bool,
    pub accept_invalid_certs: bool,
}

#[derive(Debug)]
pub enum HttpError {
    /// The server could not be reached, or TLS failed.
    Connect(String),
    /// The server refused the sign-in; what it offers to sign in with.
    Auth(String),
    /// Any other answer than 200 and a SOAP fault's 500: the status and the
    /// start of the body.
    Status(u16, String),
    /// A 3xx, with where it points (Autodiscover follows those).
    Redirect(String),
    Timeout,
    Io(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Connect(e) => write!(f, "could not connect: {e}"),
            HttpError::Auth(offered) if offered.is_empty() => write!(f, "sign-in refused"),
            HttpError::Auth(offered) => write!(f, "sign-in refused (server offers {offered})"),
            HttpError::Status(code, body) => write!(f, "HTTP {code}: {body}"),
            HttpError::Redirect(to) => write!(f, "redirected to {to}"),
            HttpError::Timeout => write!(f, "the server did not answer in time"),
            HttpError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// How the connection signs in, once it has learned how.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Scheme {
    None,
    Basic,
    /// NTLM, under the name the server offered it by: `NTLM`, or
    /// `Negotiate`, which takes a bare NTLM token as well.
    Ntlm(&'static str),
}

pub struct Connection {
    pub endpoint: Endpoint,
    tls: TlsOptions,
    login: String,
    password: String,
    sender: Option<SendRequest<Full<Bytes>>>,
    /// The TLS connection's channel binding, for NTLM.
    binding: Option<Vec<u8>>,
    scheme: Option<Scheme>,
    /// NTLM has signed in this connection.
    signed_in: bool,
}

struct Reply {
    status: u16,
    headers: hyper::HeaderMap,
    body: Vec<u8>,
}

impl Connection {
    pub fn new(endpoint: Endpoint, tls: TlsOptions, login: &str, password: &str) -> Connection {
        Connection {
            endpoint,
            tls,
            login: login.to_string(),
            password: password.to_string(),
            sender: None,
            binding: None,
            scheme: None,
            signed_in: false,
        }
    }

    /// POST a SOAP document and return the reply's body: a 200's, or a
    /// 500's, which carries a SOAP fault for the caller to read.
    pub async fn post(&mut self, body: &[u8]) -> Result<Vec<u8>, HttpError> {
        match tokio::time::timeout(REQUEST_TIMEOUT, self.post_inner(body)).await {
            Ok(r) => r,
            Err(_) => {
                // Whatever was in flight is lost with the connection.
                self.drop_connection();
                Err(HttpError::Timeout)
            }
        }
    }

    async fn post_inner(&mut self, body: &[u8]) -> Result<Vec<u8>, HttpError> {
        // A kept connection the server has closed fails at once; one more
        // try opens a fresh one.
        for attempt in 0..2 {
            match self.post_once(body).await {
                Err(HttpError::Io(e)) if attempt == 0 => {
                    tracing::debug!(target: "hylki::ews", "connection lost ({e}), reconnecting");
                    self.drop_connection();
                }
                r => return r,
            }
        }
        unreachable!()
    }

    fn drop_connection(&mut self) {
        self.sender = None;
        self.signed_in = false;
    }

    async fn post_once(&mut self, body: &[u8]) -> Result<Vec<u8>, HttpError> {
        let reply = match self.scheme {
            Some(Scheme::Basic) => self.send(body, Some(self.basic())).await?,
            Some(Scheme::Ntlm(word)) if !self.signed_in => self.ntlm(word, body).await?,
            Some(_) => {
                let reply = self.send(body, None).await?;
                if reply.status == 401 {
                    // The server forgot the sign-in (a recycled worker
                    // process): sign in again on a new connection.
                    self.drop_connection();
                    return Box::pin(self.post_once(body)).await;
                }
                reply
            }
            None => self.discover(body).await?,
        };
        finish(reply)
    }

    /// The first request: ask without signing in, and sign in the way the
    /// server's 401 offers, NTLM before Basic.
    async fn discover(&mut self, body: &[u8]) -> Result<Reply, HttpError> {
        let reply = self.send(body, None).await?;
        if reply.status != 401 {
            self.scheme = Some(Scheme::None);
            return Ok(reply);
        }
        let offered = offered_schemes(&reply.headers);
        let has = |s: &str| offered.iter().any(|o| o.eq_ignore_ascii_case(s));
        if has("NTLM") {
            self.scheme = Some(Scheme::Ntlm("NTLM"));
            self.ntlm("NTLM", body).await
        } else if has("Negotiate") {
            self.scheme = Some(Scheme::Ntlm("Negotiate"));
            self.ntlm("Negotiate", body).await
        } else if has("Basic") {
            if !self.endpoint.tls {
                return Err(HttpError::Auth(i18n_offer(&offered)));
            }
            self.scheme = Some(Scheme::Basic);
            let reply = self.send(body, Some(self.basic())).await?;
            if reply.status == 401 {
                self.scheme = None;
                return Err(HttpError::Auth(String::new()));
            }
            Ok(reply)
        } else {
            Err(HttpError::Auth(i18n_offer(&offered)))
        }
    }

    fn basic(&self) -> String {
        let pair = format!("{}:{}", self.login, self.password);
        format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair))
    }

    /// Sign this connection in with NTLM, the request going with the third
    /// message. A connection already in use is replaced first, so the
    /// handshake starts on a fresh one.
    async fn ntlm(&mut self, word: &'static str, body: &[u8]) -> Result<Reply, HttpError> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let (handshake, negotiate) = ntlm::Handshake::new();
        // The first leg carries no body: the server answers it with the
        // challenge whatever it holds.
        let reply = self.send(&[], Some(format!("{word} {}", b64.encode(negotiate)))).await?;
        if reply.status != 401 {
            return Err(HttpError::Status(reply.status, excerpt(&reply.body)));
        }
        let challenge = reply
            .headers
            .get_all(hyper::header::WWW_AUTHENTICATE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|v| {
                let (scheme, token) = v.trim().split_once(' ')?;
                scheme.eq_ignore_ascii_case(word).then(|| token.trim().to_string())
            })
            .and_then(|t| b64.decode(t).ok())
            .and_then(|raw| ntlm::Challenge::parse(&raw))
            .ok_or_else(|| HttpError::Auth(String::new()))?;
        let spn = format!("HTTP/{}", self.endpoint.host);
        let auth = handshake.authenticate(&challenge, &self.login, &self.password, self.binding.as_deref(), &spn);
        let reply = self.send(body, Some(format!("{word} {}", b64.encode(auth)))).await?;
        if reply.status == 401 {
            self.drop_connection();
            return Err(HttpError::Auth(String::new()));
        }
        self.signed_in = true;
        Ok(reply)
    }

    /// POST a request whose reply comes as a stream, handing each piece of
    /// the body to `on_data` as it arrives, until the server ends it or
    /// `limit` passes: how streaming notifications are read. The connection
    /// must already be signed in (a request through [`Connection::post`]
    /// first does that), since NTLM's handshake needs whole replies.
    pub async fn post_streaming(
        &mut self,
        body: &[u8],
        limit: std::time::Duration,
        mut on_data: impl FnMut(&[u8]),
    ) -> Result<(), HttpError> {
        let authorization = match self.scheme {
            Some(Scheme::Basic) => Some(self.basic()),
            Some(Scheme::Ntlm(_)) if !self.signed_in => return Err(HttpError::Auth(String::new())),
            None => return Err(HttpError::Auth(String::new())),
            _ => None,
        };
        if !self.sender.as_ref().is_some_and(|s| !s.is_closed()) {
            return Err(HttpError::Io("not connected".into()));
        }
        let req = hyper::Request::post(self.endpoint.path.as_str())
            .header(hyper::header::HOST, self.endpoint.host_header())
            .header(hyper::header::CONTENT_TYPE, "text/xml; charset=utf-8")
            .header(hyper::header::ACCEPT, "text/xml")
            .header(hyper::header::USER_AGENT, concat!("Hylki/", env!("CARGO_PKG_VERSION")));
        let req = match authorization {
            Some(a) => req.header(hyper::header::AUTHORIZATION, a),
            None => req,
        }
        .body(Full::new(Bytes::copy_from_slice(body)))
        .map_err(|e| HttpError::Io(e.to_string()))?;
        let read = async {
            let sender = self.sender.as_mut().expect("checked above");
            sender.ready().await.map_err(|e| HttpError::Io(e.to_string()))?;
            let res = sender.send_request(req).await.map_err(|e| HttpError::Io(e.to_string()))?;
            match res.status().as_u16() {
                200 => {}
                401 | 403 => return Err(HttpError::Auth(String::new())),
                code => return Err(HttpError::Status(code, String::new())),
            }
            let mut body = res.into_body();
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|e| HttpError::Io(e.to_string()))?;
                if let Some(data) = frame.data_ref() {
                    on_data(data);
                }
            }
            Ok(())
        };
        match tokio::time::timeout(limit, read).await {
            Ok(r) => {
                if r.is_err() {
                    self.drop_connection();
                }
                r
            }
            Err(_) => {
                self.drop_connection();
                Err(HttpError::Timeout)
            }
        }
    }

    /// Where a plain GET of the endpoint is redirected, if it is: how
    /// Autodiscover's plain-HTTP address points at the real one.
    pub async fn get_redirect(&mut self) -> Option<String> {
        let reply = tokio::time::timeout(CONNECT_TIMEOUT, self.request(hyper::Method::GET, &[], None)).await.ok()?.ok()?;
        match finish(reply) {
            Err(HttpError::Redirect(to)) if !to.is_empty() => Some(to),
            _ => None,
        }
    }

    async fn send(&mut self, body: &[u8], authorization: Option<String>) -> Result<Reply, HttpError> {
        self.request(hyper::Method::POST, body, authorization).await
    }

    async fn request(
        &mut self,
        method: hyper::Method,
        body: &[u8],
        authorization: Option<String>,
    ) -> Result<Reply, HttpError> {
        if !self.sender.as_ref().is_some_and(|s| !s.is_closed()) {
            self.connect().await?;
        }
        let sender = self.sender.as_mut().expect("connected above");
        sender.ready().await.map_err(|e| HttpError::Io(e.to_string()))?;
        let mut req = hyper::Request::builder()
            .method(method)
            .uri(self.endpoint.path.as_str())
            .header(hyper::header::HOST, self.endpoint.host_header())
            .header(hyper::header::CONTENT_TYPE, "text/xml; charset=utf-8")
            .header(hyper::header::ACCEPT, "text/xml")
            .header(hyper::header::USER_AGENT, concat!("Hylki/", env!("CARGO_PKG_VERSION")));
        if let Some(a) = authorization {
            req = req.header(hyper::header::AUTHORIZATION, a);
        }
        let req = req
            .body(Full::new(Bytes::copy_from_slice(body)))
            .map_err(|e| HttpError::Io(e.to_string()))?;
        let res = sender.send_request(req).await.map_err(|e| HttpError::Io(e.to_string()))?;
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        // Read to the end, so the connection is free for the next request.
        let body = res
            .into_body()
            .collect()
            .await
            .map_err(|e| HttpError::Io(e.to_string()))?
            .to_bytes()
            .to_vec();
        if headers
            .get(hyper::header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("close"))
        {
            self.drop_connection();
        }
        Ok(Reply { status, headers, body })
    }

    async fn connect(&mut self) -> Result<(), HttpError> {
        self.signed_in = false;
        let addr = (self.endpoint.host.as_str(), self.endpoint.port);
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr))
            .await
            .map_err(|_| HttpError::Timeout)?
            .map_err(|e| HttpError::Connect(e.to_string()))?;
        let _ = tcp.set_nodelay(true);
        let sender = if self.endpoint.tls {
            let connector = super::super::tls_connector(
                &self.endpoint.host,
                self.tls.accept_hostname_mismatch,
                self.tls.accept_invalid_certs,
            );
            let stream = tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(&self.endpoint.host, tcp))
                .await
                .map_err(|_| HttpError::Timeout)?
                .map_err(|e| HttpError::Connect(e.to_string()))?;
            self.binding = stream.tls_server_end_point().ok().flatten().map(|h| ntlm::tls_server_end_point(&h));
            handshake(stream).await?
        } else {
            self.binding = None;
            handshake(tcp).await?
        };
        self.sender = Some(sender);
        Ok(())
    }
}

async fn handshake<S>(stream: S) -> Result<SendRequest<Full<Bytes>>, HttpError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (sender, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
        .await
        .map_err(|e| HttpError::Connect(e.to_string()))?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!(target: "hylki::ews", "connection ended: {e}");
        }
    });
    Ok(sender)
}

/// A reply's body, or why there is none to read.
fn finish(reply: Reply) -> Result<Vec<u8>, HttpError> {
    match reply.status {
        // A SOAP fault comes as a 500, with the fault to read.
        200 | 500 => Ok(reply.body),
        401 | 403 => Err(HttpError::Auth(String::new())),
        301 | 302 | 303 | 307 | 308 => Err(HttpError::Redirect(
            reply
                .headers
                .get(hyper::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string(),
        )),
        code => Err(HttpError::Status(code, excerpt(&reply.body))),
    }
}

/// The schemes a 401 offers, by name.
fn offered_schemes(headers: &hyper::HeaderMap) -> Vec<String> {
    headers
        .get_all(hyper::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|v| v.split_whitespace().next())
        .filter(|w| !w.contains('='))
        .map(str::to_string)
        .collect()
}

fn i18n_offer(offered: &[String]) -> String {
    offered.join(", ")
}

fn excerpt(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    text.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_read_from_what_people_type() {
        let e = Endpoint::parse("https://mail.uni.example/EWS/Exchange.asmx").unwrap();
        assert_eq!((e.tls, e.host.as_str(), e.port, e.path.as_str()), (true, "mail.uni.example", 443, "/EWS/Exchange.asmx"));
        let e = Endpoint::parse("mail.uni.example").unwrap();
        assert_eq!(e.url(), "https://mail.uni.example/EWS/Exchange.asmx");
        let e = Endpoint::parse("http://127.0.0.1:18080/EWS/Exchange.asmx").unwrap();
        assert_eq!((e.tls, e.port, e.host_header()), (false, 18080, "127.0.0.1:18080".to_string()));
        assert!(Endpoint::parse("https:///x").is_none());
    }

    #[test]
    fn a_401_lists_what_it_offers() {
        let mut h = hyper::HeaderMap::new();
        h.append(hyper::header::WWW_AUTHENTICATE, "Negotiate".parse().unwrap());
        h.append(hyper::header::WWW_AUTHENTICATE, "NTLM".parse().unwrap());
        h.append(hyper::header::WWW_AUTHENTICATE, "Basic realm=\"mail.uni.example\"".parse().unwrap());
        assert_eq!(offered_schemes(&h), ["Negotiate", "NTLM", "Basic"]);
    }
}

/// Against a server that demands NTLM, such as tests/ntlm-rig's pyspnego
/// one: `EWS_NTLM_TEST='https://127.0.0.1:18444/x|CORP\alice|pw-alice|ok'`,
/// the last field `ok` or `refused` for what should happen.
/// cargo test --bin hylki ews::http::live -- --ignored --nocapture
#[cfg(test)]
mod live {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn ntlm_sign_in() {
        let Ok(spec) = std::env::var("EWS_NTLM_TEST") else { return };
        let f: Vec<&str> = spec.split('|').collect();
        let mut c = Connection::new(Endpoint::parse(f[0]).unwrap(), TlsOptions::default(), f[1], f[2]);
        let first = c.post(b"one").await;
        println!("first: {:?}", first.as_ref().map(|b| String::from_utf8_lossy(b).to_string()));
        if f[3] == "ok" {
            assert_eq!(first.unwrap(), b"ok:one");
            // The connection stays signed in.
            assert_eq!(c.post(b"two").await.unwrap(), b"ok-again:two");
        } else {
            assert!(matches!(first, Err(HttpError::Auth(_))), "{first:?}");
        }
    }
}
