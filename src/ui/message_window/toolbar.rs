//! The pop-out's header toolbar: its buttons in the saved order, folding
//! into the ⋯ menu when the window is narrow, and that menu.

use super::types::{MessageWindowInput, MessageWindowOutput};
use super::MessageWindow;
use crate::config::ToolbarItem;
use crate::i18n::i18n;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use adw::prelude::*;
use relm4::prelude::*;

/// The header's buttons, one per toolbar item, packed in the saved order.
pub(super) struct Toolbar {
    pub(super) header: adw::HeaderBar,
    pub(super) buttons: Vec<(ToolbarItem, gtk::Button)>,
    pub(super) overflow: gtk::Button,
    pub(super) spinner: gtk::Spinner,
    pub(super) breakpoint: adw::Breakpoint,
}

/// One flat icon button for each toolbar item, with the main window's icons
/// and tooltips, and the ⋯ and spinner the row also needs.
pub(super) fn build_toolbar(
    header: &adw::HeaderBar,
    sender: &ComponentSender<MessageWindow>,
) -> Toolbar {
    let button = |icon: &str, tooltip: String, msg: fn() -> MessageWindowInput| {
        let b = gtk::Button::from_icon_name(icon);
        b.set_tooltip_text(Some(&tooltip));
        b.add_css_class("flat");
        let s = sender.input_sender().clone();
        b.connect_clicked(move |_| {
            let _ = s.send(msg());
        });
        b
    };
    let buttons = ToolbarItem::ALL
        .iter()
        .map(|item| {
            let b = match item {
                ToolbarItem::Reply => button("mail-reply-sender-symbolic", i18n("Reply"), || {
                    MessageWindowInput::Reply
                }),
                ToolbarItem::ReplyAll => {
                    button("mail-reply-all-symbolic", i18n("Reply All"), || {
                        MessageWindowInput::ReplyAll
                    })
                }
                ToolbarItem::Forward => button("mail-forward-symbolic", i18n("Forward"), || {
                    MessageWindowInput::Forward
                }),
                // One glyph in both states, as in the main window; the
                // starred state carries color only.
                ToolbarItem::Star => button("hylki-non-starred-symbolic", i18n("Star"), || {
                    MessageWindowInput::ToggleStar
                }),
                ToolbarItem::Archive => button("mail-archive-symbolic", i18n("Archive"), || {
                    MessageWindowInput::Archive
                }),
                ToolbarItem::Delete => button("user-trash-symbolic", i18n("Delete"), || {
                    MessageWindowInput::Delete
                }),
                ToolbarItem::Spam => {
                    button("mail-mark-junk-symbolic", i18n("Mark as Spam"), || {
                        MessageWindowInput::Spam
                    })
                }
                ToolbarItem::ReadUnread => {
                    button("mail-unread-symbolic", i18n("Mark as Unread"), || {
                        MessageWindowInput::ToggleRead
                    })
                }
                ToolbarItem::Tags => button("tag-outline-symbolic", i18n("Tags"), || {
                    MessageWindowInput::TagMenu
                }),
                ToolbarItem::MoveTo => button("folder-symbolic", i18n("Move To…"), || {
                    MessageWindowInput::MoveTo
                }),
                ToolbarItem::Find => button(
                    "loupe-with-arrow-symbolic",
                    i18n("Find in message (Ctrl+F)"),
                    || MessageWindowInput::Find,
                ),
                // The preview, not the print dialog: it shows what will come
                // out and prints from there (#359).
                ToolbarItem::Print => button(
                    "printer-symbolic",
                    i18n("Print Preview (Ctrl+Shift+P)"),
                    || MessageWindowInput::PrintPreview,
                ),
            };
            (*item, b)
        })
        .collect();
    // Rightmost, beside the window controls, as in the main window.
    let overflow = button("view-more-horizontal-symbolic", i18n("Actions"), || {
        MessageWindowInput::OverflowMenu
    });
    overflow.set_visible(false);
    header.pack_end(&overflow);
    let spinner = gtk::Spinner::new();
    spinner.set_valign(gtk::Align::Center);
    spinner.set_tooltip_text(Some(&i18n("Downloading attachments…")));
    Toolbar {
        header: header.clone(),
        buttons,
        overflow,
        spinner,
        breakpoint: adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            0.0,
            adw::LengthUnit::Px,
        )),
    }
}

impl MessageWindow {
    /// With breakpoints a window's minimum width is its own size request,
    /// not its content's, and narrower content is cut off at the right. The
    /// toolbar folds to fit, but the reader and the drawer's header cannot,
    /// so the window is kept at least as wide as they need. Measured once
    /// the drawer has taken in its files.
    pub(super) fn fit_min_width(&self) {
        let window = self.window.clone();
        let content = self.content.clone();
        gtk::glib::idle_add_local_once(move || {
            let need = content.measure(gtk::Orientation::Horizontal, -1).0;
            window.set_size_request(need.max(360), 294);
        });
    }

    pub(super) fn button(&self, item: ToolbarItem) -> Option<&gtk::Button> {
        self.bar
            .buttons
            .iter()
            .find(|(i, _)| *i == item)
            .map(|(_, b)| b)
    }

    /// Pack the buttons in the saved order, as the main window does: the
    /// left group from the start, the right group from the end (which fills
    /// right to left, so in reverse), then the attachments spinner as the
    /// innermost of the right group. Buttons on neither side stay unpacked.
    /// The fold point is measured afresh for the new row.
    pub(super) fn relayout_toolbar(&self) {
        let header = &self.bar.header;
        for (_, b) in &self.bar.buttons {
            if b.parent().is_some() {
                header.remove(b);
            }
        }
        if self.bar.spinner.parent().is_some() {
            header.remove(&self.bar.spinner);
        }
        for item in &self.toolbar.left {
            if let Some(b) = self.button(*item) {
                header.pack_start(b);
            }
        }
        for item in self.toolbar.right.iter().rev() {
            if let Some(b) = self.button(*item) {
                header.pack_end(b);
            }
        }
        header.pack_end(&self.bar.spinner);

        // The width the whole row needs: every packed button shown, the
        // folded state's ⋯ hidden, plus a little slack so the fold comes a
        // step ahead of a squeeze. Tags without any tag counts too.
        let shown: Vec<(gtk::Button, bool)> = self
            .bar
            .buttons
            .iter()
            .map(|(_, b)| (b.clone(), b.is_visible()))
            .collect();
        for (b, _) in &shown {
            b.set_visible(b.parent().is_some());
        }
        let overflow = self.bar.overflow.is_visible();
        self.bar.overflow.set_visible(false);
        let need = header.measure(gtk::Orientation::Horizontal, -1).1;
        for (b, visible) in shown {
            b.set_visible(visible);
        }
        self.bar.overflow.set_visible(overflow);
        self.bar
            .breakpoint
            .set_condition(Some(&adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MaxWidth,
                f64::from(need + 24),
                adw::LengthUnit::Px,
            )));
    }

    /// The buttons' state: which show (the right group only while the row
    /// fits), and the ones that say the message's state.
    pub(super) fn sync_toolbar(&self) {
        for (item, b) in &self.bar.buttons {
            let right = self.toolbar.right.contains(item);
            let mut visible = !(right && self.collapsed);
            if *item == ToolbarItem::Tags {
                visible &= !self.tags.is_empty();
            }
            b.set_visible(visible);
            match item {
                ToolbarItem::Star => {
                    b.set_tooltip_text(Some(&if self.msg.starred {
                        i18n("Remove Star")
                    } else {
                        i18n("Star")
                    }));
                    if self.msg.starred {
                        b.add_css_class("star-active");
                    } else {
                        b.remove_css_class("star-active");
                    }
                }
                // The icon shows the ACTION (read envelope = "mark as
                // read"), matching the menus.
                ToolbarItem::ReadUnread => {
                    b.set_icon_name(if self.msg.unread {
                        "hylki-mail-read-symbolic"
                    } else {
                        "mail-unread-symbolic"
                    });
                    b.set_tooltip_text(Some(&if self.msg.unread {
                        i18n("Mark as Read")
                    } else {
                        i18n("Mark as Unread")
                    }));
                }
                ToolbarItem::Spam => {
                    b.set_icon_name(if self.in_junk {
                        "mail-mark-notjunk-symbolic"
                    } else {
                        "mail-mark-junk-symbolic"
                    });
                    b.set_tooltip_text(Some(&if self.in_junk {
                        i18n("Not Spam")
                    } else {
                        i18n("Mark as Spam")
                    }));
                }
                _ => {}
            }
        }
        self.bar
            .overflow
            .set_visible(self.collapsed && !self.toolbar.right.is_empty());
        let loading = !self.attachments_loading.is_empty();
        self.bar.spinner.set_spinning(loading);
        self.bar.spinner.set_visible(loading);
    }

    /// The tags (#71), ticked where the message carries them.
    fn tag_entries(&self, sender: &ComponentSender<Self>) -> Vec<MenuEntry> {
        let s = sender.output_sender().clone();
        let message = self.msg.clone();
        crate::ui::message_list::tag_menu_entries(&self.tags, &self.msg, move |keyword, add| {
            let _ = s.send(MessageWindowOutput::SetTag {
                message: Box::new(message.clone()),
                keyword,
                add,
            });
        })
    }

    pub(super) fn show_tag_menu(&self, sender: &ComponentSender<Self>) {
        if self.tags.is_empty() {
            return;
        }
        // Gmail's l asks for the menu with the button folded away or left
        // out of the toolbar: then it hangs from the middle of the header.
        let b: gtk::Widget = match self.button(ToolbarItem::Tags).filter(|b| b.is_mapped()) {
            Some(b) => b.clone().upcast(),
            None => self.bar.header.clone().upcast(),
        };
        show_context_menu(
            &b,
            f64::from(b.width() / 2),
            f64::from(b.height()),
            vec![self.tag_entries(sender)],
        );
    }

    /// The folded toolbar's ⋯ menu: the right group, in its own order, with
    /// the main window's labels and icons.
    pub(super) fn show_overflow_menu(&self, sender: &ComponentSender<Self>) {
        let entry = |label: String, icon: &str, msg: fn() -> MessageWindowInput| {
            let s = sender.input_sender().clone();
            MenuEntry::new(label, move || {
                let _ = s.send(msg());
            })
            .icon(format!("{icon}-symbolic"))
        };
        let mut section = Vec::new();
        for item in &self.toolbar.right {
            match item {
                ToolbarItem::Reply => {
                    section.push(entry(i18n("Reply"), "mail-reply-sender", || {
                        MessageWindowInput::Reply
                    }))
                }
                ToolbarItem::ReplyAll => {
                    section.push(entry(i18n("Reply All"), "mail-reply-all", || {
                        MessageWindowInput::ReplyAll
                    }))
                }
                ToolbarItem::Forward => {
                    section.push(entry(i18n("Forward"), "mail-forward", || {
                        MessageWindowInput::Forward
                    }))
                }
                ToolbarItem::Star => section.push(if self.msg.starred {
                    entry(i18n("Remove Star"), "hylki-non-starred", || {
                        MessageWindowInput::ToggleStar
                    })
                } else {
                    entry(i18n("Star"), "starred", || MessageWindowInput::ToggleStar)
                }),
                ToolbarItem::Archive => {
                    section.push(entry(i18n("Archive"), "mail-archive", || {
                        MessageWindowInput::Archive
                    }))
                }
                ToolbarItem::Delete => section.push(entry(i18n("Delete"), "user-trash", || {
                    MessageWindowInput::Delete
                })),
                ToolbarItem::Spam => section.push(if self.in_junk {
                    entry(i18n("Not Spam"), "mail-mark-notjunk", || {
                        MessageWindowInput::Spam
                    })
                } else {
                    entry(i18n("Mark as Spam"), "mail-mark-junk", || {
                        MessageWindowInput::Spam
                    })
                }),
                ToolbarItem::ReadUnread => section.push(if self.msg.unread {
                    entry(i18n("Mark as Read"), "hylki-mail-read", || {
                        MessageWindowInput::ToggleRead
                    })
                } else {
                    entry(i18n("Mark as Unread"), "mail-unread", || {
                        MessageWindowInput::ToggleRead
                    })
                }),
                ToolbarItem::Tags => {
                    if !self.tags.is_empty() {
                        section.push(
                            MenuEntry::submenu(i18n("Tags"), vec![self.tag_entries(sender)])
                                .icon("tag-outline-symbolic"),
                        );
                    }
                }
                ToolbarItem::MoveTo => {
                    if self.restorable {
                        section.push(entry(i18n("Move to Inbox"), "mail-inbox", || {
                            MessageWindowInput::MoveToInbox
                        }));
                    }
                    section.push(entry(i18n("Move To…"), "folder", || {
                        MessageWindowInput::MoveTo
                    }));
                }
                ToolbarItem::Find => {
                    section.push(entry(i18n("Find in Message"), "loupe-with-arrow", || {
                        MessageWindowInput::Find
                    }))
                }
                ToolbarItem::Print => section.push(entry(i18n("Print Preview"), "printer", || {
                    MessageWindowInput::PrintPreview
                })),
            }
        }
        let b = &self.bar.overflow;
        show_context_menu(
            b,
            f64::from(b.width() / 2),
            f64::from(b.height()),
            vec![section],
        );
    }
}
