//! The sidebar's rows while Contacts is open: every contact, then each
//! account's address books with their groups under them, as the mail
//! sidebar lists accounts and folders. Picking a row picks what the
//! contact list shows; contacts dragged from the list are copied onto a
//! book (moved, with Ctrl held) or put in a group.

use adw::prelude::*;

use crate::i18n::i18n;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::contacts_page::{dragged_contacts, ContactScope, ContactTree, DropOn, TreeAction};
use crate::ui::sidebar::SidebarInput;

/// The rows built for `tree`, with the scope each one picks. In the icon
/// rail (`collapsed`) the books show as icons alone and the groups not at
/// all.
pub fn build(
    tree: &ContactTree,
    collapsed: bool,
    input: &relm4::Sender<SidebarInput>,
) -> (gtk::ListBox, Vec<(ContactScope, gtk::ListBoxRow)>) {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("navigation-sidebar");
    if collapsed {
        list.add_css_class("rail-collapsed");
    }
    let mut rows = Vec::new();

    let all = row(&i18n("All Contacts"), "x-office-address-book-symbolic", tree.total, 0, collapsed);
    list.append(&all);
    rows.push((ContactScope::All, all));

    let mut account: Option<&str> = None;
    for book in &tree.books {
        // A heading for each account that holds books of its own name.
        if !collapsed && !book.account.is_empty() && account != Some(book.account.as_str()) {
            list.append(&heading(&book.account));
        }
        account = Some(book.account.as_str());
        let icon = if book.directory { "network-server-symbolic" } else { "x-office-address-book-symbolic" };
        let count = if book.directory { usize::MAX } else { book.count };
        let r = row(&book.name, icon, count, 0, collapsed);
        if collapsed {
            r.set_tooltip_text(Some(&if book.account.is_empty() {
                book.name.clone()
            } else {
                format!("{} · {}", book.account, book.name)
            }));
        }
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
            let r = row(&g.name, "system-users-symbolic", g.count, 1, false);
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

    {
        let (s, scopes) = (input.clone(), rows.clone());
        list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            if let Some((scope, _)) = scopes.iter().find(|(_, r)| r == row) {
                let _ = s.send(SidebarInput::ContactTreeSelected(scope.clone()));
            }
        });
    }
    (list, rows)
}

/// A row: icon, name and, unless `count` is `usize::MAX`, how many it
/// holds; `depth` 1 indents a group under its book.
fn row(name: &str, icon: &str, count: usize, depth: i32, collapsed: bool) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hbox.add_css_class("folder-row");
    let img = gtk::Image::from_icon_name(icon);
    img.add_css_class("folder-icon");
    crate::ui::sidebar::pin_icon_size(&img);
    hbox.append(&img);
    if collapsed {
        hbox.set_halign(gtk::Align::Center);
        row.set_tooltip_text(Some(name));
    } else {
        hbox.set_margin_start(8 + depth * 18);
        let label = gtk::Label::new(Some(name));
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        hbox.append(&label);
        if count != usize::MAX {
            let n = gtk::Label::new(Some(&count.to_string()));
            n.add_css_class("dim-label");
            n.add_css_class("caption");
            hbox.append(&n);
        }
    }
    row.set_child(Some(&hbox));
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
    label.set_margin_top(12);
    label.set_margin_start(8);
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
