//! The settings window's messages: what it starts with, what the app and
//! its own rows send it, and the preference changes it reports.

use super::*;

/// Initial data for the settings window.
#[derive(Debug)]
pub struct PrefInit {
    pub auto_remote_content: bool,
    pub show_remote_banner: bool,
    pub show_spoof_banner: bool,
    pub gravatar: bool,
    pub avatars: bool,
    /// Your own mail wears its mailbox's face, not a sender's circle (#189).
    pub own_mailbox_face: bool,
    /// Tagged rows are washed with their tag's color (PR #383).
    pub tag_row_tint: bool,
    pub sender_logos: bool,
    pub date_style: DateStyle,
    pub clock_style: ClockStyle,
    /// The chosen interface language code; empty = the system's (#179).
    pub language: String,
    pub fetch_interval_secs: u64,
    pub push: bool,
    pub metered_save_data: bool,
    pub palette_collapse_secs: u64,
    /// Seconds a message card's actions palette stays open — its own, not
    /// the message list's.
    pub card_palette_collapse_secs: u64,
    pub threading: bool,
    pub threads_expanded: bool,
    /// Reading pane shows conversations newest-message-first.
    pub thread_newest_first: bool,
    /// Which messages of a conversation open folded (#326).
    pub fold_messages: crate::config::FoldMessages,
    /// Reader always shows the recipients line under the sender.
    pub always_show_recipients: bool,
    /// The OpenPGP chip says its verdict in words (#300).
    pub pgp_labels: bool,
    /// Lone messages render as inset cards, like conversation messages.
    pub single_message_card: bool,
    /// The Light / Dark Mode switch is shown in the reader header (PR #386).
    pub theme_switch: bool,
    /// Icons come from the desktop's icon theme, not Hylki's own.
    pub desktop_icons: bool,
    /// The Reader View switch is shown in the reader header.
    pub reader_switch: bool,
    /// What Reader View does when a message is opened.
    pub reader_default: crate::config::ReaderDefault,
    /// The message zoom every launch starts at, in percent.
    pub reader_zoom: u32,
    /// Each conversation message lists its own attachments (#213).
    pub card_attachments: bool,
    /// The attachment drawer beneath the reader is shown (#213).
    pub attachment_drawer: bool,
    /// Conversation rows may expand into their members in the message list.
    pub thread_expansion: bool,
    /// A conversation's row speaks for the newest message anywhere in the
    /// account, the replies you sent included (#236).
    pub thread_row_newest: bool,
    /// Deleting a whole selected conversation asks for confirmation.
    pub confirm_thread_delete: bool,
    /// Conversation card actions hide until the card is hovered.
    pub card_actions_hover: bool,
    /// With the ⋯ toggle off: card actions appear automatically on hover.
    pub card_actions_auto: bool,
    /// The list rows carry an actions palette line at all.
    pub list_palette: bool,
    /// The list's actions palette opens on row hover (no ⋯ click).
    pub list_palette_hover: bool,
    /// A message card's ⋯ opens the card menu instead of sliding its
    /// actions palette out.
    pub card_palette_menu: bool,
    /// Message rows take a sideways swipe to archive / delete (#92).
    pub swipe_enabled: bool,
    /// The message list's swipe-gesture sides are swapped (#swipe).
    pub swipe_reversed: bool,
    /// How far a trackpad two-finger swipe has to travel to fire the action.
    pub swipe_sensitivity: f64,
    /// "New message" composes inline over the reading pane (vs a window).
    pub compose_inline: bool,
    /// Reply, Reply All and Forward open in the reading pane (vs a window).
    pub reply_inline: bool,
    pub reply_fields: bool,
    /// Settings → System → GNOME Files: what handed-in files open into.
    pub files: crate::config::FilesPrefs,
    /// Settings → System → Links: the browser links open in (#232). Empty =
    /// the desktop's default, `ask` = its app chooser, otherwise a desktop
    /// entry id.
    pub link_browser: String,
    /// The identity new messages are sent from (#157); empty = the open
    /// folder's account. One of `identities`' addresses.
    pub compose_default_from: String,
    pub paste_plain: bool,
    pub return_paragraph: bool,
    pub toolbar_expanded: bool,
    /// The font rich-text messages are written in, as a CSS family list,
    /// and its size in points; empty and 0 are the defaults.
    pub compose_font: String,
    pub compose_font_size: u32,
    /// Settings → Reading → Printing (#359).
    pub print_options: crate::config::PrintOptions,
    pub spellcheck: bool,
    pub spellcheck_langs: String,
    pub message_theme: MessageTheme,
    /// Set every message in the reader's own font (#56).
    pub override_fonts: bool,
    /// That font, as a Pango description; empty = the interface font.
    pub reader_font: String,
    /// Ignore the senders' text and background colors (#56).
    pub override_colors: bool,
    /// Plain-text messages in monospace (#181), and the font ("" = the
    /// desktop's monospace font).
    pub plain_monospace: bool,
    pub plain_font: String,
    /// New messages start as plain text (#180).
    pub compose_format: crate::config::ComposeFormat,
    /// Replies and forwards to plain-text mail are written in plain text (#436).
    pub follow_plain: bool,
    /// Where the split reply opens in the reading pane (#212).
    pub reply_position: crate::config::ReplyPosition,
    /// Where the signature sits in a reply or forward (#237).
    pub signature_position: crate::config::SignaturePosition,
    pub signature_dashes: bool,
    /// Undo Send delay in seconds, 0 for off (#434).
    pub undo_send_secs: u32,
    pub app_theme: AppTheme,
    pub text_scale: u32,
    /// The appearance theme's id ("system" for the stock GNOME colors).
    pub theme: String,
    pub notifications: bool,
    pub notification_content: bool,
    pub notification_buttons: crate::config::NotificationButtons,
    pub show_attachments: bool,
    pub show_unified: bool,
    pub unified_chips: crate::config::UnifiedChips,
    pub unified_kinds: crate::config::UnifiedKinds,
    pub show_accounts: bool,
    pub section_style: crate::config::SectionStyle,
    pub filtered_placement: crate::config::SectionPlacement,
    pub tags_placement: crate::config::SectionPlacement,
    pub favorites_placement: crate::config::SectionPlacement,
    pub contacts_placement: crate::config::SectionPlacement,
    pub chevrons_left: bool,
    pub account_click_folds: bool,
    pub start_view: crate::config::StartView,
    pub folder_sort: crate::config::FolderSort,
    pub console_mode: bool,
    pub status_bar_top: bool,
    pub read_mark: crate::config::ReadMark,
    pub sidebar_hover_expand: bool,
    pub remember_sidebar: bool,
    pub remember_rail: bool,
    pub rail_dots: bool,
    pub seen_counts: bool,
    pub rail_fold: crate::config::RailFold,
    /// The reader header's buttons, per side and in order.
    pub reader_toolbar: ReaderToolbar,
    /// Focus Mode: the master switch and the parts it strips.
    pub focus: crate::config::FocusMode,
    pub preview_lines: u32,
    pub list_layout: crate::config::ListLayout,
    pub list_columns: Vec<crate::config::ListColumn>,
    pub list_headings: bool,
    pub list_column_scroll: bool,
    pub list_group_by_date: bool,
    pub list_pills_below: bool,
    pub list_search_open: bool,
    pub contacts_search_open: bool,
    pub list_search_bottom: bool,
    pub list_bulk_bar_bottom: bool,
    pub single_key_shortcuts: bool,
    pub gmail_keys: bool,
    pub keep_downloads_days: u32,
    pub run_in_background: bool,
    pub autostart: bool,
    pub tray: bool,
    pub tray_icon: TrayIcon,
    pub tray_mail: bool,
    pub launcher_badge: LauncherBadge,
    /// The chosen app icon (an `app_icon::catalog` id).
    pub app_icon: String,
    /// The accounts panel (built by the AccountsWindow component), shown
    /// behind the window's "Accounts" tab.
    pub accounts_panel: gtk::Widget,
    /// Open showing the Accounts tab instead of Preferences.
    pub start_on_accounts: bool,
    /// The category to open on, when the app remembers one from earlier
    /// this session; overrides `start_on_accounts`.
    pub start_page: Option<String>,
    /// The persisted "this window opens to" choice (true = Accounts).
    pub settings_open_accounts: bool,
    /// The accounts component's inbox, for the sidebar to pick its pages.
    pub accounts_sender: relm4::Sender<crate::ui::accounts::AccountsInput>,
    /// (name, address) per account and alias, for the OpenPGP page's
    /// key generator.
    pub identities: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum PrefInput {
    /// The Contacts page: the books new contacts can go to, freshly read.
    SetContactBooks(Vec<crate::contacts::Book>),
    /// The Hylki book's size changed (an import, a deletion).
    SetLocalContactCount(usize),
    ToggleAvatars(bool),
    ChangeDateStyle(u32),
    ChangeClockStyle(u32),
    ChangeLanguage(u32),
    ChangeRenderer(u32),
    /// The GNOME Files extension (#188): install this build's copy, remove
    /// the installed one, ask Files to quit so it reloads.
    NautilusInstall,
    NautilusRemove,
    NautilusRestartFiles,
    /// The GNOME Files hand-off rows: destination, over-the-limit action,
    /// and the limit in MB.
    ChangeFilesAction(u32),
    ChangeFilesLarge(u32),
    ChangeFilesLimit(u32),
    /// Settings → System → Links: the browser row moved (#232).
    ChangeLinkBrowser(u32),
    /// The app changed the Files preferences (a dialog's "always do this").
    SetFilesPrefs(crate::config::FilesPrefs),
    /// The spelling languages changed, possibly from a composer (#365).
    SetSpellcheckLangs(String),
    /// Re-read the extension's state (the System page came into view; Files
    /// may have loaded the extension since).
    NautilusRefresh,
    /// Settings → System → Storage (#394): measure what downloaded mail
    /// takes up, off the main thread, and the answer.
    MeasureStorage,
    StorageMeasured {
        bodies: u64,
        files: u64,
        total: u64,
    },
    RemoveDownloads,
    RemoveDownloadsNow,
    ChangeKeepDownloads(u32),
    ToggleThreading(bool),
    ChangeReaderDefault(u32),
    ChangeReaderZoom(u32),
    ToggleThreadExpansion(bool),
    ChangeCardActionsMode(u32),
    ToggleListPalette(bool),
    ToggleSwipeEnabled(bool),
    /// The "Send new messages from" combo: 0 = the open folder's account,
    /// then `identities` in order.
    ChangeComposeDefaultFrom(u32),
    ChangeFetchInterval(u32),
    ToggleNotifications(bool),
    ToggleNotificationButton(crate::config::NotificationButton, bool),
    /// The new-mail sound (#292): on or off, which one (an index into the
    /// built-ins, then Custom File), pick a file, it was picked, hear it.
    ToggleSound(bool),
    ToggleSoundFocused(bool),
    ChangeSound(u32),
    ChooseSound,
    SoundChosen(std::path::PathBuf),
    PlaySound,
    ToggleShowUnified(bool),
    ToggleUnifiedChipAllInboxes(bool),
    TogglePrintPageNumbers(bool),
    TogglePrintDate(bool),
    TogglePrintPunchMargin(bool),
    ToggleUnifiedChipStarred(bool),
    ToggleUnifiedChipDrafts(bool),
    ToggleUnifiedChipArchive(bool),
    ToggleUnifiedChipJunk(bool),
    ToggleUnifiedChipTrash(bool),
    ToggleUnifiedStarred(bool),
    ToggleUnifiedSent(bool),
    ToggleUnifiedDrafts(bool),
    ToggleUnifiedArchive(bool),
    ToggleUnifiedJunk(bool),
    ToggleUnifiedTrash(bool),
    ToggleShowAccounts(bool),
    /// One of Focus Mode's switches.
    ToggleFocus(crate::config::FocusPart, bool),
    /// The app's Focus Mode changed elsewhere (menu, shortcut): follow.
    SetFocusMode(crate::config::FocusMode),
    /// The main menu flipped "Show Accounts": the switch follows.
    SetShowAccounts(bool),
    ChangeChevronSide(u32),
    ChangeStartView(u32),
    ChangeFolderSort(u32),
    ChangeSectionStyle(u32),
    ChangeFilteredPlacement(u32),
    ChangeTagsPlacement(u32),
    ChangeFavoritesPlacement(u32),
    ChangeContactsPlacement(u32),
    /// A reader toolbar chip was dropped: `key` names the button, `side`
    /// the zone (None = not shown), `index` its place in that zone.
    ToolbarDrop {
        key: String,
        side: Option<ToolbarSide>,
        index: usize,
    },
    /// A column chip dropped among the shown columns at `index`, or among
    /// the hidden ones (#334).
    ColumnDrop {
        key: String,
        shown: bool,
        index: usize,
    },
    ColumnRestore,
    ToolbarRestore,
    /// The showcase's stand-in for a drag hovering a zone: open the gap at
    /// `index` of the zone (0 left, 1 right, 2 not shown).
    ToolbarGapPreview {
        zone: usize,
        index: usize,
    },
    ToggleRailFoldEnabled(bool),
    ToggleRailFoldAccounts(bool),
    ToggleRailFoldAllInboxes(bool),
    ToggleRailFoldStarred(bool),
    ToggleRailFoldSent(bool),
    ToggleRailFoldDrafts(bool),
    ToggleRailFoldArchive(bool),
    ToggleRailFoldJunk(bool),
    ToggleRailFoldTrash(bool),
    ToggleRailFoldFiltered(bool),
    ToggleRailFoldTags(bool),
    ChangePreviewLines(u32),
    ChangeListLayout(u32),
    ChangeReadMark(u32),
    ExportSettings,
    ExportLog,
    ImportSettings,
    ChangeTrayIcon(u32),
    ChangeLauncherBadge(u32),
    ChangeMessageTheme(u32),
    ChangeComposeFormat(u32),
    ChangeReplyPosition(u32),
    ChangeFoldMessages(u32),
    ChangeSignaturePosition(u32),
    ChangeUndoSend(u32),
    ChangeAppTheme(u32),
    ChangeTextScale(u32),
    ChangeSettingsOpen(u32),
    /// Switch the window to the Accounts panel (true) or Preferences (false).
    ShowAccounts(bool),
    /// Put the deferred pages back into the stack (scheduled after the
    /// first paint).
    MountPages,
    /// The settings search (#260): the text typed, and a result picked.
    Search(String),
    SearchPick(usize),
    /// Ctrl+F: open the search, or close it when open.
    ToggleSearch,
    /// A fresh accounts panel for this open (the window is kept between
    /// opens; the panel is rebuilt over the current accounts).
    SetAccountsPanel {
        panel: gtk::Widget,
        sender: relm4::Sender<crate::ui::accounts::AccountsInput>,
    },
    /// A sidebar category was chosen (#141).
    SelectPage(String),
    /// The open editor says it holds unsaved changes: ask before leaving it
    /// for this page.
    LeaveEditorPrompt(String),
    /// Leave the editor for this page — it is saved, discarded, or was never
    /// touched.
    LeaveEditorTo(String),
    /// Select a category by id from outside (the app's showcase hook).
    ShowPageById(String),
    /// Message List, scrolled to its Columns group (#443).
    RevealColumns,
    /// An accounts-panel editor subpage (account, filter or tag) opened on
    /// the named settings page, or closed — hide/show the shared header so
    /// the editor's own header takes over the window.
    EditorOpen(Option<&'static str>),
    /// The Cloud Storage page's account editor is up (or gone).
    CloudEditorOpen(bool),
    /// What the Contacts page asks of the window and the app.
    Contacts(crate::ui::contact_sources::ContactSourcesOutput),
}

#[derive(Debug)]
pub enum PrefOutput {
    /// The Contacts page's buttons: the app opens the file dialogs and the
    /// confirmation, and does the work off the main thread.
    ImportLocalContacts,
    ExportLocalContacts,
    DeleteAllLocalContacts,
    SetContactBook(String),
    /// The Contacts page is showing: read the books new contacts can go to
    /// (D-Bus, so off the main thread) and send them back.
    LoadContactBooks,
    /// An address book was added, changed, removed or switched on or off
    /// for Hylki (#408): sync the books and read the contacts again.
    ContactSourcesChanged,
    /// A category was shown, so the app can reopen the window on it.
    PageShown(String),
    SetAutoRemoteContent(bool),
    SetShowRemoteBanner(bool),
    SetShowSpoofBanner(bool),
    SetGravatar(bool),
    SetAvatars(bool),
    SetOwnMailboxFace(bool),
    SetTagRowTint(bool),
    SetSenderLogos(bool),
    SetDateStyle(DateStyle),
    SetClockStyle(ClockStyle),
    /// The interface language code chosen, "" for the system's (#179).
    SetLanguage(String),
    /// A `GSK_RENDERER` value, or empty for GTK's own choice (#403).
    SetRenderer(String),
    SetThreading(bool),
    SetThreadsExpanded(bool),
    SetThreadNewestFirst(bool),
    SetFoldMessages(crate::config::FoldMessages),
    SetAlwaysShowRecipients(bool),
    SetPgpLabels(bool),
    SetSingleMessageCard(bool),
    SetReaderSwitch(bool),
    SetThemeSwitch(bool),
    SetDesktopIcons(bool),
    SetReaderDefault(crate::config::ReaderDefault),
    SetReaderZoom(u32),
    SetCardAttachments(bool),
    SetAttachmentDrawer(bool),
    SetThreadExpansion(bool),
    SetThreadRowNewest(bool),
    SetConfirmThreadDelete(bool),
    SetCardActionsMode {
        hover_toggle: bool,
        hover_auto: bool,
    },
    SetListPalette(bool),
    SetListPaletteHover(bool),
    SetCardPaletteMenu(bool),
    SetSwipeEnabled(bool),
    SetSwipeReversed(bool),
    SetSwipeSensitivity(f64),
    SetComposeInline(bool),
    SetReplyInline(bool),
    SetReplyFields(bool),
    SetFilesPrefs(crate::config::FilesPrefs),
    /// The browser links open in (#232): "" = the desktop's default,
    /// "ask" = its app chooser, otherwise a desktop entry id.
    SetLinkBrowser(String),
    SetComposeDefaultFrom(String),
    SetPastePlain(bool),
    SetReturnParagraph(bool),
    SetToolbarExpanded(bool),
    SetComposeFont(String),
    SetComposeFontSize(u32),
    SetSpellcheck(bool),
    SetSpellcheckLangs(String),
    SetFetchInterval(u64),
    SetPush(bool),
    SetMeteredSaveData(bool),
    SetNotifications(bool),
    SetNotificationContent(bool),
    SetNotificationButtons(crate::config::NotificationButtons),
    SetAttachmentsRow(bool),
    SetShowUnified(bool),
    SetUnifiedChips(crate::config::UnifiedChips),
    SetPrintOptions(crate::config::PrintOptions),
    SetUnifiedKinds(crate::config::UnifiedKinds),
    SetShowAccounts(bool),
    SetChevronsLeft(bool),
    SetAccountClickFolds(bool),
    SetStartView(crate::config::StartView),
    SetFolderSort(crate::config::FolderSort),
    SetSectionStyle(crate::config::SectionStyle),
    SetFilteredPlacement(crate::config::SectionPlacement),
    SetTagsPlacement(crate::config::SectionPlacement),
    SetFavoritesPlacement(crate::config::SectionPlacement),
    SetContactsPlacement(crate::config::SectionPlacement),
    SetConsoleMode(bool),
    SetStatusBarTop(bool),
    SetReadMark(crate::config::ReadMark),
    ExportSettings,
    ExportLog,
    ImportSettings,
    SetSidebarHoverExpand(bool),
    SetRememberSidebar(bool),
    SetRememberRail(bool),
    SetRailDots(bool),
    SetSeenCounts(bool),
    SetReaderToolbar(ReaderToolbar),
    SetFocusMode(crate::config::FocusMode),
    SetRailFold(crate::config::RailFold),
    SetAppTheme(AppTheme),
    SetTextScale(u32),
    /// A theme was picked in the gallery (its id, or "system").
    SetTheme(String),
    /// The "this window opens to" choice changed (true = Accounts).
    SetSettingsOpenAccounts(bool),
    SetPreviewLines(u32),
    SetListLayout(crate::config::ListLayout),
    /// The single-line list's columns, in order (#334).
    SetListColumns(Vec<crate::config::ListColumn>),
    SetListHeadings(bool),
    /// Scroll a single line too wide for the pane, or fit its columns (#443).
    SetListColumnScroll(bool),
    /// Date headings over a list sorted by date (#435).
    SetGroupByDate(bool),
    /// A card's tags and labels on a line under its text (#374).
    SetPillsBelow(bool),
    /// Keep the list's search field open (#446).
    SetListSearchOpen(bool),
    /// Keep Contacts' search field open.
    SetContactsSearchOpen(bool),
    /// The list's search field under the rows, not above them (#496).
    SetListSearchBottom(bool),
    /// The list's bulk bar under the rows, not above them (#496).
    SetListBulkBarBottom(bool),
    /// Every column back to its own width (#334).
    ResetColumnWidths,
    SetSingleKey(bool),
    SetGmailKeys(bool),
    /// How many days downloaded mail is kept; 0 keeps it (#394).
    SetKeepDownloads(u32),
    SetRunInBackground(bool),
    SetAutostart(bool),
    SetTray(bool),
    SetTrayIcon(TrayIcon),
    SetTrayMail(bool),
    SetLauncherBadge(LauncherBadge),
    SetAppIcon(String),
    SetPaletteCollapse(u64),
    SetCardPaletteCollapse(u64),
    SetMessageTheme(MessageTheme),
    SetOverrideFonts(bool),
    SetReaderFont(String),
    SetOverrideColors(bool),
    SetPlainMonospace(bool),
    SetPlainFont(String),
    SetComposeFormat(crate::config::ComposeFormat),
    SetFollowPlain(bool),
    SetReplyPosition(crate::config::ReplyPosition),
    SetSignaturePosition(crate::config::SignaturePosition),
    SetSignatureDashes(bool),
    SetUndoSend(u32),
    Closed,
}
