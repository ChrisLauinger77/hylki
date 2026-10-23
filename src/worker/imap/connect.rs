//! Connecting and signing in: TLS or STARTTLS, password or XOAUTH2, and the
//! first folder listing.

use tokio::net::TcpStream;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::models::Account;
use crate::worker::auth::{fetch_oauth_token, oauth_user, sign_in_password};
use crate::worker::imap::folders::list_folder_metadata;
use crate::worker::imap::gmail::note_gmail_account;
use crate::worker::imap::stream::ImapIo;
use crate::worker::imap::ImapSession;
use crate::worker::tls::{is_loopback_host, tls_connector};
use crate::worker::{accent_for, forget_access_token, offline_text, WorkerEvent};

/// Connect, announce the account, and list folders, updating the cache. Emits a
/// fresh `Folders` event only when the listing differs from the cache (so an
/// unchanged list doesn't trigger a redundant UI rebuild). Returns `None` (after
/// emitting an error) if the connection could not be established.
pub(super) async fn connect_and_list(
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) -> Option<ImapSession> {
    // The Refresh button already says the network is down; an error per
    // account and per attempt would only repeat it (#442).
    if !crate::power::reaches(&account.imap_host) {
        return None;
    }
    emit(WorkerEvent::Status(i18n_f(
        "Connecting to {host}…",
        &[("host", &account.imap_host)],
    )));

    let result = match connect(account).await {
        Ok(mut session) => {
            emit(WorkerEvent::Account(Account {
                id: account_id,
                name: account.name.clone(),
                email: account.email.clone(),
                label: account.display_label(),
                accent: accent_for(account_id).into(),
            }));
            let cached = cache
                .map(|c| c.load_folders(account_id))
                .unwrap_or_default();
            match list_folder_metadata(
                account_id,
                &mut session,
                &account.folder_roles,
                &account.hidden_folders,
                Some(&cached),
            )
            .await
            {
                // An empty LIST can't be right — INBOX always exists (RFC
                // 3501). Keep whatever the cache has instead of wiping it.
                Ok(folders) if folders.is_empty() => {}
                Ok(folders) => {
                    note_gmail_account(account_id, account, &folders);
                    let changed = cache
                        .map(|c| {
                            !crate::cache::folders_equal(&c.load_folders(account_id), &folders)
                        })
                        .unwrap_or(true);
                    if let Some(c) = cache {
                        c.save_folders(account_id, &folders);
                    }
                    if changed {
                        emit(WorkerEvent::Folders(folders));
                    }
                }
                Err(e) => emit(WorkerEvent::net_error(i18n_f(
                    "Could not list folders: {e}",
                    &[("e", &(e).to_string())],
                ))),
            }
            Some(session)
        }
        Err(e) => {
            emit(WorkerEvent::net_error(i18n_f(
                "Connection failed: {e}",
                &[("e", &(e).to_string())],
            )));
            None
        }
    };

    emit(WorkerEvent::Status(String::new()));
    result
}

/// XOAUTH2 SASL authenticator for async-imap.
pub(in crate::worker) struct XOAuth2 {
    user: String,
    token: String,
    step: u8,
}

impl async_imap::Authenticator for XOAuth2 {
    type Response = Vec<u8>;
    fn process(&mut self, challenge: &[u8]) -> Self::Response {
        self.step += 1;
        if self.step == 1 {
            // Initial SASL response.
            format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token).into_bytes()
        } else {
            // The server rejected auth and sent an error challenge; XOAUTH2 requires
            // an empty response so the server then sends the tagged error (rather
            // than the exchange deadlocking).
            let _ = challenge;
            Vec::new()
        }
    }
}

/// Whether the IMAP connection opens in plaintext and upgrades with STARTTLS
/// instead of negotiating TLS from the first byte. 993 is always implicit TLS
/// and 143 is the conventional STARTTLS port; a local bridge listens on a port
/// of its own (Proton Bridge defaults to 1143) and speaks STARTTLS there.
/// GNOME Online Accounts records which it is, and is followed (#254).
pub(in crate::worker) fn imap_uses_starttls(account: &AccountConfig) -> bool {
    if let Some(sec) = &account.security {
        return sec.imap_starttls;
    }
    account.imap_port != 993 && (account.imap_port == 143 || is_loopback_host(&account.imap_host))
}

/// Hard ceiling on one IMAP connection attempt — TCP, TLS, and authentication
/// together. A server (or middlebox) that accepts the socket and then stalls
/// would otherwise hang the worker with no error ever surfacing.
const IMAP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub(in crate::worker) async fn connect(
    account: &AccountConfig,
) -> Result<ImapSession, Box<dyn std::error::Error>> {
    if !crate::power::reaches(&account.imap_host) {
        return Err(offline_text().into());
    }
    // Read before the connect timeout starts: the keyring's own wait is not
    // the server's, and must not be reported as one.
    let password = sign_in_password(account, false).await?;
    match tokio::time::timeout(IMAP_CONNECT_TIMEOUT, connect_inner(account, &password)).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "connecting to {} timed out after {} seconds",
            account.imap_host,
            IMAP_CONNECT_TIMEOUT.as_secs()
        )
        .into()),
    }
}

async fn connect_inner(
    account: &AccountConfig,
    password: &str,
) -> Result<ImapSession, Box<dyn std::error::Error>> {
    // An Exchange (EWS) account imported from Online Accounts before #316
    // has no IMAP server, and the resolver's answer for an empty name says
    // nothing a user could act on.
    if account.imap_host.trim().is_empty() {
        return Err(i18n(
            "This account has no IMAP server. Hylki can't use Exchange accounts from Online Accounts: remove it and add it as an IMAP or Microsoft 365 account.",
        )
        .into());
    }
    let tcp = TcpStream::connect((account.imap_host.as_str(), account.imap_port)).await?;
    let accept_invalid = account
        .security
        .as_ref()
        .is_some_and(|s| s.imap_accept_invalid_certs);
    let tls = tls_connector(
        &account.imap_host,
        account.tls_accept_hostname_mismatch,
        accept_invalid,
    );
    let client = if imap_uses_starttls(account) {
        let mut plain = async_imap::Client::new(tcp);
        // Consume the plaintext greeting before issuing STARTTLS. Nothing secret
        // has been sent at this point; the credentials go out below, after the
        // socket has been upgraded.
        let _ = plain.read_response().await;
        plain.run_command_and_check_ok("STARTTLS", None).await?;
        let stream = tls
            .connect(account.imap_host.as_str(), plain.into_inner())
            .await?;
        // A server that accepted STARTTLS does not send a second greeting.
        async_imap::Client::new(ImapIo::new(stream))
    } else {
        let stream = tls.connect(account.imap_host.as_str(), tcp).await?;
        let mut client = async_imap::Client::new(ImapIo::new(stream));
        // Consume the server greeting before issuing commands. LOGIN tolerates an
        // unread greeting, but the AUTHENTICATE handshake reads it as the command
        // reply and deadlocks — so read it explicitly here.
        let _ = client.read_response().await;
        client
    };
    // The sign-in for the console: mechanism and user, never the secret.
    let session = if account.oauth {
        // XOAUTH2 with a fresh access token (from GOA or a native refresh token).
        tracing::debug!(target: "hylki::imap", "> AUTHENTICATE XOAUTH2 {}", oauth_user(account));
        let token = match fetch_oauth_token(account).await {
            Some(t) => t,
            None => {
                tracing::warn!(target: "hylki::imap", "< no OAuth token (AUTHENTICATE XOAUTH2)");
                return Err("could not get an OAuth token".into());
            }
        };
        let auth = XOAuth2 {
            user: oauth_user(account),
            token,
            step: 0,
        };
        let r = client
            .authenticate("XOAUTH2", auth)
            .await
            .map_err(|(e, _client)| e);
        match &r {
            Ok(_) => tracing::debug!(target: "hylki::imap", "< OK (AUTHENTICATE XOAUTH2)"),
            Err(e) => {
                tracing::warn!(target: "hylki::imap", "< {e} (AUTHENTICATE XOAUTH2)");
                forget_access_token(account);
            }
        }
        r?
    } else {
        tracing::debug!(target: "hylki::imap", "> LOGIN {} ****", account.username);
        let r = client
            .login(&account.username, password)
            .await
            .map_err(|(e, _client)| e);
        match &r {
            Ok(_) => tracing::debug!(target: "hylki::imap", "< OK (LOGIN)"),
            Err(e) => tracing::warn!(target: "hylki::imap", "< {e} (LOGIN)"),
        }
        r?
    };
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xoauth2_sends_the_bearer_once_then_an_empty_answer() {
        use async_imap::Authenticator;
        let mut auth = XOAuth2 {
            user: "me@x".into(),
            token: "tok".into(),
            step: 0,
        };
        assert_eq!(
            auth.process(b""),
            b"user=me@x\x01auth=Bearer tok\x01\x01".to_vec()
        );
        assert_eq!(auth.process(b"{\"status\":\"400\"}"), Vec::<u8>::new());
        assert_eq!(auth.process(b""), Vec::<u8>::new());
    }
}
