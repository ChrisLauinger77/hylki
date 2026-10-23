//! Applying general and appearance settings.

use super::*;

impl AppModel {
    /// Push the date and clock preference into the formatter and redraw whatever
    /// shows a date: every row carries one, as does the open message.
    pub(super) fn apply_date_style(&self) {
        crate::datefmt::set_style(self.date_style, self.clock_style);
        self.message_list.emit(MessageListInput::RefreshDates);
        let current = self.current.clone();
        self.show_message(current, false);
    }
}
