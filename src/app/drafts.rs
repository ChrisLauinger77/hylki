//! Drafts and templates: their folders, and the pickers that choose one.

use super::*;

impl AppModel {
    /// New Message from Template (#360): every template the app knows, to
    /// start a message from.
    pub(super) fn show_template_picker(&self, sender: &ComponentSender<Self>) {
        let mut templates = Vec::new();
        for (account_id, folder) in self.templates_folders() {
            let list = match self.message_cache.get(&(account_id, folder.id)) {
                Some(l) => l.to_vec(),
                None => self
                    .cache
                    .as_ref()
                    .map(|c| c.load_messages(account_id, &folder.path, folder.id))
                    .unwrap_or_default(),
            };
            let name = self.account_name(account_id);
            templates.extend(list.into_iter().map(|m| (m, name.clone())));
        }
        templates.sort_by_key(|(m, _)| m.subject.to_lowercase());
        let s = sender.clone();
        show_message_picker(
            self.window.upcast_ref(),
            &i18n("New Message from Template"),
            &i18n("The new message starts as a copy of the template you pick."),
            &i18n("No templates yet. Save one from the composer's ⋯ menu with Save as Template."),
            &i18n("Use Template"),
            templates,
            (),
            move |(), template| s.input(AppMsg::TemplatePicked(template)),
        );
    }

    /// Each account's Templates folder (#360), in account order.
    pub(super) fn templates_folders(&self) -> Vec<(u32, Folder)> {
        let mut ids: Vec<u32> = self.folders.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| Some((id, self.folder_of_kind(id, FolderKind::Templates)?.clone())))
            .collect()
    }

    /// The "Continue a Draft" picker over every draft the app knows.
    pub(super) fn show_draft_picker(&self, hand_off: FileHandOff, sender: &ComponentSender<Self>) {
        let drafts = self.drafts_for_pick();
        let s = sender.clone();
        show_message_picker(
            self.window.upcast_ref(),
            &i18n("Continue a Draft"),
            &i18n("The files go into the draft you pick."),
            &i18n("No drafts to continue."),
            &i18n("Attach to This Draft"),
            drafts,
            hand_off,
            move |hand_off, draft| s.input(AppMsg::HandOffDraft { hand_off, draft }),
        );
    }

    /// Tell the reader which folders hold templates (#360), so a template's
    /// card wears its banner.
    pub(super) fn push_template_folders(&self) {
        let folders = self
            .folders
            .iter()
            .flat_map(|(account_id, fs)| {
                fs.iter()
                    .filter(|f| f.kind == FolderKind::Templates)
                    .map(move |f| (*account_id, f.id))
            })
            .collect();
        self.message_view
            .emit(MessageViewInput::SetTemplateFolders(folders));
    }

    /// Each account's Drafts folder, in account order.
    pub(super) fn drafts_folders(&self) -> Vec<(u32, Folder)> {
        let mut ids: Vec<u32> = self.folders.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| {
                let f = self
                    .folders
                    .get(&id)?
                    .iter()
                    .find(|f| f.kind == FolderKind::Drafts)?;
                Some((id, f.clone()))
            })
            .collect()
    }

    /// Every account's saved drafts, newest first, each with its account's
    /// name for the picker.
    fn drafts_for_pick(&self) -> Vec<(Message, String)> {
        let mut out = Vec::new();
        for (account_id, drafts) in self.drafts_folders() {
            let list = match self.message_cache.get(&(account_id, drafts.id)) {
                Some(l) => l.to_vec(),
                None => self
                    .cache
                    .as_ref()
                    .map(|c| c.load_messages(account_id, &drafts.path, drafts.id))
                    .unwrap_or_default(),
            };
            let name = self.account_name(account_id);
            out.extend(list.into_iter().map(|m| (m, name.clone())));
        }
        out.sort_by_key(|(m, _)| std::cmp::Reverse(m.timestamp));
        out
    }

    /// Messages a reply could answer: what the reading pane shows first,
    /// then every account's Inbox (and the other folders listed this
    /// session, Drafts, Templates, Sent, Junk and Trash left out), newest
    /// first.
    pub(super) fn messages_for_pick(&self) -> Vec<(Message, String)> {
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        let mut out = Vec::new();
        for m in self.current.iter().chain(self.current_thread.iter()) {
            if seen.insert((m.account_id, m.id)) {
                out.push((m.clone(), self.account_name(m.account_id)));
            }
        }
        let mut rest = Vec::new();
        let mut ids: Vec<u32> = self.folders.keys().copied().collect();
        ids.sort_unstable();
        for account_id in ids {
            let Some(folders) = self.folders.get(&account_id) else {
                continue;
            };
            let name = self.account_name(account_id);
            for f in folders {
                if matches!(
                    f.kind,
                    FolderKind::Drafts
                        | FolderKind::Templates
                        | FolderKind::Sent
                        | FolderKind::Junk
                        | FolderKind::Trash
                ) {
                    continue;
                }
                let list = match self.message_cache.get(&(account_id, f.id)) {
                    Some(l) => l.to_vec(),
                    None if f.kind == FolderKind::Inbox => self
                        .cache
                        .as_ref()
                        .map(|c| c.load_messages(account_id, &f.path, f.id))
                        .unwrap_or_default(),
                    None => Vec::new(),
                };
                for m in list {
                    if seen.insert((m.account_id, m.id)) {
                        rest.push((m, name.clone()));
                    }
                }
            }
        }
        rest.sort_by_key(|(m, _)| std::cmp::Reverse(m.timestamp));
        rest.truncate(500);
        out.extend(rest);
        out
    }
}
