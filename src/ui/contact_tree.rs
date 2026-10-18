//! The list under the sidebar's Contacts row: each account's address books
//! with their groups under them. Picking a row picks what the contact list
//! shows; contacts dragged from the list are copied onto a book (moved, with
//! Ctrl held) or put in a group.

use adw::prelude::*;

use crate::i18n::i18n;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::contacts_page::{dragged_contacts, ContactScope, ContactTree, DropOn, TreeAction, TreeBook};
use crate::ui::sidebar::SidebarInput;

/// What [`fill`] made: the rows with the scope each picks, and what the
/// sidebar folds and opens them with.
pub struct Filled {
    pub rows: Vec<(ContactScope, gtk::ListBoxRow)>,
    pub layout: Vec<TreeRow>,
    /// Each foldable node's chevron, by its key.
    pub chevrons: Vec<(String, gtk::Image)>,
}

/// A row that hides while a node above it (its account, its book) is
/// folded, sliding as folder rows do.
pub struct TreeRow {
    pub row: gtk::ListBoxRow,
    pub revealer: gtk::Revealer,
    /// The keys of the nodes above it.
    pub under: Vec<String>,
}

/// The key that folds an account's books.
fn account_key(account: &str) -> String {
    format!("account:{account}")
}

/// The key that folds a book's groups.
fn book_key(uid: &str) -> String {
    format!("book:{uid}")
}

/// Fill `list`, the Contacts row's list in the sidebar, with `tree`: the
/// books that are no account's first, then each account, its books under
/// it and each book's groups under the book, each account and each book
/// with groups folding as a folder with sub-folders does (`folded` holds
/// the keys of those folded). In the icon rail (`collapsed`) the books show
/// as icons alone, with no accounts or groups.
pub fn fill(
    list: &gtk::ListBox,
    tree: &ContactTree,
    collapsed: bool,
    inset: bool,
    folded: &std::collections::HashSet<String>,
    input: &relm4::Sender<SidebarInput>,
) -> Filled {
    let mut out = Filled { rows: Vec::new(), layout: Vec::new(), chevrons: Vec::new() };
    // The books of no account head the list; the accounts keep their order.
    let mut books: Vec<&TreeBook> = tree.books.iter().filter(|b| b.account.is_empty()).collect();
    books.extend(tree.books.iter().filter(|b| !b.account.is_empty()));
    let mut account: Option<&str> = None;
    for book in books {
        let mut under = Vec::new();
        if !book.account.is_empty() && !collapsed {
            let key = account_key(&book.account);
            if account != Some(book.account.as_str()) {
                let (row, chevron) = heading(&book.account, &key, folded, input);
                list.append(&row);
                out.chevrons.push((key.clone(), chevron));
            }
            under.push(key);
        }
        account = Some(book.account.as_str());

        let icon = if book.directory { "network-server-symbolic" } else { "x-office-address-book-symbolic" };
        let tip = if book.account.is_empty() { book.name.clone() } else { format!("{} · {}", book.account, book.name) };
        let count = (!book.directory).then_some(book.count);
        let key = book_key(&book.uid);
        let expander = (!collapsed && !book.groups.is_empty()).then(|| expander(&key, folded, input));
        if let Some((_, chevron)) = &expander {
            out.chevrons.push((key.clone(), chevron.clone()));
        }
        let lead = expander.map(|(button, _)| button.upcast::<gtk::Widget>());
        let r = row(&book.name, &tip, icon, count, lead, 0, collapsed, inset);
        if book.writable && !book.directory {
            drop_target(&r, DropOn::Book(book.uid.clone()), input);
        }
        if !book.directory {
            let (s, uid, writable, groups) = (input.clone(), book.uid.clone(), book.writable, book.takes_groups);
            menu_on(&r, move || {
                let act = |action: TreeAction| {
                    let s = s.clone();
                    move || {
                        let _ = s.send(SidebarInput::ContactTreeMenu(action.clone()));
                    }
                };
                let mut sections = Vec::new();
                if writable {
                    let mut entries = vec![MenuEntry::new(i18n("New Contact"), act(TreeAction::NewContact(uid.clone())))
                        .icon("contact-new-symbolic")];
                    if groups {
                        entries.push(
                            MenuEntry::new(i18n("New Group"), act(TreeAction::NewGroup(uid.clone())))
                                .icon("system-users-symbolic"),
                        );
                    }
                    sections.push(entries);
                }
                sections.push(vec![MenuEntry::new(i18n("Rename…"), act(TreeAction::RenameBook(uid.clone())))
                    .icon("document-edit-symbolic")]);
                sections
            });
        }
        list.append(&r);
        out.layout.push(slider(&r, under.clone(), folded));
        out.rows.push((ContactScope::Book(book.uid.clone()), r));
        if collapsed {
            continue;
        }
        under.push(key);
        for g in &book.groups {
            let r = row(&g.name, &g.name, "system-users-symbolic", Some(g.count), None, 1, false, inset);
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
            out.layout.push(slider(&r, under.clone(), folded));
            out.rows.push((ContactScope::Group { book: g.book.clone(), uid: g.uid.clone() }, r));
        }
    }
    // Built at full height in one pass, then sliding for the clicks after.
    let revealers: Vec<gtk::Revealer> = out.layout.iter().map(|t| t.revealer.clone()).collect();
    gtk::glib::idle_add_local_once(move || {
        for r in revealers {
            r.set_transition_duration(200);
        }
    });
    out
}

/// Put `row`'s content in a revealer, shown unless a node it is `under` is
/// folded.
fn slider(row: &gtk::ListBoxRow, under: Vec<String>, folded: &std::collections::HashSet<String>) -> TreeRow {
    let hidden = under.iter().any(|k| folded.contains(k));
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(0);
    if let Some(content) = row.child() {
        row.set_child(gtk::Widget::NONE);
        revealer.set_child(Some(&content));
    }
    revealer.set_reveal_child(!hidden);
    row.set_child(Some(&revealer));
    row.set_visible(!hidden);
    TreeRow { row: row.clone(), revealer, under }
}

/// The caret that folds node `key`, as a folder's that has sub-folders:
/// pointing on when folded, turned down when open.
fn expander(key: &str, folded: &std::collections::HashSet<String>, input: &relm4::Sender<SidebarInput>) -> (gtk::Button, gtk::Image) {
    let img = gtk::Image::from_icon_name("pan-end-symbolic");
    img.add_css_class("tree-expander-icon");
    if !folded.contains(key) {
        img.add_css_class("open");
    }
    img.set_pixel_size(12);
    let btn = gtk::Button::new();
    btn.set_child(Some(&img));
    btn.add_css_class("flat");
    btn.add_css_class("tree-expander");
    btn.set_valign(gtk::Align::Center);
    btn.set_tooltip_text(Some(i18n("Show or hide").as_str()));
    let (s, key) = (input.clone(), key.to_string());
    btn.connect_clicked(move |_| {
        let _ = s.send(SidebarInput::ToggleContactNode(key.clone()));
    });
    (btn, img)
}

/// A row shaped as the other unified rows' are: the caret slot (a caret
/// for a book with groups, else a spacer as wide), an icon, the name and,
/// when `count` is given, how many it holds; `depth` 1 sets a group under
/// its book.
#[allow(clippy::too_many_arguments)]
fn row(
    name: &str,
    tip: &str,
    icon: &str,
    count: Option<usize>,
    expander: Option<gtk::Widget>,
    depth: i32,
    collapsed: bool,
    inset: bool,
) -> gtk::ListBoxRow {
    let img = gtk::Image::from_icon_name(icon);
    img.add_css_class("folder-icon");
    crate::ui::sidebar::pin_icon_size(&img);
    let lead = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    if !collapsed {
        let slot: gtk::Widget = expander.unwrap_or_else(|| {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_width_request(crate::ui::sidebar::TREE_EXPANDER_WIDTH);
            spacer.upcast()
        });
        slot.set_margin_start(depth * crate::ui::sidebar::TREE_EXPANDER_WIDTH);
        lead.append(&slot);
    }
    lead.append(&img);
    let (row, badge) = crate::ui::sidebar::build_unified_sub_row(&lead, name, tip, 0, collapsed, inset, false);
    badge.set_visible(false);
    if !collapsed {
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

/// An account's name over its books, with the caret that folds them: not
/// a row to pick; a click on it folds or opens too.
fn heading(
    text: &str,
    key: &str,
    folded: &std::collections::HashSet<String>,
    input: &relm4::Sender<SidebarInput>,
) -> (gtk::ListBoxRow, gtk::Image) {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_can_focus(false);
    row.add_css_class("contact-account-row");
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    hbox.add_css_class("folder-row");
    let (button, chevron) = expander(key, folded, input);
    hbox.append(&button);
    let label = gtk::Label::new(Some(text));
    label.add_css_class("dim-label");
    label.add_css_class("caption-heading");
    label.set_halign(gtk::Align::Start);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    hbox.append(&label);
    row.set_child(Some(&hbox));
    let click = gtk::GestureClick::new();
    let (s, key) = (input.clone(), key.to_string());
    click.connect_released(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let _ = s.send(SidebarInput::ToggleContactNode(key.clone()));
    });
    row.add_controller(click);
    (row, chevron)
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
