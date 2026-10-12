//! EWS requests over the account's connection (#391): a typed operation in,
//! its response messages out, with the server's faults, refusals and
//! requests to slow down turned into errors the backend can act on.

use ews::response::{ResponseClass, ResponseCode, ResponseError};
use ews::server_version::ExchangeServerVersion;
use ews::soap::{Envelope, Header};
use ews::{MessageXml, Operation, OperationResponse};

use super::http::{Connection, HttpError};

#[derive(Debug)]
pub enum EwsError {
    Http(HttpError),
    /// The reply was not a SOAP document this build can read.
    Parse(String),
    /// A SOAP fault: its code, when there is one, and its text.
    Fault(Option<ResponseCode>, String),
    /// One of the operation's response messages was an error.
    Response(ResponseError),
}

impl std::fmt::Display for EwsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EwsError::Http(e) => write!(f, "{e}"),
            EwsError::Parse(e) => write!(f, "unreadable reply from the server: {e}"),
            EwsError::Fault(_, text) => write!(f, "{text}"),
            EwsError::Response(e) => write!(f, "{}", e.message_text),
        }
    }
}

impl EwsError {
    pub fn code(&self) -> Option<&ResponseCode> {
        match self {
            EwsError::Fault(code, _) => code.as_ref(),
            EwsError::Response(e) => Some(&e.response_code),
            _ => None,
        }
    }

    /// The server or the network failed, rather than the request: worth
    /// telling apart in the notification bar, which clears those once the
    /// next request works.
    pub fn is_network(&self) -> bool {
        matches!(self, EwsError::Http(HttpError::Connect(_) | HttpError::Timeout | HttpError::Io(_)))
    }

    pub fn is_auth(&self) -> bool {
        matches!(self, EwsError::Http(HttpError::Auth(_)))
    }
}

impl From<HttpError> for EwsError {
    fn from(e: HttpError) -> Self {
        EwsError::Http(e)
    }
}

/// How often a busy server is waited for before the request gives up.
const BUSY_RETRIES: usize = 3;
/// The longest one wait for a busy server lasts, whatever it asks for.
const BUSY_WAIT_CAP_MS: u64 = 30_000;

pub struct Client {
    pub conn: Connection,
    /// The schema version asked for: 2013 SP1 for flags, 2010 SP2 for a
    /// server that refuses that.
    version: ExchangeServerVersion,
}

pub type Messages<Op> = Vec<ResponseClass<<<Op as Operation>::Response as OperationResponse>::Message>>;

impl Client {
    pub fn new(conn: Connection) -> Client {
        Client { conn, version: ExchangeServerVersion::Exchange2013_SP1 }
    }

    /// Run `op`, waiting out a busy server, and hand back its response
    /// messages as they came, successes and errors alike.
    pub async fn call<Op: Operation>(&mut self, op: &Op) -> Result<Messages<Op>, EwsError> {
        let mut busy = 0;
        let mut downgraded = false;
        loop {
            match self.call_once(op).await {
                Err(e) if busy < BUSY_RETRIES && wait_asked(&e).is_some() => {
                    let ms = wait_asked(&e).unwrap_or(1000).min(BUSY_WAIT_CAP_MS);
                    tracing::info!(target: "hylki::ews", "server busy, waiting {ms} ms");
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    busy += 1;
                }
                Err(e)
                    if !downgraded
                        && matches!(e.code(), Some(ResponseCode::ErrorInvalidServerVersion)) =>
                {
                    tracing::info!(target: "hylki::ews", "server refuses Exchange2013_SP1, asking as 2010 SP2");
                    self.version = ExchangeServerVersion::Exchange2010_SP2;
                    downgraded = true;
                }
                r => return r,
            }
        }
    }

    /// [`Client::call`] for an operation on one thing: its one response
    /// message, an error one becoming the error.
    pub async fn call_one<Op: Operation>(
        &mut self,
        op: &Op,
    ) -> Result<<<Op as Operation>::Response as OperationResponse>::Message, EwsError> {
        match self.call(op).await?.into_iter().next() {
            Some(ResponseClass::Success(m) | ResponseClass::Warning(m)) => Ok(m),
            Some(ResponseClass::Error(e)) => Err(EwsError::Response(e)),
            None => Err(EwsError::Parse("no response message".into())),
        }
    }

    /// Whether the server takes the 2013 schema, which flags and previews
    /// need.
    pub fn has_2013(&self) -> bool {
        matches!(self.version, ExchangeServerVersion::Exchange2013 | ExchangeServerVersion::Exchange2013_SP1)
    }

    /// Run an operation written out by hand, for what the `ews` crate cannot
    /// say (FindItem's sort order): `body` is the operation's element, and
    /// the reply is read as `Op`'s.
    pub async fn call_xml<Op: Operation>(&mut self, body: &str) -> Result<Messages<Op>, EwsError> {
        let mut busy = 0;
        loop {
            let version = format!("{:?}", self.version);
            let document = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                 <soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\" \
                 xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\" \
                 xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\">\
                 <soap:Header><t:RequestServerVersion Version=\"{version}\"/></soap:Header>\
                 <soap:Body>{body}</soap:Body></soap:Envelope>"
            );
            tracing::debug!(target: "hylki::ews", "> {}", <Op as Operation>::NAME);
            let r = match self.conn.post(document.as_bytes()).await {
                Err(HttpError::Status(503, body)) => Err(EwsError::Fault(Some(ResponseCode::ErrorServerBusy), body)),
                Err(e) => Err(e.into()),
                Ok(reply) => parse_reply::<Op>(&reply),
            };
            match r {
                Err(e) if busy < BUSY_RETRIES && wait_asked(&e).is_some() => {
                    let ms = wait_asked(&e).unwrap_or(1000).min(BUSY_WAIT_CAP_MS);
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    busy += 1;
                }
                r => return r,
            }
        }
    }

    /// Run an operation the `ews` crate has no types for at all
    /// (DeleteAttachment): the reply comes back as text, an error response
    /// message or a fault turned into the error.
    pub async fn call_raw(&mut self, body: &str) -> Result<String, EwsError> {
        let version = format!("{:?}", self.version);
        let document = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\" \
             xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\" \
             xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\">\
             <soap:Header><t:RequestServerVersion Version=\"{version}\"/></soap:Header>\
             <soap:Body>{body}</soap:Body></soap:Envelope>"
        );
        let reply = String::from_utf8_lossy(&self.conn.post(document.as_bytes()).await?).into_owned();
        let text_of = |tag: &str| -> Option<String> {
            let start = reply.find(&format!("{tag}>"))? + tag.len() + 1;
            let end = reply[start..].find('<')? + start;
            Some(reply[start..end].trim().to_string())
        };
        if reply.contains("ResponseClass=\"Error\"") || reply.contains(":Fault>") {
            let text = text_of("MessageText").or_else(|| text_of("faultstring")).unwrap_or_else(|| "error".into());
            return Err(EwsError::Fault(None, text));
        }
        Ok(reply)
    }

    async fn call_once<Op: Operation>(&mut self, op: &Op) -> Result<Messages<Op>, EwsError> {
        let envelope = Envelope { headers: vec![Header::RequestServerVersion { version: self.version }], body: op };
        let document = envelope.as_xml_document().map_err(|e| EwsError::Parse(e.to_string()))?;
        tracing::debug!(target: "hylki::ews", "> {}", <Op as Operation>::NAME);
        let reply = match self.conn.post(&document).await {
            Err(HttpError::Status(503, body)) => {
                return Err(EwsError::Fault(Some(ResponseCode::ErrorServerBusy), body));
            }
            r => r?,
        };
        let parsed = parse_reply::<Op>(&reply)?;
        tracing::debug!(target: "hylki::ews", "< {} ({} messages)", <Op as Operation>::NAME, parsed.len());
        Ok(parsed)
    }
}

/// Read a reply. The `ews` crate expects every non-fault reply to carry SOAP
/// headers and panics on one without; a server that leaves them out must
/// not take the account's worker down, so that is read as a parse error.
pub fn parse_reply<Op: Operation>(reply: &[u8]) -> Result<Messages<Op>, EwsError> {
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Envelope::<Op::Response>::from_xml_document(reply)
    }))
    .map_err(|_| EwsError::Parse("reply without SOAP headers".into()))?;
    match parsed {
        Ok(envelope) => Ok(envelope.body.into_response_messages()),
        Err(ews::Error::RequestFault(fault)) => {
            let detail = fault.detail.as_ref();
            let code = detail.and_then(|d| d.response_code);
            let busy = detail.and_then(|d| match &d.message_xml {
                Some(MessageXml::ServerBusy(b)) => Some(b.back_off_milliseconds),
                _ => None,
            });
            let text = detail.and_then(|d| d.message.clone()).unwrap_or_else(|| fault.faultstring.clone());
            match busy {
                Some(ms) => Err(EwsError::Fault(Some(ResponseCode::ErrorServerBusy), format!("{ms}"))),
                None => Err(EwsError::Fault(code, text)),
            }
        }
        Err(e) => Err(EwsError::Parse(e.to_string())),
    }
}

/// How long a busy server asks to be left alone, when that is the error.
fn wait_asked(e: &EwsError) -> Option<u64> {
    match e {
        EwsError::Fault(Some(ResponseCode::ErrorServerBusy), text) => Some(text.parse().unwrap_or(1000)),
        EwsError::Response(r) if r.response_code == ResponseCode::ErrorServerBusy => {
            Some(match &r.message_xml {
                Some(MessageXml::ServerBusy(b)) => u64::from(b.back_off_milliseconds),
                _ => 1000,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ews::get_folder::GetFolder;

    /// Replies recorded from gromox (temp/ews-rig), whose SOAP prefix is
    /// upper case and whose times carry nanoseconds: every one reads.
    #[test]
    fn recorded_replies_read() {
        use ews::{
            create_item::CreateItem, find_item::FindItem, get_item::GetItem, move_item::MoveItem,
            sync_folder_hierarchy::SyncFolderHierarchy, update_item::UpdateItem,
        };
        macro_rules! fixture {
            ($name:literal) => {
                include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/worker/ews/fixtures/", $name, ".xml"))
            };
        }
        let ok = |n: usize, what: &str| assert!(n > 0, "{what}: no response messages");
        let folders = parse_reply::<GetFolder>(fixture!("02-GetFolder-wellknown")).unwrap();
        ok(folders.len(), "GetFolder");
        let hierarchy = parse_reply::<SyncFolderHierarchy>(fixture!("04-SyncFolderHierarchy-initial")).unwrap();
        ok(hierarchy.len(), "SyncFolderHierarchy");
        let found = parse_reply::<FindItem>(fixture!("05-FindItem-inbox")).unwrap();
        ok(found.len(), "FindItem");
        let mime = parse_reply::<GetItem>(fixture!("08-GetItem-mime-attachment")).unwrap();
        let ResponseClass::Success(m) = &mime[0] else { panic!("{mime:?}") };
        assert!(m.items.inner[0].inner_message().mime_content.is_some());
        let full = parse_reply::<GetItem>(fixture!("09-GetItem-default-nonascii")).unwrap();
        ok(full.len(), "GetItem");
        ok(parse_reply::<UpdateItem>(fixture!("11-UpdateItem-IsRead")).unwrap().len(), "UpdateItem");
        ok(parse_reply::<CreateItem>(fixture!("15-CreateItem-SaveOnly-draft")).unwrap().len(), "CreateItem");
        ok(parse_reply::<MoveItem>(fixture!("16-MoveItem-draft-to-Projects")).unwrap().len(), "MoveItem");
    }

    #[test]
    fn a_busy_fault_says_how_long_to_wait() {
        let reply = br#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <s:Fault>
      <faultcode xmlns:a="http://schemas.microsoft.com/exchange/services/2006/types">a:ErrorServerBusy</faultcode>
      <faultstring xml:lang="en-US">The server cannot service this request right now. Try again later.</faultstring>
      <detail>
        <e:ResponseCode xmlns:e="http://schemas.microsoft.com/exchange/services/2006/errors">ErrorServerBusy</e:ResponseCode>
        <e:Message xmlns:e="http://schemas.microsoft.com/exchange/services/2006/errors">The server cannot service this request right now. Try again later.</e:Message>
        <t:MessageXml xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
          <t:Value Name="BackOffMilliseconds">297749</t:Value>
        </t:MessageXml>
      </detail>
    </s:Fault>
  </s:Body>
</s:Envelope>"#;
        let e = parse_reply::<GetFolder>(reply).unwrap_err();
        assert_eq!(wait_asked(&e), Some(297749));
    }

    #[test]
    fn a_reply_without_headers_is_an_error_not_a_crash() {
        let reply = br#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <m:GetFolderResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
      <m:ResponseMessages/>
    </m:GetFolderResponse>
  </s:Body>
</s:Envelope>"#;
        assert!(matches!(parse_reply::<GetFolder>(reply), Err(EwsError::Parse(_))));
    }
}
