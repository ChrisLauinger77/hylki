//! The right-click menu of a message card in the reader.

use super::*;

impl AppModel {
    /// The menu a right-click on a reader card opens (the message list
    /// row's menu, for that one message), anchored on the window at (x, y).
    pub(super) fn show_card_menu(
        &self,
        m: Message,
        x: f64,
        y: f64,
        hit: crate::ui::message_view::MenuHit,
        popout: Option<(u32, u32)>,
        sender: &ComponentSender<Self>,
    ) {
        use crate::ui::context_menu::{show_context_menu, MenuEntry};
        // A pop-out's menu opens over the pop-out, and acts as its own
        // buttons do; with the window gone there is nothing to anchor to.
        let parent: gtk::Widget = match popout {
            Some(key) => match self.popouts.get(&key) {
                Some(p) => p.window.clone().upcast(),
                None => return,
            },
            None => self.window.clone().upcast(),
        };
        let window = parent.clone().downcast::<gtk::Window>().ok();
        // Through the card path: a reply started from a card belongs in the
        // pane's inline composer, like the card's own buttons — the row path
        // would open a compose window. Every other action falls through to
        // the row behaviour there. A pop-out has no pane, so it takes the row
        // path, as its toolbar does.
        let item = |action: RowAction, label: String, icon: &str| -> MenuEntry {
            let s = sender.input_sender().clone();
            let message = m.clone();
            MenuEntry::new(label, move || {
                let message = Box::new(message.clone());
                let _ = s.send(if popout.is_some() {
                    AppMsg::RowAction {
                        action,
                        message,
                        conversation: Vec::new(),
                    }
                } else {
                    AppMsg::CardAction { action, message }
                });
            })
            .icon(format!("{icon}-symbolic"))
        };
        let kind = self.folder_kind(m.account_id, m.folder_id);
        let in_junk = kind == Some(FolderKind::Junk);
        let restorable = matches!(kind, Some(FolderKind::Trash | FolderKind::Junk));
        let mut sections = Vec::new();
        let here = crate::ui::message_view::hit_menu_entries(&hit, window);
        if !here.is_empty() {
            sections.push(here);
        }
        sections.extend([
            // A template (#360) is used or edited, never answered.
            if kind == Some(FolderKind::Templates) {
                vec![
                    item(
                        RowAction::EditAsNew,
                        i18n("Use Template"),
                        "mail-message-new",
                    ),
                    item(
                        RowAction::EditTemplate,
                        i18n("Edit Template"),
                        "document-edit",
                    ),
                ]
            } else {
                vec![
                    item(RowAction::Reply, i18n("Reply"), "mail-reply-sender"),
                    item(RowAction::ReplyAll, i18n("Reply All"), "mail-reply-all"),
                    item(RowAction::Forward, i18n("Forward"), "mail-forward"),
                    item(
                        RowAction::EditAsNew,
                        i18n("Edit as New Message"),
                        "document-edit",
                    ),
                ]
            },
            vec![
                if m.starred {
                    item(
                        RowAction::ToggleStar,
                        i18n("Remove Star"),
                        "hylki-non-starred",
                    )
                } else {
                    item(RowAction::ToggleStar, i18n("Star"), "starred")
                },
                if m.unread {
                    item(
                        RowAction::ToggleRead,
                        i18n("Mark as Read"),
                        "hylki-mail-read",
                    )
                } else {
                    item(RowAction::ToggleRead, i18n("Mark as Unread"), "mail-unread")
                },
            ],
        ]);
        if !self.tags.is_empty() {
            let s = sender.input_sender().clone();
            let message = m.clone();
            let entries =
                crate::ui::message_list::tag_menu_entries(&self.tags, &m, move |keyword, add| {
                    let _ = s.send(AppMsg::SetTag {
                        message: Box::new(message.clone()),
                        keyword,
                        add,
                    });
                });
            sections.push(vec![
                MenuEntry::submenu(i18n("Tags"), vec![entries]).icon("tag-outline-symbolic")
            ]);
        }
        let mut acts = Vec::new();
        if in_junk {
            acts.push(item(
                RowAction::NotSpam,
                i18n("Not Spam"),
                "mail-mark-notjunk",
            ));
        } else {
            if restorable {
                acts.push(item(
                    RowAction::MoveToInbox,
                    i18n("Move to Inbox"),
                    "mail-inbox",
                ));
            }
            acts.push(item(
                RowAction::Spam,
                i18n("Mark as Spam"),
                "mail-mark-junk",
            ));
        }
        // The folder picker opens over the main window, so not from a pop-out.
        if popout.is_none() {
            let s = sender.input_sender().clone();
            let message = m.clone();
            acts.push(
                MenuEntry::new(i18n("Move To…"), move || {
                    let _ = s.send(AppMsg::ListMoveTo {
                        messages: vec![message.clone()],
                        offer_whole: false,
                        x,
                        y,
                    });
                })
                .icon("folder-symbolic"),
            );
        }
        acts.push(item(RowAction::Archive, i18n("Archive"), "mail-archive"));
        acts.push(item(RowAction::Delete, i18n("Delete"), "user-trash"));
        sections.push(acts);
        sections.push(vec![
            item(
                RowAction::AddContact,
                i18n("Add Sender to Contacts"),
                "contact-new",
            ),
            item(
                RowAction::CreateFilter,
                i18n("Create Filter from Message…"),
                "filter-folder",
            ),
        ]);
        // Remote content, per message. The banner offers the same thing, but
        // it can be switched off (Settings → Privacy) and then there is
        // nothing to click — so the choice lives here too, and here it goes
        // both ways: the banner can only ever let content in.
        {
            let showing = self.remote_allowed(&m);
            let s = sender.input_sender().clone();
            let (account_id, id) = (m.account_id, m.id);
            let label = if showing {
                i18n("Block Remote Content")
            } else {
                i18n("Show Remote Content")
            };
            let icon = if showing {
                "security-high"
            } else {
                "image-x-generic"
            };
            sections.push(vec![MenuEntry::new(label, move || {
                let _ = s.send(AppMsg::SetRemoteContent {
                    account_id,
                    id,
                    show: !showing,
                });
            })
            .icon(format!("{icon}-symbolic"))]);
        }
        // Expand All and Collapse All, for the whole conversation (#326).
        if let Some((any_folded, any_open, reader)) = hit.folds.clone() {
            let open = reader.clone();
            sections.push(vec![
                MenuEntry::new(i18n("Expand All Messages"), move || {
                    open.emit(crate::ui::message_view::MessageViewInput::FoldAll(false));
                })
                .icon("pan-down-symbolic")
                .enabled(any_folded),
                MenuEntry::new(i18n("Collapse All Messages"), move || {
                    reader.emit(crate::ui::message_view::MessageViewInput::FoldAll(true));
                })
                .icon("pan-up-symbolic")
                .enabled(any_open),
            ]);
        }
        let mut last = Vec::new();
        if let Some(t) = hit.translate.clone() {
            let (account_id, id) = (m.account_id, m.id);
            last.push(
                MenuEntry::new(t.label, move || {
                    t.reader
                        .emit(crate::ui::message_view::MessageViewInput::Translate {
                            account_id,
                            id,
                        });
                })
                .icon("translate-symbolic"),
            );
        }
        last.push(item(RowAction::ViewSource, i18n("View Source"), "code"));
        last.push(item(
            RowAction::SaveAsFile,
            i18n("Save as File…"),
            "document-save",
        ));
        sections.push(last);
        // Printing prints what the reader shows, the whole conversation in
        // it, so the reader that was right-clicked is the one asked (#359).
        {
            let print = |label: String, preview: bool| -> MenuEntry {
                let s = sender.input_sender().clone();
                let window = popout
                    .and_then(|key| self.popouts.get(&key))
                    .map(|p| p.controller.sender().clone());
                MenuEntry::new(label, move || match &window {
                    Some(w) => w.emit(if preview {
                        MessageWindowInput::PrintPreview
                    } else {
                        MessageWindowInput::Print
                    }),
                    None => {
                        let _ = s.send(if preview {
                            AppMsg::PrintPreview
                        } else {
                            AppMsg::PrintMessage
                        });
                    }
                })
            };
            sections.push(vec![
                print(i18n("Print Preview"), true).icon("printer-symbolic"),
                print(i18n("Print…"), false),
            ]);
        }
        show_context_menu(&parent, x, y, sections);
    }
}
