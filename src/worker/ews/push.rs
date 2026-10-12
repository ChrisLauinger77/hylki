//! New mail as it arrives (#391): an EWS streaming subscription, read on a
//! connection of its own so the account's requests never wait behind it.
//! Each notification names the folders that changed; the account loop
//! lists those again. While the stream is down the loop polls as before.

use tokio::sync::mpsc;

use super::client::Client;
use super::http::HttpError;

/// What the push task tells the account loop.
#[derive(Debug, PartialEq)]
pub enum Push {
    /// The stream is up: polling can slow down.
    Up,
    /// The stream is down and being retried: poll at the usual pace.
    Down,
    /// These folders (EWS ids) changed.
    Changed(Vec<String>),
}

/// How long one GetStreamingEvents request stays open, in minutes (EWS
/// allows 1 to 30); a fresh one follows each.
const STREAM_MINUTES: u64 = 29;
const RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(30);
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(600);

/// Keep a subscription streaming until the account loop goes away.
pub async fn run(mut client: Client, tx: mpsc::UnboundedSender<Push>) {
    let mut retry = RETRY_FIRST;
    let mut up = false;
    loop {
        if tx.is_closed() {
            return;
        }
        let subscription = match subscribe(&mut client).await {
            Ok(id) => id,
            Err(e) => {
                tracing::info!(target: "hylki::ews", "push: could not subscribe: {e}");
                if up {
                    up = false;
                    let _ = tx.send(Push::Down);
                }
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(RETRY_MAX);
                continue;
            }
        };
        tracing::info!(target: "hylki::ews", "push: subscribed");
        retry = RETRY_FIRST;
        if !up {
            up = true;
            let _ = tx.send(Push::Up);
        }
        // One stream after another on the subscription, until the server
        // says it is gone or the connection fails.
        loop {
            let mut buffer = String::new();
            let mut lost = false;
            let body = format!(
                "<m:GetStreamingEvents><m:SubscriptionIds><t:SubscriptionId>{subscription}</t:SubscriptionId>\
                 </m:SubscriptionIds><m:ConnectionTimeout>{STREAM_MINUTES}</m:ConnectionTimeout></m:GetStreamingEvents>"
            );
            let limit = std::time::Duration::from_secs(STREAM_MINUTES * 60 + 120);
            let r = client
                .conn
                .post_streaming(&wrap(&body), limit, |data| {
                    buffer.push_str(&String::from_utf8_lossy(data));
                    while let Some(document) = take_document(&mut buffer) {
                        let (folders, error) = read_notification(&document);
                        if !folders.is_empty() {
                            let _ = tx.send(Push::Changed(folders));
                        }
                        lost |= error;
                    }
                })
                .await;
            if tx.is_closed() {
                return;
            }
            match r {
                // The stream ran its course: open the next.
                Ok(()) if !lost => continue,
                Err(HttpError::Timeout) => continue,
                Ok(()) => tracing::info!(target: "hylki::ews", "push: the subscription ended"),
                Err(e) => tracing::info!(target: "hylki::ews", "push: stream failed: {e}"),
            }
            break;
        }
    }
}

/// The same envelope [`Client::call_raw`] writes, for the streaming request.
fn wrap(body: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         xmlns:t=\"http://schemas.microsoft.com/exchange/services/2006/types\" \
         xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\">\
         <soap:Header><t:RequestServerVersion Version=\"Exchange2010_SP2\"/></soap:Header>\
         <soap:Body>{body}</soap:Body></soap:Envelope>"
    )
    .into_bytes()
}

/// A streaming subscription to every folder, or, on a server that refuses
/// that, to the Inbox; its id.
async fn subscribe(client: &mut Client) -> Result<String, super::client::EwsError> {
    const EVENTS: &str = "<t:EventTypes><t:EventType>NewMailEvent</t:EventType><t:EventType>CreatedEvent</t:EventType>\
         <t:EventType>DeletedEvent</t:EventType><t:EventType>ModifiedEvent</t:EventType>\
         <t:EventType>MovedEvent</t:EventType><t:EventType>CopiedEvent</t:EventType></t:EventTypes>";
    let all = format!(
        "<m:Subscribe><m:StreamingSubscriptionRequest SubscribeToAllFolders=\"true\">{EVENTS}\
         </m:StreamingSubscriptionRequest></m:Subscribe>"
    );
    let reply = match client.call_raw(&all).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "hylki::ews", "push: all-folder subscription refused ({e}), the Inbox alone");
            let inbox = format!(
                "<m:Subscribe><m:StreamingSubscriptionRequest><t:FolderIds><t:DistinguishedFolderId Id=\"inbox\"/>\
                 </t:FolderIds>{EVENTS}</m:StreamingSubscriptionRequest></m:Subscribe>"
            );
            client.call_raw(&inbox).await?
        }
    };
    element_text(&reply, "SubscriptionId")
        .map(str::to_string)
        .ok_or_else(|| super::client::EwsError::Parse("no subscription id".into()))
}

/// The first whole SOAP document at the front of `buffer`, taken out of it:
/// a stream carries one after another, under whatever prefix the server
/// gives the envelope.
fn take_document(buffer: &mut String) -> Option<String> {
    let mut from = 0;
    loop {
        let at = buffer[from..].find("Envelope>")? + from;
        let end = at + "Envelope>".len();
        // Only a closing tag ends a document: `</`, a prefix if any, then
        // the name.
        let closing = buffer[..at].rfind("</").is_some_and(|open| {
            let prefix = &buffer[open + 2..at];
            prefix.is_empty() || prefix.strip_suffix(':').is_some_and(|p| p.chars().all(|c| c.is_ascii_alphanumeric()))
        });
        if closing {
            let document = buffer[..end].to_string();
            buffer.replace_range(..end, "");
            return Some(document);
        }
        from = end;
    }
}

/// The folders a notification document names (every folder id in it: the
/// folder an item event happened in, a moved item's old folder, a changed
/// folder), and whether it reports the subscription in trouble.
fn read_notification(document: &str) -> (Vec<String>, bool) {
    let mut folders: Vec<String> = Vec::new();
    let mut rest = document;
    while let Some(at) = rest.find("FolderId Id=\"") {
        let start = at + "FolderId Id=\"".len();
        let Some(len) = rest[start..].find('"') else { break };
        let id = &rest[start..start + len];
        if !folders.iter().any(|f| f == id) {
            folders.push(id.to_string());
        }
        rest = &rest[start + len..];
    }
    let error = document.contains("ResponseClass=\"Error\"")
        || element_text(document, "ConnectionStatus").is_some_and(|s| s == "Closed");
    (folders, error)
}

/// The text of the first element with this local name.
fn element_text<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = xml;
    loop {
        let at = rest.find('<')?;
        rest = &rest[at + 1..];
        let end = rest.find('>')?;
        let tag = &rest[..end];
        let local = tag.split_whitespace().next().unwrap_or("").rsplit(':').next().unwrap_or("");
        if local == name && !tag.ends_with('/') {
            let body = &rest[end + 1..];
            return Some(body[..body.find('<')?].trim());
        }
        rest = &rest[end..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_splits_into_its_documents() {
        let recorded = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/worker/ews/fixtures/30-GetStreamingEvents.xml"));
        let mut buffer = String::new();
        let mut documents = Vec::new();
        // Fed in uneven pieces, as a stream arrives.
        for piece in recorded.as_bytes().chunks(97) {
            buffer.push_str(&String::from_utf8_lossy(piece));
            while let Some(d) = take_document(&mut buffer) {
                documents.push(d);
            }
        }
        assert!(documents.len() >= 2, "{}", documents.len());
        let changed: Vec<Vec<String>> = documents.iter().map(|d| read_notification(d).0).filter(|f| !f.is_empty()).collect();
        assert!(!changed.is_empty());
        assert!(documents.iter().all(|d| !read_notification(d).1));
    }

    #[test]
    fn the_subscription_id_and_trouble_are_read() {
        let reply = r#"<m:SubscribeResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SubscriptionId>AgAAAAUAAAA=</m:SubscriptionId></m:SubscribeResponseMessage>"#;
        assert_eq!(element_text(reply, "SubscriptionId"), Some("AgAAAAUAAAA="));
        let gone = r#"<m:GetStreamingEventsResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorSubscriptionNotFound</m:ResponseCode></m:GetStreamingEventsResponseMessage>"#;
        assert!(read_notification(gone).1);
        let closed = "<m:ConnectionStatus>Closed</m:ConnectionStatus>";
        assert!(read_notification(closed).1);
    }
}
