//! Sending: the undo-send delay, handing a message to its worker, and the
//! answered marks shown before the server has them.

use super::*;

impl AppModel {
    /// Send's first stop (#434): the message waits out the Undo Send delay
    /// with Undo on offer in a toast, listed in the Outbox, then goes to its
    /// worker.
    pub(super) fn queue_send(
        &mut self,
        origin: crate::outgoing::Origin,
        out: Box<OutgoingMessage>,
        sent_path: Option<String>,
        sender: &ComponentSender<Self>,
    ) {
        let account_id = out.from_account_id;
        let Some(account) = account_id
            .checked_sub(1)
            .and_then(|i| self.effective_config().get(i as usize).cloned())
        else {
            self.send_to(
                account_id,
                MailRequest::Send {
                    message: out,
                    sent_path,
                    ticket: 0,
                },
            );
            return;
        };
        self.send_ticket += 1;
        let ticket = self.send_ticket;
        self.mark_answered_ahead(ticket, &out);
        let delay = self.undo_send_secs;
        let toast = adw::Toast::builder()
            .priority(adw::ToastPriority::High)
            .timeout(0)
            .build();
        let o = crate::outgoing::Outgoing {
            ticket,
            account_id,
            account,
            message: out,
            sent_path,
            queued_at: crate::datefmt::now(),
            origin,
            held: delay > 0,
            toast: toast.clone(),
        };
        toast.set_title(&o.sending_title());
        if delay > 0 {
            toast.set_button_label(Some(&i18n("Undo")));
            let s = sender.clone();
            toast.connect_button_clicked(move |_| s.input(AppMsg::UndoSend(ticket)));
            // The timer is never cancelled: Undo or Send now take the
            // message off the list, and this finds nothing to do.
            let s = sender.clone();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_secs(delay.into()),
                move || s.input(AppMsg::DispatchSend(ticket)),
            );
            self.show_toast(&toast);
        } else {
            // With nothing to undo, there is no toast.
            self.send_to(
                account_id,
                MailRequest::Send {
                    message: o.message.clone(),
                    sent_path: o.sent_path.clone(),
                    ticket,
                },
            );
        }
        self.outgoing.borrow_mut().push(o);
        self.push_outbox();
    }

    /// Mark what a reply or forward answers as soon as it is sent, rather
    /// than once it has gone (#454): the messages the worker will mark, as
    /// the cache knows them. One already marked is left out, so taking the
    /// marks off again cannot clear what another send or client set.
    fn mark_answered_ahead(&mut self, ticket: u64, out: &OutgoingMessage) {
        let account_id = out.from_account_id;
        let copies = crate::worker::answered_copies(
            self.cache.as_ref(),
            account_id,
            &out.in_reply_to,
            &out.forwards,
        );
        let mut marked = Vec::new();
        for (path, uid, forwarded) in copies {
            let Some(fid) = self
                .folders
                .get(&account_id)
                .and_then(|fs| fs.iter().find(|f| f.path == path))
                .map(|f| f.id)
            else {
                continue;
            };
            let (answered, was_forwarded) = self
                .cache
                .as_ref()
                .map(|c| c.answer_state(account_id, &path, uid))
                .unwrap_or_default();
            if if forwarded { was_forwarded } else { answered } {
                continue;
            }
            self.show_answered(account_id, fid, uid, forwarded, true);
            marked.push((account_id, fid, uid, forwarded));
        }
        if !marked.is_empty() {
            self.pending_answers.insert(ticket, marked);
        }
    }

    /// The send was undone, or failed: its early marks come off.
    pub(super) fn unmark_answered(&mut self, ticket: u64) {
        for (account_id, fid, uid, forwarded) in
            self.pending_answers.remove(&ticket).unwrap_or_default()
        {
            self.show_answered(account_id, fid, uid, forwarded, false);
        }
    }

    /// Put a replied or forwarded mark on a message, or take it off,
    /// wherever the app holds it.
    pub(super) fn show_answered(
        &mut self,
        account_id: u32,
        fid: u32,
        uid: u32,
        forwarded: bool,
        on: bool,
    ) {
        let same = |c: &Message| c.account_id == account_id && c.folder_id == fid && c.uid == uid;
        let mark = |c: &mut Message| {
            if forwarded {
                c.forwarded = on
            } else {
                c.answered = on
            }
        };
        let mut id = None;
        for msgs in [
            self.message_cache
                .get_mut(&(account_id, fid))
                .map(|c| &mut **c),
            self.unified_slices.get_mut(&(account_id, fid)),
        ]
        .into_iter()
        .flatten()
        {
            for c in msgs.iter_mut().filter(|c| same(c)) {
                mark(c);
                id = Some(c.id);
            }
        }
        self.current_thread
            .iter_mut()
            .filter(|c| same(c))
            .for_each(mark);
        if let Some(cur) = self.current.as_mut().filter(|c| same(c)) {
            mark(cur);
        }
        if let Some(id) = id {
            self.message_list.emit(MessageListInput::SetAnswered {
                slot: (account_id, fid, id),
                forwarded,
                on,
            });
        }
    }

    /// [`apply_pending_seen`] for the marks of a send still on its way: a
    /// list fetched before it went keeps them.
    pub(super) fn apply_pending_answers(
        &self,
        account_id: u32,
        folder_id: u32,
        mut messages: Vec<Message>,
    ) -> Vec<Message> {
        for &(a, f, uid, forwarded) in self.pending_answers.values().flatten() {
            if (a, f) != (account_id, folder_id) {
                continue;
            }
            for m in messages.iter_mut().filter(|m| m.uid == uid) {
                if forwarded {
                    m.forwarded = true;
                } else {
                    m.answered = true;
                }
            }
        }
        messages
    }

    /// The delay is over, or Send now was asked for: hand the message to its
    /// worker. Undo is no longer possible.
    pub(super) fn dispatch_send(&mut self, ticket: u64) {
        let request = {
            let mut list = self.outgoing.borrow_mut();
            let Some(o) = list.iter_mut().find(|o| o.ticket == ticket && o.held) else {
                return;
            };
            o.held = false;
            o.toast.dismiss();
            (o.account_id, o.message.clone(), o.sent_path.clone())
        };
        let (account_id, message, sent_path) = request;
        self.send_to(
            account_id,
            MailRequest::Send {
                message,
                sent_path,
                ticket,
            },
        );
        self.push_outbox();
    }

    /// Undo Send: the message comes back to the composer it was written in,
    /// as it was. A split reply reopens beside its card if that card is
    /// still on screen; otherwise it gets a window.
    pub(super) fn undo_send(&mut self, ticket: u64, sender: &ComponentSender<Self>) {
        let taken = {
            let mut list = self.outgoing.borrow_mut();
            list.iter()
                .position(|o| o.ticket == ticket && o.held)
                .map(|i| list.remove(i))
        };
        let Some(o) = taken else { return };
        o.toast.dismiss();
        self.unmark_answered(ticket);
        self.push_outbox();
        self.send_morph.finish();
        self.drop_provisional(&o.message.in_reply_to);
        if self.showing_outbox
            && self
                .current
                .as_ref()
                .is_some_and(|m| m.id == crate::outgoing::row_id(ticket))
        {
            self.current = None;
            self.show_message(None, false);
        }
        let account_id = o.account_id;
        let prefill = unsent_prefill(*o.message);
        let free = self.reader_compose.is_none() && !self.showing_contacts && !self.showing_outbox;
        match o.origin {
            crate::outgoing::Origin::Split(answering)
                if free
                    && self
                        .current
                        .iter()
                        .chain(self.current_thread.iter())
                        .any(|m| (m.account_id, m.id) == answering) =>
            {
                self.open_inline_reply(account_id, prefill, Some(answering), sender);
            }
            crate::outgoing::Origin::Pane if free => {
                self.open_inline_reply(account_id, prefill, None, sender)
            }
            _ => self.open_compose(account_id, prefill, sender),
        }
    }
}
