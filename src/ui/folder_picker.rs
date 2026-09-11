//! The "Move To" popover (#164): an account's folders as a searchable list
//! anchored to the toolbar button, so mail can be filed without dragging it
//! to the sidebar. Rows follow the sidebar's order and indentation; a row
//! picked (clicked, or Enter on the first match) hands its path back.

use gtk::prelude::*;

use crate::i18n::i18n;
use crate::models::Folder;

/// Open the picker below the point (`x`, `y`) of `parent`, listing
/// `folders` less the one at `exclude` (the message's own). `on_pick` gets
/// the chosen folder's path.
pub fn show_folder_picker(
    parent: &impl IsA<gtk::Widget>,
    x: f64,
    y: f64,
    folders: Vec<Folder>,
    exclude: Option<String>,
    on_pick: impl Fn(String) + 'static,
) {
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.set_position(gtk::PositionType::Bottom);
    popover.add_css_class("menu");

    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.add_css_class("context-menu-list");

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(i18n("Search folders").as_str()));
    column.append(&search);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("navigation-sidebar");
    let refs: Vec<&Folder> = folders.iter().collect();
    for f in &folders {
        if exclude.as_deref() == Some(f.path.as_str()) {
            continue;
        }
        let depth = crate::ui::sidebar::folder_depth(f, &refs);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.set_margin_start(4 + 16 * depth as i32);
        let icon = gtk::Image::from_icon_name(f.kind.icon());
        icon.set_pixel_size(16);
        row.append(&icon);
        let label = gtk::Label::new(Some(&f.name));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        row.append(&label);
        let lbr = gtk::ListBoxRow::new();
        lbr.set_child(Some(&row));
        // The search filter reads the name back from here.
        unsafe { lbr.set_data("folder-name", f.name.to_lowercase()) };
        unsafe { lbr.set_data("folder-path", f.path.clone()) };
        list.append(&lbr);
    }

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(true);
    scroller.set_max_content_height(380);
    scroller.set_min_content_width(240);
    scroller.set_child(Some(&list));
    column.append(&scroller);

    let on_pick = std::rc::Rc::new(on_pick);
    {
        let popover = popover.clone();
        let on_pick = on_pick.clone();
        list.connect_row_activated(move |_, row| {
            let path: Option<std::ptr::NonNull<String>> = unsafe { row.data("folder-path") };
            if let Some(path) = path {
                let path = unsafe { path.as_ref() }.clone();
                popover.popdown();
                on_pick(path);
            }
        });
    }
    {
        let list = list.clone();
        search.connect_search_changed(move |e| {
            let query = e.text().to_lowercase();
            let mut i = 0;
            while let Some(row) = list.row_at_index(i) {
                let name: Option<std::ptr::NonNull<String>> = unsafe { row.data("folder-name") };
                let hit = query.is_empty()
                    || name.is_some_and(|n| unsafe { n.as_ref() }.contains(&query));
                row.set_visible(hit);
                i += 1;
            }
        });
    }
    {
        // Enter files into the first folder still listed.
        let list = list.clone();
        search.connect_activate(move |_| {
            let mut i = 0;
            while let Some(row) = list.row_at_index(i) {
                if row.is_visible() {
                    row.activate();
                    return;
                }
                i += 1;
            }
        });
    }

    popover.set_child(Some(&column));
    popover.set_parent(parent);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.connect_closed(|p| p.unparent());
    popover.popup();
    search.grab_focus();

    // VIREO_SHOWCASE_MOVE=1 captures the picker a second after it opens
    // (the window snapshot never includes a popover).
    if std::env::var("VIREO_SHOWCASE_MOVE").is_ok() {
        if let Ok(path) = std::env::var("VIREO_SHOWCASE") {
            let column = column.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1000), move || {
                crate::app::showcase_capture(column.upcast_ref(), &path);
            });
        }
    }
}
