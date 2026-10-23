//! Desktop notifications for new mail and what their buttons do.

use super::*;

/// Resolve a notification's `(account_id, folder_id, message_id)` to the full
/// cached [`Message`] (#38): the notified message may not be in the current
/// view, but the worker upserts it into the cache before notifying, so the
/// cache always has it.
pub(super) fn notified_message(
    account_id: u32,
    folder_id: u32,
    message_id: u32,
    folders: &std::collections::HashMap<u32, Vec<Folder>>,
) -> Option<Message> {
    let path = folders
        .get(&account_id)?
        .iter()
        .find(|f| f.id == folder_id)?
        .path
        .clone();
    let cache = crate::cache::Cache::open().ok()?;
    cache
        .load_messages(account_id, &path, folder_id)
        .into_iter()
        .find(|m| m.id == message_id)
}

impl AppModel {
    /// Tell the person about new Inbox mail: the notification, with its
    /// sound, while the window is in the background or closed; the sound
    /// alone while it is in front, when Settings asks for that (#337).
    /// GNOME shows no banner for the app in front, and neither does Hylki.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn announce_new_mail(
        &self,
        account_id: u32,
        folder_id: u32,
        message_id: u32,
        from: &str,
        subject: &str,
        others: usize,
        in_place: bool,
    ) {
        // HYLKI_SHOWCASE_IN_FRONT (demo) stands in for the focus, which a
        // window opened from a script never gets.
        let in_front = self.window.is_active()
            || (demo_mode() && std::env::var_os("HYLKI_SHOWCASE_IN_FRONT").is_some());
        if !in_front {
            crate::notify::new_mail(
                account_id, folder_id, message_id, from, subject, others, in_place,
            );
        } else if crate::config::sound_when_focused() {
            tracing::info!("new mail with the window in front: the sound alone");
            crate::notify::sound_for_new_mail();
        } else {
            tracing::debug!("new mail with the window in front: no notification, no sound");
        }
    }

    pub(super) fn show_toast(&self, toast: &adw::Toast) {
        if let Some(t) = self.toasts.get() {
            t.add_toast(toast.clone());
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_notifications(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Notice(text) => {
                self.notifications.emit(NotifyInput::Push {
                    text,
                    error: false,
                    connectivity: false,
                });
            }

            AppMsg::OpenMessageFromNotification {
                account_id,
                folder_id,
                message_id,
            } => {
                // The user clicked a new-mail notification: they've engaged with
                // that account's mail, so clear its toast, then navigate to the
                // message's folder and open it in the reader.
                crate::notify::withdraw_mail(account_id);
                tracing::info!(
                    "notification: open account {account_id} folder {folder_id} message {message_id} (unified row: {})",
                    self.unified_inboxes_shown()
                );
                if let Some((name, path, kind)) = self
                    .folders
                    .get(&account_id)
                    .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
                    .map(|f| (f.name.clone(), f.path.clone(), f.kind))
                {
                    // Ask for the body before anything else goes to the
                    // account's worker. Opening Inboxes below asks every
                    // account for its inbox list, and the worker serves one
                    // request at a time: the body request the selection
                    // sends afterwards would wait behind that whole fetch
                    // (and the IDLE hand-off before it), so the message the
                    // user clicked sat on a spinner while the list loaded.
                    // Sent first, it comes back into the body cache, which
                    // the selection reads before fetching. A body already
                    // there (prefetched on arrival) needs nothing.
                    let uid = self
                        .message_cache
                        .get(&(account_id, folder_id))
                        .and_then(|msgs| msgs.iter().find(|m| m.id == message_id))
                        .map(|m| m.uid);
                    if let Some(uid) = uid {
                        if !self.body_cache.contains_key(&(account_id, folder_id, uid)) {
                            self.send_to(
                                account_id,
                                MailRequest::LoadBody {
                                    message_id,
                                    path: path.clone(),
                                    uid,
                                },
                            );
                        }
                    }
                    // Mail that landed in an inbox opens in the unified
                    // Inboxes when the sidebar has that row (the view the
                    // app opens with; the account's own Inbox row may sit
                    // inside a folded section, where a highlight has nowhere
                    // to show). Without the row — one account, or the
                    // preference off — or when a filter filed the message
                    // elsewhere, its folder opens directly and the sidebar
                    // unfolds the account to show where that is. Both emit
                    // the (cached) list synchronously, so the SelectAndLoad
                    // that follows finds the row and opens it.
                    if kind == FolderKind::Inbox
                        && self.unified_inboxes_shown()
                        && self.in_unified(account_id)
                    {
                        self.open_unified(UnifiedView::Kind(FolderKind::Inbox), &sender);
                    } else {
                        self.select_folder(account_id, folder_id, name, path);
                    }
                    self.message_list
                        .emit(MessageListInput::SelectAndLoad((account_id, message_id)));
                }
            }

            AppMsg::NotificationMarkRead {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    // set_read clears the account's notification itself.
                    self.user_set_flags(read_label(true), &[m], FlagKind::Read, true);
                }
            }

            AppMsg::NotificationArchive {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.move_to(m, FolderKind::Archive);
                    crate::notify::withdraw_mail(account_id);
                }
            }

            AppMsg::NotificationDelete {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    // The same path as the Delete key: to Trash, with the
                    // usual undo; a message already there asks first.
                    self.delete_messages(vec![m], &sender);
                    crate::notify::withdraw_mail(account_id);
                }
            }

            AppMsg::NotificationSpam {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.mark_spam_msg(m);
                    crate::notify::withdraw_mail(account_id);
                }
            }

            // The message is opening in the reader (the action sent
            // OpenMessageFromNotification first); the composer waits for
            // the body it quotes when that is still on its way.
            AppMsg::NotificationReply {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    let m = self.with_cached_body(m);
                    if m.body.is_empty() {
                        self.pending_notified_reply = Some(m);
                    } else {
                        self.open_reply(
                            m.account_id,
                            self.reply_context(&m, reply_prefill(&m)),
                            (m.account_id, m.id),
                            &sender,
                        );
                    }
                }
            }

            AppMsg::NotificationForward {
                account_id,
                folder_id,
                message_id,
            } => {
                if let Some(m) = notified_message(account_id, folder_id, message_id, &self.folders)
                {
                    self.forward(m, true, &sender);
                }
            }

            AppMsg::Status { account_id, text } => {
                if text.is_empty() {
                    self.busy.remove(&account_id);
                } else {
                    self.busy.insert(account_id);
                }
                self.update_busy_indicator();
                // A sync going quiet must not blank the status while background
                // bulk operations are still reporting there.
                if !text.is_empty() || self.bulk_pending == 0 {
                    self.notifications.emit(NotifyInput::SetStatus(text));
                }
            }

            AppMsg::Error {
                account_id,
                text,
                connectivity,
            } => {
                // The log gets the whole error (a parser dump included, which
                // is what diagnoses #226-style replies); the bar gets it
                // readable.
                tracing::error!("[account {account_id}] {text}");
                // A connection that died with the network: the Refresh
                // button already says it is down (#442).
                if connectivity && !crate::power::reaches_internet() {
                    return;
                }
                let text = crate::worker::readable_error(&text);
                let label = self.account_label(account_id);
                // Desktop-notify only genuine failures (not transient connectivity
                // blips that auto-recover), and only when unfocused — the in-app bar
                // already surfaces it while you're looking.
                if self.notifications_enabled && !connectivity && !self.window.is_active() {
                    crate::notify::error(account_id, &format!("{label}: mail error"), &text);
                }
                self.notifications.emit(NotifyInput::Push {
                    text: format!("{label}: {text}"),
                    error: true,
                    connectivity,
                });
            }

            AppMsg::DirectoryError(text) => {
                self.notifications.emit(NotifyInput::Push {
                    text,
                    error: true,
                    connectivity: false,
                });
            }

            AppMsg::NotifyCount(n) => self.notify_count = n,
            AppMsg::ToggleNotifications => self.notifications.emit(NotifyInput::ToggleBar),
            AppMsg::StatusBarShown(shown) => {
                self.status_bar_shown = shown;
                self.rebuild_help_menu();
            }
            _ => unreachable!("not routed to on_notifications"),
        }
    }
}
