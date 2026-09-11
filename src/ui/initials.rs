//! The initials circle a sender gets when no picture is known: drawn here
//! rather than by `adw::Avatar`'s label, so the letters sit centred by their
//! ink. A label centres its *logical* box, and a glyph's ink rarely fills
//! that box evenly — a lone "J" or "T" drifts, and pairs lean — which is
//! what this paintable corrects, at every size it is drawn at.
//!
//! Colours are libadwaita's own avatar palette (the same fourteen gradients
//! `adw::Avatar` picks from, chosen by the same hash of the name), so a
//! sender keeps the colour they have always had.

use std::cell::{Cell, RefCell};

use gtk::{gdk, glib, graphene, gsk, pango, prelude::*, subclass::prelude::*};

/// libadwaita's `avatar.color1`…`color14`: gradient top, gradient bottom,
/// text (from its stylesheet; one palette for both themes).
const PALETTE: [(&str, &str, &str); 14] = [
    ("#83b6ec", "#337fdc", "#cfe1f5"),
    ("#7ad9f1", "#0f9ac8", "#caeaf2"),
    ("#8de6b1", "#29ae74", "#cef8d8"),
    ("#b5e98a", "#6ab85b", "#e6f9d7"),
    ("#f8e359", "#d29d09", "#f9f4e1"),
    ("#ffcb62", "#d68400", "#ffead1"),
    ("#ffa95a", "#ed5b00", "#ffe5c5"),
    ("#f78773", "#e62d42", "#f8d2ce"),
    ("#e973ab", "#e33b6a", "#fac7de"),
    ("#cb78d4", "#9945b5", "#e7c2e8"),
    ("#9e91e8", "#7a59ca", "#d5d2f5"),
    ("#e3cf9c", "#b08952", "#f2eade"),
    ("#be916d", "#785336", "#e5d6ca"),
    ("#c0bfbc", "#6e6d71", "#d8d7d3"),
];

/// The palette entry `adw::Avatar` gives `text`: GLib's `g_str_hash`
/// (djb2) modulo the palette, as libadwaita does it.
fn palette_index(text: &str) -> usize {
    let hash = text
        .bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33).wrapping_add(u32::from(b)));
    (hash % PALETTE.len() as u32) as usize
}

/// The initials `adw::Avatar` would show for `text`: the first letter of
/// the first word and of the last, uppercased; nothing for an empty name.
pub fn initials_of(text: &str) -> String {
    let upper = text.trim().to_uppercase();
    let mut words = upper.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric));
    let Some(first) = words.next() else { return String::new() };
    let mut out: String = first.chars().take(1).collect();
    if let Some(last) = words.last() {
        out.extend(last.chars().take(1));
    }
    out
}

thread_local! {
    /// A label to borrow the theme font from (its Pango context carries
    /// the display's font settings), never shown.
    static FONT_SOURCE: gtk::Label = gtk::Label::new(None);
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct InitialsPaintable {
        pub initials: RefCell<String>,
        pub color: Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InitialsPaintable {
        const NAME: &'static str = "VireoInitialsPaintable";
        type Type = super::InitialsPaintable;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for InitialsPaintable {}

    impl PaintableImpl for InitialsPaintable {
        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::SIZE | gdk::PaintableFlags::CONTENTS
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else { return };
            let (top, bottom, fg) = PALETTE[self.color.get() % PALETTE.len()];
            let rgba = |s: &str| gdk::RGBA::parse(s).unwrap_or(gdk::RGBA::BLACK);
            // The circle's ground: the avatar clips this to its disc.
            snapshot.append_linear_gradient(
                &graphene::Rect::new(0.0, 0.0, width as f32, height as f32),
                &graphene::Point::new(0.0, 0.0),
                &graphene::Point::new(0.0, height as f32),
                &[gsk::ColorStop::new(0.0, rgba(top)), gsk::ColorStop::new(1.0, rgba(bottom))],
            );

            let initials = self.initials.borrow();
            if initials.is_empty() {
                return;
            }
            // Bold, at a size that lets a pair sit comfortably inside the
            // disc: the same weight of presence the avatar's own label has.
            let size = width.min(height);
            let px = size * if initials.chars().count() > 1 { 0.42 } else { 0.5 };
            let layout = FONT_SOURCE.with(|l| l.create_pango_layout(Some(initials.as_str())));
            let mut desc = layout.context().font_description().unwrap_or_default();
            desc.set_weight(pango::Weight::Bold);
            desc.set_absolute_size(px * f64::from(pango::SCALE));
            layout.set_font_description(Some(&desc));

            // Centre the ink, not the logical box.
            let scale = f64::from(pango::SCALE);
            let (ink, _) = layout.extents();
            let ink_x = f64::from(ink.x()) / scale;
            let ink_y = f64::from(ink.y()) / scale;
            let ink_w = f64::from(ink.width()) / scale;
            let ink_h = f64::from(ink.height()) / scale;
            let dx = (width - ink_w) / 2.0 - ink_x;
            let dy = (height - ink_h) / 2.0 - ink_y;
            snapshot.save();
            snapshot.translate(&graphene::Point::new(dx as f32, dy as f32));
            snapshot.append_layout(&layout, &rgba(fg));
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct InitialsPaintable(ObjectSubclass<imp::InitialsPaintable>)
        @implements gdk::Paintable;
}

impl InitialsPaintable {
    /// The circle for `name` (a sender's display name): its initials in the
    /// colour libadwaita would give that name. `None` when there is no
    /// letter to show, so the avatar falls back to its silhouette.
    pub fn for_name(name: &str) -> Option<Self> {
        let initials = initials_of(name);
        if initials.is_empty() {
            return None;
        }
        let this: Self = glib::Object::new();
        this.imp().initials.replace(initials);
        this.imp().color.set(palette_index(name));
        Some(this)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_follow_the_avatar_rules() {
        assert_eq!(initials_of("Marcus Chen"), "MC");
        assert_eq!(initials_of("  sophie   turner "), "ST");
        assert_eq!(initials_of("Apple"), "A");
        assert_eq!(initials_of("GNOME Foundation Board"), "GB");
        assert_eq!(initials_of("émile zola"), "ÉZ");
        assert_eq!(initials_of("  "), "");
        // GLib's djb2, as libadwaita hashes the name for its colour.
        assert_eq!(palette_index(""), (5381u32 % 14) as usize);
        assert!(palette_index("Marcus Chen") < PALETTE.len());
    }
}
