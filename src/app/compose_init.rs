//! Setting up a composer: the From identities it offers and which one it opens
//! with.

use std::collections::HashMap;

use crate::config::{split_identity, AccountConfig};
use crate::ui::compose::{ComposeAccount, ComposePrefill};

/// Selectable "from" identities, in display order: each account, then
/// one entry per send-as alias it defines (#34) — same transport and
/// signature, a different From on the wire. Built from the CONFIG, not
/// the live account list: launched cold by a mailto/file hand-off
/// (Nautilus's "Send by email", #105) the composer opens before any
/// worker has connected, and the live list is still empty — which
/// hid the From row entirely.
/// The demo's stand-in accounts count here too, so a demo reply
/// carries a signature like a real one.
pub(super) fn compose_identities(
    account_order: &[String],
    config: &[AccountConfig],
    server_identities: &HashMap<u32, Vec<(String, String)>>,
) -> Vec<ComposeAccount> {
    let mut emails: Vec<String> = Vec::new();
    for email in account_order {
        if config.iter().any(|c| c.enabled && &c.email == email) && !emails.contains(email) {
            emails.push(email.clone());
        }
    }
    for c in config.iter().filter(|c| c.enabled) {
        if !emails.contains(&c.email) {
            emails.push(c.email.clone());
        }
    }
    emails
        .iter()
        .flat_map(|email| {
            let Some((idx, cfg)) = config.iter().enumerate().find(|(_, c)| &c.email == email)
            else {
                return Vec::new();
            };
            let id = idx as u32 + 1;
            let label = if cfg.name.trim().is_empty() {
                cfg.email.clone()
            } else {
                format!("{} <{}>", cfg.name, cfg.email)
            };
            let cfg = Some(cfg);
            let (signatures, default) = cfg.map(|c| c.signature_set()).unwrap_or_default();
            let signature = default
                .map(|i| signatures[i].html.clone())
                .unwrap_or_default();
            let pgp_key = cfg.and_then(|c| c.pgp_key.clone());
            let sign_default = cfg.is_some_and(|c| c.sign_by_default);
            let mut identities = vec![ComposeAccount {
                id,
                label,
                signature: signature.clone(),
                signatures: signatures.clone(),
                email: email.clone(),
                pgp_key: pgp_key.clone(),
                sign_default,
                alias_from: None,
            }];
            for alias in cfg.map(|c| c.aliases.as_slice()).unwrap_or_default() {
                let (name, addr) = split_identity(&alias.identity);
                if addr.is_empty() {
                    continue;
                }
                let display = if name.is_empty() {
                    addr.clone()
                } else {
                    format!("{name} <{addr}>")
                };
                identities.push(ComposeAccount {
                    id,
                    label: display.clone(),
                    signature: signature.clone(),
                    signatures: signatures.clone(),
                    email: addr,
                    pgp_key: pgp_key.clone(),
                    sign_default,
                    alias_from: Some(display),
                });
            }
            // A JMAP server's own identities (#346), unless the account
            // already lists the address. A catch-all (`*@domain`) is no
            // address to send from; typing one into From uses it.
            for (name, addr) in server_identities
                .get(&id)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                if addr.starts_with("*@")
                    || identities
                        .iter()
                        .any(|i| i.email.eq_ignore_ascii_case(addr))
                {
                    continue;
                }
                let display = if name.is_empty() || name.eq_ignore_ascii_case(addr) {
                    addr.clone()
                } else {
                    format!("{name} <{addr}>")
                };
                identities.push(ComposeAccount {
                    id,
                    label: display.clone(),
                    signature: signature.clone(),
                    signatures: signatures.clone(),
                    email: addr.clone(),
                    pgp_key: pgp_key.clone(),
                    sign_default,
                    alias_from: Some(display),
                });
            }
            identities
        })
        .collect()
}

/// Default identity: for a reply, whichever of the account's addresses
/// the original was sent to — mail to an alias is answered as the alias.
/// For a new message, the default sender chosen in Settings (#157), if
/// it is one of this account's addresses. Otherwise (and when nothing
/// matches) the account's own address.
/// A reply to a `+tag` address of one of the account's identities
/// (#347) answers from that address, as typed, through the identity.
///
/// Returns the index into `accounts`; a `+tag` reply also sets
/// `prefill.custom_from`.
pub(super) fn choose_identity(
    accounts: &[ComposeAccount],
    account_id: u32,
    prefill: &mut ComposePrefill,
) -> usize {
    let plus = (prefill.custom_from.trim().is_empty())
        .then(|| {
            crate::worker::parse_recipients(&prefill.reply_addressed_to)
                .into_iter()
                .find_map(|(_, addr)| {
                    let (user, host) = addr.split_once('@')?;
                    let (base, _tag) = user.split_once('+')?;
                    let plain = format!("{base}@{host}");
                    let i = accounts
                        .iter()
                        .position(|c| c.id == account_id && c.email.eq_ignore_ascii_case(&plain))?;
                    let (name, _) = crate::config::split_identity(
                        &accounts[i]
                            .alias_from
                            .clone()
                            .unwrap_or_else(|| accounts[i].label.clone()),
                    );
                    Some((i, crate::worker::format_recipient(&name, &addr)))
                })
        })
        .flatten();
    if let Some((_, from)) = &plus {
        prefill.custom_from = from.clone();
    }
    let hay = prefill.reply_addressed_to.to_lowercase();
    let want = prefill.from_address.trim().to_lowercase();
    let by_reply = || {
        (!hay.is_empty())
            .then(|| {
                accounts.iter().position(|c| {
                    c.id == account_id && hay.contains(c.email.to_lowercase().as_str())
                })
            })
            .flatten()
    };
    plus.map(|(i, _)| i)
        .or_else(by_reply)
        .or_else(|| {
            (!want.is_empty())
                .then(|| {
                    accounts
                        .iter()
                        .position(|c| c.id == account_id && c.email.to_lowercase() == want)
                })
                .flatten()
        })
        .or_else(|| {
            accounts
                .iter()
                .position(|c| c.id == account_id && c.alias_from.is_none())
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::demo_account_configs;

    fn identities_fixture() -> Vec<ComposeAccount> {
        let mut config = demo_account_configs();
        config.truncate(2);
        config[0].email = "a@x.com".into();
        config[0].name = "Ann".into();
        config[0].aliases = vec![
            toml::from_str("identity = \"Bea <ba@x.com>\"").unwrap(),
            toml::from_str("identity = \"Nobody <>\"").unwrap(),
        ];
        config[1].email = "c@y.org".into();
        config[1].name = " ".into();
        let mut server = HashMap::new();
        server.insert(
            1,
            vec![
                ("".to_string(), "*@x.com".to_string()),
                ("".to_string(), "BA@x.com".to_string()),
                ("Ann".to_string(), "info@x.com".to_string()),
            ],
        );
        server.insert(2, vec![("c@y.org".to_string(), "c2@y.org".to_string())]);
        compose_identities(&["c@y.org".to_string()], &config, &server)
    }

    #[test]
    fn compose_identities_follow_the_sidebar_order_then_aliases() {
        let ids = identities_fixture();
        let rows: Vec<(u32, &str, &str, Option<&str>)> = ids
            .iter()
            .map(|c| {
                (
                    c.id,
                    c.label.as_str(),
                    c.email.as_str(),
                    c.alias_from.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                (2, "c@y.org", "c@y.org", None),
                (
                    2,
                    "c@y.org <c2@y.org>",
                    "c2@y.org",
                    Some("c@y.org <c2@y.org>")
                ),
                (1, "Ann <a@x.com>", "a@x.com", None),
                (1, "Bea <ba@x.com>", "ba@x.com", Some("Bea <ba@x.com>")),
                (
                    1,
                    "Ann <info@x.com>",
                    "info@x.com",
                    Some("Ann <info@x.com>")
                ),
            ]
        );
    }

    /// B5 in the map: the reply identity is found by substring, so mail to
    /// the alias ba@x.com is answered from the account a@x.com, which comes
    /// first and is contained in it.
    #[test]
    fn reply_identity_is_chosen_by_substring_of_the_recipients() {
        let ids = identities_fixture();
        let mut reply = ComposePrefill {
            reply_addressed_to: "Bea <ba@x.com>, ".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut reply), 2);
        assert!(reply.custom_from.is_empty());
        let mut info = ComposePrefill {
            reply_addressed_to: "info@x.com".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut info), 4);
        let mut chosen = ComposePrefill {
            from_address: " BA@x.com ".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut chosen), 3);
        let mut plain = ComposePrefill::default();
        assert_eq!(choose_identity(&ids, 1, &mut plain), 2);
        assert_eq!(choose_identity(&ids, 9, &mut plain), 0);
    }

    #[test]
    fn a_reply_to_a_plus_address_keeps_it_as_the_from() {
        let ids = identities_fixture();
        let mut reply = ComposePrefill {
            reply_addressed_to: "ba+lists@x.com".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut reply), 3);
        assert_eq!(reply.custom_from, "Bea <ba+lists@x.com>");
        let mut typed = ComposePrefill {
            reply_addressed_to: "ba+lists@x.com".into(),
            custom_from: "Me <me@x.com>".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut typed), 2);
        assert_eq!(typed.custom_from, "Me <me@x.com>");
    }
}
