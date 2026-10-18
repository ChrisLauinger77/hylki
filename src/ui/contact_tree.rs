//! The list under the sidebar's Contacts row: each account's address books
//! with their groups under them. Picking a row picks what the contact list
//! shows; contacts dragged from the list are copied onto a book (moved, with
//! Ctrl held) or put in a group.

use adw::prelude::*;

use crate::i18n::i18n;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::contacts_page::{dragged_contacts, ContactScope, ContactTree, DropOn, TreeAction};
use crate::ui::sidebar::SidebarInput;

/// Fill `list`, the Contacts row's list in the sidebar, with `tree`: each
/// account's name, its address books under it and each book's groups under
/// the book. Returns the rows with the scope each picks. In the icon rail
/// (`collapsed`) the books show as icons alone and the groups not at all.
pub fn fill(
    list: &gtk::ListBox,
    tree: &ContactTree,
    collapsed: bool,
    inset: bool,
    input: &relm4::Sender<SidebarInput>,
) -> Vec<(ContactScope, gtk::ListBoxRow)> {
    let mut rows = Vec::new();
    let mut account: Option<&str> = None;
    for book in &tree.books {
        // A heading for each account that holds books of its own name.
        if !collapsed && !book.account.is_empty() && account != Some(book.account.as_str()) {
            list.append(&heading(&book.account));
        }
        account = Some(book.account.as_str());
        let icon = if book.directory { "network-server-symbolic" } else { "x-office-address-book-symbolic" };
        let tip = if book.account.is_empty() { book.name.clone() } else { format!("{} · {}", book.account, book.name) };
        let count = (!book.directory).then_some(book.count);
        let r = row(&book.name, &tip, icon, count, 0, collapsed, inset);
        if book.writable && !book.directory {
            drop_target(&r, DropOn::Book(book.uid.clone()), input);
        }
        if book.writable {
            let (s, uid, groups) = (input.clone(), book.uid.clone(), book.takes_groups);
            menu_on(&r, move || {
                let act = |action: TreeAction| {
                    let s = s.clone();
                    move || {
                        let _ = s.send(SidebarInput::ContactTreeMenu(action.clone()));
                    }
                };
                let mut entries = vec![MenuEntry::new(i18n("New Contact"), act(TreeAction::NewContact(uid.clone())))
                    .icon("contact-new-symbolic")];
                if groups {
                    entries.push(
                        MenuEntry::new(i18n("New Group"), act(TreeAction::NewGroup(uid.clone())))
                            .icon("system-users-symbolic"),
                    );
                }
                vec![entries]
            });
        }
        list.append(&r);
        rows.push((ContactScope::Book(book.uid.clone()), r));
        if collapsed {
            continue;
        }
        for g in &book.groups {
            let r = row(&g.name, &g.name, "system-users-symbolic", Some(g.count), 1, false, inset);
            if g.editable {
                drop_target(&r, DropOn::Group { book: g.book.clone(), uid: g.uid.clone() }, input);
            }
            let (s, book, uid, editable, any) = (input.clone(), g.book.clone(), g.uid.clone(), g.editable, g.count > 0);
            menu_on(&r, move || {
                let act = |action: TreeAction| {
                    let s = s.clone();
                    move || {
                        let _ = s.send(SidebarInput::ContactTreeMenu(action.clone()));
                    }
                };
                let (b, u) = (book.clone(), uid.clone());
                let mut first = vec![MenuEntry::new(i18n("New Message"), act(TreeAction::ComposeGroup { book: b, uid: u }))
                    .icon("mail-message-new-symbolic")
                    .enabled(any)];
                if !editable {
                    return vec![first];
                }
                first.push(
                    MenuEntry::new(i18n("Edit Group"), act(TreeAction::EditGroup { book: book.clone(), uid: uid.clone() }))
                        .icon("document-edit-symbolic"),
                );
                let delete = MenuEntry::new(
                    i18n("Delete Group…"),
                    act(TreeAction::DeleteGroup { book: book.clone(), uid: uid.clone() }),
                )
                .icon("user-trash-symbolic");
                vec![first, vec![delete]]
            });
            list.append(&r);
            rows.push((ContactScope::Group { book: g.book.clone(), uid: g.uid.clone() }, r));
        }
    }
    rows
}

/// A row shaped as the other unified rows' are: an icon, the name and, when
/// `count` is given, how many it holds; `depth` 1 sets a group under its
/// book.
fn row(name: &str, tip: &str, icon: &str, count: Option<usize>, depth: i32, collapsed: bool, inset: bool) -> gtk::ListBoxRow {
    let img = gtk::Image::from_icon_name(icon);
    img.add_css_class("folder-icon");
    crate::ui::sidebar::pin_icon_size(&img);
    let (row, badge) = crate::ui::sidebar::build_unified_sub_row(&img, name, tip, 0, collapsed, inset, false);
    badge.set_visible(false);
    if !collapsed {
        if depth > 0 {
            img.set_margin_start(img.margin_start() + 18 * depth);
        }
        // A count, not unread mail: dim, where the unread chip would be.
        if let (Some(n), Some(hbox)) = (count, row.child().and_downcast::<gtk::Box>()) {
            let label = gtk::Label::new(Some(&n.to_string()));
            label.add_css_class("dim-label");
            label.add_css_class("caption");
            hbox.append(&label);
        }
    }
    row
}

/// An account's name over its books: not a row to pick.
fn heading(text: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    row.set_can_focus(false);
    let label = gtk::Label::new(Some(text));
    label.add_css_class("dim-label");
    label.add_css_class("caption-heading");
    label.set_halign(gtk::Align::Start);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_margin_top(8);
    label.set_margin_start(10);
    row.set_child(Some(&label));
    row
}

/// A right-click menu on `row`, made by `sections` each time it opens.
fn menu_on(row: &gtk::ListBoxRow, sections: impl Fn() -> Vec<Vec<MenuEntry>> + 'static) {
    let click = gtk::GestureClick::new();
    click.set_button(3);
    click.connect_pressed(move |gesture, _, x, y| {
        if let Some(widget) = gesture.widget() {
            show_context_menu(&widget, x, y, sections());
        }
    });
    row.add_controller(click);
}

/// Contacts dragged from the list land on `row`: copied, or moved while
/// Ctrl is held. The row is outlined while they are over it.
fn drop_target(row: &gtk::ListBoxRow, on: DropOn, input: &relm4::Sender<SidebarInput>) {
    let target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE);
    // Ctrl as the keyboard has it: on Wayland a drop's own event carries no
    // keys.
    let held = |t: &gtk::DropTarget| {
        let keyboard = t.widget().and_then(|w| w.display().default_seat()).and_then(|seat| seat.keyboard());
        let state = keyboard.map(|k| k.modifier_state()).unwrap_or(t.current_event_state());
        (state | t.current_event_state()).contains(gtk::gdk::ModifierType::CONTROL_MASK)
    };
    {
        let row = row.downgrade();
        target.connect_enter(move |t, _, _| {
            if let Some(row) = row.upgrade() {
                row.add_css_class("drop-into");
            }
            if held(t) { gtk::gdk::DragAction::MOVE } else { gtk::gdk::DragAction::COPY }
        });
    }
    target.connect_motion(move |t, _, _| if held(t) { gtk::gdk::DragAction::MOVE } else { gtk::gdk::DragAction::COPY });
    {
        let row = row.downgrade();
        target.connect_leave(move |_| {
            if let Some(row) = row.upgrade() {
                row.remove_css_class("drop-into");
            }
        });
    }
    {
        let (row, s) = (row.downgrade(), input.clone());
        target.connect_drop(move |t, value, _, _| {
            if let Some(row) = row.upgrade() {
                row.remove_css_class("drop-into");
            }
            let Some(keys) = value.get::<String>().ok().as_deref().and_then(dragged_contacts) else { return false };
            let _ = s.send(SidebarInput::ContactTreeDrop { keys, on: on.clone(), moving: held(t) });
            true
        });
    }
    row.add_controller(target);
}
