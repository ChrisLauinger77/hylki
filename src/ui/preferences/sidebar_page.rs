//! Settings → Sidebar: the unified section, the placed sections, the icon
//! rail's fold-ups and Focus Mode.

use super::choices::{section_style_at, start_view_at};
use super::*;

impl Preferences {
    pub(super) fn on_toggle_show_unified(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.show_unified = on;
        let _ = sender.output(PrefOutput::SetShowUnified(on));
    }

    pub(super) fn on_toggle_unified_chip_all_inboxes(
        &mut self,
        on: bool,
        sender: ComponentSender<Self>,
    ) {
        self.unified_chips.all_inboxes = on;
        let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
    }

    pub(super) fn on_toggle_unified_chip_starred(
        &mut self,
        on: bool,
        sender: ComponentSender<Self>,
    ) {
        self.unified_chips.starred = on;
        let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
    }

    pub(super) fn on_toggle_unified_chip_drafts(
        &mut self,
        on: bool,
        sender: ComponentSender<Self>,
    ) {
        self.unified_chips.drafts = on;
        let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
    }

    pub(super) fn on_toggle_unified_chip_archive(
        &mut self,
        on: bool,
        sender: ComponentSender<Self>,
    ) {
        self.unified_chips.archive = on;
        let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
    }

    pub(super) fn on_toggle_unified_chip_junk(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.unified_chips.junk = on;
        let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
    }

    pub(super) fn on_toggle_unified_chip_trash(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.unified_chips.trash = on;
        let _ = sender.output(PrefOutput::SetUnifiedChips(self.unified_chips));
    }

    pub(super) fn on_toggle_unified_starred(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.unified_kinds.starred = on;
        let _ = sender.output(PrefOutput::SetUnifiedKinds(self.unified_kinds));
    }

    pub(super) fn on_toggle_unified_sent(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.unified_kinds.sent = on;
        let _ = sender.output(PrefOutput::SetUnifiedKinds(self.unified_kinds));
    }

    pub(super) fn on_toggle_unified_drafts(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.unified_kinds.drafts = on;
        let _ = sender.output(PrefOutput::SetUnifiedKinds(self.unified_kinds));
    }

    pub(super) fn on_toggle_unified_archive(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.unified_kinds.archive = on;
        let _ = sender.output(PrefOutput::SetUnifiedKinds(self.unified_kinds));
    }

    pub(super) fn on_toggle_unified_junk(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.unified_kinds.junk = on;
        let _ = sender.output(PrefOutput::SetUnifiedKinds(self.unified_kinds));
    }

    pub(super) fn on_toggle_unified_trash(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.unified_kinds.trash = on;
        let _ = sender.output(PrefOutput::SetUnifiedKinds(self.unified_kinds));
    }

    pub(super) fn on_toggle_show_accounts(&mut self, on: bool, sender: ComponentSender<Self>) {
        if self.show_accounts != on {
            self.show_accounts = on;
            let _ = sender.output(PrefOutput::SetShowAccounts(on));
        }
    }

    pub(super) fn on_toggle_focus(
        &mut self,
        part: crate::config::FocusPart,
        on: bool,
        sender: ComponentSender<Self>,
    ) {
        if self.focus.get(part) != on {
            self.focus.set(part, on);
            let _ = sender.output(PrefOutput::SetFocusMode(self.focus));
        }
    }

    pub(super) fn on_change_section_style(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let style = section_style_at(idx);
        let _ = sender.output(PrefOutput::SetSectionStyle(style));
    }

    pub(super) fn on_change_contacts_placement(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let placement = contacts_placement_from_index(idx);
        if let Some(c) = &self.contacts {
            c.emit(crate::ui::contact_sources::ContactSourcesInput::SetPlacement(placement));
        }
        let _ = sender.output(PrefOutput::SetContactsPlacement(placement));
    }

    pub(super) fn on_change_start_view(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let view = start_view_at(idx);
        let _ = sender.output(PrefOutput::SetStartView(view));
    }

    pub(super) fn on_change_folder_sort(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let sort = crate::config::FolderSort::ALL
            .get(idx as usize)
            .copied()
            .unwrap_or_default();
        let _ = sender.output(PrefOutput::SetFolderSort(sort));
    }

    pub(super) fn on_toggle_rail_fold_enabled(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.rail_fold.enabled = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }

    pub(super) fn on_toggle_rail_fold_accounts(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.rail_fold.accounts = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }

    pub(super) fn on_toggle_rail_fold_all_inboxes(
        &mut self,
        on: bool,
        sender: ComponentSender<Self>,
    ) {
        self.rail_fold.all_inboxes = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }

    pub(super) fn on_toggle_rail_fold_starred(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.rail_fold.starred = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }

    pub(super) fn on_toggle_rail_fold_sent(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.rail_fold.sent = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }

    pub(super) fn on_toggle_rail_fold_drafts(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.rail_fold.drafts = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }

    pub(super) fn on_toggle_rail_fold_archive(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.rail_fold.archive = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }

    pub(super) fn on_toggle_rail_fold_junk(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.rail_fold.junk = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }

    pub(super) fn on_toggle_rail_fold_trash(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.rail_fold.trash = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }

    pub(super) fn on_toggle_rail_fold_filtered(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.rail_fold.filtered = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }

    pub(super) fn on_toggle_rail_fold_tags(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.rail_fold.tags = on;
        let _ = sender.output(PrefOutput::SetRailFold(self.rail_fold));
    }
}
