//! The pieces every wizard page is made of: the scrolling page, its title
//! and tagline, cards, pills, and the entrance animation.

use super::*;

/// Wrap a page so a tall one scrolls instead of forcing the window taller
/// (the carousel's minimum height is its tallest page's).
pub(super) fn scrolled(inner: &gtk::Box) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    // ScrolledWindow deliberately doesn't propagate its child's expand flags
    // (it is the scrollable boundary), so without these the carousel sizes
    // the page to its natural width and neighbours peek in at the edges.
    sw.set_hexpand(true);
    sw.set_vexpand(true);
    sw.set_child(Some(inner));
    sw
}

/// One wizard page: vertically centered content in consistent margins.
pub(super) fn page(content: &gtk::Box) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.set_valign(gtk::Align::Center);
    outer.set_halign(gtk::Align::Center);
    outer.set_margin_top(24);
    outer.set_margin_bottom(24);
    outer.set_margin_start(48);
    outer.set_margin_end(48);
    outer.set_hexpand(true);
    outer.set_vexpand(true);
    outer.append(content);
    outer
}

pub(super) fn title(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("welcome-title");
    l.set_halign(gtk::Align::Center);
    l
}

pub(super) fn tagline(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("welcome-tagline");
    l.set_halign(gtk::Align::Center);
    l.set_justify(gtk::Justification::Center);
    l.set_wrap(true);
    l
}

pub(super) fn card() -> gtk::ListBox {
    let lb = gtk::ListBox::new();
    lb.add_css_class("boxed-list");
    lb.add_css_class("welcome-card");
    lb.set_selection_mode(gtk::SelectionMode::None);
    lb
}

pub(super) fn pill(label: &str) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class("pill");
    b.add_css_class("suggested-action");
    b.add_css_class("welcome-pill");
    b.set_halign(gtk::Align::Center);
    b
}

/// Fade-and-rise entrance for the hero page, staggered per widget. Runs on the
/// window's first map (animations on unmapped widgets skip to the end).
pub(super) fn entrance(widgets: &[gtk::Widget]) {
    for (i, w) in widgets.iter().cloned().enumerate() {
        w.set_opacity(0.0);
        // The rise is relative: each widget ends back on its OWN margin, so
        // layout spacing set at build time survives the animation.
        let base = w.margin_top();
        gtk::glib::timeout_add_local_once(
            std::time::Duration::from_millis(120 + 140 * i as u64),
            move || {
                let target = adw::CallbackAnimationTarget::new({
                    let w = w.clone();
                    move |v| {
                        w.set_opacity(v);
                        w.set_margin_top(base + ((1.0 - v) * 24.0) as i32);
                    }
                });
                let anim = adw::TimedAnimation::new(&w, 0.0, 1.0, 600, target);
                anim.set_easing(adw::Easing::EaseOutCubic);
                anim.play();
            },
        );
    }
}
