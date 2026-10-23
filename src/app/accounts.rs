//! Accounts: saving and removing them, their secrets in the keyring, and
//! keeping GNOME Online Accounts imports in step.

use super::*;

/// `HYLKI_DEMO` is set, so removing all real accounts leaves the app blank.
/// Whether saving `new` over `old` changes only what the app itself draws
/// or puts in the composer: the signature, the color, emoji or picture, the
/// Gravatar switch, the label, or a place in the unified section. Workers
/// never read those, so the account need not reconnect for them (#382). A
/// password counts as changed unless it is the one already stored, which
/// [`secrets_kept`] asks the keyring.
pub(super) fn look_only_change(old: &AccountConfig, new: &AccountConfig) -> bool {
    let mut probe = new.clone();
    probe.signature = old.signature.clone();
    probe.signature_html = old.signature_html;
    probe.signatures = old.signatures.clone();
    probe.default_signature = old.default_signature;
    probe.color = old.color.clone();
    probe.emoji = old.emoji.clone();
    probe.avatar = old.avatar.clone();
    probe.gravatar = old.gravatar;
    probe.label = old.label.clone();
    probe.in_unified = old.in_unified;
    // Compared as values: a map's order on the way out is no change.
    let same = match (toml::Value::try_from(&probe), toml::Value::try_from(old)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    same
}

/// Whether `new` keeps the secrets of `old`. The editor hands every secret
/// back, read from the keyring, while the config in hand may hold none:
/// compare with what is stored. Blocking: run it on the keyring thread.
pub(super) fn secrets_kept(old: &AccountConfig, new: &AccountConfig) -> bool {
    let kept = |new: &str, old: &str, stored: &dyn Fn() -> Option<String>| {
        new.is_empty() || new == old || stored().as_deref() == Some(new)
    };
    kept(&new.password, &old.password, &|| {
        config::load_password(&old.email)
    }) && kept(&new.smtp_password, &old.smtp_password, &|| {
        config::load_smtp_password(&old.email)
    }) && kept(&new.oauth_refresh, &old.oauth_refresh, &|| {
        config::load_oauth_refresh(&old.email)
    }) && new.aliases.iter().all(|alias| {
        let addr = alias.address();
        let before = old
            .aliases
            .iter()
            .find(|a| a.address().eq_ignore_ascii_case(&addr))
            .map_or("", |a| a.smtp_password.as_str());
        kept(&alias.smtp_password, before, &|| {
            config::load_alias_smtp_password(&old.email, &addr)
        })
    })
}

/// Drop the keyring entries of the accounts `removed` names, on the keyring
/// thread. `config` still holds them: their alias entries are keyed by
/// addresses only it knows.
pub(super) fn forget_account_secrets(removed: &[String], config: &[AccountConfig]) {
    let gone: Vec<(String, Option<AccountConfig>)> = removed
        .iter()
        .map(|email| {
            (
                email.clone(),
                config.iter().find(|c| &c.email == email).cloned(),
            )
        })
        .collect();
    config::keyring_later(move || {
        for (email, account) in gone {
            match account {
                Some(account) => config::delete_account_secrets(&account),
                None => config::delete_password(&email),
            }
        }
    });
}

/// On the way out: hide the windows, then let the keyring thread finish
/// what was queued for it, which a keyring waiting on its user can make take
/// a while. Up to ten seconds, so a keyring that never answers cannot keep
/// the process alive. Nothing queued, nothing waits.
pub(crate) fn finish_keyring_writes() {
    let limit = std::time::Duration::from_secs(10);
    if config::keyring_drain(std::time::Duration::ZERO) {
        return;
    }
    let tops = gtk::Window::toplevels();
    for w in (0..tops.n_items())
        .filter_map(|i| tops.item(i))
        .filter_map(|o| o.downcast::<gtk::Window>().ok())
    {
        w.set_visible(false);
    }
    if let Some(display) = gtk::gdk::Display::default() {
        display.flush();
    }
    tracing::info!("waiting for the keyring before quitting");
    if !config::keyring_drain(limit) {
        tracing::warn!(
            "quit with keyring work still queued: the keyring did not answer in {limit:?}"
        );
    }
}

/// What [`reconcile_goa`] changed: accounts dropped outright (their GOA account
/// is gone), and whether any account was paused or resumed because its Mail
/// service was toggled in GNOME Settings.
#[derive(Default)]
pub(super) struct GoaReconcile {
    pub(super) removed: Vec<String>,
    pub(super) paused_changed: bool,
    /// An account's servers or their security changed in GNOME Settings.
    pub(super) servers_changed: bool,
}

/// Reconcile imported accounts against GNOME Online Accounts: drop the ones
/// whose GOA account no longer exists, and pause — rather than remove — the ones
/// whose Mail service is switched off there, restoring their previous enabled
/// state when it comes back on. Pausing keeps every local setting (label,
/// color, signature, sidebar state) intact. The servers follow GOA's too,
/// so an edit in GNOME Settings needs no re-import (#254). `live` is a snapshot the caller
/// obtained while GOA was reachable — when it isn't, skip reconciliation
/// entirely, so a momentarily-unavailable GOA never wipes imported accounts.
pub(super) fn reconcile_goa(
    config: &mut Vec<AccountConfig>,
    live: &crate::goa::GoaLiveState,
) -> GoaReconcile {
    let mut outcome = GoaReconcile::default();
    config.retain(|c| match &c.goa_id {
        Some(id) if !live.account_ids.contains(id) => {
            outcome.removed.push(c.email.clone());
            false
        }
        _ => true,
    });
    for c in config.iter_mut() {
        let Some(id) = c.goa_id.clone() else { continue };
        if let Some(g) = live.mail.get(&id) {
            if g.apply_servers(c) {
                tracing::info!("servers for {} updated from GNOME Online Accounts", c.email);
                outcome.servers_changed = true;
            }
        }
        let mail_disabled = live.disabled_mail_ids.contains(&id);
        if mail_disabled && !c.goa_mail_disabled {
            c.goa_mail_disabled = true;
            c.goa_enabled_before_mail_disabled = c.enabled;
            c.enabled = false;
            outcome.paused_changed = true;
        } else if !mail_disabled && c.goa_mail_disabled {
            c.goa_mail_disabled = false;
            c.enabled = c.goa_enabled_before_mail_disabled;
            outcome.paused_changed = true;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::demo::demo_account_configs;

    /// Saving a new signature or color leaves the account connected (#382);
    /// a server, a name or a new password reconnects it.
    #[test]
    fn display_only_account_edits() {
        let old = demo_account_configs().remove(0);
        let mut new = old.clone();
        new.set_signatures(
            vec![crate::config::NamedSignature {
                name: "Short".into(),
                html: "Jason".into(),
            }],
            None,
        );
        new.color = Some("#ff0000".into());
        new.in_unified = !old.in_unified;
        // The editor hands the secret back; the same one is no change.
        new.password = old.password.clone();
        assert!(super::look_only_change(&old, &new) && super::secrets_kept(&old, &new));
        let mut host = new.clone();
        host.imap_host = "imap.elsewhere.org".into();
        assert!(!(super::look_only_change(&old, &host) && super::secrets_kept(&old, &host)));
        let mut name = new.clone();
        name.name = "Someone Else".into();
        assert!(!(super::look_only_change(&old, &name) && super::secrets_kept(&old, &name)));
        let mut alias = new.clone();
        alias.aliases.push(crate::config::AliasConfig {
            identity: "me@alias.org".into(),
            ..Default::default()
        });
        assert!(!(super::look_only_change(&old, &alias) && super::secrets_kept(&old, &alias)));
    }

    #[test]
    fn goa_reconcile_drops_pauses_and_resumes() {
        let mut config = demo_account_configs();
        config.truncate(3);
        config[0].goa_id = Some("gone".into());
        config[1].goa_id = Some("paused".into());
        config[1].enabled = true;
        config[2].goa_id = None;
        let removed_email = config[0].email.clone();
        let mut live = crate::goa::GoaLiveState {
            account_ids: ["paused".to_string()].into(),
            disabled_mail_ids: ["paused".to_string()].into(),
            mail: Default::default(),
        };
        let out = reconcile_goa(&mut config, &live);
        assert_eq!(out.removed, vec![removed_email]);
        assert!(out.paused_changed && !out.servers_changed);
        assert_eq!(config.len(), 2);
        assert!(config[0].goa_mail_disabled && !config[0].enabled);
        assert!(config[0].goa_enabled_before_mail_disabled);
        let again = reconcile_goa(&mut config, &live);
        assert!(!again.paused_changed && again.removed.is_empty());
        live.disabled_mail_ids.clear();
        let back = reconcile_goa(&mut config, &live);
        assert!(back.paused_changed);
        assert!(!config[0].goa_mail_disabled && config[0].enabled);
    }
}
