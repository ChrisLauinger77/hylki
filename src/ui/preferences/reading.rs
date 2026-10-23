//! Settings → Message List, Conversations and Reading, and Printing.

use super::choices::{fold_messages_at, list_layout_at, read_mark_at, reader_default_at};
use super::*;

impl Preferences {
    pub(super) fn on_toggle_avatars(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.avatars = on;
        let _ = sender.output(PrefOutput::SetAvatars(on));
    }

    pub(super) fn on_toggle_threading(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.threading = on;
        let _ = sender.output(PrefOutput::SetThreading(on));
    }

    pub(super) fn on_toggle_thread_expansion(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.thread_expansion = on;
        let _ = sender.output(PrefOutput::SetThreadExpansion(on));
    }

    pub(super) fn on_change_reader_default(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let policy = reader_default_at(idx);
        let _ = sender.output(PrefOutput::SetReaderDefault(policy));
    }

    pub(super) fn on_change_reader_zoom(&mut self, idx: u32, sender: ComponentSender<Self>) {
        if let Some(&z) = crate::config::READER_ZOOM_STEPS.get(idx as usize) {
            let _ = sender.output(PrefOutput::SetReaderZoom(z));
        }
    }

    pub(super) fn on_change_card_actions_mode(
        &mut self,
        index: u32,
        sender: ComponentSender<Self>,
    ) {
        self.card_actions_hover = index == 0;
        let (hover_toggle, hover_auto) = match index {
            0 => (true, false),
            1 => (false, true),
            _ => (false, false),
        };
        let _ = sender.output(PrefOutput::SetCardActionsMode {
            hover_toggle,
            hover_auto,
        });
    }

    pub(super) fn on_toggle_list_palette(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.list_palette = on;
        let _ = sender.output(PrefOutput::SetListPalette(on));
    }

    pub(super) fn on_toggle_swipe_enabled(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.swipe_enabled = on;
        let _ = sender.output(PrefOutput::SetSwipeEnabled(on));
    }

    pub(super) fn on_toggle_print_page_numbers(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.print_options.page_numbers = on;
        let _ = sender.output(PrefOutput::SetPrintOptions(self.print_options));
    }

    pub(super) fn on_toggle_print_date(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.print_options.date = on;
        let _ = sender.output(PrefOutput::SetPrintOptions(self.print_options));
    }

    pub(super) fn on_toggle_print_punch_margin(&mut self, on: bool, sender: ComponentSender<Self>) {
        self.print_options.punch_margin = on;
        let _ = sender.output(PrefOutput::SetPrintOptions(self.print_options));
    }

    pub(super) fn on_change_read_mark(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let policy = read_mark_at(idx);
        let _ = sender.output(PrefOutput::SetReadMark(policy));
    }

    pub(super) fn on_change_list_layout(&mut self, index: u32, sender: ComponentSender<Self>) {
        let layout = list_layout_at(index);
        self.list_single_possible = layout != crate::config::ListLayout::Cards;
        let _ = sender.output(PrefOutput::SetListLayout(layout));
    }

    pub(super) fn on_change_preview_lines(&mut self, index: u32, sender: ComponentSender<Self>) {
        // The combo lists Off, then 1, 2 and 3 lines — so the row index is
        // the number of lines.
        let _ = sender.output(PrefOutput::SetPreviewLines(index));
    }

    pub(super) fn on_change_fold_messages(&mut self, idx: u32, sender: ComponentSender<Self>) {
        let fold = fold_messages_at(idx);
        let _ = sender.output(PrefOutput::SetFoldMessages(fold));
    }
}
