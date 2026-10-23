//! Settings → Composing.

use super::*;

impl Preferences {
    pub(super) fn on_change_compose_default_from(
        &mut self,
        index: u32,
        sender: ComponentSender<Self>,
    ) {
        let addr = (index > 0)
            .then(|| self.identities.get(index as usize - 1))
            .flatten()
            .map(|(_, addr)| addr.clone())
            .unwrap_or_default();
        let _ = sender.output(PrefOutput::SetComposeDefaultFrom(addr));
    }

    pub(super) fn on_change_compose_format(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let format = match idx {
            1 => crate::config::ComposeFormat::Markdown,
            2 => crate::config::ComposeFormat::Html,
            3 => crate::config::ComposeFormat::Plain,
            _ => crate::config::ComposeFormat::Rich,
        };
        let _ = sender.output(PrefOutput::SetComposeFormat(format));
    }

    pub(super) fn on_change_reply_position(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let position = match idx {
            1 => crate::config::ReplyPosition::Bottom,
            2 => crate::config::ReplyPosition::Follow,
            _ => crate::config::ReplyPosition::Top,
        };
        let _ = sender.output(PrefOutput::SetReplyPosition(position));
    }

    pub(super) fn on_change_undo_send(&mut self, idx: u32, sender: ComponentSender<Self>) {
        if let Some(&secs) = crate::config::UNDO_SEND_CHOICES.get(idx as usize) {
            let _ = sender.output(PrefOutput::SetUndoSend(secs));
        }
    }

    pub(super) fn on_change_signature_position(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let position = match idx {
            1 => crate::config::SignaturePosition::BelowQuote,
            _ => crate::config::SignaturePosition::AboveQuote,
        };
        let _ = sender.output(PrefOutput::SetSignaturePosition(position));
    }
}
