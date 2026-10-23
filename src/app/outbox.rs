//! The Outbox: queued and scheduled mail, shown as a folder of its own.

use super::*;

impl AppModel {
    /// Push the merged queue to the Outbox view and the sidebar's badge. Oldest
    /// first, which is the order the workers send in.
    pub(super) fn push_outbox(&self) {
        let items = self.outbox_items();
        self.sidebars_emit(SidebarInput::SetOutboxCount(items.len() as u32));
        if self.showing_outbox {
            self.message_list.emit(MessageListInput::SetMessages {
                messages: items.iter().map(|i| i.as_message()).collect(),
            });
        }
    }

    /// Every account's queue merged, oldest first — the order they will send in.
    fn outbox_items(&self) -> Vec<crate::models::OutboxItem> {
        let mut items: Vec<crate::models::OutboxItem> = self
            .outbox_by_account
            .values()
            .flatten()
            .cloned()
            .chain(self.outgoing.borrow().iter().map(|o| o.as_outbox_item()))
            .collect();
        items.sort_by_key(|i| (i.queued_at, i.id));
        items
    }

    /// The queued message behind a list row, if the Outbox is what's shown.
    pub(super) fn outbox_item(
        &self,
        account_id: u32,
        id: u32,
    ) -> Option<crate::models::OutboxItem> {
        if !self.showing_outbox {
            return None;
        }
        if let Some(o) = self
            .outgoing
            .borrow()
            .iter()
            .find(|o| crate::outgoing::row_id(o.ticket) == id)
        {
            return Some(o.as_outbox_item());
        }
        self.outbox_by_account
            .get(&account_id)?
            .iter()
            .find(|i| i.id == id)
            .cloned()
    }

    /// The ticket of the message on its way out behind an Outbox row, while
    /// Undo can still take it back.
    pub(super) fn outgoing_ticket(&self, row: u32) -> Option<u64> {
        self.outgoing
            .borrow()
            .iter()
            .find(|o| o.held && crate::outgoing::row_id(o.ticket) == row)
            .map(|o| o.ticket)
    }

    /// Delete on an Outbox row. A message still waiting out its delay goes
    /// back to its composer instead, where discarding it is a choice made
    /// with the message in view; one already with its worker is past
    /// stopping.
    pub(super) fn drop_outbox_row(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        if self
            .outgoing
            .borrow()
            .iter()
            .any(|o| crate::outgoing::row_id(o.ticket) == id)
        {
            if let Some(ticket) = self.outgoing_ticket(id) {
                self.undo_send(ticket, sender);
            }
            return;
        }
        self.send_to(account_id, MailRequest::DeleteOutbox { id });
    }

    /// Render a queued message in the reader. Its bytes are stored with it, so
    /// this is a local render — the same one the worker would produce.
    pub(super) fn show_outbox_message(&mut self, item: &crate::models::OutboxItem) {
        let mut message = item.as_message();
        // A message on its way out has no bytes yet (#434): it shows as
        // its card in Sent will.
        let outgoing = self
            .outgoing
            .borrow()
            .iter()
            .find(|o| crate::outgoing::row_id(o.ticket) == item.id)
            .map(|o| {
                let body = crate::worker::sent_body_preview(&o.account, &o.message)
                    .unwrap_or_else(|| crate::worker::wrap_plain(&o.message.body));
                (body, o.message.attachments.is_empty())
            });
        if let Some((body, no_files)) = outgoing {
            message.body = crate::legacy::current_markup(body);
            message.date = item.status.clone();
            message.has_attachment = !no_files;
            self.attachments.clear();
            self.attachments_loading = false;
            self.sync_attachment_drawer();
            self.current = Some(message.clone());
            self.current_thread.clear();
            self.show_message(Some(message), false);
            return;
        }
        message.body = crate::legacy::current_markup(crate::worker::extract_body(&item.raw));
        message.date = crate::models::OutboxItem::waiting_label(item.queued_at);
        self.attachments = crate::worker::extract_attachments_of(&item.raw);
        self.attachments_loading = false;
        self.sync_attachment_drawer();
        self.current = Some(message.clone());
        self.current_thread.clear();
        self.show_message(Some(message), false);
    }

    /// Open a queued Outbox message in the composer.
    ///
    /// The queued copy stays put until the edited version is handed back to the
    /// worker: an editor that is closed again must not have destroyed the only
    /// copy of the message. Attachments are written to a private temp directory,
    /// because the composer attaches files by path and the originals are long
    /// gone (under Flatpak the portal's paths expire).
    pub(super) fn compose_from_outbox(
        &mut self,
        account_id: u32,
        id: u32,
        sender: &ComponentSender<Self>,
    ) {
        let Some(item) = self
            .outbox_by_account
            .get(&account_id)
            .and_then(|items| items.iter().find(|i| i.id == id))
            .cloned()
        else {
            return;
        };
        let editable = crate::worker::editable_from_raw(&item.raw, &item.rcpts);

        let attachments = stage_attachments(
            &format!("hylki-outbox-{account_id}-{id}"),
            &editable.attachments,
        );

        let prefill = ComposePrefill {
            to: editable.to,
            cc: editable.cc,
            bcc: editable.bcc,
            subject: editable.subject,
            body_html: editable.body_html,
            attachments,
            // Re-editing a queued message: it keeps whatever thread it was
            // already part of (carried in the raw MIME it was built from).
            in_reply_to: String::new(),
            references: String::new(),
            forwards: item.forwards.clone(),
            draft_origin: None,
            encrypt: false,
            outbox_origin: Some(id),
            reply_addressed_to: String::new(),
            from_address: crate::worker::parse_recipients(&editable.from)
                .into_iter()
                .next()
                .map(|(_, addr)| addr)
                .unwrap_or_default(),
            custom_from: editable.from,
            send_at: item.send_at,
            cloud_uploads: Vec::new(),
            inline_files: Vec::new(),
            block_remote_images: false,
            resumed: false,
            no_signature: false,
            template: false,
            original_plain: false,
        };
        // The Outbox stays the folder on screen: its list is still what's listed,
        // so its toolbar has to stay too. Leaving it would strand the user in a
        // reader offering Reply and Forward for a message that hasn't been sent.
        self.open_compose(account_id, prefill, sender);
    }

    /// Open a message the worker could not send, queue or save (#340). Its
    /// composer closed when Send was pressed, so without this the message is
    /// gone.
    pub(super) fn compose_unsent(
        &mut self,
        account_id: u32,
        m: crate::worker::OutgoingMessage,
        sender: &ComponentSender<Self>,
    ) {
        self.open_compose(account_id, unsent_prefill(m), sender);
    }
}
