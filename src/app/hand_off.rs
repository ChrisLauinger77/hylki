//! Files handed to the app (Files' Send with Hylki, a mailto with attach=, a
//! drop on the window): which composer they go to, and how.

use super::*;

/// The handed-in files' names, joined, for a dialog.
pub(super) fn hand_off_names(files: &[std::path::PathBuf]) -> String {
    files
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the handed-in files add up to on disk.
pub(super) fn hand_off_size(files: &[std::path::PathBuf]) -> u64 {
    files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// The "remember this" check a hand-off dialog carries: on, the answer
/// becomes the Settings → System → GNOME Files preference.
pub(super) fn remember_check() -> gtk::CheckButton {
    let check = gtk::CheckButton::with_label(&i18n(
        "Always do this (change it in Settings → System → GNOME Files)",
    ));
    check.set_halign(gtk::Align::Start);
    check.set_margin_top(6);
    check
}

/// Files dropped on the main window, for [`AppMsg::DropFiles`]. Folders
/// are passed over: there is nothing to attach.
pub(super) fn window_drop_target(
    sender: &ComponentSender<AppModel>,
    phase: gtk::PropagationPhase,
) -> gtk::DropTarget {
    let drop = gtk::DropTarget::new(
        gtk::gdk::FileList::static_type(),
        gtk::gdk::DragAction::COPY,
    );
    drop.set_propagation_phase(phase);
    let s = sender.input_sender().clone();
    drop.connect_drop(move |_, value, _, _| {
        let Ok(list) = value.get::<gtk::gdk::FileList>() else {
            return false;
        };
        let paths: Vec<_> = list
            .files()
            .iter()
            .filter_map(|f| f.path())
            .filter(|p| p.is_file())
            .collect();
        if paths.is_empty() {
            return false;
        }
        s.emit(AppMsg::DropFiles(paths));
        true
    });
    drop
}

/// Files handed in from outside the app (GNOME Files' "Send with Hylki",
/// "Open With Hylki", a mailto: with attach=), on their way to a composer.
#[derive(Debug, Default)]
pub(crate) struct FileHandOff {
    /// The composer's other fields (a mailto's recipient, subject, body):
    /// used when the files go into a new message.
    pub(super) base: ComposePrefill,
    /// The readable files.
    pub(super) files: Vec<std::path::PathBuf>,
    /// The ones that were named but could not be read (reported once).
    pub(super) dropped: Vec<std::path::PathBuf>,
}

/// Where a hand-off's files are going, once decided.
#[derive(Debug)]
pub(crate) enum HandOffTarget {
    New,
    Draft(Message),
    Reply(Message),
}

/// A hand-off's files split by how the composer takes them: attached, or
/// uploaded to cloud storage and linked (the ones over the size limit).
#[derive(Debug, Default, Clone)]
pub(crate) struct HandOffFiles {
    pub(super) attach: Vec<std::path::PathBuf>,
    pub(super) cloud: Vec<std::path::PathBuf>,
}

impl HandOffFiles {
    fn of(files: Vec<std::path::PathBuf>, cloud: bool) -> Self {
        if cloud {
            Self {
                attach: Vec::new(),
                cloud: files,
            }
        } else {
            Self {
                attach: files,
                cloud: Vec::new(),
            }
        }
    }
}

impl AppModel {
    /// The tail of a composer opened from outside the app (a mailto: link, a
    /// file manager's "Send by email" or "Open With"): say which files could
    /// not be attached, and if the window has not come to the front by the
    /// time the compositor would have let it, post a desktop alert whose
    /// click brings it up. The activation token a launcher passes along is
    /// what lets a window take the focus; a stale or missing one (a link
    /// relayed from a terminal, an older launcher) leaves the window where it
    /// was, behind whatever the user is looking at, and this is the next
    /// best thing.
    pub(super) fn after_hand_off(&self, attached: usize, dropped: Vec<std::path::PathBuf>) {
        if !dropped.is_empty() {
            let names: Vec<String> = dropped
                .iter()
                .map(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.display().to_string())
                })
                .collect();
            let text = crate::i18n::ni18n_f(
                "Could not attach {names}: Hylki cannot read the file.",
                "Could not attach {n} files (Hylki cannot read them): {names}",
                dropped.len() as u32,
                &[
                    ("n", &dropped.len().to_string()),
                    ("names", &names.join(", ")),
                ],
            );
            self.notifications.emit(NotifyInput::Push {
                text,
                error: true,
                connectivity: false,
            });
        }
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
            // Any window of ours in front will do: the composer, the main
            // window under an inline composer, or the dialog asking where
            // the files should go.
            let tops = gtk::Window::toplevels();
            let ours = (0..tops.n_items())
                .filter_map(|i| tops.item(i))
                .filter_map(|o| o.downcast::<gtk::Window>().ok())
                .any(|w| w.is_active());
            if !ours {
                tracing::info!("hand-off: window did not get the focus, posting an alert");
                crate::notify::compose_ready(attached as u32);
            }
        });
    }

    /// Files handed in from outside (Send with Hylki, Open With, Email…):
    /// the first step. Reports the unreadable ones, then goes where
    /// Settings → System → GNOME Files says: straight into a new message, a
    /// draft or a reply, or a dialog offering the three.
    pub(super) fn begin_hand_off(&mut self, hand_off: FileHandOff, sender: &ComponentSender<Self>) {
        self.leave_gallery();
        let dropped = hand_off.dropped.clone();
        let attached = hand_off.files.len();
        // The focus watch runs from here: whichever window opens next (the
        // dialog or the composer) is the one that should come to the front.
        self.after_hand_off(attached, dropped);
        let action = self.files_prefs.action;
        if action == config::FilesAction::Ask {
            self.ask_hand_off_action(hand_off, sender);
        } else {
            self.hand_off_action(hand_off, action, sender);
        }
    }

    /// Hand the current Files preferences to an open Settings window, so a
    /// "remember this" choice in a dialog shows there at once.
    pub(super) fn push_files_prefs(&self) {
        if let Some(p) = self.prefs.as_ref() {
            p.emit(PrefInput::SetFilesPrefs(self.files_prefs));
        }
    }

    /// The "what should these files go into" dialog.
    fn ask_hand_off_action(&self, hand_off: FileHandOff, sender: &ComponentSender<Self>) {
        let n = hand_off.files.len() as u32;
        let names = hand_off_names(&hand_off.files);
        let size = crate::models::human_size(hand_off_size(&hand_off.files));
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(
                ni18n_f(
                    "Send {n} file with Hylki",
                    "Send {n} files with Hylki",
                    n,
                    &[("n", &n.to_string())],
                )
                .as_str(),
            ),
            Some(
                i18n_f(
                    "{names} ({size}). What should the files go into?",
                    &[("names", &names), ("size", &size)],
                )
                .as_str(),
            ),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("reply", &i18n("Reply to a Message…"));
        dialog.add_response("draft", &i18n("Continue a Draft…"));
        dialog.add_response("new", &i18n("New Message"));
        dialog.set_response_appearance("new", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("new"));
        dialog.set_close_response("cancel");
        let remember = remember_check();
        dialog.set_extra_child(Some(&remember));
        let hand_off = std::rc::Rc::new(std::cell::RefCell::new(Some(hand_off)));
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            let Some(hand_off) = hand_off.borrow_mut().take() else {
                return;
            };
            let action = match resp {
                "new" => config::FilesAction::New,
                "draft" => config::FilesAction::Draft,
                "reply" => config::FilesAction::Reply,
                _ => {
                    tracing::info!("file hand-off: cancelled");
                    return;
                }
            };
            s.input(AppMsg::HandOffAction {
                hand_off,
                action,
                remember: remember.is_active(),
            });
        });
        dialog.present();
    }

    /// A destination is known: a new message goes on to the size check;
    /// a draft or reply first needs one picked.
    pub(super) fn hand_off_action(
        &mut self,
        hand_off: FileHandOff,
        action: config::FilesAction,
        sender: &ComponentSender<Self>,
    ) {
        match action {
            config::FilesAction::Ask | config::FilesAction::New => {
                self.hand_off_size_check(hand_off, HandOffTarget::New, sender);
            }
            config::FilesAction::Draft => {
                // Drafts folders never listed this run are fetched first,
                // so the picker does not miss a draft the cache has not
                // seen; a worker that stays silent is given three seconds.
                let mut waiting = HashSet::new();
                for (account_id, drafts) in self.drafts_folders() {
                    if !self.message_cache.contains_key(&(account_id, drafts.id)) {
                        waiting.insert((account_id, drafts.id));
                        self.send_to(
                            account_id,
                            MailRequest::SyncFolder {
                                folder_id: drafts.id,
                                path: drafts.path.clone(),
                            },
                        );
                    }
                }
                if waiting.is_empty() {
                    self.show_draft_picker(hand_off, sender);
                } else {
                    self.pending_draft_pick = Some((hand_off, waiting));
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(3, move || {
                        s.input(AppMsg::HandOffDraftPickNow)
                    });
                }
            }
            config::FilesAction::Reply => {
                let messages = self.messages_for_pick();
                let s = sender.clone();
                show_message_picker(
                    self.window.upcast_ref(),
                    &i18n("Reply to a Message"),
                    &i18n("The files go into a reply to the message you pick."),
                    &i18n("No messages to reply to yet."),
                    &i18n("Reply With the Files"),
                    messages,
                    hand_off,
                    move |hand_off, message| s.input(AppMsg::HandOffReply { hand_off, message }),
                );
            }
        }
    }

    /// Over the size limit, with cloud storage set up: attach anyway, or
    /// upload and link, per the preference or a dialog.
    pub(super) fn hand_off_size_check(
        &mut self,
        hand_off: FileHandOff,
        target: HandOffTarget,
        sender: &ComponentSender<Self>,
    ) {
        let total = hand_off_size(&hand_off.files);
        let limit = self.files_prefs.limit_bytes();
        let cloud_ready = !crate::cloud::load_enabled_accounts().is_empty();
        if total <= limit || !cloud_ready {
            if total > limit {
                tracing::info!("file hand-off: {total} bytes over the {limit} limit, but no cloud storage is set up");
            }
            self.hand_off_open(hand_off, target, false, sender);
            return;
        }
        match self.files_prefs.large {
            config::FilesLarge::Attach => self.hand_off_open(hand_off, target, false, sender),
            config::FilesLarge::Cloud => self.hand_off_open(hand_off, target, true, sender),
            config::FilesLarge::Ask => {
                let n = hand_off.files.len() as u32;
                let names = hand_off_names(&hand_off.files);
                let dialog = adw::MessageDialog::new(
                    Some(&self.window),
                    Some(i18n("Large files").as_str()),
                    Some(
                        ni18n_f(
                            "{names} is {size}, over the {limit} limit. Upload it to cloud storage and put a download link in the message instead?",
                            "{names} add up to {size}, over the {limit} limit. Upload them to cloud storage and put download links in the message instead?",
                            n,
                            &[
                                ("names", &names),
                                ("size", &crate::models::human_size(total)),
                                ("limit", &crate::models::human_size(limit)),
                            ],
                        )
                        .as_str(),
                    ),
                );
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("attach", &i18n("Attach Anyway"));
                dialog.add_response("cloud", &i18n("Upload to Cloud Storage"));
                dialog.set_response_appearance("cloud", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("cloud"));
                dialog.set_close_response("cancel");
                let remember = remember_check();
                dialog.set_extra_child(Some(&remember));
                let pending = std::rc::Rc::new(std::cell::RefCell::new(Some((hand_off, target))));
                let s = sender.clone();
                dialog.connect_response(None, move |_, resp| {
                    let Some((hand_off, target)) = pending.borrow_mut().take() else {
                        return;
                    };
                    let cloud = match resp {
                        "cloud" => true,
                        "attach" => false,
                        _ => {
                            tracing::info!("file hand-off: cancelled at the size check");
                            return;
                        }
                    };
                    s.input(AppMsg::HandOffOpen {
                        hand_off,
                        target,
                        cloud,
                        remember: remember.is_active(),
                    });
                });
                dialog.present();
            }
        }
    }

    /// The last step: open the composer the files were meant for.
    pub(super) fn hand_off_open(
        &mut self,
        hand_off: FileHandOff,
        target: HandOffTarget,
        cloud: bool,
        sender: &ComponentSender<Self>,
    ) {
        let files = HandOffFiles::of(hand_off.files, cloud);
        tracing::info!(
            "file hand-off: {} to attach, {} to upload, into {}",
            files.attach.len(),
            files.cloud.len(),
            match &target {
                HandOffTarget::New => "a new message",
                HandOffTarget::Draft(_) => "a draft",
                HandOffTarget::Reply(_) => "a reply",
            }
        );
        match target {
            HandOffTarget::New => {
                let account = self
                    .current
                    .as_ref()
                    .map(|m| m.account_id)
                    .unwrap_or_else(|| self.active_account());
                let mut prefill = hand_off.base;
                prefill.attachments = files.attach;
                prefill.cloud_uploads = files.cloud;
                let (account, prefill) = self.new_message_from(account, prefill);
                if self.compose_inline {
                    self.open_inline_reply(account, prefill, None, sender);
                } else {
                    self.open_compose(account, prefill, sender);
                }
            }
            HandOffTarget::Draft(m) => {
                let inline = self.compose_inline;
                self.open_draft(m, inline, files, sender);
            }
            HandOffTarget::Reply(m) => {
                let body = if !m.body.is_empty() {
                    Some(m.body.clone())
                } else {
                    self.body_cache.get(&body_key(&m)).cloned()
                };
                match body {
                    Some(html) => {
                        let mut m = m;
                        m.body = html;
                        self.open_hand_off_reply(m, files, sender);
                    }
                    None => {
                        if let Some(path) = self.resolve_folder_path(&m) {
                            self.send_to(
                                m.account_id,
                                MailRequest::LoadBody {
                                    message_id: m.id,
                                    path,
                                    uid: m.uid,
                                },
                            );
                        }
                        self.pending_reply = Some((m, files));
                    }
                }
            }
        }
    }

    /// A reply to `m` (its body loaded) carrying handed-in files. Splits
    /// the reading pane when `m` is what it is showing, as Reply does; a
    /// message picked from elsewhere gets its reply in a window, the pane
    /// left as it was.
    pub(super) fn open_hand_off_reply(
        &mut self,
        m: Message,
        files: HandOffFiles,
        sender: &ComponentSender<Self>,
    ) {
        let mut prefill = self.reply_context(&m, reply_prefill(&m));
        prefill.attachments = files.attach;
        prefill.cloud_uploads = files.cloud;
        let on_screen = self
            .current
            .as_ref()
            .is_some_and(|c| c.account_id == m.account_id && c.id == m.id)
            || self
                .current_thread
                .iter()
                .any(|t| t.account_id == m.account_id && t.id == m.id);
        if on_screen {
            self.open_reply(m.account_id, prefill, (m.account_id, m.id), sender);
        } else {
            self.open_compose(m.account_id, prefill, sender);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_off_files_go_to_one_side() {
        let files = vec![std::path::PathBuf::from("/a"), "/b".into()];
        let attached = HandOffFiles::of(files.clone(), false);
        assert_eq!((attached.attach.len(), attached.cloud.len()), (2, 0));
        let linked = HandOffFiles::of(files, true);
        assert_eq!((linked.attach.len(), linked.cloud.len()), (0, 2));
    }
}
