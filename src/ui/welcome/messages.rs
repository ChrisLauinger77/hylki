//! The wizard's messages: its pages' input, what it hands the app, and the
//! results of its connection tests and sign-ins.

use super::*;

/// The settings chosen on the privacy + personalize pages, applied by the app
/// through its normal Set* handlers when the wizard finishes.
#[derive(Debug, Clone)]
pub struct WelcomePrefs {
    pub block_remote: bool,
    pub gravatar: bool,
    pub sender_logos: bool,
    pub notification_content: bool,
    pub preview_lines: u32,
    pub avatars: bool,
    pub threading: bool,
    /// The app icon picked from the gallery (an `app_icon::catalog` id), or
    /// empty when nothing was picked over an icon set outside Hylki.
    pub app_icon: String,
}

#[derive(Debug)]
pub enum WelcomeInput {
    Next,
    Back,
    ProviderChanged,
    TestAndAdd,
    ImportGoa(usize),
    RescanGoa,
    OpenOnlineAccounts,
    /// The page or the manual form changed: relabel the footer button.
    RefreshFooter,
    Finish,
    /// The hero page's language drop-down moved to choice `n`.
    LanguageChanged(u32),
}

#[derive(Debug)]
pub enum WelcomeOutput {
    /// A manual account passed its connection test, or a custom OAuth one
    /// signed in: save + connect it.
    AddAccount(Box<AccountConfig>),
    /// A GNOME Online Accounts import was chosen.
    ImportGoa(Box<AccountConfig>),
    /// The chosen privacy/personalize settings (sent once, on finish).
    Prefs(WelcomePrefs),
    /// The wizard is done (window already closing).
    Done,
    /// A language was picked on the first page: its locale code, "" for
    /// the system's. The app saves it and comes back in that language.
    Language(String),
}

#[derive(Debug)]
pub enum WelcomeCmd {
    Tested {
        account: Box<AccountConfig>,
        result: ConnTest,
        seq: u32,
    },
    /// The browser sign-in for a custom OAuth account ended: its refresh
    /// token, or why not.
    SignedIn {
        account: Box<AccountConfig>,
        /// The refresh token, and who signed in when the provider says.
        result: Result<(String, Option<(String, String)>), String>,
        seq: u32,
    },
}
