//! Signing an account in: the browser or in-app OAuth flow, the extra
//! consents for the directory and labels, and the Online Accounts panel.

use super::form::trimmed;
use super::providers::provider_at;
use super::*;

/// The editor's sign-in: in a window of Hylki's own for a redirect Hylki
/// cannot listen on (#329), otherwise in the browser. `user` is the address
/// typed, which picks the identity broker's account and, with `hint`, the
/// account Microsoft signs in (#410). The answer comes back through `wrap`.
pub(super) fn sign_in(
    root: &adw::Bin,
    sender: &ComponentSender<AccountsWindow>,
    settings: OAuthSettings,
    user: Option<String>,
    hint: bool,
    wrap: fn(Result<(String, Option<(String, String)>), String>) -> AccountsCmd,
) {
    if !settings.redirect_uri.trim().is_empty() {
        let parent = root.root().and_downcast::<gtk::Window>();
        let s = sender.clone();
        crate::ui::web_signin::run(
            parent.as_ref(),
            &settings.clone(),
            user.clone(),
            hint,
            move |answer| {
                s.oneshot_command(async move {
                    let r = tokio::task::spawn_blocking(move || {
                        let (request, code) = answer?;
                        let refresh =
                            crate::oauth::exchange_code(&settings, &request, &code)?.refresh_token;
                        let (name, address, refresh) =
                            crate::oauth::microsoft_whoami(&settings, &refresh, user.as_deref())?;
                        Ok((refresh, Some((name, address))))
                    })
                    .await
                    .unwrap_or_else(|_| Err("sign-in task failed".into()));
                    wrap(r)
                });
            },
        );
        return;
    }
    sender.oneshot_command(async move {
        let r = tokio::task::spawn_blocking(move || {
            let hint = user.as_deref().filter(|_| hint);
            let refresh = crate::oauth::run_flow_as(&settings, hint)?.refresh_token;
            // Microsoft says who signed in, so the account is named
            // for the mailbox reached (#329).
            if settings.token_url.contains("microsoftonline") {
                let (name, address, refresh) =
                    crate::oauth::microsoft_whoami(&settings, &refresh, None)?;
                return Ok((refresh, Some((name, address))));
            }
            Ok((refresh, None))
        })
        .await
        .unwrap_or_else(|_| Err("sign-in task failed".into()));
        wrap(r)
    });
}

/// A list-item factory whose labels never ellipsize, so a `ComboRow` popup grows
/// to fit its longest option instead of truncating it.
/// Open GNOME Settings → Online Accounts. Uses D-Bus app activation so it works
/// both natively and inside a Flatpak (with `--talk-name=org.gnome.Settings`);
/// falls back to the CLI on non-GNOME/older setups.
pub(crate) fn open_online_accounts() {
    if activate_online_accounts_panel().is_err() {
        let _ = std::process::Command::new("gnome-control-center")
            .arg("online-accounts")
            .spawn();
    }
}

fn activate_online_accounts_panel() -> Result<(), gtk::glib::Error> {
    let conn = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE)?;
    // org.freedesktop.Application.ActivateAction(action: s, parameter: av, a{sv}).
    // GNOME Settings' "launch-panel" action takes a (sav): (panel_id, extra_args).
    let panel = ("online-accounts", Vec::<gtk::glib::Variant>::new()).to_variant();
    let params: Vec<gtk::glib::Variant> = vec![panel];
    let platform: std::collections::HashMap<String, gtk::glib::Variant> =
        std::collections::HashMap::new();
    let args = ("launch-panel", params, platform).to_variant();
    conn.call_sync(
        Some("org.gnome.Settings"),
        "/org/gnome/Settings",
        "org.freedesktop.Application",
        "ActivateAction",
        Some(&args),
        None,
        gtk::gio::DBusCallFlags::NONE,
        -1,
        gtk::gio::Cancellable::NONE,
    )?;
    Ok(())
}

impl AccountsWindow {
    /// A sign-in that asked for more scopes (#410, #411) finished: its
    /// refresh token is the one to save, and a new account takes its
    /// address and name from it.
    pub(super) fn take_consent(
        &mut self,
        widgets: &AccountsWindowWidgets,
        result: Result<(String, Option<(String, String)>), String>,
    ) -> Result<(), String> {
        // A saved account signs in as itself: another mailbox picked in
        // the browser would swap the account's token.
        let saved = self
            .editing
            .and_then(|i| self.accounts.get(i))
            .filter(|a| a.oauth && a.goa_id.is_none())
            .map(|a| a.email.clone());
        let (refresh, who) = result.and_then(|(refresh, who)| match (&saved, &who) {
            (Some(email), Some((_, address)))
                if !address.is_empty() && !address.eq_ignore_ascii_case(email) =>
            {
                Err(i18n_f(
                    "Signed in as {address}, not {email}.",
                    &[("address", address), ("email", email)],
                ))
            }
            _ => Ok((refresh, who)),
        })?;
        self.pending_oauth_refresh = Some(refresh);
        // A new account was signed in by this as well.
        if let Some((name, address)) = who {
            if !address.is_empty() {
                widgets.email_row.set_text(&address);
            }
            if !name.is_empty() && widgets.name_row.text().trim().is_empty() {
                widgets.name_row.set_text(&name);
            }
        }
        Ok(())
    }

    /// Build the OAuth client config from the form. Google/Microsoft use built-in
    /// endpoints + credentials; "Custom OAuth" uses the user-entered fields.
    pub(super) fn oauth_settings_from_form(
        &self,
        widgets: &AccountsWindowWidgets,
    ) -> OAuthSettings {
        let provider = provider_at(widgets.provider_row.selected()).oauth_name();
        if let Some(name) = provider {
            let p = crate::oauth::preset(name).unwrap();
            let (client_id, client_secret) = crate::oauth::provider_credentials(name);
            let mut settings = OAuthSettings {
                auth_url: p.auth_url.to_string(),
                token_url: p.token_url.to_string(),
                client_id,
                client_secret,
                scopes: p.scopes.to_string(),
                redirect_uri: String::new(),
            };
            if name == "microsoft" {
                crate::oauth::microsoft_advanced(
                    &mut settings,
                    &trimmed(&widgets.ms_client_row),
                    &trimmed(&widgets.ms_tenant_row),
                    &trimmed(&widgets.ms_scopes_row),
                    &trimmed(&widgets.ms_redirect_row),
                );
                // Built from the preset, the scopes would lose the
                // directory's at the next sign-in or refresh (#410).
                if self.directory_granted {
                    settings.scopes = crate::oauth::with_directory_scopes(&settings.scopes);
                }
                if self.labels_granted {
                    settings.scopes = crate::oauth::with_scopes(
                        &settings.scopes,
                        &crate::oauth::MICROSOFT_LABEL_SCOPES,
                    );
                }
            }
            settings
        } else {
            OAuthSettings {
                auth_url: trimmed(&widgets.oauth_auth_url_row),
                token_url: trimmed(&widgets.oauth_token_url_row),
                client_id: trimmed(&widgets.oauth_client_id_row),
                client_secret: widgets.oauth_secret_row.text().to_string(),
                scopes: trimmed(&widgets.oauth_scope_row),
                redirect_uri: String::new(),
            }
        }
    }
}
